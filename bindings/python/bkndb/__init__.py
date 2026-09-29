"""bkndb — embedded hybrid graph + relational database engine for Python.

Public API. Only import from here — :mod:`bkndb._native` is the raw
UniFFI-generated layer and is an implementation detail that may change
shape between releases without notice.

    import bkndb
    from bkndb import col, Column, TableSchema

    with bkndb.open("my.bkndb") as db:
        doc = db.create_node("Document", {"title": "..."})
        db.create_table(TableSchema("chunks", [Column("id", int), Column("doc", int), Column("text", str)],
                                    primary_key="id", auto_increment=True, indexes=["doc"]))
        with db.transaction() as tx:
            tx.insert("chunks", {"doc": doc, "text": "..."})
        rows = db.select("chunks", col("doc") == doc)
"""
from __future__ import annotations

from importlib import metadata as _metadata

from .database import Database, Table, Transaction
from .errors import (
    BackendError,
    BknDbError,
    ConstraintViolationError,
    DatabaseClosedError,
    DatabaseLockedError,
    DuplicateKeyError,
    EncodingError,
    InvalidArgumentError,
    NotFoundError,
    ReservedTableNameError,
    SchemaMismatchError,
    TableNotFoundError,
    TransactionAbortedError,
    TransactionClosedError,
    TransactionError,
    TransactionInProgressError,
)
from .query import Agg, Col, Expr, col
from .types import (
    AggregateRow,
    Column,
    Direction,
    Edge,
    Hub,
    Neighbor,
    Node,
    Path,
    PathStep,
    Properties,
    PropertiesLike,
    PropertyValue,
    Row,
    SyncResult,
    TableSchema,
    TraversalHit,
    TypedNeighbor,
)

# `bkndb.open(...)` works, but `open` is left out of `__all__` so that
# `from bkndb import *` can't shadow the builtin.
open = Database.open
in_memory = Database.in_memory

try:
    __version__ = _metadata.version("bkndb")
except _metadata.PackageNotFoundError:  # running from a source tree without an install
    __version__ = "0.0.0+unknown"

__all__ = [
    "Database",
    "Transaction",
    "Table",
    "in_memory",
    # queries
    "col",
    "Col",
    "Expr",
    "Agg",
    # schemas & results
    "Column",
    "TableSchema",
    "Row",
    "AggregateRow",
    "Direction",
    "Node",
    "Edge",
    "Neighbor",
    "TypedNeighbor",
    "TraversalHit",
    "Path",
    "PathStep",
    "Hub",
    "SyncResult",
    "Properties",
    "PropertiesLike",
    "PropertyValue",
    # errors
    "BknDbError",
    "BackendError",
    "TableNotFoundError",
    "NotFoundError",
    "EncodingError",
    "ReservedTableNameError",
    "DatabaseLockedError",
    "DatabaseClosedError",
    "DuplicateKeyError",
    "SchemaMismatchError",
    "ConstraintViolationError",
    "TransactionError",
    "TransactionInProgressError",
    "TransactionClosedError",
    "TransactionAbortedError",
    "InvalidArgumentError",
]
