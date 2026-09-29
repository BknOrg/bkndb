"""Pythonic value types for bkndb's public API.

Two jobs live here:

1. Converting between plain Python values (``str``/``int``/``float``/
   ``bool``/``bytes``/``None``) and bkndb's tagged ``FfiPropValue`` union —
   application code never imports ``FfiPropValue`` directly.
2. Plain, ``__slots__``-based result dataclasses (:class:`Node`, :class:`Edge`,
   ...) wrapping the raw ``Ffi*Record`` types the native layer returns, with
   properties already converted back to plain dicts.

Everything in :mod:`bkndb._native` is considered private — this module (and
:mod:`bkndb.database`) is the only place allowed to import from it.
"""
from __future__ import annotations

import dataclasses
import enum
import typing

from ._native.bkndb_ffi import (
    FfiDirection,
    FfiEdgeRecord,
    FfiHubRecord,
    FfiNeighbor,
    FfiNodeRecord,
    FfiPathResult,
    FfiPathStep,
    FfiPropValue,
)

PropertyValue = typing.Union[None, str, int, float, bool, bytes]
Properties = typing.Dict[str, PropertyValue]


def to_ffi_value(value: PropertyValue) -> FfiPropValue:
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


def from_ffi_value(value: FfiPropValue) -> PropertyValue:
    if value.is_null():
        return None
    return value[0]


def to_ffi_properties(properties: Properties) -> typing.Dict[str, FfiPropValue]:
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


@dataclasses.dataclass(frozen=True, slots=True)
class Node:
    id: int
    label: str
    properties: Properties

    @classmethod
    def _from_ffi(cls, record: FfiNodeRecord) -> "Node":
        return cls(id=record.id, label=record.label, properties=from_ffi_properties(record.properties))


@dataclasses.dataclass(frozen=True, slots=True)
class Edge:
    id: int
    from_node: int
    to_node: int
    edge_type: str
    properties: Properties

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
class PathStep:
    node_id: int
    via_edge_id: typing.Optional[int]
    edge_type: typing.Optional[str]

    @classmethod
    def _from_ffi(cls, record: FfiPathStep) -> "PathStep":
        return cls(node_id=record.node_id, via_edge_id=record.via_edge_id, edge_type=record.edge_type)


@dataclasses.dataclass(frozen=True, slots=True)
class Path:
    node_ids: typing.List[int]
    edge_ids: typing.List[int]
    steps: typing.List[PathStep]

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
    node_ids: typing.List[int]
    edge_ids: typing.List[int]
