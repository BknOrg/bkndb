"""Pythonic wrapper around the raw UniFFI-generated ``BknDbEngine``.

This is the one hand-maintained adapter layer between bkndb's native
extension and its public Python API. It exists so the underlying Rust
surface can grow without breaking callers — application code should only
ever import from :mod:`bkndb`, never reach into :mod:`bkndb._native`.
"""
from __future__ import annotations

import csv
import json
import os
import typing

from . import _io, errors, types
from ._native.bkndb_ffi import (
    BknDbEngine,
    BknDbTransaction,
    FfiEdgeInput,
    FfiLinkedEdgeInput,
    FfiLsmOptions,
    FfiNodeInput,
    FfiNodeRef,
    FfiSyncBatch,
    FfiTableRows,
    FfiVectorMetric,
)
from .query import Agg, OrderBy, Where, _where_expr, col, to_ffi_filter, to_ffi_query

T = typing.TypeVar("T")

NodeSpec = typing.Tuple[str, types.PropertiesLike]
EdgeSpec = typing.Tuple[int, str, int, types.PropertiesLike]
NodeRefLike = typing.Union[int, types.NewNode]
SyncEdgeSpec = typing.Tuple[NodeRefLike, str, NodeRefLike, types.PropertiesLike]


def _call(fn: typing.Callable[[], T]) -> T:
    """Runs `fn`, translating anything the native layer raises into a public exception."""
    try:
        return fn()
    except BaseException as exc:
        mapped = errors.wrap(exc)
        if mapped is exc:
            raise
        raise mapped from exc


def _node_ref(r: "NodeRefLike") -> typing.Any:
    if isinstance(r, types.NewNode):
        return FfiNodeRef.NEW(r.index)
    return FfiNodeRef.EXISTING(r)


def _props(p: typing.Optional[typing.Mapping[str, types.PropertyValue]]) -> dict:
    return types.to_ffi_properties(p or {})


Params = typing.Union[typing.Sequence[types.PropertyValue], typing.Mapping[str, types.PropertyValue], None]


def _params(params: Params) -> typing.Tuple[list, typing.Optional[dict]]:
    if params is None:
        return [], None
    if isinstance(params, typing.Mapping):
        return [], {str(k): types.to_ffi_value(v) for k, v in params.items()}
    if isinstance(params, (str, bytes)):
        raise TypeError("params must be a list/tuple or a dict, not a single value")
    return [types.to_ffi_value(v) for v in params], None


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

    def nodes_by_label(self, label: str) -> typing.List[int]:
        """Ids of every node with `label`, ascending."""
        return _call(lambda: self._h.nodes_by_label(label))

    def find_nodes(self, label: str, property: str, value: types.PropertyValue) -> typing.List[int]:  # noqa: A002
        """Ids of nodes with `label` whose `property` equals `value`. A fast
        lookup once :meth:`Database.create_node_index` indexed the property."""
        ffi = types.to_ffi_value(value)
        return _call(lambda: self._h.find_nodes(label, property, ffi))

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

    # ----- query languages -------------------------------------------------------

    def sql(self, query: str, params: "Params" = None) -> types.QueryResult:
        """Runs one SQL statement on the relational tables::

            db.sql("CREATE TABLE users (id INT PRIMARY KEY AUTOINCREMENT, name TEXT, age INT)")
            db.sql("INSERT INTO users (name, age) VALUES (?, ?)", ["Ana", 30])
            db.sql("SELECT name FROM users WHERE age >= :min ORDER BY name", {"min": 18}).column("name")

        ``params`` is a list (for ``?``, ``?N``, ``$N``) or a dict (for
        ``:name``). A ``SELECT`` reads a snapshot; any other statement runs
        atomically (on a :class:`Transaction`, inside it). Raises
        :class:`bkndb.QueryError` for syntax errors."""
        positional, named = _params(params)
        return types.QueryResult._from_ffi(_call(lambda: self._h.sql(query, positional, named)))

    def graph_query(self, query: str, params: "Params" = None) -> types.QueryResult:
        """Runs a Cypher-style ``MATCH ... RETURN ...`` query on the graph::

            db.graph_query(
                "MATCH (a:Person {name: $name})-[:KNOWS*1..2]->(b) RETURN DISTINCT b.name",
                {"name": "Ana"},
            )

        Nodes come back as ``{"id", "label", "properties"}`` dicts, edges as
        ``{"id", "type", "from", "to", "properties"}``. ``params``: a dict
        for ``$name``, or a list for ``$1``/``?``."""
        positional, named = _params(params)
        return types.QueryResult._from_ffi(_call(lambda: self._h.graph_query(query, positional, named)))


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
        # Stream in pk-ordered batches when the handle comes from a Database.
        if isinstance(self._ops, Database):
            return self._ops.iter_rows(self.name)
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
        path: "_io.PathLike",
        *,
        memtable_flush_bytes: typing.Optional[int] = None,
        compaction_trigger_files: typing.Optional[int] = None,
        block_size: typing.Optional[int] = None,
        compression: typing.Optional[bool] = None,
    ) -> "Database":
        """Opens or creates an on-disk single-file database at `path`.

        Raises :class:`bkndb.DatabaseLockedError` if another handle or process
        has it open, and :class:`bkndb.CorruptionError` if its structure is
        damaged. The keyword options tune the storage engine:
        ``memtable_flush_bytes`` (write buffer size, default 16 MiB),
        ``compaction_trigger_files`` (default 16), ``block_size`` (bytes per
        on-disk block, default 4096) and ``compression`` (lz4, default on).
        """
        path = os.fspath(path)
        tuned = (memtable_flush_bytes, compaction_trigger_files, block_size, compression)
        if all(o is None for o in tuned):
            return cls(_call(lambda: BknDbEngine.open(path)), path)
        options = FfiLsmOptions(
            memtable_flush_bytes=memtable_flush_bytes if memtable_flush_bytes is not None else 16 * 1024 * 1024,
            compaction_trigger_files=compaction_trigger_files if compaction_trigger_files is not None else 16,
            block_size_bytes=block_size,
            compression=compression,
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
        """Rewrites the file with only live data: reclaims space from
        overwritten/deleted data and upgrades older on-disk formats. Blocks
        writers while it runs."""
        _call(self._h.compact)

    # ----- search --------------------------------------------------------------------

    def create_fulltext_index(self, table: str, column: str) -> bool:
        """Builds a full-text index over a ``str`` column (existing rows are
        indexed now, later writes automatically). ``False`` if it exists."""
        return _call(lambda: self._h.create_fulltext_index(table, column))

    def drop_fulltext_index(self, table: str, column: str) -> bool:
        return _call(lambda: self._h.drop_fulltext_index(table, column))

    def fulltext_indexes(self, table: str) -> typing.List[str]:
        """Columns of `table` that have a full-text index."""
        return list(_call(lambda: self._h.list_fulltext_indexes(table)))

    def search_text(
        self,
        table: str,
        column: str,
        query: str,
        limit: int = 10,
        *,
        match_all: bool = False,
        where: Where = None,
    ) -> typing.List[types.ScoredRow]:
        """Rows whose `column` best matches `query`, ranked by BM25 (needs
        :meth:`create_fulltext_index`). Matching is case-insensitive on
        whole words; ``word*`` matches a prefix. ``match_all=True`` requires
        every word; ``where`` filters the rows further."""
        flt = to_ffi_filter(where)
        hits = _call(lambda: self._h.search_text(table, column, query, limit, match_all, flt))
        return [types.ScoredRow._from_ffi(h) for h in hits]

    def search_vector(
        self,
        table: str,
        column: str,
        vector: typing.Iterable[float],
        limit: int = 10,
        *,
        metric: str = "cosine",
        where: Where = None,
    ) -> typing.List[types.ScoredRow]:
        """The `limit` rows whose embedding in `column` is nearest to
        `vector` — exact search, scanning the (filtered) rows. Embeddings are
        a ``list`` of numbers or :func:`bkndb.pack_vector` bytes. ``metric``:
        ``"cosine"`` / ``"dot"`` (score = similarity, highest first) or
        ``"euclidean"`` (score = distance, lowest first)."""
        metrics = {"cosine": FfiVectorMetric.COSINE, "dot": FfiVectorMetric.DOT, "euclidean": FfiVectorMetric.EUCLIDEAN}
        try:
            m = metrics[metric]
        except KeyError:
            raise ValueError(f"metric must be one of {sorted(metrics)}") from None
        values = [float(v) for v in vector]
        flt = to_ffi_filter(where)
        hits = _call(lambda: self._h.search_vector(table, column, values, limit, m, flt))
        return [types.ScoredRow._from_ffi(h) for h in hits]

    # ----- operations --------------------------------------------------------------

    def backup(self, path: "_io.PathLike") -> None:
        """Writes a consistent, compacted copy of everything committed so far
        to a new ``.bkndb`` file at `path` (which must not exist yet). Reads
        and writes carry on meanwhile; an open transaction's uncommitted
        writes are not included. On-disk databases only."""
        dest = os.fspath(path)
        _call(lambda: self._h.backup(dest))

    def stats(self) -> types.DbStats:
        """Node, edge and per-table row counts (from one snapshot), plus file
        figures for on-disk databases. Counting scans the data, so this is
        for monitoring and tooling rather than hot paths."""
        return types.DbStats._from_ffi(_call(self._h.stats))

    def verify_integrity(self) -> types.IntegrityReport:
        """Re-reads and checksums every stored byte. Raises
        :class:`bkndb.CorruptionError` naming the first damaged structure."""
        return types.IntegrityReport._from_ffi(_call(self._h.verify_integrity))

    def _schema(self, table: str) -> types.TableSchema:
        schema = self.table_schema(table)
        if schema is None:
            raise errors.TableNotFoundError(table)
        return schema

    def iter_rows(
        self,
        table: str,
        where: Where = None,
        *,
        batch_size: int = 1000,
        columns: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.Iterator[types.Row]:
        """Lazily yields the rows matching `where` in primary-key order,
        fetching ``batch_size`` rows at a time, so memory stays bounded however
        large the table is.

        Each batch is read from the latest committed state, so rows written
        while iterating may or may not show up; no row is yielded twice.
        """
        if batch_size < 1:
            raise ValueError("batch_size must be >= 1")
        pk = self._schema(table).primary_key
        base = _where_expr(where)
        last: typing.Optional[types.PropertyValue] = None
        while True:
            after = None if last is None else col(pk) > last
            if base is None:
                cond = after
            elif after is None:
                cond = base
            else:
                cond = base & after
            batch = self.select(table, cond, order_by=pk, limit=batch_size, columns=columns)
            yield from batch
            if len(batch) < batch_size:
                return
            last = batch[-1].pk

    def export_jsonl(self, table: str, path: "_io.PathLike", where: Where = None) -> int:
        """Writes the rows matching `where` to `path` as JSON Lines (one
        object per row, primary key included); returns how many."""
        schema = self._schema(table)
        n = 0
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            for row in self.iter_rows(table, where):
                f.write(json.dumps(_io.row_to_json(schema, row), ensure_ascii=False))
                f.write("\n")
                n += 1
        return n

    def export_csv(self, table: str, path: "_io.PathLike", where: Where = None) -> int:
        """Writes the rows matching `where` to `path` as CSV with a header
        row (primary key first, then columns in schema order); returns how
        many rows."""
        schema = self._schema(table)
        header = _io.csv_header(schema)
        n = 0
        with open(path, "w", encoding="utf-8", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(header)
            for row in self.iter_rows(table, where):
                writer.writerow(_io.row_to_csv(header, schema, row))
                n += 1
        return n

    def _import(
        self,
        table: str,
        rows: typing.Iterable[typing.Dict[str, types.PropertyValue]],
        mode: str,
        batch_size: int,
    ) -> int:
        if mode not in ("upsert", "insert"):
            raise ValueError("mode must be 'upsert' or 'insert'")
        if batch_size < 1:
            raise ValueError("batch_size must be >= 1")
        n = 0
        with self.transaction() as tx:
            write = tx.upsert_many if mode == "upsert" else tx.insert_many
            batch: typing.List[typing.Dict[str, types.PropertyValue]] = []
            for row in rows:
                batch.append(row)
                if len(batch) >= batch_size:
                    write(table, batch)
                    n += len(batch)
                    batch = []
            if batch:
                write(table, batch)
                n += len(batch)
        return n

    def import_jsonl(self, table: str, path: "_io.PathLike", *, mode: str = "upsert", batch_size: int = 1000) -> int:
        """Loads rows from a JSON Lines file (as written by
        :meth:`export_jsonl`) into an existing table, all in one transaction:
        nothing is written if any row fails. ``mode="upsert"`` (default)
        replaces rows whose primary key exists; ``"insert"`` raises
        :class:`bkndb.DuplicateKeyError` instead. Returns how many rows."""
        self._schema(table)

        def rows() -> typing.Iterator[typing.Dict[str, types.PropertyValue]]:
            with open(path, encoding="utf-8") as f:
                for line_no, line in enumerate(f, 1):
                    if not line.strip():
                        continue
                    try:
                        obj = json.loads(line)
                    except json.JSONDecodeError as exc:
                        raise errors.InvalidArgumentError(f"line {line_no}: invalid JSON: {exc}") from None
                    if not isinstance(obj, dict):
                        raise errors.InvalidArgumentError(f"line {line_no}: expected a JSON object")
                    yield {k: _io.from_json_value(v) for k, v in obj.items()}

        return self._import(table, rows(), mode, batch_size)

    def import_csv(self, table: str, path: "_io.PathLike", *, mode: str = "upsert", batch_size: int = 1000) -> int:
        """Loads rows from a CSV file with a header row of column names into
        an existing table, converting each cell to its column's type, all in
        one transaction. Empty cells are left out (so DEFAULT/NULL applies,
        and an auto-increment key is assigned). ``mode`` as for
        :meth:`import_jsonl`. Returns how many rows."""
        schema = self._schema(table)

        def rows() -> typing.Iterator[typing.Dict[str, types.PropertyValue]]:
            with open(path, encoding="utf-8", newline="") as f:
                reader = csv.reader(f)
                header = next(reader, None)
                if header is None:
                    return
                for record in reader:
                    if not record:
                        continue
                    yield _io.csv_record_to_row(schema, header, record, reader.line_num)

        return self._import(table, rows(), mode, batch_size)

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

    def count_nodes(self, label: str) -> int:
        """Number of nodes with `label`."""
        return _call(lambda: self._h.count_nodes(label))

    def create_node_index(self, label: str, property: str) -> bool:  # noqa: A002
        """Indexes `property` of nodes with `label` for :meth:`find_nodes`
        (existing nodes are indexed immediately; only int/str values).
        Returns ``False`` if the index already existed."""
        return _call(lambda: self._h.create_node_index(label, property))

    def drop_node_index(self, label: str, property: str) -> bool:  # noqa: A002
        return _call(lambda: self._h.drop_node_index(label, property))

    def node_indexes(self) -> typing.List[typing.Tuple[str, str]]:
        """Every node property index as ``(label, property)``."""
        return [(i.label, i.property) for i in _call(self._h.list_node_indexes)]

    def rebuild_graph_indexes(self) -> None:
        """Rebuilds all graph indexes. Only needed once for database files
        created by bkndb versions without graph indexes, where label lookups
        otherwise fall back to (correct but slower) full scans."""
        _call(self._h.rebuild_graph_indexes)

    def find_weighted_path(
        self,
        start: int,
        target: int,
        weight: str = "weight",
        default_weight: float = 1.0,
        direction: types.Direction = types.Direction.OUT,
        edge_types: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.Optional[types.WeightedPath]:
        """Lowest-cost path (Dijkstra): each edge costs its numeric `weight`
        property, or `default_weight` if it has none. Weights must be >= 0."""
        types_list = list(edge_types) if edge_types is not None else None
        result = _call(
            lambda: self._h.find_weighted_path(
                start, target, direction._to_ffi(), types_list, weight, float(default_weight)
            )
        )
        if result is None:
            return None
        return types.WeightedPath(path=types.Path._from_ffi(result.path), cost=result.cost)

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
        edges: typing.Sequence[SyncEdgeSpec] = (),
        rows: typing.Optional[typing.Mapping[str, typing.Sequence[types.PropertiesLike]]] = None,
    ) -> types.SyncResult:
        """Ingests `nodes`, `edges` and relational `rows` (``{table: [row, ...]}``,
        upserted by primary key) in a single atomic transaction. An edge
        endpoint may be :class:`bkndb.NewNode` to refer to a node of this batch."""
        rows = rows or {}
        plain, linked = [], []
        for frm, edge_type, to, p in edges:
            if isinstance(frm, types.NewNode) or isinstance(to, types.NewNode):
                linked.append(
                    FfiLinkedEdgeInput(_from=_node_ref(frm), edge_type=edge_type, to=_node_ref(to), properties=_props(p))
                )
            else:
                plain.append(FfiEdgeInput(_from=frm, edge_type=edge_type, to=to, properties=_props(p)))
        batch = FfiSyncBatch(
            nodes=[FfiNodeInput(label=label, properties=_props(p)) for label, p in nodes],
            edges=plain,
            rows=[FfiTableRows(table=t, rows=[_props(r) for r in rs]) for t, rs in rows.items()],
            linked_edges=linked,
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
