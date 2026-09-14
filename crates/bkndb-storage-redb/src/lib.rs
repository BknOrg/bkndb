use std::ops::Bound;
use std::path::Path;

use bkndb_core::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};
use redb::{Database, ReadTransaction, ReadableTable, TableError, WriteTransaction};

/// Disk-backed storage engine used on desktop, mobile, and embedded Linux.
pub struct RedbStorageBackend {
    db: Database,
}

impl RedbStorageBackend {
    /// Opens the `.bkndb` file at `path`, creating it if it does not exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BknError> {
        let db = Database::create(path).map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(Self { db })
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
            .begin_read()
            .map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(RedbReadTx {
            tx,
            _marker: std::marker::PhantomData,
        })
    }

    fn begin_write(&self) -> Result<Self::WriteTx<'_>, BknError> {
        let tx = self
            .db
            .begin_write()
            .map_err(|e| BknError::Backend(e.to_string()))?;
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
            Err(e) => return Err(BknError::Backend(e.to_string())),
        };
        let value = redb_table
            .get(key)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(value.map(|guard| guard.value().to_vec()))
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(Vec::new()),
            Err(e) => return Err(BknError::Backend(e.to_string())),
        };
        let iter = redb_table
            .range::<&[u8]>((start, end))
            .map_err(|e| BknError::Backend(e.to_string()))?;
        let mut out = Vec::new();
        for entry in iter {
            let (k, v) = entry.map_err(|e| BknError::Backend(e.to_string()))?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

impl StorageReadTx for RedbWriteTx<'_> {
    fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(None),
            Err(e) => return Err(BknError::Backend(e.to_string())),
        };
        let value = redb_table
            .get(key)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(value.map(|guard| guard.value().to_vec()))
    }

    fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        let def = table_def(table);
        let redb_table = match self.tx.open_table(def) {
            Ok(t) => t,
            Err(ref e) if is_missing_table(e) => return Ok(Vec::new()),
            Err(e) => return Err(BknError::Backend(e.to_string())),
        };
        let iter = redb_table
            .range::<&[u8]>((start, end))
            .map_err(|e| BknError::Backend(e.to_string()))?;
        let mut out = Vec::new();
        for entry in iter {
            let (k, v) = entry.map_err(|e| BknError::Backend(e.to_string()))?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

impl StorageWriteTx for RedbWriteTx<'_> {
    fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        let def = table_def(table);
        let mut redb_table = self
            .tx
            .open_table(def)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        redb_table
            .insert(key, value)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(())
    }

    fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        let def = table_def(table);
        let mut redb_table = self
            .tx
            .open_table(def)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        redb_table
            .remove(key)
            .map_err(|e| BknError::Backend(e.to_string()))?;
        Ok(())
    }

    fn commit(self) -> Result<(), BknError> {
        self.tx
            .commit()
            .map_err(|e| BknError::Backend(e.to_string()))
    }
}
