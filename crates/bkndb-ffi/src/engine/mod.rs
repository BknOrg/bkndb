use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use bkndb::{LsmOptions, LsmStorageBackend, MemoryStorageBackend};
use bkndb_core::graph::{Direction, EdgeId, NodeId};
use bkndb_core::relational::{Agg, Query, TableSchema, VectorIndexOptions, VectorSearchOptions};
use bkndb_core::Db;

use crate::error::FfiBknError;
use crate::lang::{params, FfiQueryResult};
use crate::ops::{self, FfiProps};
use crate::relational::{
    filter_expr, FfiAgg, FfiAggregateRow, FfiExprNode, FfiQuery, FfiRow, FfiScoredRow, FfiTableSchema, FfiVectorIndexInfo,
    FfiVectorMetric,
};
use crate::transaction::{BknDbTransaction, TxWorker, Worker};
use crate::types::{
    FfiDbStats, FfiDirection, FfiEdgeInput, FfiEdgeRecord, FfiHubRecord, FfiIntegrityReport, FfiLsmOptions, FfiNeighbor, FfiNodeInput, FfiNodeRecord,
    FfiPathResult, FfiPropValue, FfiPropertyIndex, FfiSyncBatch, FfiSyncResult, FfiTraversalHit, FfiTypedNeighbor,
    FfiTableCount, FfiWeightedPath,
};

enum Backend {
    Disk(Db<LsmStorageBackend>),
    Mem(Db<MemoryStorageBackend>),
}

/// Runs `$body` against the open database bound to `$db`, mapping errors to
/// `FfiBknError` (and failing with `DatabaseClosed` after `close()`).
macro_rules! with_db {
    ($self:expr, $db:ident => $body:expr) => {{
        let guard = $self.inner.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(Backend::Disk($db)) => ($body).map_err(FfiBknError::from),
            Some(Backend::Mem($db)) => ($body).map_err(FfiBknError::from),
            None => Err(FfiBknError::DatabaseClosed),
        }
    }};
}

/// Runs `$body` in a snapshot read transaction bound to `$r`.
macro_rules! read {
    ($self:expr, |$r:ident| $body:expr) => {
        with_db!($self, db => db.read_tx(|$r| $body))
    };
}

/// Runs `$body` in its own atomic write transaction bound to `$b`. Refuses
/// while an explicit transaction is open: the write would otherwise wait for
/// the writer lock that transaction holds — forever, if both are driven from
/// the same thread.
macro_rules! write {
    ($self:expr, |$b:ident| $body:expr) => {{
        if $self.tx_open.load(Ordering::SeqCst) {
            Err(FfiBknError::TransactionInProgress)
        } else {
            with_db!($self, db => db.write_tx(|$b| $body))
        }
    }};
}

mod graph;
mod relational;
mod search;

/// The main entrypoint for BknDb, thread-safe and exported via UniFFI.
#[derive(uniffi::Object)]
pub struct BknDbEngine {
    inner: RwLock<Option<Backend>>,
    tx_open: Arc<AtomicBool>,
}

impl BknDbEngine {
    fn wrap(backend: Backend) -> Arc<Self> {
        Arc::new(Self { inner: RwLock::new(Some(backend)), tx_open: Arc::new(AtomicBool::new(false)) })
    }
}

fn neighbors_of(v: Vec<FfiTypedNeighbor>) -> Vec<FfiNeighbor> {
    v.into_iter().map(|n| FfiNeighbor { node_id: n.node_id, edge_id: n.edge_id }).collect()
}

#[uniffi::export]
impl BknDbEngine {
    /// Opens or creates an on-disk single-file database at `path`. Fails with
    /// `DatabaseLocked` if another handle or process has it open.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, FfiBknError> {
        Ok(Self::wrap(Backend::Disk(Db::new(LsmStorageBackend::open(path)?))))
    }

    /// Like [`BknDbEngine::open`], with tuning options for the storage engine.
    #[uniffi::constructor]
    pub fn open_with_options(path: String, options: FfiLsmOptions) -> Result<Arc<Self>, FfiBknError> {
        let opts = LsmOptions {
            memtable_flush_bytes: usize::try_from(options.memtable_flush_bytes).unwrap_or(usize::MAX),
            compaction_trigger_files: options.compaction_trigger_files.max(2) as usize,
            block_size_bytes: options.block_size_bytes.map_or(LsmOptions::default().block_size_bytes, |b| b.max(64) as usize),
            compression: options.compression.unwrap_or(true),
        };
        Ok(Self::wrap(Backend::Disk(Db::new(LsmStorageBackend::open_with_options(path, opts)?))))
    }

    /// Creates an ephemeral in-memory database instance.
    #[uniffi::constructor]
    pub fn in_memory() -> Result<Arc<Self>, FfiBknError> {
        Ok(Self::wrap(Backend::Mem(Db::new(MemoryStorageBackend::new()))))
    }

    /// Closes the database, releasing its file lock. Every later call fails
    /// with `DatabaseClosed`. Idempotent; fails while a transaction is open.
    pub fn close(&self) -> Result<(), FfiBknError> {
        if self.tx_open.load(Ordering::SeqCst) {
            return Err(FfiBknError::TransactionInProgress);
        }
        self.inner.write().unwrap_or_else(|e| e.into_inner()).take();
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.inner.read().unwrap_or_else(|e| e.into_inner()).is_none()
    }

    /// Reclaims disk space from overwritten and deleted data (on-disk
    /// databases only; a no-op in memory).
    pub fn compact(&self) -> Result<(), FfiBknError> {
        if self.tx_open.load(Ordering::SeqCst) {
            return Err(FfiBknError::TransactionInProgress);
        }
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(Backend::Disk(db)) => Ok(db.backend().force_compact()?),
            Some(Backend::Mem(_)) => Ok(()),
            None => Err(FfiBknError::DatabaseClosed),
        }
    }

    /// Writes a consistent, compacted copy of everything committed so far to
    /// a new file at `dest` (which must not exist), without blocking readers
    /// or writers. On-disk databases only.
    pub fn backup(&self, dest: String) -> Result<(), FfiBknError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(Backend::Disk(db)) => Ok(db.backend().backup_to(dest)?),
            Some(Backend::Mem(_)) => Err(FfiBknError::InvalidArgument {
                message: "backup needs an on-disk database; this one is in memory".to_string(),
            }),
            None => Err(FfiBknError::DatabaseClosed),
        }
    }

    /// Node/edge/row counts (by scanning, from one snapshot) plus file-level
    /// storage figures for on-disk databases.
    pub fn stats(&self) -> Result<FfiDbStats, FfiBknError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let (logical, storage) = match guard.as_ref() {
            Some(Backend::Disk(db)) => (db.stats()?, Some(db.backend().stats()?.into())),
            Some(Backend::Mem(db)) => (db.stats()?, None),
            None => return Err(FfiBknError::DatabaseClosed),
        };
        Ok(FfiDbStats {
            nodes: logical.nodes,
            edges: logical.edges,
            tables: logical.tables.into_iter().map(|(table, rows)| FfiTableCount { table, rows }).collect(),
            storage,
        })
    }

    /// Re-reads and checksums every stored byte, failing with `Corruption`
    /// on the first damaged structure.
    pub fn verify_integrity(&self) -> Result<FfiIntegrityReport, FfiBknError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(Backend::Disk(db)) => Ok(db.backend().verify_integrity()?.into()),
            Some(Backend::Mem(_)) => Ok(bkndb::IntegrityReport::default().into()),
            None => Err(FfiBknError::DatabaseClosed),
        }
    }

    /// Runs one SQL statement (see the crate docs of `bkndb_core::lang::sql`):
    /// a `SELECT` against a snapshot, anything else in its own atomic write
    /// transaction. Parameters: `?`/`?N`/`$N` from `positional`, `:name` from
    /// `named`.
    #[uniffi::method(default(positional = [], named = None))]
    pub fn sql(
        &self,
        query: String,
        positional: Vec<FfiPropValue>,
        named: Option<HashMap<String, FfiPropValue>>,
    ) -> Result<FfiQueryResult, FfiBknError> {
        let params = params(positional, named);
        let read_only = bkndb_core::lang::sql::parse(&query)?.is_read_only();
        let result = if read_only {
            read!(self, |r| r.relational().sql(&query, params))?
        } else {
            write!(self, |b| b.relational().sql(&query, params))?
        };
        Ok(result.into())
    }

    /// Runs a graph `MATCH ... RETURN ...` query against a snapshot.
    /// Parameters: `$name` from `named`, `$N`/`?` from `positional`.
    #[uniffi::method(default(positional = [], named = None))]
    pub fn graph_query(
        &self,
        query: String,
        positional: Vec<FfiPropValue>,
        named: Option<HashMap<String, FfiPropValue>>,
    ) -> Result<FfiQueryResult, FfiBknError> {
        let params = params(positional, named);
        Ok(read!(self, |r| r.graph().query(&query, params))?.into())
    }

    /// Opens an explicit write transaction. Only one can be open at a time;
    /// while it is, writes through the engine itself fail with
    /// `TransactionInProgress` (reads keep working on the last committed state).
    pub fn begin_transaction(&self) -> Result<Arc<BknDbTransaction>, FfiBknError> {
        if self.tx_open.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Err(FfiBknError::TransactionInProgress);
        }
        let worker = {
            let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(Backend::Disk(db)) => Worker::start(db.clone()).map(TxWorker::Disk),
                Some(Backend::Mem(db)) => Worker::start(db.clone()).map(TxWorker::Mem),
                None => Err(FfiBknError::DatabaseClosed),
            }
        };
        match worker {
            Ok(w) => Ok(Arc::new(BknDbTransaction::new(w, self.tx_open.clone()))),
            Err(e) => {
                self.tx_open.store(false, Ordering::SeqCst);
                Err(e)
            }
        }
    }

}
