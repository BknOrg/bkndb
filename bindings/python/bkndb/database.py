"""Pythonic wrapper around the raw UniFFI-generated ``BknDbEngine``.

This is the one hand-maintained adapter layer between bkndb's native
extension and its public Python API. It exists so the underlying Rust
surface can grow without breaking callers — application code should only
ever import from :mod:`bkndb`, never reach into :mod:`bkndb._native`.
"""
from __future__ import annotations

import typing

from . import errors, types
from ._native.bkndb_ffi import (
    BknDbEngine,
    BknDbTransaction,
    FfiEdgeInput,
    FfiLsmOptions,
    FfiNodeInput,
    FfiSyncBatch,
    FfiTableRows,
)
from .query import Agg, OrderBy, Where, to_ffi_query

T = typing.TypeVar("T")

NodeSpec = typing.Tuple[str, types.PropertiesLike]
EdgeSpec = typing.Tuple[int, str, int, types.PropertiesLike]


def _call(fn: typing.Callable[[], T]) -> T:
    """Runs `fn`, translating anything the native layer raises into a public exception."""
    try:
        return fn()
    except BaseException as exc:
        mapped = errors.wrap(exc)
        if mapped is exc:
            raise
        raise mapped from exc


def _props(p: typing.Optional[typing.Mapping[str, types.PropertyValue]]) -> dict:
    return types.to_ffi_properties(p or {})


def _row_or_none(record) -> typing.Optional[types.Row]:
    return types.Row._from_ffi(record) if record is not None else None


class _Operations:
    """Operations available both on a :class:`Database` (each call is its own
    atomic transaction) and inside a :class:`Transaction`."""

    @property
    def _h(self) -> typing.Any:  # BknDbEngine | BknDbTransaction
        raise NotImplementedError

    # ----- nodes & edges ----------------------------------------------------

    def create_node(self, label: str, properties: typing.Optional[types.PropertiesLike] = None) -> int:
        """Creates a graph node and returns its id."""
        ffi = _props(properties)
        return _call(lambda: self._h.create_node(label, ffi))

    def create_nodes_bulk(self, nodes: typing.Sequence[NodeSpec]) -> typing.List[int]:
        """Creates several nodes atomically from `(label, properties)` pairs."""
        items = [FfiNodeInput(label=label, properties=_props(p)) for label, p in nodes]
        return _call(lambda: self._h.create_nodes_bulk(items))

    def get_node(self, node_id: int) -> typing.Optional[types.Node]:
        """The node with this id, or ``None``."""
        record = _call(lambda: self._h.get_node(node_id))
        return types.Node._from_ffi(record) if record is not None else None

    def update_node(
        self,
        node_id: int,
        set: typing.Optional[types.PropertiesLike] = None,  # noqa: A002 - mirrors SQL SET
        unset: typing.Iterable[str] = (),
    ) -> None:
        """Adds/replaces the properties in `set` and removes those in `unset`.
        Raises :class:`bkndb.NotFoundError` if the node doesn't exist."""
        ffi, drop = _props(set), list(unset)
        _call(lambda: self._h.update_node_properties(node_id, ffi, drop))

    def delete_node(self, node_id: int) -> None:
        """Deletes a node and all its edges."""
        _call(lambda: self._h.delete_node(node_id))

    def create_edge(
        self,
        from_node: int,
        edge_type: str,
        to_node: int,
        properties: typing.Optional[types.PropertiesLike] = None,
    ) -> int:
        """Creates a directed edge between two existing nodes and returns its id."""
        ffi = _props(properties)
        return _call(lambda: self._h.create_edge(from_node, edge_type, to_node, ffi))

    def create_edges_bulk(self, edges: typing.Sequence[EdgeSpec]) -> typing.List[int]:
        """Creates several edges atomically from `(from, edge_type, to, properties)` tuples."""
        items = [
            FfiEdgeInput(_from=frm, edge_type=edge_type, to=to, properties=_props(p))
            for frm, edge_type, to, p in edges
        ]
        return _call(lambda: self._h.create_edges_bulk(items))

    def get_edge(self, edge_id: int) -> typing.Optional[types.Edge]:
        """The edge with this id, or ``None``."""
        record = _call(lambda: self._h.get_edge(edge_id))
        return types.Edge._from_ffi(record) if record is not None else None

    def update_edge(
        self,
        edge_id: int,
        set: typing.Optional[types.PropertiesLike] = None,  # noqa: A002
        unset: typing.Iterable[str] = (),
    ) -> None:
        """See :meth:`update_node`."""
        ffi, drop = _props(set), list(unset)
        _call(lambda: self._h.update_edge_properties(edge_id, ffi, drop))

    def delete_edge(self, edge_id: int) -> bool:
        """Deletes one edge; returns whether it existed."""
        return _call(lambda: self._h.delete_edge(edge_id))

    def neighbors(
        self,
        node_id: int,
        direction: types.Direction = types.Direction.OUT,
        edge_type: typing.Optional[str] = None,
    ) -> typing.List[types.TypedNeighbor]:
        """Neighbors of `node_id`, over every edge type unless `edge_type` is given."""
        result = _call(lambda: self._h.neighbors(node_id, direction._to_ffi(), edge_type))
        return [types.TypedNeighbor._from_ffi(n) for n in result]

    # ----- tables --------------------------------------------------------------

    def create_table(self, schema: types.TableSchema) -> bool:
        """Registers a table. Returns ``False`` if an identical one already
        exists; raises :class:`bkndb.SchemaMismatchError` if a different one does."""
        ffi = schema._to_ffi()
        return _call(lambda: self._h.create_table(ffi))

    def ensure_table(self, schema: types.TableSchema) -> None:
        """Creates the table, or migrates it to `schema`: added/removed
        columns (defaults are backfilled), constraints and indexes. The
        primary key and existing column types can't change. Atomic."""
        ffi = schema._to_ffi()
        _call(lambda: self._h.ensure_table(ffi))

    def drop_table(self, name: str) -> bool:
        """Deletes a table and all its rows; returns whether it existed."""
        return _call(lambda: self._h.drop_table(name))

    # ----- rows ----------------------------------------------------------------

    def insert(self, table: str, values: types.PropertiesLike) -> types.PropertyValue:
        """Inserts a row and returns its primary key (generated for
        auto-increment tables). Raises :class:`bkndb.DuplicateKeyError` if taken."""
        ffi = _props(values)
        return types.from_ffi_value(_call(lambda: self._h.insert(table, ffi)))

    def insert_many(self, table: str, rows: typing.Iterable[types.PropertiesLike]) -> typing.List[types.PropertyValue]:
        """Inserts several rows atomically; returns their primary keys."""
        ffi = [_props(r) for r in rows]
        return [types.from_ffi_value(v) for v in _call(lambda: self._h.insert_many(table, ffi))]

    def upsert(self, table: str, values: types.PropertiesLike) -> types.PropertyValue:
        """Inserts a row, or replaces the row with the same primary key."""
        ffi = _props(values)
        return types.from_ffi_value(_call(lambda: self._h.upsert(table, ffi)))

    def upsert_many(self, table: str, rows: typing.Iterable[types.PropertiesLike]) -> typing.List[types.PropertyValue]:
        ffi = [_props(r) for r in rows]
        return [types.from_ffi_value(v) for v in _call(lambda: self._h.upsert_many(table, ffi))]

    def get_row(self, table: str, pk: types.PropertyValue) -> typing.Optional[types.Row]:
        """The row with this primary key, or ``None``."""
        ffi = types.to_ffi_value(pk)
        return _row_or_none(_call(lambda: self._h.get_row(table, ffi)))

    def select(
        self,
        table: str,
        where: Where = None,
        *,
        order_by: OrderBy = None,
        limit: typing.Optional[int] = None,
        offset: int = 0,
        columns: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.List[types.Row]:
        """Rows matching `where` (a :func:`bkndb.col` expression, or a dict of
        ``column: value`` equalities). ``order_by`` takes column names, ``-``
        prefixed for descending; ``columns`` projects the returned values."""
        q = to_ffi_query(where, order_by=order_by, limit=limit, offset=offset, columns=columns)
        return [types.Row._from_ffi(r) for r in _call(lambda: self._h.select(table, q))]

    def count(self, table: str, where: Where = None) -> int:
        """Number of rows matching `where`."""
        q = to_ffi_query(where)
        return _call(lambda: self._h.count(table, q))

    def update_rows(self, table: str, set: types.PropertiesLike, where: Where = None) -> int:  # noqa: A002
        """Sets the columns in `set` on every row matching `where` (every row
        if `where` is ``None``); returns how many rows changed."""
        q, ffi = to_ffi_query(where), _props(set)
        return _call(lambda: self._h.update_rows(table, q, ffi))

    def delete_rows(self, table: str, where: Where = None) -> int:
        """Deletes every row matching `where` (every row if ``None``); returns
        how many were removed."""
        q = to_ffi_query(where)
        return _call(lambda: self._h.delete_rows(table, q))

    def table(self, name: str) -> "Table":
        """A handle on one table, so its name needn't be repeated."""
        return Table(self, name)


class Table:
    """A table-scoped view over a :class:`Database` or :class:`Transaction`::

        people = db.table("people")
        people.insert({"name": "Ana"})
        people.select(col("age") > 30, order_by="-age")
    """

    __slots__ = ("_ops", "name")

    def __init__(self, ops: _Operations, name: str) -> None:
        self._ops = ops
        self.name = name

    def insert(self, values: types.PropertiesLike) -> types.PropertyValue:
        return self._ops.insert(self.name, values)

    def insert_many(self, rows: typing.Iterable[types.PropertiesLike]) -> typing.List[types.PropertyValue]:
        return self._ops.insert_many(self.name, rows)

    def upsert(self, values: types.PropertiesLike) -> types.PropertyValue:
        return self._ops.upsert(self.name, values)

    def upsert_many(self, rows: typing.Iterable[types.PropertiesLike]) -> typing.List[types.PropertyValue]:
        return self._ops.upsert_many(self.name, rows)

    def get(self, pk: types.PropertyValue) -> typing.Optional[types.Row]:
        return self._ops.get_row(self.name, pk)

    def select(self, where: Where = None, **kwargs: typing.Any) -> typing.List[types.Row]:
        return self._ops.select(self.name, where, **kwargs)

    def count(self, where: Where = None) -> int:
        return self._ops.count(self.name, where)

    def update(self, set: types.PropertiesLike, where: Where = None) -> int:  # noqa: A002
        return self._ops.update_rows(self.name, set, where)

    def delete(self, where: Where = None) -> int:
        return self._ops.delete_rows(self.name, where)

    def __iter__(self) -> typing.Iterator[types.Row]:
        return iter(self.select())

    def __len__(self) -> int:
        return self.count()

    def __repr__(self) -> str:
        return f"<bkndb.Table {self.name!r}>"


class Transaction(_Operations):
    """An explicit write transaction — use via :meth:`Database.transaction`::

        with db.transaction() as tx:
            a = tx.create_node("Person", {"name": "Ana"})
            tx.insert("people", {"node_id": a, "name": "Ana"})
        # committed here; rolled back instead if the block raised

    Reads through the transaction see its own uncommitted writes. If an
    operation fails the transaction is aborted and can only be rolled back.
    Only one transaction can be open per database; while it is, writing
    through the :class:`Database` itself raises
    :class:`bkndb.TransactionInProgressError`.
    """

    def __init__(self, handle: BknDbTransaction) -> None:
        self._tx = handle

    @property
    def _h(self) -> BknDbTransaction:
        return self._tx

    @property
    def active(self) -> bool:
        """Whether the transaction is still open."""
        return self._tx.is_active()

    def commit(self) -> None:
        """Makes every write durable and visible atomically."""
        _call(self._tx.commit)

    def rollback(self) -> None:
        """Discards every write. Safe to call more than once."""
        _call(self._tx.rollback)

    def __enter__(self) -> "Transaction":
        return self

    def __exit__(self, exc_type: typing.Any, exc: typing.Any, tb: typing.Any) -> None:
        if not self.active:
            return
        if exc_type is None:
            self.commit()
        else:
            self.rollback()

    def __repr__(self) -> str:
        return f"<bkndb.Transaction {'active' if self.active else 'finished'}>"


class Database(_Operations):
    """An embedded, single-file (or in-memory) bkndb database.

    Construct via :meth:`open`/:meth:`in_memory` (or the top-level
    ``bkndb.open``/``bkndb.in_memory`` shortcuts), ideally as a context
    manager so the file lock is released deterministically::

        with bkndb.open("my.bkndb") as db:
            doc = db.create_node("Document", {"title": "..."})
    """

    def __init__(self, engine: BknDbEngine, location: str = ":memory:") -> None:
        self._engine: typing.Optional[BknDbEngine] = engine
        self._location = location

    @classmethod
    def open(
        cls,
        path: str,
        *,
        memtable_flush_bytes: typing.Optional[int] = None,
        compaction_trigger_files: typing.Optional[int] = None,
    ) -> "Database":
        """Opens or creates an on-disk single-file database at `path`.

        Raises :class:`bkndb.DatabaseLockedError` if another handle or process
        has it open. The keyword options tune the storage engine.
        """
        path = str(path)
        if memtable_flush_bytes is None and compaction_trigger_files is None:
            return cls(_call(lambda: BknDbEngine.open(path)), path)
        options = FfiLsmOptions(
            memtable_flush_bytes=memtable_flush_bytes if memtable_flush_bytes is not None else 16 * 1024 * 1024,
            compaction_trigger_files=compaction_trigger_files if compaction_trigger_files is not None else 16,
        )
        return cls(_call(lambda: BknDbEngine.open_with_options(path, options)), path)

    @classmethod
    def in_memory(cls) -> "Database":
        """Creates an ephemeral in-memory database."""
        return cls(_call(BknDbEngine.in_memory))

    @property
    def _h(self) -> BknDbEngine:
        if self._engine is None:
            raise errors.DatabaseClosedError("this Database has been closed")
        return self._engine

    @property
    def closed(self) -> bool:
        return self._engine is None

    def close(self) -> None:
        """Closes the database and releases its file lock. Safe to call more
        than once. Raises :class:`bkndb.TransactionInProgressError` while a
        transaction is open."""
        if self._engine is None:
            return
        _call(self._engine.close)
        self._engine = None

    def __enter__(self) -> "Database":
        return self

    def __exit__(self, *_exc_info: object) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"<bkndb.Database {self._location!r} {'closed' if self.closed else 'open'}>"

    def transaction(self) -> Transaction:
        """Opens an explicit write transaction (see :class:`Transaction`)."""
        return Transaction(_call(self._h.begin_transaction))

    def compact(self) -> None:
        """Reclaims disk space from overwritten and deleted data."""
        _call(self._h.compact)

    # ----- graph queries ---------------------------------------------------------

    def neighbors_out(self, node_id: int, edge_type: str) -> typing.List[types.Neighbor]:
        """Outgoing neighbors of `node_id` along edges of type `edge_type`."""
        result = _call(lambda: self._h.neighbors_out(node_id, edge_type))
        return [types.Neighbor._from_ffi(n) for n in result]

    def neighbors_in(self, node_id: int, edge_type: str) -> typing.List[types.Neighbor]:
        """Incoming neighbors of `node_id` along edges of type `edge_type`."""
        result = _call(lambda: self._h.neighbors_in(node_id, edge_type))
        return [types.Neighbor._from_ffi(n) for n in result]

    def degree(
        self,
        node_id: int,
        direction: types.Direction = types.Direction.OUT,
        edge_type: typing.Optional[str] = None,
    ) -> int:
        """Number of edges at `node_id` in `direction` (optionally of one type)."""
        return _call(lambda: self._h.degree(node_id, direction._to_ffi(), edge_type))

    def traverse(
        self,
        start: int,
        direction: types.Direction = types.Direction.OUT,
        max_depth: int = 10,
        edge_types: typing.Optional[typing.Sequence[str]] = None,
        node_label: typing.Optional[str] = None,
    ) -> typing.List[types.TraversalHit]:
        """Breadth-first traversal from `start` (returned first, at depth 0)."""
        types_list = list(edge_types) if edge_types is not None else None
        result = _call(lambda: self._h.traverse(start, direction._to_ffi(), max_depth, types_list, node_label))
        return [types.TraversalHit._from_ffi(h) for h in result]

    def find_shortest_path(
        self,
        start: int,
        target: int,
        direction: types.Direction = types.Direction.OUT,
        edge_types: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.Optional[types.Path]:
        """Unweighted shortest path from `start` to `target`, via BFS."""
        types_list = list(edge_types) if edge_types is not None else None
        result = _call(lambda: self._h.find_shortest_path(start, target, direction._to_ffi(), types_list))
        return types.Path._from_ffi(result) if result is not None else None

    def top_hubs(
        self,
        k: int,
        direction: types.Direction = types.Direction.OUT,
        label: typing.Optional[str] = None,
    ) -> typing.List[types.Hub]:
        """The top `k` nodes by degree, optionally only those with `label`."""
        result = _call(lambda: self._h.top_hubs(k, direction._to_ffi(), label))
        return [types.Hub._from_ffi(h) for h in result]

    def cascade_delete(self, root: int, containment_edge: str) -> typing.List[int]:
        """Recursively deletes `root` and all descendants reachable via `containment_edge`."""
        return _call(lambda: self._h.cascade_delete(root, containment_edge))

    # ----- tables -------------------------------------------------------------------

    def list_tables(self) -> typing.List[types.TableSchema]:
        return [types.TableSchema._from_ffi(s) for s in _call(self._h.list_tables)]

    def table_schema(self, name: str) -> typing.Optional[types.TableSchema]:
        s = _call(lambda: self._h.table_schema(name))
        return types.TableSchema._from_ffi(s) if s is not None else None

    def create_index(self, table: str, column: str) -> None:
        """Adds a secondary index (existing rows are indexed immediately)."""
        _call(lambda: self._h.create_index(table, column))

    def drop_index(self, table: str, column: str) -> None:
        _call(lambda: self._h.drop_index(table, column))

    def aggregate(
        self,
        table: str,
        aggregates: typing.Sequence[Agg],
        where: Where = None,
        group_by: typing.Sequence[str] = (),
    ) -> typing.List[types.AggregateRow]:
        """``GROUP BY group_by`` aggregates over the rows matching `where`.
        Without `group_by` there is exactly one result row."""
        q = to_ffi_query(where)
        ffi_aggs = [a._to_ffi() for a in aggregates]
        rows = _call(lambda: self._h.aggregate(table, q, list(group_by), ffi_aggs))
        return [types.AggregateRow._from_ffi(r) for r in rows]

    # ----- bulk sync ----------------------------------------------------------------

    def sync_batch(
        self,
        nodes: typing.Sequence[NodeSpec] = (),
        edges: typing.Sequence[EdgeSpec] = (),
        rows: typing.Optional[typing.Mapping[str, typing.Sequence[types.PropertiesLike]]] = None,
    ) -> types.SyncResult:
        """Ingests `nodes`, `edges` and relational `rows` (``{table: [row, ...]}``,
        upserted by primary key) in a single atomic transaction."""
        rows = rows or {}
        batch = FfiSyncBatch(
            nodes=[FfiNodeInput(label=label, properties=_props(p)) for label, p in nodes],
            edges=[
                FfiEdgeInput(_from=frm, edge_type=edge_type, to=to, properties=_props(p))
                for frm, edge_type, to, p in edges
            ],
            rows=[FfiTableRows(table=t, rows=[_props(r) for r in rs]) for t, rs in rows.items()],
        )
        result = _call(lambda: self._h.sync_batch(batch))
        return types.SyncResult(
            node_ids=list(result.node_ids),
            edge_ids=list(result.edge_ids),
            row_pks={t: [types.from_ffi_value(v) for v in pks] for t, pks in zip(rows, result.row_pks)},
        )

    # ----- integrations ---------------------------------------------------------------

    def select_df(self, table: str, where: Where = None, **kwargs: typing.Any) -> "typing.Any":
        """Like :meth:`select`, as a ``pandas.DataFrame`` indexed by primary
        key. Requires ``pip install bkndb[pandas]``."""
        try:
            import pandas as pd
        except ImportError as exc:  # pragma: no cover - depends on environment
            raise ImportError("select_df requires pandas: pip install 'bkndb[pandas]'") from exc
        schema = self.table_schema(table)
        pk_name = schema.primary_key if schema is not None else "pk"
        rows = self.select(table, where, **kwargs)
        frame = pd.DataFrame.from_records([r.values for r in rows], index=[r.pk for r in rows])
        frame.index.name = pk_name
        return frame

    def to_networkx(
        self,
        start: int,
        max_depth: int = 10,
        direction: types.Direction = types.Direction.BOTH,
    ) -> "typing.Any":
        """The subgraph reachable from `start` within `max_depth` hops, as a
        ``networkx.MultiDiGraph`` (node/edge attributes carry label, type and
        properties). Requires ``pip install bkndb[networkx]``."""
        try:
            import networkx as nx
        except ImportError as exc:  # pragma: no cover - depends on environment
            raise ImportError("to_networkx requires networkx: pip install 'bkndb[networkx]'") from exc
        graph = nx.MultiDiGraph()
        reached = [h.node_id for h in self.traverse(start, direction, max_depth)]
        inside = set(reached)
        for node_id in reached:
            node = self.get_node(node_id)
            if node is not None:
                graph.add_node(node_id, label=node.label, **node.properties)
        for node_id in reached:
            for n in self.neighbors(node_id, types.Direction.OUT):
                if n.node_id in inside:
                    edge = self.get_edge(n.edge_id)
                    props = edge.properties if edge is not None else {}
                    graph.add_edge(node_id, n.node_id, key=n.edge_id, type=n.edge_type, **props)
        return graph
