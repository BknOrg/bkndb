//! Explicit snapshot conversions into a new file; source and destination formats
//! are never inferred from a .bkndb extension or overwritten in place.
use crate::{LsmStorageBackend, RedbStorageBackend};
use bkndb_core::BknError;
use bkndb_engine::{
    Engine, StorageWriteTx, TableSpec,
    io::{NativeIo, OpenMode, StorageIo},
};
use std::{path::Path, sync::Arc};
fn error(e: impl std::fmt::Display) -> BknError {
    BknError::Backend(e.to_string())
}
fn target(dest: &Path) -> Result<std::path::PathBuf, BknError> {
    if dest.try_exists().map_err(error)? {
        return Err(BknError::InvalidArgument(
            "migration destination exists".into(),
        ));
    }
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        NativeIo.create_dir_all(parent).map_err(error)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".migrate.tmp");
    let tmp = std::path::PathBuf::from(tmp);
    drop(NativeIo.open(&tmp, OpenMode::CreateNew).map_err(error)?);
    Ok(tmp)
}
/// Export an already-open Redb snapshot to a verified new LSM file. The caller
/// controls opening/closing the source; no implicit Redb recovery is performed.
pub fn redb_to_lsm(source: &RedbStorageBackend, dest: impl AsRef<Path>) -> Result<(), BknError> {
    let dest = dest.as_ref();
    let tmp = target(dest)?;
    let result = (|| {
        let db =
            Engine::open_with_io(&tmp, Default::default(), Arc::new(NativeIo)).map_err(error)?;
        let mut tx = Some(db.begin_write_owned().map_err(error)?);
        let mut size = 0;
        source.export_to(|table, key, value| {
            tx.as_mut()
                .unwrap()
                .put(TableSpec(table), key, value)
                .map_err(error)?;
            size += table.len() + key.len() + value.len() + 32;
            if size >= 4 * 1024 * 1024 {
                tx.take().unwrap().commit().map_err(error)?;
                tx = Some(db.begin_write_owned().map_err(error)?);
                size = 0;
            }
            Ok(())
        })?;
        tx.take().unwrap().commit().map_err(error)?;
        db.force_compact().map_err(error)?;
        db.verify_integrity().map_err(error)?;
        drop(db);
        NativeIo.publish(&tmp, dest).map_err(error)?;
        NativeIo.sync_parent(dest).map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    let mut lock = tmp.into_os_string();
    lock.push(".lock");
    let _ = std::fs::remove_file(lock);
    result
}
/// Export an LSM snapshot to a verified new Redb file.
pub fn lsm_to_redb(source: &LsmStorageBackend, dest: impl AsRef<Path>) -> Result<(), BknError> {
    let dest = dest.as_ref();
    let tmp = target(dest)?;
    let result = (|| {
        let db = RedbStorageBackend::open(&tmp)?;
        let mut batch = Vec::new();
        let mut size = 0;
        source
            .engine()
            .export_to(|table, key, value| {
                size += table.len() + key.len() + value.len() + 32;
                batch.push((table.to_owned(), key.to_vec(), value.to_vec()));
                if size >= 4 * 1024 * 1024 {
                    db.import_batch(&batch)
                        .map_err(|e| bkndb_engine::Error::Backend(e.to_string()))?;
                    batch.clear();
                    size = 0;
                }
                Ok(())
            })
            .map_err(error)?;
        db.import_batch(&batch)?;
        db.verify_integrity()?;
        drop(db);
        NativeIo.publish(&tmp, dest).map_err(error)?;
        NativeIo.sync_parent(dest).map_err(error)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
