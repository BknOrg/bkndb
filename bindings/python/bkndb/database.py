"""Pythonic wrapper around the raw UniFFI-generated ``BknDbEngine``.

This is the one hand-maintained adapter layer between bkndb's native
extension and its public Python API. It exists so the underlying Rust
surface can grow (new methods, new record fields) without breaking every
caller's code — application code should only ever import from :mod:`bkndb`,
never reach into :mod:`bkndb._native` directly.
"""
from __future__ import annotations

import typing

from . import errors, types
from ._native.bkndb_ffi import (
    BknDbEngine,
    FfiBknError,
    FfiEdgeInput,
    FfiNodeInput,
    FfiSyncBatch,
)

T = typing.TypeVar("T")


def _call(fn: typing.Callable[[], T]) -> T:
    """Runs `fn`, translating a raw `FfiBknError` into a public exception."""
    try:
        return fn()
    # UniFFI's generated `FfiBknError` really is an `Exception` subclass at
    # runtime (it's reassigned to one after its variant classes are built) —
    # pyright can't see through that dynamic reassignment and flags this as
    # not deriving from `BaseException`, a known false positive.
    except FfiBknError as exc:  # type: ignore
        raise errors.translate(exc) from exc


class Database:
    """An embedded, single-file (or in-memory) bkndb database.

    Construct via :meth:`open`/:meth:`in_memory` (or the top-level
    ``bkndb.open``/``bkndb.in_memory`` shortcuts), and optionally use as a
    context manager::

        with bkndb.open("my.bkndb") as db:
            doc = db.create_node("Document", {"title": "..."})
    """

    def __init__(self, engine: BknDbEngine) -> None:
        self._engine: typing.Optional[BknDbEngine] = engine

    @classmethod
    def open(cls, path: str) -> "Database":
        """Opens or creates an on-disk single-file database at `path`."""
        return cls(_call(lambda: BknDbEngine.open(path)))

    @classmethod
    def in_memory(cls) -> "Database":
        """Creates an ephemeral in-memory database instance."""
        return cls(_call(BknDbEngine.in_memory))

    def close(self) -> None:
        """Releases the underlying native handle. Safe to call more than once."""
        self._engine = None

    def __enter__(self) -> "Database":
        return self

    def __exit__(self, *_exc_info: object) -> None:
        self.close()

    @property
    def _handle(self) -> BknDbEngine:
        if self._engine is None:
            raise errors.BknDbError("this Database has already been closed")
        return self._engine

    # ----- nodes ------------------------------------------------------

    def create_node(self, label: str, properties: typing.Optional[types.Properties] = None) -> int:
        """Creates a single graph node and returns its id."""
        ffi_props = types.to_ffi_properties(properties or {})
        return _call(lambda: self._handle.create_node(label, ffi_props))

    def create_nodes_bulk(self, nodes: typing.Sequence[typing.Tuple[str, types.Properties]]) -> typing.List[int]:
        """Creates several nodes atomically. `nodes` is a sequence of `(label, properties)`."""
        items = [FfiNodeInput(label=label, properties=types.to_ffi_properties(props)) for label, props in nodes]
        return _call(lambda: self._handle.create_nodes_bulk(items))

    def get_node(self, node_id: int) -> typing.Optional[types.Node]:
        record = _call(lambda: self._handle.get_node(node_id))
        return types.Node._from_ffi(record) if record is not None else None

    def delete_node(self, node_id: int) -> None:
        """Deletes a node and all incident edges atomically."""
        _call(lambda: self._handle.delete_node(node_id))

    # ----- edges ------------------------------------------------------

    def create_edge(
        self,
        from_node: int,
        edge_type: str,
        to_node: int,
        properties: typing.Optional[types.Properties] = None,
    ) -> int:
        """Creates a directed edge between two existing nodes and returns its id."""
        ffi_props = types.to_ffi_properties(properties or {})
        return _call(lambda: self._handle.create_edge(from_node, edge_type, to_node, ffi_props))

    def create_edges_bulk(
        self,
        edges: typing.Sequence[typing.Tuple[int, str, int, types.Properties]],
    ) -> typing.List[int]:
        """Creates several edges atomically. `edges` is a sequence of `(from, edge_type, to, properties)`."""
        items = [
            FfiEdgeInput(_from=frm, edge_type=edge_type, to=to, properties=types.to_ffi_properties(props))
            for frm, edge_type, to, props in edges
        ]
        return _call(lambda: self._handle.create_edges_bulk(items))

    def get_edge(self, edge_id: int) -> typing.Optional[types.Edge]:
        record = _call(lambda: self._handle.get_edge(edge_id))
        return types.Edge._from_ffi(record) if record is not None else None

    # ----- traversal ----------------------------------------------------

    def neighbors_out(self, node_id: int, edge_type: str) -> typing.List[types.Neighbor]:
        """Outgoing neighbors of `node_id` along edges of type `edge_type`."""
        result = _call(lambda: self._handle.neighbors_out(node_id, edge_type))
        return [types.Neighbor._from_ffi(n) for n in result]

    def neighbors_in(self, node_id: int, edge_type: str) -> typing.List[types.Neighbor]:
        """Incoming neighbors of `node_id` along edges of type `edge_type`."""
        result = _call(lambda: self._handle.neighbors_in(node_id, edge_type))
        return [types.Neighbor._from_ffi(n) for n in result]

    def find_shortest_path(
        self,
        start: int,
        target: int,
        direction: types.Direction = types.Direction.OUT,
        edge_types: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.Optional[types.Path]:
        """Unweighted shortest path from `start` to `target`, via BFS."""
        types_list = list(edge_types) if edge_types is not None else None
        result = _call(lambda: self._handle.find_shortest_path(start, target, direction._to_ffi(), types_list))
        return types.Path._from_ffi(result) if result is not None else None

    def top_hubs(
        self,
        k: int,
        direction: types.Direction = types.Direction.OUT,
        edge_type: typing.Optional[str] = None,
    ) -> typing.List[types.Hub]:
        """The top `k` hub nodes by degree centrality, optionally filtered by edge type."""
        result = _call(lambda: self._handle.top_hubs(k, direction._to_ffi(), edge_type))
        return [types.Hub._from_ffi(h) for h in result]

    def cascade_delete(self, root: int, containment_edge: str) -> typing.List[int]:
        """Recursively deletes `root` and all descendants reachable via `containment_edge`."""
        return _call(lambda: self._handle.cascade_delete(root, containment_edge))

    # ----- bulk sync ------------------------------------------------------

    def sync_batch(
        self,
        nodes: typing.Sequence[typing.Tuple[str, types.Properties]] = (),
        edges: typing.Sequence[typing.Tuple[int, str, int, types.Properties]] = (),
    ) -> types.SyncResult:
        """Ingests `nodes` and `edges` together in a single atomic transaction."""
        batch = FfiSyncBatch(
            nodes=[FfiNodeInput(label=label, properties=types.to_ffi_properties(props)) for label, props in nodes],
            edges=[
                FfiEdgeInput(_from=frm, edge_type=edge_type, to=to, properties=types.to_ffi_properties(props))
                for frm, edge_type, to, props in edges
            ],
        )
        result = _call(lambda: self._handle.sync_batch(batch))
        return types.SyncResult(node_ids=list(result.node_ids), edge_ids=list(result.edge_ids))
