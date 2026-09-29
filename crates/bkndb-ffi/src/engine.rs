use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use bkndb::{LsmOptions, LsmStorageBackend, MemoryStorageBackend};
use bkndb_core::graph::{Direction, EdgeId, NodeId};
use bkndb_core::relational::{Agg, Query, TableSchema};
use bkndb_core::Db;

use crate::error::FfiBknError;
use crate::lang::{params, FfiQueryResult};
use crate::ops::{self, FfiProps};
use crate::relational::{
    filter_expr, FfiAgg, FfiAggregateRow, FfiExprNode, FfiQuery, FfiRow, FfiScoredRow, FfiTableSchema, FfiVectorMetric,
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

    // ---- graph: nodes & edges ----

    /// Creates a single graph node with the given label and properties.
    pub fn create_node(&self, label: String, properties: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        write!(self, |b| ops::create_node(b, &label, properties))
    }

    /// Creates multiple nodes in a single atomic transaction.
    pub fn create_nodes_bulk(&self, nodes: Vec<FfiNodeInput>) -> Result<Vec<u64>, FfiBknError> {
        let nodes: Vec<(String, FfiProps)> = nodes.into_iter().map(|n| (n.label, n.properties)).collect();
        write!(self, |b| ops::create_nodes(b, nodes))
    }

    /// Retrieves a node by its numeric ID.
    pub fn get_node(&self, id: u64) -> Result<Option<FfiNodeRecord>, FfiBknError> {
        Ok(with_db!(self, db => db.graph().get_node(NodeId(id)))?.map(|n| ops::node_record(id, n)))
    }

    /// Deletes a node and all incident edges atomically.
    pub fn delete_node(&self, id: u64) -> Result<(), FfiBknError> {
        write!(self, |b| ops::delete_node(b, id))
    }

    /// Replaces/adds the properties in `set` and removes those named in
    /// `unset`, atomically. Fails with `NotFound` if the node doesn't exist.
    pub fn update_node_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        write!(self, |b| ops::update_node(b, id, set, unset))
    }

    /// Creates a directed edge between two existing nodes.
    pub fn create_edge(
        &self,
        from: u64,
        edge_type: String,
        to: u64,
        properties: HashMap<String, FfiPropValue>,
    ) -> Result<u64, FfiBknError> {
        write!(self, |b| ops::create_edge(b, from, &edge_type, to, properties))
    }

    /// Creates multiple edges in a single atomic transaction.
    pub fn create_edges_bulk(&self, edges: Vec<FfiEdgeInput>) -> Result<Vec<u64>, FfiBknError> {
        let edges: Vec<_> = edges.into_iter().map(|e| (e.from, e.edge_type, e.to, e.properties)).collect();
        write!(self, |b| ops::create_edges(b, edges))
    }

    /// Retrieves an edge by its numeric ID.
    pub fn get_edge(&self, id: u64) -> Result<Option<FfiEdgeRecord>, FfiBknError> {
        Ok(with_db!(self, db => db.graph().get_edge(EdgeId(id)))?.map(|e| ops::edge_record(id, e)))
    }

    /// Deletes one edge; returns whether it existed.
    pub fn delete_edge(&self, id: u64) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::delete_edge(b, id))
    }

    /// See [`BknDbEngine::update_node_properties`].
    pub fn update_edge_properties(
        &self,
        id: u64,
        set: HashMap<String, FfiPropValue>,
        unset: Vec<String>,
    ) -> Result<(), FfiBknError> {
        write!(self, |b| ops::update_edge(b, id, set, unset))
    }

    // ---- graph: indexes ----

    /// Ids of every node with `label`, ascending.
    pub fn nodes_by_label(&self, label: String) -> Result<Vec<u64>, FfiBknError> {
        read!(self, |r| ops::nodes_by_label(r, &label))
    }

    /// Number of nodes with `label`.
    pub fn count_nodes(&self, label: String) -> Result<u64, FfiBknError> {
        Ok(self.nodes_by_label(label)?.len() as u64)
    }

    /// Ids of nodes with `label` whose `property` equals `value`, ascending.
    /// A lookup when the property is indexed (`create_node_index`), else a
    /// scan of the label's nodes.
    pub fn find_nodes(&self, label: String, property: String, value: FfiPropValue) -> Result<Vec<u64>, FfiBknError> {
        let value = value.into();
        read!(self, |r| ops::find_nodes(r, &label, &property, value))
    }

    /// Indexes `property` of nodes with `label` (backfilled immediately;
    /// only Int/Str values are indexed). Returns `false` if it already existed.
    pub fn create_node_index(&self, label: String, property: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::set_node_index(b, &label, &property, true))
    }

    pub fn drop_node_index(&self, label: String, property: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::set_node_index(b, &label, &property, false))
    }

    pub fn list_node_indexes(&self) -> Result<Vec<FfiPropertyIndex>, FfiBknError> {
        read!(self, |r| ops::node_indexes(r))
    }

    /// Rebuilds every graph index. Only needed once for databases created
    /// by versions without graph indexes (lookups work without it, but scan).
    pub fn rebuild_graph_indexes(&self) -> Result<(), FfiBknError> {
        write!(self, |b| ops::rebuild_graph_indexes(b))
    }

    // ---- graph: traversal ----

    /// Lowest-cost path (Dijkstra): each edge costs its numeric
    /// `weight_property`, or `default_weight` when it has none. Weights must
    /// be non-negative.
    pub fn find_weighted_path(
        &self,
        start: u64,
        target: u64,
        direction: FfiDirection,
        edge_types: Option<Vec<String>>,
        weight_property: String,
        default_weight: f64,
    ) -> Result<Option<FfiWeightedPath>, FfiBknError> {
        read!(self, |r| ops::weighted_path(r, start, target, direction.into(), edge_types, &weight_property, default_weight))
    }

    /// Returns outgoing neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_out(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        Ok(neighbors_of(self.neighbors(node, FfiDirection::Out, Some(edge_type))?))
    }

    /// Returns incoming neighbors for `node` along edges of type `edge_type`.
    pub fn neighbors_in(&self, node: u64, edge_type: String) -> Result<Vec<FfiNeighbor>, FfiBknError> {
        Ok(neighbors_of(self.neighbors(node, FfiDirection::In, Some(edge_type))?))
    }

    /// Neighbors of `node` in `direction`, over every edge type unless
    /// `edge_type` is given.
    pub fn neighbors(&self, node: u64, direction: FfiDirection, edge_type: Option<String>) -> Result<Vec<FfiTypedNeighbor>, FfiBknError> {
        read!(self, |r| ops::neighbors(r, node, direction.into(), edge_type.as_deref()))
    }

    /// Number of edges at `node` in `direction` (optionally of one type).
    pub fn degree(&self, node: u64, direction: FfiDirection, edge_type: Option<String>) -> Result<u64, FfiBknError> {
        Ok(self.neighbors(node, direction, edge_type)?.len() as u64)
    }

    /// Breadth-first traversal from `start` up to `max_depth` hops. Returns
    /// `start` itself first (depth 0). `edge_types` restricts which edges are
    /// followed; `node_label` only visits (and expands) nodes with that label.
    pub fn traverse(
        &self,
        start: u64,
        direction: FfiDirection,
        max_depth: u32,
        edge_types: Option<Vec<String>>,
        node_label: Option<String>,
    ) -> Result<Vec<FfiTraversalHit>, FfiBknError> {
        read!(self, |r| ops::traverse(r, start, direction.into(), max_depth, edge_types, node_label))
    }

    /// Computes the unweighted shortest path between `start` and `target` using BFS.
    pub fn find_shortest_path(
        &self,
        start: u64,
        target: u64,
        direction: FfiDirection,
        edge_types: Option<Vec<String>>,
    ) -> Result<Option<FfiPathResult>, FfiBknError> {
        let refs: Option<Vec<&str>> = edge_types.as_ref().map(|v| v.iter().map(String::as_str).collect());
        let path = with_db!(self, db => db.graph().find_shortest_path(
            NodeId(start),
            NodeId(target),
            Direction::from(direction),
            refs.as_deref(),
        ))?;
        Ok(path.map(Into::into))
    }

    /// Finds the top `k` hub nodes by degree centrality, optionally filtered by node label.
    pub fn top_hubs(&self, k: u32, direction: FfiDirection, edge_type: Option<String>) -> Result<Vec<FfiHubRecord>, FfiBknError> {
        let hubs = with_db!(self, db => db.graph().top_hubs(k as usize, direction.into(), edge_type.as_deref()))?;
        Ok(hubs.into_iter().map(|(n, d)| FfiHubRecord { node_id: n.0, degree: d as u64 }).collect())
    }

    /// Recursively cascade-deletes `root` and all descendants reachable via `containment_edge`.
    pub fn cascade_delete(&self, root: u64, containment_edge: String) -> Result<Vec<u64>, FfiBknError> {
        write!(self, |b| ops::cascade_delete(b, root, &containment_edge))
    }

    /// Ingests graph nodes, edges and relational rows (upserted by primary
    /// key) in a single atomic transaction.
    pub fn sync_batch(&self, batch: FfiSyncBatch) -> Result<FfiSyncResult, FfiBknError> {
        write!(self, |b| ops::sync(b, batch))
    }

    // ---- relational: schema ----

    /// Registers a table. Returns `false` if an identical definition already
    /// exists; fails if a different one does (use `ensure_table` to migrate).
    pub fn create_table(&self, schema: FfiTableSchema) -> Result<bool, FfiBknError> {
        let schema = TableSchema::try_from(schema)?;
        write!(self, |b| ops::create_table(b, schema))
    }

    /// Creates the table, or migrates the existing one to `schema` (columns,
    /// constraints and indexes; existing rows are backfilled/validated).
    pub fn ensure_table(&self, schema: FfiTableSchema) -> Result<(), FfiBknError> {
        let schema = TableSchema::try_from(schema)?;
        write!(self, |b| ops::ensure_table(b, schema))
    }

    /// Deletes a table and all its rows; returns whether it existed.
    pub fn drop_table(&self, name: String) -> Result<bool, FfiBknError> {
        write!(self, |b| ops::drop_table(b, &name))
    }

    /// Adds a (backfilled) secondary index.
    pub fn create_index(&self, table: String, column: String) -> Result<(), FfiBknError> {
        write!(self, |b| ops::set_index(b, &table, &column, true))
    }

    pub fn drop_index(&self, table: String, column: String) -> Result<(), FfiBknError> {
        write!(self, |b| ops::set_index(b, &table, &column, false))
    }

    pub fn list_tables(&self) -> Result<Vec<FfiTableSchema>, FfiBknError> {
        read!(self, |r| ops::list_tables(r))?.iter().map(FfiTableSchema::try_from).collect()
    }

    pub fn table_schema(&self, name: String) -> Result<Option<FfiTableSchema>, FfiBknError> {
        read!(self, |r| ops::table_schema(r, &name))?.as_ref().map(FfiTableSchema::try_from).transpose()
    }

    // ---- relational: rows ----

    /// Inserts a row; returns its primary key (generated for auto-increment
    /// tables). Fails with `DuplicateKey` if the key is taken.
    pub fn insert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        write!(self, |b| ops::insert(b, &table, values, false)).map(Into::into)
    }

    pub fn insert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(write!(self, |b| ops::insert_many(b, &table, rows, false))?.into_iter().map(Into::into).collect())
    }

    /// Inserts a row or replaces the one with the same primary key.
    pub fn upsert(&self, table: String, values: HashMap<String, FfiPropValue>) -> Result<FfiPropValue, FfiBknError> {
        write!(self, |b| ops::insert(b, &table, values, true)).map(Into::into)
    }

    pub fn upsert_many(&self, table: String, rows: Vec<HashMap<String, FfiPropValue>>) -> Result<Vec<FfiPropValue>, FfiBknError> {
        Ok(write!(self, |b| ops::insert_many(b, &table, rows, true))?.into_iter().map(Into::into).collect())
    }

    pub fn get_row(&self, table: String, pk: FfiPropValue) -> Result<Option<FfiRow>, FfiBknError> {
        let pk = pk.into();
        read!(self, |r| ops::get_row(r, &table, pk))
    }

    pub fn select(&self, table: String, query: FfiQuery) -> Result<Vec<FfiRow>, FfiBknError> {
        let query = Query::try_from(query)?;
        read!(self, |r| ops::select(r, &table, &query))
    }

    pub fn count(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        read!(self, |r| ops::count(r, &table, &query))
    }

    /// `GROUP BY group_by` aggregates over the rows matching `query`. With no
    /// `group_by`, returns exactly one row.
    pub fn aggregate(
        &self,
        table: String,
        query: FfiQuery,
        group_by: Vec<String>,
        aggregates: Vec<FfiAgg>,
    ) -> Result<Vec<FfiAggregateRow>, FfiBknError> {
        let query = Query::try_from(query)?;
        let aggs = aggregates.into_iter().map(Agg::try_from).collect::<Result<Vec<_>, _>>()?;
        read!(self, |r| ops::aggregate(r, &table, &query, &group_by, &aggs))
    }

    /// Sets the columns in `set` on every row matching `query`; returns how
    /// many rows changed.
    pub fn update_rows(&self, table: String, query: FfiQuery, set: HashMap<String, FfiPropValue>) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        write!(self, |b| ops::update_rows(b, &table, &query, set))
    }

    /// Deletes every row matching `query`; returns how many were removed.
    pub fn delete_rows(&self, table: String, query: FfiQuery) -> Result<u64, FfiBknError> {
        let query = Query::try_from(query)?;
        write!(self, |b| ops::delete_rows(b, &table, &query))
    }

    // ---- search ----

    /// Builds a full-text (BM25) index over a text column, backfilling
    /// existing rows; `false` if it already exists.
    pub fn create_fulltext_index(&self, table: String, column: String) -> Result<bool, FfiBknError> {
        write!(self, |b| b.relational().create_fulltext_index(&table, &column))
    }

    pub fn drop_fulltext_index(&self, table: String, column: String) -> Result<bool, FfiBknError> {
        write!(self, |b| b.relational().drop_fulltext_index(&table, &column))
    }

    pub fn list_fulltext_indexes(&self, table: String) -> Result<Vec<String>, FfiBknError> {
        read!(self, |r| r.relational().fulltext_indexes(&table))
    }

    /// Up to `limit` rows whose `column` best matches `query` (BM25).
    /// `word*` is a prefix match; `match_all` requires every word.
    #[uniffi::method(default(match_all = false, filter = []))]
    pub fn search_text(
        &self,
        table: String,
        column: String,
        query: String,
        limit: u32,
        match_all: bool,
        filter: Vec<FfiExprNode>,
    ) -> Result<Vec<FfiScoredRow>, FfiBknError> {
        let filter = filter_expr(filter)?;
        let hits = read!(self, |r| r.relational().search_text(&table, &column, &query, limit as usize, match_all, filter.as_ref()))?;
        Ok(hits.into_iter().map(Into::into).collect())
    }

    /// The `limit` rows whose embedding in `column` (a list of numbers, or
    /// bytes of little-endian f32s) is nearest to `vector`.
    #[uniffi::method(default(filter = []))]
    pub fn search_vector(
        &self,
        table: String,
        column: String,
        vector: Vec<f32>,
        limit: u32,
        metric: FfiVectorMetric,
        filter: Vec<FfiExprNode>,
    ) -> Result<Vec<FfiScoredRow>, FfiBknError> {
        let filter = filter_expr(filter)?;
        let hits = read!(self, |r| r.relational().search_vector(&table, &column, &vector, limit as usize, metric.into(), filter.as_ref()))?;
        Ok(hits.into_iter().map(Into::into).collect())
    }
}
