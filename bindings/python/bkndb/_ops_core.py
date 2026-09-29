"""Operations shared by :class:`bkndb.Database` and :class:`bkndb.Transaction`,
plus the :class:`Table` and :class:`Transaction` wrappers."""
from __future__ import annotations

import typing

from . import types
from ._common import EdgeSpec, NodeSpec, Params, _call, _params, _props, _row_or_none
from ._native.bkndb_ffi import BknDbTransaction, FfiEdgeInput, FfiNodeInput
from .query import OrderBy, Where, to_ffi_query


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
        from .database import Database

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
