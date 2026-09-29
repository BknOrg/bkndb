"""Table schemas, rows, statistics and query/search results (see :mod:`bkndb.types`)."""
from __future__ import annotations

import dataclasses
import datetime
import typing
import uuid

from ._native.bkndb_ffi import (
    FfiAggregateRow,
    FfiColumn,
    FfiColumnKind,
    FfiDbStats,
    FfiIntegrityReport,
    FfiQueryResult,
    FfiRow,
    FfiScoredRow,
    FfiStorageStats,
    FfiTableSchema,
    FfiVectorIndexInfo,
)
from ._values import Properties, PropertyValue, from_ffi_properties, from_ffi_value, to_ffi_value


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


@dataclasses.dataclass(frozen=True, slots=True)
class VectorIndexInfo:
    """An approximate (HNSW) vector index (see
    :meth:`bkndb.Database.create_vector_index`)."""

    column: str
    #: ``"cosine"``, ``"dot"`` or ``"euclidean"``.
    metric: str
    m: int
    ef_construction: int
    #: Dimensions of the indexed vectors; ``None`` while the index is empty.
    dimensions: typing.Optional[int]
    #: Vectors currently indexed.
    vectors: int

    @classmethod
    def _from_ffi(cls, i: FfiVectorIndexInfo) -> "VectorIndexInfo":
        return cls(
            column=i.column,
            metric=i.metric.name.lower(),
            m=i.m,
            ef_construction=i.ef_construction,
            dimensions=i.dimensions,
            vectors=i.vectors,
        )
