//! Borrowing sibling of [`crate::kv::Kv`] for use inside an already-open
//! batch transaction — same validated get/put/delete/range surface, but
//! reading/writing through the caller's already-open transaction instead of
//! opening/committing its own.

use std::ops::Bound;

use crate::{check_table_name, BknError, StorageReadTx, StorageWriteTx, TableSpec};

pub struct ReadKv<'s, R: StorageReadTx> {
    rtx: &'s R,
}

impl<'s, R: StorageReadTx> ReadKv<'s, R> {
    pub(crate) fn new(rtx: &'s R) -> Self {
        Self { rtx }
    }

    pub fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        check_table_name(table.0)?;
        self.rtx.get(table, key)
    }

    pub fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        check_table_name(table.0)?;
        self.rtx.range(table, start, end)
    }
}

pub struct BatchKv<'s, W: StorageWriteTx> {
    wtx: &'s mut W,
}

impl<'s, W: StorageWriteTx> BatchKv<'s, W> {
    pub(crate) fn new(wtx: &'s mut W) -> Self {
        Self { wtx }
    }

    pub fn get(&self, table: TableSpec, key: &[u8]) -> Result<Option<Vec<u8>>, BknError> {
        check_table_name(table.0)?;
        self.wtx.get(table, key)
    }

    pub fn range(
        &self,
        table: TableSpec,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, BknError> {
        check_table_name(table.0)?;
        self.wtx.range(table, start, end)
    }

    pub fn put(&mut self, table: TableSpec, key: &[u8], value: &[u8]) -> Result<(), BknError> {
        check_table_name(table.0)?;
        self.wtx.put(table, key, value)
    }

    pub fn delete(&mut self, table: TableSpec, key: &[u8]) -> Result<(), BknError> {
        check_table_name(table.0)?;
        self.wtx.delete(table, key)
    }
}
