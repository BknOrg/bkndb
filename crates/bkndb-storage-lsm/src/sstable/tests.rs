use super::*;

const SMALL: WriteOptions = WriteOptions { block_size: 64, compress: true };

fn sample_entries() -> Vec<(Vec<u8>, LsmValue)> {
    (0u32..50)
        .map(|i| (i.to_be_bytes().to_vec(), LsmValue::Value(format!("value-{i}-{}", "x".repeat(20)).into_bytes())))
        .collect()
}

fn ok_entries(v: &[(Vec<u8>, LsmValue)]) -> impl Iterator<Item = Result<(Vec<u8>, LsmValue), BknError>> + '_ {
    v.iter().cloned().map(Ok)
}

fn temp_file() -> (tempfile::TempDir, Arc<File>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("container.bkndb");
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).unwrap();
    (dir, Arc::new(file))
}

fn collect(c: SstCursor) -> Vec<u32> {
    c.map(|r| u32::from_be_bytes(r.unwrap().0.as_slice().try_into().unwrap())).collect()
}

#[test]
fn write_then_open_get_roundtrip() {
    for opts in [SMALL, WriteOptions { block_size: 4096, compress: false }] {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), opts).unwrap();

        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
        assert!(!handle.is_legacy());
        for (k, v) in &entries {
            assert_eq!(handle.get(k).unwrap().as_ref(), Some(v));
        }
        assert_eq!(handle.get(&999u32.to_be_bytes()).unwrap(), None);
        let counts = handle.verify().unwrap();
        assert_eq!(counts.entries, 50);
        assert_eq!(counts.blocks_unchecked, 0);
    }
}

#[test]
fn small_blocks_compress_and_split() {
    let (_dir, file) = temp_file();
    let entries = sample_entries();
    let h = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
    assert!(h.blocks.len() > 10, "64-byte blocks must split 50 entries into many blocks");
    let raw = SstableHandle::write(&file, 2, ok_entries(&entries), entries.len(), WriteOptions { block_size: 4096, compress: false }).unwrap();
    let packed = SstableHandle::write(&file, 3, ok_entries(&entries), entries.len(), WriteOptions { block_size: 4096, compress: true }).unwrap();
    assert!(packed.blob_len < raw.blob_len, "repetitive values must compress");
}

#[test]
fn cursor_respects_bounds_across_blocks() {
    let (_dir, file) = temp_file();
    let entries = sample_entries();
    let h = Arc::new(SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap());

    let key = |i: u32| i.to_be_bytes().to_vec();
    assert_eq!(collect(h.cursor(Bound::Included(key(10)), Bound::Excluded(key(15)))), vec![10, 11, 12, 13, 14]);
    assert_eq!(collect(h.cursor(Bound::Excluded(key(47)), Bound::Unbounded)), vec![48, 49]);
    assert_eq!(collect(h.cursor(Bound::Unbounded, Bound::Included(key(2)))), vec![0, 1, 2]);
    assert_eq!(collect(h.cursor(Bound::Included(key(60)), Bound::Unbounded)), Vec::<u32>::new());
    assert_eq!(collect(h.cursor(Bound::Unbounded, Bound::Unbounded)).len(), 50);
}

#[test]
fn tombstones_round_trip() {
    let (_dir, file) = temp_file();
    let entries = vec![(b"a".to_vec(), LsmValue::Value(b"1".to_vec())), (b"b".to_vec(), LsmValue::Tombstone)];
    let written = SstableHandle::write(&file, 1, ok_entries(&entries), 2, SMALL).unwrap();
    let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
    assert_eq!(handle.get(b"b").unwrap(), Some(LsmValue::Tombstone));
}

#[test]
fn empty_sstable_is_valid() {
    let (_dir, file) = temp_file();
    let written = SstableHandle::write(&file, 1, std::iter::empty(), 0, SMALL).unwrap();
    let handle = Arc::new(SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap());
    assert_eq!(handle.get(b"a").unwrap(), None);
    assert_eq!(handle.cursor(Bound::Unbounded, Bound::Unbounded).count(), 0);
}

#[test]
fn two_blobs_coexist_in_one_shared_file() {
    let (_dir, file) = temp_file();
    let first = vec![(b"a".to_vec(), LsmValue::Value(b"first-a".to_vec()))];
    let second = vec![(b"a".to_vec(), LsmValue::Value(b"second-a".to_vec()))];

    let w1 = SstableHandle::write(&file, 1, ok_entries(&first), 1, SMALL).unwrap();
    let w2 = SstableHandle::write(&file, 2, ok_entries(&second), 1, SMALL).unwrap();
    assert!(w2.base_offset > w1.base_offset, "the second blob must be appended after the first");

    let h1 = SstableHandle::open(&file, 1, w1.base_offset, w1.blob_len).unwrap();
    let h2 = SstableHandle::open(&file, 2, w2.base_offset, w2.blob_len).unwrap();
    assert_eq!(h1.get(b"a").unwrap(), Some(LsmValue::Value(b"first-a".to_vec())));
    assert_eq!(h2.get(b"a").unwrap(), Some(LsmValue::Value(b"second-a".to_vec())));
}

#[test]
fn flipped_data_byte_is_reported_as_corruption() {
    let (_dir, file) = temp_file();
    let entries = sample_entries();
    let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
    let victim = written.base_offset + 3;
    let mut byte = [0u8];
    (&*file).seek(SeekFrom::Start(victim)).unwrap();
    (&*file).read_exact(&mut byte).unwrap();
    (&*file).seek(SeekFrom::Start(victim)).unwrap();
    (&*file).write_all(&[byte[0] ^ 0xFF]).unwrap();

    let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
    assert!(matches!(handle.get(&0u32.to_be_bytes()), Err(BknError::Corruption(_))));
    assert!(matches!(handle.verify(), Err(BknError::Corruption(_))));
    // Blocks that weren't touched still read fine.
    assert!(handle.get(&49u32.to_be_bytes()).unwrap().is_some());
}

#[test]
fn flipped_index_byte_fails_open() {
    let (_dir, file) = temp_file();
    let entries = sample_entries();
    let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
    let victim = written.base_offset + written.blob_len - FOOTER_V2_LEN - 2;
    (&*file).seek(SeekFrom::Start(victim)).unwrap();
    (&*file).write_all(&[0xAB]).unwrap();
    assert!(matches!(SstableHandle::open(&file, 1, written.base_offset, written.blob_len), Err(BknError::Corruption(_))));
}

#[test]
fn legacy_v1_blobs_remain_readable() {
    let (_dir, file) = temp_file();
    let entries = sample_entries();
    let (base, len) = write_v1(&file, &entries, 4);
    let handle = Arc::new(SstableHandle::open(&file, 1, base, len).unwrap());
    assert!(handle.is_legacy());
    for (k, v) in &entries {
        assert_eq!(handle.get(k).unwrap().as_ref(), Some(v));
    }
    let key = |i: u32| i.to_be_bytes().to_vec();
    assert_eq!(collect(handle.cursor(Bound::Included(key(5)), Bound::Excluded(key(9)))), vec![5, 6, 7, 8]);
    let counts = handle.verify().unwrap();
    assert_eq!((counts.entries, counts.blocks_verified), (50, 0));
}
