"""Pythonic value types for bkndb's public API.

Two jobs live here:

1. Converting between plain Python values (``str``/``int``/``float``/
   ``bool``/``bytes``/``None``) and bkndb's tagged ``FfiPropValue`` union —
   application code never imports ``FfiPropValue`` directly.
2. Plain, ``__slots__``-based result dataclasses (:class:`Node`,
   :class:`Row`, ...) wrapping the raw ``Ffi*`` records the native layer
   returns, with values already converted back to plain Python.

The implementations live in the private modules ``_values`` (conversion),
``_records`` (graph results) and ``_tables`` (schemas, rows, statistics,
query/search results); this module is their public face.

Everything in :mod:`bkndb._native` is private — only this package's own
modules import from it.
"""
from __future__ import annotations

import typing

from ._records import (
    Direction,
    Edge,
    Hub,
    Neighbor,
    NewNode,
    Node,
    Path,
    PathStep,
    SyncResult,
    TraversalHit,
    TypedNeighbor,
    WeightedPath,
)
from ._tables import (
    AggregateRow,
    Column,
    ColumnType,
    DbStats,
    IntegrityReport,
    QueryResult,
    Row,
    ScoredRow,
    StorageStats,
    TableSchema,
    VectorIndexInfo,
)
from ._values import (
    Properties,
    PropertiesLike,
    PropertyValue,
    datetime_to_micros,
    from_ffi_properties,
    from_ffi_value,
    micros_to_datetime,
    to_ffi_properties,
    to_ffi_value,
)


__all__ = [
    "Direction",
    "Edge",
    "Hub",
    "Neighbor",
    "NewNode",
    "Node",
    "Path",
    "PathStep",
    "SyncResult",
    "TraversalHit",
    "TypedNeighbor",
    "WeightedPath",
    "AggregateRow",
    "Column",
    "ColumnType",
    "DbStats",
    "IntegrityReport",
    "QueryResult",
    "Row",
    "ScoredRow",
    "StorageStats",
    "TableSchema",
    "VectorIndexInfo",
    "Properties",
    "PropertiesLike",
    "PropertyValue",
    "datetime_to_micros",
    "from_ffi_properties",
    "from_ffi_value",
    "micros_to_datetime",
    "to_ffi_properties",
    "to_ffi_value",
    "pack_vector",
]


def pack_vector(values: typing.Iterable[float]) -> bytes:
    """Packs an embedding as little-endian float32 bytes — the compact way to
    store vectors for :meth:`bkndb.Database.search_vector` (4 bytes per
    dimension, versus ~12 for a list of floats). Accepts any iterable of
    numbers, including a NumPy array."""
    import struct

    floats = [float(v) for v in values]
    return struct.pack(f"<{len(floats)}f", *floats)
