"""Graph, table, bulk-sync and integration methods of :class:`bkndb.Database`."""
from __future__ import annotations

import typing

from . import types
from ._common import NodeSpec, SyncEdgeSpec, _call, _node_ref, _props
from ._native.bkndb_ffi import FfiEdgeInput, FfiLinkedEdgeInput, FfiNodeInput, FfiSyncBatch, FfiTableRows
from ._ops_core import _Operations
from .query import Agg, Where, to_ffi_query


class _GraphTableOps(_Operations):
    """Graph queries, table helpers, bulk sync and integrations (part of
    :class:`bkndb.Database`)."""

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
