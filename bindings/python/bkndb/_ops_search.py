"""Search methods of :class:`bkndb.Database`."""
from __future__ import annotations

import typing

from . import types
from ._common import _call, _metric
from ._ops_core import _Operations
from .query import Where, to_ffi_filter


class _SearchOps(_Operations):
    """Full-text and vector search (part of :class:`bkndb.Database`)."""

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
        exact: bool = False,
        ef_search: typing.Optional[int] = None,
    ) -> typing.List[types.ScoredRow]:
        """The `limit` rows whose embedding in `column` is nearest to
        `vector`. Embeddings are a ``list`` of numbers or
        :func:`bkndb.pack_vector` bytes. ``metric``: ``"cosine"`` / ``"dot"``
        (score = similarity, highest first) or ``"euclidean"`` (score =
        distance, lowest first).

        With a vector index for this column and metric (see
        :meth:`create_vector_index`) the search is approximate and fast;
        ``ef_search`` (default ``max(4 * limit, 64)``) trades speed for
        recall. Otherwise, or with ``exact=True``, it scans the (filtered)
        rows and is exact."""
        m = _metric(metric)
        values = [float(v) for v in vector]
        flt = to_ffi_filter(where)
        hits = _call(lambda: self._h.search_vector(table, column, values, limit, m, flt, exact, ef_search))
        return [types.ScoredRow._from_ffi(h) for h in hits]

    def create_vector_index(
        self,
        table: str,
        column: str,
        *,
        metric: str = "cosine",
        m: int = 16,
        ef_construction: int = 200,
    ) -> bool:
        """Builds an approximate nearest-neighbour (HNSW) index over an
        embedding column (``list`` or ``bytes``) for one `metric`, so
        :meth:`search_vector` no longer scans every row. Existing rows are
        indexed now, later writes automatically (in the same transaction).
        Every indexed vector must then have the same length. ``m`` (links
        per node) and ``ef_construction`` trade build speed and size for
        recall. ``False`` if the column already has an index (drop it first
        to change the parameters)."""
        mm = _metric(metric)
        return _call(lambda: self._h.create_vector_index(table, column, mm, m, ef_construction))

    def drop_vector_index(self, table: str, column: str) -> bool:
        return _call(lambda: self._h.drop_vector_index(table, column))

    def vector_indexes(self, table: str) -> typing.List[types.VectorIndexInfo]:
        """The vector indexes of `table`."""
        return [types.VectorIndexInfo._from_ffi(i) for i in _call(lambda: self._h.list_vector_indexes(table))]
