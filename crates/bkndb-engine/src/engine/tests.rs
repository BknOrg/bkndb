use super::*;
use crate::Engine;

const T: TableSpec = TableSpec("t");

/// Builds a complete database file exactly as format v1 laid it out:
/// one uncompressed, checksum-free SSTable, a manifest, a v1 header.
fn write_legacy_database(path: &Path, rows: &[(Vec<u8>, Vec<u8>)]) {
    let file = container::open_container_file(path).unwrap();
    file.set_len(container::BODY_START).unwrap();
    let entries: Vec<(Vec<u8>, LsmValue)> = rows
        .iter()
        .map(|(k, v)| (keys::encode_key(T, k), LsmValue::Value(v.clone())))
        .collect();
    let (offset, length) = crate::sstable::write_v1(&file, &entries, 4);
    let placeholder = Manifest {
        next_sstable_id: 2,
        sstables: vec![SstableRef {
            generation: 1,
            offset,
            length,
        }],
        wal_region_start: 0,
    };
    let manifest_offset = offset + length;
    let len = placeholder.encode().unwrap().len() as u64;
    let bytes = Manifest {
        wal_region_start: manifest_offset + len,
        ..placeholder
    }
    .encode()
    .unwrap();
    container::write_blob_at(&file, manifest_offset, &bytes).unwrap();
    container::write_legacy_header(&file, 1, manifest_offset, bytes.len() as u64);
    file.sync_all().unwrap();
}

#[test]
fn legacy_is_read_only_and_migrates_without_changing_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.bkndb");
    let rows: Vec<(Vec<u8>, Vec<u8>)> = (0u32..40)
        .map(|i| (i.to_be_bytes().to_vec(), format!("v{i}").into_bytes()))
        .collect();
    write_legacy_database(&path, &rows);
    let original = std::fs::read(&path).unwrap();
    let db = LsmStorageBackend::open(&path).unwrap();
    assert_eq!(
        db.begin_read()
            .unwrap()
            .range(T, Bound::Unbounded, Bound::Unbounded)
            .unwrap(),
        rows
    );
    assert!(matches!(
        db.begin_write(),
        Err(BknError::UnsupportedFormat(_))
    ));
    assert!(db.force_compact().is_err());
    let dest = dir.path().join("migrated.bkndb");
    db.migrate_to(&dest).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let migrated = LsmStorageBackend::open(&dest).unwrap();
    assert_eq!(migrated.stats().unwrap().legacy_sstable_count, 0);
    assert_eq!(
        migrated
            .begin_read()
            .unwrap()
            .range(T, Bound::Unbounded, Bound::Unbounded)
            .unwrap(),
        rows
    );
    let mut tx = migrated.begin_write().unwrap();
    tx.put(T, b"new", b"value").unwrap();
    tx.commit().unwrap();
    migrated.verify_integrity().unwrap();
}

#[test]
fn invalid_live_data_preserves_a_stale_compaction_recovery_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    {
        let db = Engine::open_with_options(
            &path,
            LsmOptions {
                memtable_flush_bytes: 1,
                compaction_trigger_files: 100,
                block_size_bytes: 128,
                compression: false,
            },
        )
        .unwrap();
        let mut tx = db.begin_write().unwrap();
        tx.put(T, b"key", b"value").unwrap();
        tx.commit().unwrap();
    }
    let tmp = compaction_tmp_path(&path);
    std::fs::copy(&path, &tmp).unwrap();
    let recovery = std::fs::read(&tmp).unwrap();
    let file = container::open_container_file(&path).unwrap();
    let header = container::read_header(&file).unwrap().unwrap();
    let manifest = Manifest::decode(&container::read_manifest(&file, &header).unwrap()).unwrap();
    let offset = manifest.sstables[0].offset;
    let mut byte = [0];
    file.read_exact_at(offset, &mut byte).unwrap();
    file.write_all_at(offset, &[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    assert!(
        Engine::open(&path).is_err(),
        "corrupt data must not authorize removal of a recovery candidate"
    );
    assert_eq!(std::fs::read(&tmp).unwrap(), recovery);
}

#[test]
fn empty_live_file_does_not_discard_a_compaction_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let tmp = compaction_tmp_path(&path);
    std::fs::write(&path, []).unwrap();
    std::fs::write(&tmp, b"potential recovery copy").unwrap();
    assert!(matches!(
        Engine::open(&path),
        Err(BknError::RecoveryRequired(_))
    ));
    assert!(std::fs::read(&path).unwrap().is_empty());
    assert_eq!(std::fs::read(&tmp).unwrap(), b"potential recovery copy");
}
#[test]
fn repair_does_not_initialize_an_empty_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty");
    let dest = dir.path().join("destination");
    std::fs::write(&path, []).unwrap();
    assert!(Engine::repair_to(&path, &dest).is_err());
    assert!(std::fs::read(&path).unwrap().is_empty());
    assert!(!dest.exists());
}
