use super::*;

const T: TableSpec = TableSpec("t");

/// Builds a complete database file exactly as format v1 laid it out:
/// one uncompressed, checksum-free SSTable, a manifest, a v1 header.
fn write_legacy_database(path: &Path, rows: &[(Vec<u8>, Vec<u8>)]) {
    let file = container::open_container_file(path).unwrap();
    file.set_len(container::BODY_START).unwrap();
    let entries: Vec<(Vec<u8>, LsmValue)> = rows.iter().map(|(k, v)| (keys::encode_key(T, k), LsmValue::Value(v.clone()))).collect();
    let (offset, length) = crate::sstable::write_v1(&file, &entries, 4);
    let placeholder = Manifest {
        next_sstable_id: 2,
        sstables: vec![SstableRef { generation: 1, offset, length }],
        wal_region_start: 0,
    };
    let manifest_offset = offset + length;
    let len = placeholder.encode().unwrap().len() as u64;
    let bytes = Manifest { wal_region_start: manifest_offset + len, ..placeholder }.encode().unwrap();
    container::write_blob_at(&file, manifest_offset, &bytes).unwrap();
    container::write_legacy_header(&file, 1, manifest_offset, bytes.len() as u64);
    file.sync_all().unwrap();
}

#[test]
fn legacy_v1_database_opens_and_is_upgraded_by_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.bkndb");
    let rows: Vec<(Vec<u8>, Vec<u8>)> = (0u32..40).map(|i| (i.to_be_bytes().to_vec(), format!("v{i}").into_bytes())).collect();
    write_legacy_database(&path, &rows);

    {
        let db = LsmStorageBackend::open(&path).unwrap();
        let stats = db.stats().unwrap();
        assert_eq!((stats.sstable_count, stats.legacy_sstable_count), (1, 1));
        let r = db.begin_read().unwrap();
        assert_eq!(r.get(T, &7u32.to_be_bytes()).unwrap(), Some(b"v7".to_vec()));
        assert_eq!(r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap(), rows);
        let report = db.verify_integrity().unwrap();
        assert_eq!((report.entries, report.blocks_verified), (40, 0));
        assert!(report.legacy_blocks_unchecked > 0);

        // New commits land in a v2 SSTable next to the legacy one.
        let mut w = db.begin_write().unwrap();
        w.put(T, &100u32.to_be_bytes(), b"new").unwrap();
        w.delete(T, &0u32.to_be_bytes()).unwrap();
        w.commit().unwrap();
        db.force_compact().unwrap();
        let stats = db.stats().unwrap();
        assert_eq!((stats.sstable_count, stats.legacy_sstable_count), (1, 0));
    }

    let db = LsmStorageBackend::open(&path).unwrap();
    let r = db.begin_read().unwrap();
    let all = r.range(T, Bound::Unbounded, Bound::Unbounded).unwrap();
    assert_eq!(all.len(), 40);
    assert_eq!(all[0].0, 1u32.to_be_bytes().to_vec());
    assert_eq!(r.get(T, &100u32.to_be_bytes()).unwrap(), Some(b"new".to_vec()));
    let report = db.verify_integrity().unwrap();
    assert_eq!((report.legacy_blocks_unchecked, report.entries), (0, 40));
}

#[test]
fn force_compact_upgrades_a_lone_legacy_sstable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.bkndb");
    write_legacy_database(&path, &[(b"k".to_vec(), b"v".to_vec())]);
    let db = LsmStorageBackend::open(&path).unwrap();
    db.force_compact().unwrap();
    assert_eq!(db.stats().unwrap().legacy_sstable_count, 0);
    assert_eq!(db.begin_read().unwrap().get(T, b"k").unwrap(), Some(b"v".to_vec()));
}
