//! Explicit, long-lived write transactions over FFI.
//!
//! A backend write transaction can't be stored in a UniFFI object directly:
//! it borrows the database and (for the LSM engine) holds the writer lock
//! guard, which is `!Send`, while exported objects must be `Send + Sync`.
//! So each transaction runs on its own worker thread that owns the open
//! batch; the exported [`BknDbTransaction`] only holds a channel to it and
//! ships each operation over as a closure. Reads inside the transaction see
//! its own uncommitted writes.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use bkndb::MemoryStorageBackend;
use bkndb::LsmStorageBackend;
use bkndb_core::relational::{Query, TableSchema};
use bkndb_core::{BknError, Db, DbWriteBatch, StorageBackend};

use crate::error::FfiBknError;
use crate::ops::{self, FfiProps};
use crate::relational::{FfiQuery, FfiRow, FfiTableSchema};
use crate::types::{FfiDirection, FfiEdgeInput, FfiEdgeRecord, FfiNodeInput, FfiNodeRecord, FfiPropValue, FfiTypedNeighbor};

type Job<B> = Box<dyn for<'a> FnOnce(&mut DbWriteBatch<<B as StorageBackend>::WriteTx<'a>>) + Send>;

enum Cmd<B: StorageBackend> {
    Run(Job<B>),
    Commit(mpsc::SyncSender<Result<(), BknError>>),
    Rollback,
}

/// The worker thread owning one open write batch.
pub(crate) struct Worker<B: StorageBackend> {
    sender: mpsc::Sender<Cmd<B>>,
    thread: Option<JoinHandle<()>>,
}

fn worker_gone() -> FfiBknError {
    FfiBknError::Backend { message: "transaction worker thread exited unexpectedly".to_string() }
}

impl<B: StorageBackend + 'static> Worker<B> {
    /// Spawns the worker and waits until its write transaction is open
    /// (i.e. it holds the writer lock).
    pub(crate) fn start(db: Db<B>) -> Result<Self, FfiBknError> {
        let (sender, commands) = mpsc::channel::<Cmd<B>>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), BknError>>(1);
        let thread = std::thread::Builder::new()
            .name("bkndb-transaction".to_string())
            .spawn(move || {
                let mut batch = match db.begin_write() {
                    Ok(batch) => {
                        let _ = ready_tx.send(Ok(()));
                        batch
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Ends (rolling back by dropping `batch`) on Rollback, or when
                // the sender side is dropped without an explicit commit.
                while let Ok(cmd) = commands.recv() {
                    match cmd {
                        Cmd::Run(job) => job(&mut batch),
                        Cmd::Commit(reply) => {
                            let _ = reply.send(batch.commit());
                            return;
                        }
                        Cmd::Rollback => return,
                    }
                }
            })
            .map_err(|e| FfiBknError::Backend { message: format!("cannot start transaction thread: {e}") })?;
        ready_rx.recv().map_err(|_| worker_gone())??;
        Ok(Self { sender, thread: Some(thread) })
    }

    pub(crate) fn run<T: Send + 'static>(
        &self,
        f: impl for<'a> FnOnce(&mut DbWriteBatch<B::WriteTx<'a>>) -> Result<T, BknError> + Send + 'static,
    ) -> Result<T, FfiBknError> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.sender
            .send(Cmd::Run(Box::new(move |batch| {
                let _ = reply_tx.send(f(batch));
            })))
            .map_err(|_| worker_gone())?;
        Ok(reply_rx.recv().map_err(|_| worker_gone())??)
    }

    fn finish(mut self, commit: bool) -> Result<(), FfiBknError> {
        let result = if commit {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            self.sender.send(Cmd::Commit(reply_tx)).map_err(|_| worker_gone())?;
            reply_rx.recv().map_err(|_| worker_gone())?.map_err(Into::into)
        } else {
            let _ = self.sender.send(Cmd::Rollback);
            Ok(())
        };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        result
    }
}

impl<B: StorageBackend> Drop for Worker<B> {
    fn drop(&mut self) {
        // Rollback if never finished; a closed channel also ends the worker.
        let _ = self.sender.send(Cmd::Rollback);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub(crate) enum TxWorker {
    Disk(Worker<LsmStorageBackend>),
    Mem(Worker<MemoryStorageBackend>),
}

/// An explicit write transaction. Every operation runs inside it and sees
/// its earlier writes; nothing is visible to others or durable until
/// [`BknDbTransaction::commit`]. If any operation fails, the transaction is
/// aborted: later operations and `commit` fail, and it must be rolled back.
/// Dropping an unfinished transaction rolls it back.
#[derive(uniffi::Object)]
pub struct BknDbTransaction {
    worker: Mutex<Option<TxWorker>>,
    aborted: Mutex<Option<String>>,
    /// Shared with the engine: cleared when this transaction ends.
    engine_tx_open: Arc<AtomicBool>,
}

impl BknDbTransaction {
    pub(crate) fn new(worker: TxWorker, engine_tx_open: Arc<AtomicBool>) -> Self {
        Self { worker: Mutex::new(Some(worker)), aborted: Mutex::new(None), engine_tx_open }
    }

    fn check_usable(&self) -> Result<(), FfiBknError> {
        match self.aborted.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(message) => Err(FfiBknError::TransactionAborted { message: message.clone() }),
            None => Ok(()),
        }
    }

    fn note_failure<T>(&self, res: Result<T, FfiBknError>) -> Result<T, FfiBknError> {
        if let Err(e) = &res
            && !matches!(e, FfiBknError::TransactionClosed)
        {
            *self.aborted.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
        }
        res
    }

    fn end(&self, commit: bool) -> Result<(), FfiBknError> {
        let worker = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(worker) = worker else {
            return Err(FfiBknError::TransactionClosed);
        };
        let aborted = self.aborted.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let result = match (commit, aborted) {
            (true, Some(message)) => {
                drop(worker); // roll back
                Err(FfiBknError::TransactionAborted { message })
            }
            (commit, _) => match worker {
                TxWorker::Disk(w) => w.finish(commit),
                TxWorker::Mem(w) => w.finish(commit),
            },
        };
        self.engine_tx_open.store(false, Ordering::SeqCst);
        result
    }
}

impl Drop for BknDbTransaction {
    fn drop(&mut self) {
        let _ = self.end(false);
    }
}

/// Runs `$body` (with the open batch bound to `$b`) inside the transaction.
macro_rules! in_tx {
    ($self:expr, |$b:ident| $body:expr) => {{
        $self.check_usable()?;
        let res = {
            let guard = $self.worker.lock().unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(TxWorker::Disk(w)) => w.run(move |$b| $body),
                Some(TxWorker::Mem(w)) => w.run(move |$b| $body),
                None => Err(FfiBknError::TransactionClosed),
            }
        };
        $self.note_failure(res)
    }};
}

#[uniffi::export]
impl BknDbTransaction {
    /// Makes every write of this transaction durable and visible atomically.
    pub fn commit(&self) -> Result<(), FfiBknError> {
        self.end(true)
    }

    /// Discards every write of this transaction.
    pub fn rollback(&self) -> Result<(), FfiBknError> {
        match self.end(false) {
            Err(FfiBknError::TransactionClosed) | Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Whether the transaction is still open (not committed or rolled back).
    pub fn is_active(&self) -> bool {
        self.worker.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    // ---- graph ----

    pub fn create_node(&self, label: String, properties: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        in_tx!(self, |b| ops::create_node(b, &label, properties))
    }

    pub fn create_nodes_bulk(&self, nodes: Vec<FfiNodeInput>) -> Result<Vec<u64>, FfiBknError> {
        let nodes: Vec<(String, FfiProps)> = nodes.into_iter().map(|n| (n.label, n.properties)).collect();
        in_tx!(self, |b| ops::create_nodes(b, nodes))
    }

    pub fn create_edge(
        &self,
        from: u64,
        edge_type: String,
        to: u64,
        properties: HashMap<String, FfiPropValue>,
    ) -> Result<u64, FfiBknError> {
        in_tx!(self, |b| ops::create_edge(b, from, &edge_type, to, properties))
    }

    pub fn create_edges_bulk(&self, edges: Vec<FfiEdgeInput>) -> Result<Vec<u64>, FfiBknError> {
        let edges: Vec<_> = edges.into_iter().map(|e| (e.from, e.edge_type, e.to, e.properties)).collect();
        in_tx!(self, |b| ops::create_edges(b, edges))
    }

    pub fn get_node(&self, id: u64) -> Result<Option<FfiNodeRecord>, FfiBknError> {
        in_tx!(self, |b| ops::tx_get_node(b, id))
    }

    pub fn get_edge(&self, id: u64) -> Result<Option<FfiEdgeRecord>, FfiBknError> {
        in_tx!(self, |b| ops::tx_get_edge(b, id))
    }

    pub fn neighbors(&self, node: u64, direction: FfiDirection, edge_type: Option<String>) -> Result<Vec<FfiTypedNeighbor>, FfiBknError> {
        in_tx!(self, |b| ops::tx_neighbors(b, node, direction.into(), edge_type.as_deref()))
    }

    pub fn delete_node(&self, id: u64) -> Result<(), FfiBknError> {
        in_tx!(self, |b| ops::delete_node(b, id))
    }

    pub fn delete_edge(&self, id: u64) -> Result<bool, FfiBknError> {
        in_tx!(self, |b| ops::delete_edge(b, id))
    }

    pub fn update_node_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        in_tx!(self, |b| ops::update_node(b, id, set, unset))
    }

    pub fn update_edge_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        in_tx!(self, |b| ops::update_edge(b, id, set, unset))
    }

    // ---- relational ----

    pub fn create_table(&self, schema: FfiTableSchema) -> Result<bool, FfiBknError> {
        let schema = self.note_failure(TableSchema::try_from(schema))?;
        in_tx!(self, |b| ops::create_table(b, schema))
    }

    pub fn ensure_table(&self, schema: FfiTableSchema) -> Result<(), FfiBknError> {
        let schema = self.note_failure(TableSchema::try_from(schema))?;
        in_tx!(self, |b| ops::ensure_table(b, schema))
    }

    pub fn drop_table(&self, name: String) -> Result<bool, FfiBknError> {
        in_tx!(self, |b| ops::drop_table(b, &name))
    }

    pub fn insert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        in_tx!(self, |b| ops::insert(b, &table, values, false)).map(Into::into)
    }

    pub fn insert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(in_tx!(self, |b| ops::insert_many(b, &table, rows, false))?.into_iter().map(Into::into).collect())
    }

    pub fn upsert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        in_tx!(self, |b| ops::insert(b, &table, values, true)).map(Into::into)
    }

    pub fn upsert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(in_tx!(self, |b| ops::insert_many(b, &table, rows, true))?.into_iter().map(Into::into).collect())
    }

    pub fn get_row(&self, table: String, pk: FfiPropValue) -> Result<Option<FfiRow>, FfiBknError> {
        in_tx!(self, |b| ops::tx_get_row(b, &table, pk.into()))
    }

    pub fn select(&self, table: String, query: FfiQuery) -> Result<Vec<FfiRow>, FfiBknError> {
        let query = self.note_failure(Query::try_from(query))?;
        in_tx!(self, |b| ops::tx_select(b, &table, &query))
    }

    pub fn count(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = self.note_failure(Query::try_from(query))?;
        in_tx!(self, |b| ops::tx_count(b, &table, &query))
    }

    pub fn update_rows(&self, table: String, query: FfiQuery, set: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        let query = self.note_failure(Query::try_from(query))?;
        in_tx!(self, |b| ops::update_rows(b, &table, &query, set))
    }

    pub fn delete_rows(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = self.note_failure(Query::try_from(query))?;
        in_tx!(self, |b| ops::delete_rows(b, &table, &query))
    }
}
