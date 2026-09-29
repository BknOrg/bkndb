"""Pythonic value types for bkndb's public API.

Two jobs live here:

1. Converting between plain Python values (``str``/``int``/``float``/
   ``bool``/``bytes``/``None``) and bkndb's tagged ``FfiPropValue`` union —
   application code never imports ``FfiPropValue`` directly.
2. Plain, ``__slots__``-based result dataclasses (:class:`Node`,
   :class:`Row`, ...) wrapping the raw ``Ffi*`` records the native layer
   returns, with values already converted back to plain Python.

Everything in :mod:`bkndb._native` is private — this module, :mod:`bkndb.query`
and :mod:`bkndb.database` are the only places allowed to import from it.
"""
from __future__ import annotations

import dataclasses
import datetime
import enum
import typing
import uuid

from ._native.bkndb_ffi import (
    FfiAggregateRow,
    FfiColumn,
    FfiDbStats,
    FfiIntegrityReport,
    FfiStorageStats,
    FfiColumnKind,
    FfiDirection,
    FfiEdgeRecord,
    FfiHubRecord,
    FfiNeighbor,
    FfiNodeRecord,
    FfiPathResult,
    FfiPathStep,
    FfiPropValue,
    FfiQueryResult,
    FfiRow,
    FfiScoredRow,
    FfiTableSchema,
    FfiTraversalHit,
    FfiTypedNeighbor,
)

#: A property / column value: ``None``, ``str``, ``int``, ``float``,
#: ``bool``, ``bytes``, ``datetime.datetime`` (stored as UTC microseconds),
#: ``uuid.UUID``, a ``list`` of values or a ``dict`` of ``str`` -> value
#: (nesting freely, like JSON). ``bytearray`` and ``tuple`` are also
#: accepted on input.
PropertyValue = typing.Union[
    None,
    str,
    int,
    float,
    bool,
    bytes,
    datetime.datetime,
    uuid.UUID,
    typing.List[typing.Any],
    typing.Dict[str, typing.Any],
]

_EPOCH = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
_MICRO = datetime.timedelta(microseconds=1)
_U64 = (1 << 64) - 1


def datetime_to_micros(value: datetime.datetime) -> int:
    """Microseconds since the epoch; a naive datetime is taken as UTC."""
    if value.tzinfo is None:
        value = value.replace(tzinfo=datetime.timezone.utc)
    return (value - _EPOCH) // _MICRO


def micros_to_datetime(micros: int) -> datetime.datetime:
    """An aware UTC datetime."""
    return _EPOCH + datetime.timedelta(microseconds=micros)
#: Properties as returned by bkndb.
Properties = typing.Dict[str, PropertyValue]
#: Properties as accepted by bkndb: any mapping (so ``dict[str, str]`` fits).
PropertiesLike = typing.Mapping[str, PropertyValue]


# UniFFI rebinds `FfiPropValue` to its tagged-union class at import time,
# which static checkers can't follow — hence `Any` at this boundary.
def to_ffi_value(value: typing.Union[PropertyValue, bytearray]) -> typing.Any:
    if value is None:
        return FfiPropValue.NULL()
    # `bool` must be checked before `int` — `bool` is an `int` subclass in
    # Python, so `isinstance(True, int)` is true and would otherwise shadow
    # the BOOL branch entirely.
    if isinstance(value, bool):
        return FfiPropValue.BOOL(value)
    if isinstance(value, int):
        return FfiPropValue.INT(value)
    if isinstance(value, float):
        return FfiPropValue.FLOAT(value)
    if isinstance(value, str):
        return FfiPropValue.STR(value)
    if isinstance(value, (bytes, bytearray)):
        return FfiPropValue.BYTES(bytes(value))
    if isinstance(value, datetime.datetime):
        return FfiPropValue.TIMESTAMP(datetime_to_micros(value))
    if isinstance(value, uuid.UUID):
        return FfiPropValue.UUID(hi=value.int >> 64, lo=value.int & _U64)
    if isinstance(value, (list, tuple)):
        return FfiPropValue.LIST([to_ffi_value(v) for v in value])
    if isinstance(value, typing.Mapping):
        items = {}
        for k, v in value.items():
            if not isinstance(k, str):
                raise TypeError(f"map keys must be str, got {type(k).__name__}")
            items[k] = to_ffi_value(v)
        return FfiPropValue.MAP(items)
    raise TypeError(f"unsupported property value type: {type(value).__name__}")


def from_ffi_value(value: typing.Any) -> PropertyValue:
    if value.is_null():
        return None
    if value.is_timestamp():
        return micros_to_datetime(value[0])
    if value.is_uuid():
        return uuid.UUID(int=(value.hi << 64) | value.lo)
    if value.is_list():
        return [from_ffi_value(v) for v in value[0]]
    if value.is_map():
        return {k: from_ffi_value(v) for k, v in value[0].items()}
    return value[0]


def to_ffi_properties(properties: typing.Mapping[str, PropertyValue]) -> typing.Dict[str, FfiPropValue]:
    return {key: to_ffi_value(val) for key, val in properties.items()}


def from_ffi_properties(properties: typing.Mapping[str, FfiPropValue]) -> Properties:
    return {key: from_ffi_value(val) for key, val in properties.items()}


class Direction(enum.Enum):
    """Which way to traverse edges — outgoing, incoming, or both."""

    OUT = "out"
    IN = "in"
    BOTH = "both"

    def _to_ffi(self) -> FfiDirection:
        return {
            Direction.OUT: FfiDirection.OUT,
            Direction.IN: FfiDirection.IN,
            Direction.BOTH: FfiDirection.BOTH,
        }[self]


# ---- graph results ----------------------------------------------------------


@dataclasses.dataclass(frozen=True, slots=True)
class Node:
    id: int
    label: str
    properties: Properties = dataclasses.field(hash=False)

    @classmethod
    def _from_ffi(cls, record: FfiNodeRecord) -> "Node":
        return cls(id=record.id, label=record.label, properties=from_ffi_properties(record.properties))


@dataclasses.dataclass(frozen=True, slots=True)
class Edge:
    id: int
    from_node: int
    to_node: int
    edge_type: str
    properties: Properties = dataclasses.field(hash=False)

    @classmethod
    def _from_ffi(cls, record: FfiEdgeRecord) -> "Edge":
        return cls(
            id=record.id,
            from_node=record._from,
            to_node=record.to,
            edge_type=record.edge_type,
            properties=from_ffi_properties(record.properties),
        )


@dataclasses.dataclass(frozen=True, slots=True)
class Neighbor:
    node_id: int
    edge_id: int

    @classmethod
    def _from_ffi(cls, record: FfiNeighbor) -> "Neighbor":
        return cls(node_id=record.node_id, edge_id=record.edge_id)


@dataclasses.dataclass(frozen=True, slots=True)
class TypedNeighbor:
    """A neighbor plus the type of the edge leading to it."""

    node_id: int
    edge_id: int
    edge_type: str

    @classmethod
    def _from_ffi(cls, record: FfiTypedNeighbor) -> "TypedNeighbor":
        return cls(node_id=record.node_id, edge_id=record.edge_id, edge_type=record.edge_type)


@dataclasses.dataclass(frozen=True, slots=True)
class TraversalHit:
    """A node reached by :meth:`bkndb.Database.traverse` (the start node has depth 0)."""

    node_id: int
    depth: int
    via_edge_id: typing.Optional[int]
    parent_id: typing.Optional[int]

    @classmethod
    def _from_ffi(cls, record: FfiTraversalHit) -> "TraversalHit":
        return cls(
            node_id=record.node_id,
            depth=record.depth,
            via_edge_id=record.via_edge_id,
            parent_id=record.parent_id,
        )


@dataclasses.dataclass(frozen=True, slots=True)
class PathStep:
    node_id: int
    via_edge_id: typing.Optional[int]
    edge_type: typing.Optional[str]

    @classmethod
    def _from_ffi(cls, record: FfiPathStep) -> "PathStep":
        return cls(node_id=record.node_id, via_edge_id=record.via_edge_id, edge_type=record.edge_type)


@dataclasses.dataclass(frozen=True, slots=True)
class Path:
    node_ids: typing.List[int] = dataclasses.field(hash=False)
    edge_ids: typing.List[int] = dataclasses.field(hash=False)
    steps: typing.List[PathStep] = dataclasses.field(hash=False)

    @classmethod
    def _from_ffi(cls, record: FfiPathResult) -> "Path":
        return cls(
            node_ids=list(record.node_ids),
            edge_ids=list(record.edge_ids),
            steps=[PathStep._from_ffi(s) for s in record.steps],
        )


@dataclasses.dataclass(frozen=True, slots=True)
class WeightedPath:
    """A lowest-cost path (see :meth:`bkndb.Database.find_weighted_path`)."""

    path: Path
    cost: float


@dataclasses.dataclass(frozen=True, slots=True)
class NewNode:
    """In :meth:`bkndb.Database.sync_batch` edges, refers to the node at
    ``index`` (0-based) of the same batch's ``nodes`` — whose id isn't known
    until the batch runs::

        db.sync_batch(
            nodes=[("File", {}), ("Function", {})],
            edges=[(NewNode(0), "DEFINES", NewNode(1), {}), (repo_id, "CONTAINS", NewNode(0), {})],
        )
    """

    index: int


@dataclasses.dataclass(frozen=True, slots=True)
class Hub:
    node_id: int
    degree: int

    @classmethod
    def _from_ffi(cls, record: FfiHubRecord) -> "Hub":
        return cls(node_id=record.node_id, degree=record.degree)


@dataclasses.dataclass(frozen=True, slots=True)
class SyncResult:
    node_ids: typing.List[int] = dataclasses.field(hash=False)
    edge_ids: typing.List[int] = dataclasses.field(hash=False)
    #: Primary keys of the synced rows, per table, in the order given.
    row_pks: typing.Dict[str, typing.List[PropertyValue]] = dataclasses.field(hash=False, default_factory=dict)


# ---- relational ---------------------------------------------------------------

ColumnType = typing.Union[str, type]

_KINDS: typing.Dict[typing.Any, FfiColumnKind] = {
    bool: FfiColumnKind.BOOL,
    int: FfiColumnKind.INT,
    float: FfiColumnKind.FLOAT,
    str: FfiColumnKind.STR,
    bytes: FfiColumnKind.BYTES,
    datetime.datetime: FfiColumnKind.TIMESTAMP,
    uuid.UUID: FfiColumnKind.UUID,
    list: FfiColumnKind.LIST,
    dict: FfiColumnKind.MAP,
    "bool": FfiColumnKind.BOOL,
    "int": FfiColumnKind.INT,
    "float": FfiColumnKind.FLOAT,
    "str": FfiColumnKind.STR,
    "bytes": FfiColumnKind.BYTES,
    "timestamp": FfiColumnKind.TIMESTAMP,
    "datetime": FfiColumnKind.TIMESTAMP,
    "uuid": FfiColumnKind.UUID,
    "list": FfiColumnKind.LIST,
    "map": FfiColumnKind.MAP,
    "dict": FfiColumnKind.MAP,
}
_KIND_NAMES = {
    FfiColumnKind.BOOL: "bool",
    FfiColumnKind.INT: "int",
    FfiColumnKind.FLOAT: "float",
    FfiColumnKind.STR: "str",
    FfiColumnKind.BYTES: "bytes",
    FfiColumnKind.TIMESTAMP: "timestamp",
    FfiColumnKind.UUID: "uuid",
    FfiColumnKind.LIST: "list",
    FfiColumnKind.MAP: "map",
}


@dataclasses.dataclass(frozen=True, slots=True)
class Column:
    """One column of a :class:`TableSchema`.

    ``type`` is a Python type (``int``, ``float``, ``str``, ``bool``,
    ``bytes``) or its name. ``nullable=False`` makes it NOT NULL,
    ``unique=True`` adds a UNIQUE constraint (and an index), and ``default``
    is filled in when an inserted row omits the column.
    """

    name: str
    type: ColumnType
    nullable: bool = True
    unique: bool = False
    default: PropertyValue = None

    def _to_ffi(self) -> FfiColumn:
        try:
            kind = _KINDS[self.type]
        except KeyError:
            raise TypeError(f"unsupported column type for '{self.name}': {self.type!r}") from None
        return FfiColumn(
            name=self.name,
            kind=kind,
            nullable=self.nullable,
            unique=self.unique,
            default_value=None if self.default is None else to_ffi_value(self.default),
        )

    @classmethod
    def _from_ffi(cls, c: FfiColumn) -> "Column":
        return cls(
            name=c.name,
            type=_KIND_NAMES[c.kind],
            nullable=c.nullable,
            unique=c.unique,
            default=None if c.default_value is None else from_ffi_value(c.default_value),
        )


@dataclasses.dataclass(frozen=True, slots=True)
class TableSchema:
    """Definition of a relational table::

        TableSchema(
            "users",
            [Column("id", int), Column("email", str, nullable=False, unique=True)],
            primary_key="id",
            auto_increment=True,
            indexes=["email"],
        )
    """

    name: str
    columns: typing.Sequence[Column] = dataclasses.field(hash=False)
    primary_key: str = "id"
    auto_increment: bool = False
    indexes: typing.Sequence[str] = dataclasses.field(default=(), hash=False)

    def column(self, name: str) -> typing.Optional[Column]:
        return next((c for c in self.columns if c.name == name), None)

    def _to_ffi(self) -> FfiTableSchema:
        return FfiTableSchema(
            name=self.name,
            columns=[c._to_ffi() for c in self.columns],
            primary_key=self.primary_key,
            auto_increment=self.auto_increment,
            indexed_columns=list(self.indexes),
        )

    @classmethod
    def _from_ffi(cls, s: FfiTableSchema) -> "TableSchema":
        return cls(
            name=s.name,
            columns=tuple(Column._from_ffi(c) for c in s.columns),
            primary_key=s.primary_key,
            auto_increment=s.auto_increment,
            indexes=tuple(s.indexed_columns),
        )


@dataclasses.dataclass(frozen=True, slots=True)
class Row:
    """A relational row: its primary key and its other column values.

    Supports ``row["column"]`` and ``row.get("column")`` for the non-pk
    columns; the primary key is ``row.pk``.
    """

    pk: PropertyValue
    values: Properties = dataclasses.field(hash=False)

    def __getitem__(self, column: str) -> PropertyValue:
        return self.values[column]

    def get(self, column: str, default: PropertyValue = None) -> PropertyValue:
        return self.values.get(column, default)

    def to_dict(self, pk_column: str = "pk") -> Properties:
        """The row as one flat dict, with the pk stored under ``pk_column``."""
        return {pk_column: self.pk, **self.values}

    @classmethod
    def _from_ffi(cls, record: FfiRow) -> "Row":
        return cls(pk=from_ffi_value(record.pk), values=from_ffi_properties(record.values))


@dataclasses.dataclass(frozen=True, slots=True)
class AggregateRow:
    """One group of a grouped aggregate: the group key values (in
    ``group_by`` order) and one value per requested aggregate."""

    group: typing.Tuple[PropertyValue, ...]
    values: typing.Tuple[PropertyValue, ...]

    @classmethod
    def _from_ffi(cls, record: FfiAggregateRow) -> "AggregateRow":
        return cls(
            group=tuple(from_ffi_value(v) for v in record.group),
            values=tuple(from_ffi_value(v) for v in record.values),
        )


@dataclasses.dataclass(frozen=True, slots=True)
class StorageStats:
    """File-level figures of an on-disk database (see :attr:`DbStats.storage`)."""

    #: Size of the ``.bkndb`` file.
    file_bytes: int
    #: On-disk sorted segments; many of them means compaction is due.
    sstable_count: int
    #: Segments still in the older, checksum-less format (upgraded by :meth:`bkndb.Database.compact`).
    legacy_sstable_count: int
    sstable_bytes: int
    #: Stored entries including superseded versions and deletion markers.
    sstable_entries: int
    #: Recent writes buffered in memory (durable in the write-ahead log).
    memtable_entries: int
    memtable_bytes: int
    wal_bytes: int
    #: Dead space that :meth:`bkndb.Database.compact` would give back.
    reclaimable_bytes: int

    @classmethod
    def _from_ffi(cls, s: FfiStorageStats) -> "StorageStats":
        return cls(**{f.name: getattr(s, f.name) for f in dataclasses.fields(cls)})


@dataclasses.dataclass(frozen=True, slots=True)
class DbStats:
    """What :meth:`bkndb.Database.stats` reports."""

    nodes: int
    edges: int
    #: Row count per relational table.
    tables: typing.Mapping[str, int] = dataclasses.field(hash=False)
    #: ``None`` for in-memory databases.
    storage: typing.Optional[StorageStats] = None

    @classmethod
    def _from_ffi(cls, s: FfiDbStats) -> "DbStats":
        return cls(
            nodes=s.nodes,
            edges=s.edges,
            tables={t.table: t.rows for t in s.tables},
            storage=StorageStats._from_ffi(s.storage) if s.storage is not None else None,
        )


@dataclasses.dataclass(frozen=True, slots=True)
class IntegrityReport:
    """What :meth:`bkndb.Database.verify_integrity` checked."""

    sstables_checked: int
    #: Blocks whose checksum was recomputed and matched.
    blocks_verified: int
    #: Blocks in the older format, which carry no checksum (decoded, but not checksummed).
    legacy_blocks_unchecked: int
    entries: int
    wal_records: int

    @classmethod
    def _from_ffi(cls, r: FfiIntegrityReport) -> "IntegrityReport":
        return cls(**{f.name: getattr(r, f.name) for f in dataclasses.fields(cls)})


@dataclasses.dataclass(frozen=True, slots=True)
class QueryResult:
    """What :meth:`bkndb.Database.sql` / :meth:`bkndb.Database.graph_query`
    return: column names, rows (tuples, in column order) and, for writes,
    how many rows were affected. Iterating yields the rows."""

    columns: typing.Tuple[str, ...]
    rows: typing.List[typing.Tuple[typing.Any, ...]] = dataclasses.field(hash=False)
    affected: int = 0

    @classmethod
    def _from_ffi(cls, r: FfiQueryResult) -> "QueryResult":
        return cls(
            columns=tuple(r.columns),
            rows=[tuple(from_ffi_value(v) for v in row) for row in r.rows],
            affected=r.affected,
        )

    def __iter__(self) -> typing.Iterator[typing.Tuple[typing.Any, ...]]:
        return iter(self.rows)

    def __len__(self) -> int:
        return len(self.rows)

    def dicts(self) -> typing.List[typing.Dict[str, typing.Any]]:
        """The rows as ``{column: value}`` dicts."""
        return [dict(zip(self.columns, row)) for row in self.rows]

    def scalar(self) -> typing.Any:
        """The first column of the first row (``None`` if there are no rows)."""
        return self.rows[0][0] if self.rows and self.rows[0] else None

    def column(self, name: str) -> typing.List[typing.Any]:
        """Every value of one column."""
        i = self.columns.index(name)
        return [row[i] for row in self.rows]


@dataclasses.dataclass(frozen=True, slots=True)
class ScoredRow:
    """A search hit (see :meth:`bkndb.Database.search_text` and
    :meth:`bkndb.Database.search_vector`)."""

    row: "Row"
    #: BM25 relevance for text; cosine similarity / dot product / Euclidean
    #: distance for vectors.
    score: float

    @classmethod
    def _from_ffi(cls, s: FfiScoredRow) -> "ScoredRow":
        return cls(row=Row._from_ffi(s.row), score=s.score)


def pack_vector(values: typing.Iterable[float]) -> bytes:
    """Packs an embedding as little-endian float32 bytes — the compact way to
    store vectors for :meth:`bkndb.Database.search_vector` (4 bytes per
    dimension, versus ~12 for a list of floats). Accepts any iterable of
    numbers, including a NumPy array."""
    import struct

    floats = [float(v) for v in values]
    return struct.pack(f"<{len(floats)}f", *floats)

