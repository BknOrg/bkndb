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
import enum
import typing

from ._native.bkndb_ffi import (
    FfiAggregateRow,
    FfiColumn,
    FfiColumnKind,
    FfiDirection,
    FfiEdgeRecord,
    FfiHubRecord,
    FfiNeighbor,
    FfiNodeRecord,
    FfiPathResult,
    FfiPathStep,
    FfiPropValue,
    FfiRow,
    FfiTableSchema,
    FfiTraversalHit,
    FfiTypedNeighbor,
)

#: A property / column value. ``bytearray`` is also accepted on input.
PropertyValue = typing.Union[None, str, int, float, bool, bytes]
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
    raise TypeError(f"unsupported property value type: {type(value).__name__}")


def from_ffi_value(value: typing.Any) -> PropertyValue:
    if value.is_null():
        return None
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
    "bool": FfiColumnKind.BOOL,
    "int": FfiColumnKind.INT,
    "float": FfiColumnKind.FLOAT,
    "str": FfiColumnKind.STR,
    "bytes": FfiColumnKind.BYTES,
}
_KIND_NAMES = {
    FfiColumnKind.BOOL: "bool",
    FfiColumnKind.INT: "int",
    FfiColumnKind.FLOAT: "float",
    FfiColumnKind.STR: "str",
    FfiColumnKind.BYTES: "bytes",
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
