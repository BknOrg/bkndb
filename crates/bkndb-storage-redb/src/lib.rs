use std::fs;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use bkndb_core::{BknError, KvIter, KvPairs, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use redb::{
    CommitError, CompactionError, Database, DatabaseError, ReadTransaction, ReadableTable,
    ReadableTableMetadata, StorageError, TableError, TableHandle, TransactionError,
    WriteTransaction,
};

/// Point-in-time storage figures for a Redb-backed database file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedbStats {
    /// Size of the `.bkndb` file on disk.
    pub file_bytes: u64,
    /// Number of active tables (minimum 1 for an initialized on-disk database).
    pub table_count: usize,
    /// Bytes stored across all user and system tables (including B-tree metadata).
    pub stored_bytes: u64,
    /// Total key-value entries across all tables.
    pub entries: u64,
    /// Reclaimable bytes outside active structures.
    pub reclaimable_bytes: u64,
}

/// Summary returned by [`RedbStorageBackend::verify_integrity`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrityReport {
    pub sstables_checked: usize,
    pub blocks_verified: u64,
    pub legacy_blocks_unchecked: u64,
    pub entries: u64,
    pub wal_records: u64,
}

fn map_storage_err(err: StorageError) -> BknError {
    match err {
        StorageError::Corrupted(msg) => BknError::Corruption(msg),
        other => BknError::Backend(other.to_string()),
    }
}

fn map_db_err(err: DatabaseError, path: &Path) -> BknError {
    match err {
        DatabaseError::DatabaseAlreadyOpen => BknError::DatabaseLocked(path.display().to_string()),
        DatabaseError::Storage(s) => map_storage_err(s),
        DatabaseError::UpgradeRequired(v) => {
            BknError::Corruption(format!("database format upgrade required (version {v})"))
        }
        other => BknError::Backend(other.to_string()),
    }
}

fn map_tx_err(err: TransactionError) -> BknError {
    match err {
        TransactionError::Storage(s) => map_storage_err(s),
        other => BknError::Backend(other.to_string()),
    }
}

fn map_table_err(err: TableError) -> BknError {
    match err {
        TableError::Storage(s) => map_storage_err(s),
        other => BknError::Backend(other.to_string()),
    }
}

fn map_commit_err(err: CommitError) -> BknError {
    match err {
        CommitError::Storage(s) => map_storage_err(s),
        other => BknError::Backend(other.to_string()),
    }
}

fn map_compaction_err(err: CompactionError) -> BknError {
    match err {
        CompactionError::Storage(s) => map_storage_err(s),
        other => BknError::Backend(other.to_string()),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Disk-backed storage engine used on desktop, mobile, and embedded Linux.
pub struct RedbStorageBackend {
    path: PathBuf,
    db: RwLock<Database>,
}

impl RedbStorageBackend {
    /// Opens the `.bkndb` file at `path`, creating it if it does not exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BknError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| BknError::Backend(e.to_string()))?;
        }
        let db = Database::create(&path).map_err(|e| map_db_err(e, &path))?;
        Ok(Self {
            path,
            db: RwLock::new(db),
        })
    }

    /// Returns the database file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the current size of the database file in bytes.
    pub fn file_size(&self) -> Result<u64, BknError> {
        Ok(fs::metadata(&self.path)
            .map_err(|e| BknError::Backend(e.to_string()))?
            .len())
    }

    /// Forces compaction of the Redb database file to reclaim free pages.
    pub fn force_compact(&self) -> Result<(), BknError> {
        let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
        db.compact().map_err(map_compaction_err)?;
        Ok(())
    }

    /// Writes a consistent snapshot copy of the database to `dest`.
    /// Fails with [`BknError::Backend`] if `dest` already exists.
    pub fn backup_to(&self, dest: impl AsRef<Path>) -> Result<(), BknError> {
        let dest = dest.as_ref();
        if dest.exists() {
            return Err(BknError::Backend(format!(
                "backup target '{}' already exists",
                dest.display()
            )));
        }
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| BknError::Backend(e.to_string()))?;
        }

        let read_tx = {
            let db = self.db.read().unwrap_or_else(|e| e.into_inner());
            db.begin_read().map_err(map_tx_err)?
        };

        let tmp = with_suffix(dest, ".backup.tmp");
        let _ = fs::remove_file(&tmp);

        let copy_res = (|| -> Result<(), BknError> {
            let dest_db = Database::create(&tmp).map_err(|e| map_db_err(e, &tmp))?;
            let write_tx = dest_db.begin_write().map_err(map_tx_err)?;
            {
                let tables = read_tx.list_tables().map_err(map_storage_err)?;
                for handle in tables {
                    let name = handle.name();
                    let def: redb::TableDefinition<'_, &'static [u8], &'static [u8]> =
                        redb::TableDefinition::new(name);
                    let src_table = read_tx.open_table(def).map_err(map_table_err)?;
                    let mut dst_table = write_tx.open_table(def).map_err(map_table_err)?;
                    let iter = src_table
                        .range::<&[u8]>(..)
                        .map_err(map_storage_err)?;
                    for entry in iter {
                        let (k, v) = entry.map_err(map_storage_err)?;
                        dst_table
                            .insert(k.value(), v.value())
                            .map_err(map_storage_err)?;
                    }
                }
            }
            write_tx.commit().map_err(map_commit_err)?;
            // Explicitly drop `dest_db` before renaming so the file lock is released on Windows.
            drop(dest_db);
            fs::rename(&tmp, dest).map_err(|e| BknError::Backend(e.to_string()))?;
            Ok(())
        })();

        if copy_res.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        copy_res
    }

    /// Computes point-in-time file and table statistics.
    pub fn stats(&self) -> Result<RedbStats, BknError> {
        let file_bytes = self.file_size()?;
        let read_tx = {
            let db = self.db.read().unwrap_or_else(|e| e.into_inner());
            db.begin_read().map_err(map_tx_err)?
        };
        let mut table_count = 0usize;
        let mut stored_bytes = 0u64;
        let mut entries = 0u64;

        for handle in read_tx.list_tables().map_err(map_storage_err)? {
            table_count += 1;
            let name = handle.name();
            let def: redb::TableDefinition<'_, &'static [u8], &'static [u8]> =
                redb::TableDefinition::new(name);
            let table = read_tx.open_table(def).map_err(map_table_err)?;
            entries += table.len().map_err(map_storage_err)?;
            let st = table.stats().map_err(map_storage_err)?;
            stored_bytes += st.stored_bytes() + st.metadata_bytes();
        }

        Ok(RedbStats {
            file_bytes,
            table_count: table_count.max(1),
            stored_bytes,
            entries,
            reclaimable_bytes: 0,
        })
    }

    /// Verifies database integrity by running Redb's page check and scanning
    /// every stored table and key-value entry.
    pub fn verify_integrity(&self) -> Result<IntegrityReport, BknError> {
        {
            let mut db = self.db.write().unwrap_or_else(|e| e.into_inner());
            db.check_integrity().map_err(|e| map_db_err(e, &self.path))?;
        }

        let read_tx = {
            let db = self.db.read().unwrap_or_else(|e| e.into_inner());
            db.begin_read().map_err(map_tx_err)?
        };

        let mut tables_checked = 0usize;
        let mut pages_verified = 0u64;
        let mut entries = 0u64;

        for handle in read_tx.list_tables().map_err(map_storage_err)? {
            tables_checked += 1;
            let name = handle.name();
            let def: redb::TableDefinition<'_, &'static [u8], &'static [u8]> =
                redb::TableDefinition::new(name);
            let table = read_tx.open_table(def).map_err(map_table_err)?;
            let st = table.stats().map_err(map_storage_err)?;
            pages_verified += st.leaf_pages() + st.branch_pages();

            let iter = table
                .range::<&[u8]>(..)
                .map_err(map_storage_err)?;
            for entry in iter {
                let (k, v) = entry.map_err(map_storage_err)?;
                std::hint::black_box((k.value(), v.value()));
                entries += 1;
            }
        }

        Ok(IntegrityReport {
            sstables_checked: tables_checked.max(1),
            blocks_verified: pages_verified.max(1),
            legacy_blocks_unchecked: 0,
            entries,
            wal_records: 0,
        })
    }
}

fn table_def(table: TableSpec) -> redb::TableDefinition<'static, &'static [u8], &'static [u8]> {
    redb::TableDefinition::new(table.0)
}

pub struct RedbReadTx<'a> {
    tx: ReadTransaction,
    _marker: std::marker::PhantomData<&'a RedbStorageBackend>,
}

pub struct RedbWriteTx<'a> {
    tx: WriteTransaction,
    _marker: std::marker::PhantomData<&'a RedbStorageBackend>,
}

impl StorageBackend for RedbStorageBackend {
    type ReadTx<'a> = RedbReadTx<'a>;
    type WriteTx<'a> = RedbWriteTx<'a>;

    fn begin_read(&self) -> Result<Self::ReadTx<'_>, BknError> {
        let tx = self
            .db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .begin_read()
            .map_err(map_tx_err)?;
        Ok(RedbReadTx {
            tx,
            _marker: std::marker::PhantomData,
        })
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        let tx = self
            .db
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .begin_write()
            .map_err(map_tx_err)?;
        Ok(RedbWriteTx {
            tx,
            _marker: std::marker::PhantomData,
        })
    }
}

/// A table that has never been written to doesn't exist yet in redb, and
/// opening it for reads errors out. That must read as "key not found", not
/// as a hard error, or a fresh database would fail on its very first `get`.
fn is_missing_table(err: &TableError) -> bool {
    matches!(err, TableError::TableDoesNotExist(_))
}

impl StorageReadTx for RedbReadTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(None),
            Err(e) => return Err(map_table_err(e)),
        };
        let value = redb_table.get(key).map_err(map_storage_err)?;
        Ok(value.map(|guard| guard.value().to_vec()))
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(Vec::new()),
            Err(e) => return Err(map_table_err(e)),
        };
        let iter = redb_table
            .range::<&[u8]>((start, end))
            .map_err(map_storage_err)?;
        let mut out = Vec::new();
        for entry in iter {
            let (k, v) = entry.map_err(map_storage_err)?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }

    /// Streams straight from redb's B-tree cursor: a read transaction's
    /// table handle is owned (`'static`), so the iterator needn't borrow it.
    fn scan<'a>(&'a self, table: TableSpec, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<KvIter<'a>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(Box::new(std::iter::empty())),
            Err(e) => return Err(map_table_err(e)),
        };
        let iter = redb_table
            .range::<&[u8]>((start, end))
            .map_err(map_storage_err)?;
        Ok(Box::new(iter.map(|entry| {
            let (k, v) = entry.map_err(map_storage_err)?;
            Ok((k.value().to_vec(), v.value().to_vec()))
        })))
    }
}

impl StorageReadTx for RedbWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(None),
            Err(e) => return Err(map_table_err(e)),
        };
        let value = redb_table.get(key).map_err(map_storage_err)?;
        Ok(value.map(|guard| guard.value().to_vec()))
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(Vec::new()),
            Err(e) => return Err(map_table_err(e)),
        };
        let iter = redb_table
            .range::<&[u8]>((start, end))
            .map_err(map_storage_err)?;
        let mut out = Vec::new();
        for entry in iter {
            let (k, v) = entry.map_err(map_storage_err)?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

impl StorageWriteTx for RedbWriteTx<'_> {
    fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        let def = table_def(table);
        let mut redb_table = self.tx.open_table(def).map_err(map_table_err)?;
        redb_table.insert(key, value).map_err(map_storage_err)?;
        Ok(())
    }

    fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        let def = table_def(table);
        let mut redb_table = self.tx.open_table(def).map_err(map_table_err)?;
        redb_table.remove(key).map_err(map_storage_err)?;
        Ok(())
    }

    fn commit(self) -> Result<(), BknError> {
        self.tx.commit().map_err(map_commit_err)
    }
}
