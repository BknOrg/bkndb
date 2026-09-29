"""Graph result records (see :mod:`bkndb.types`)."""
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
    FfiTraversalHit,
    FfiTypedNeighbor,
)
from ._values import Properties, PropertyValue, from_ffi_properties


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
