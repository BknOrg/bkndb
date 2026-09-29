"""bkndb — embedded hybrid graph + relational database engine for Python.

Public API. Only import from here — :mod:`bkndb._native` is the raw
UniFFI-generated layer and is an implementation detail that may change
shape between releases without notice.

    import bkndb

    with bkndb.open("my.bkndb") as db:
        doc = db.create_node("Document", {"title": "..."})
        concept = db.create_node("Concept", {"name": "Attention"})
        db.create_edge(doc, "DISCUSSES", concept)
"""
from __future__ import annotations

from .database import Database
from .errors import (
    BackendError,
    BknDbError,
    ConstraintViolationError,
    DatabaseLockedError,
    DuplicateKeyError,
    EncodingError,
    NotFoundError,
    ReservedTableNameError,
    SchemaMismatchError,
    TableNotFoundError,
)
from .types import (
    Direction,
    Edge,
    Hub,
    Neighbor,
    Node,
    Path,
    PathStep,
    Properties,
    PropertyValue,
    SyncResult,
)

open = Database.open
in_memory = Database.in_memory

__version__ = "0.1.0"

__all__ = [
    "Database",
    "open",
    "in_memory",
    "BknDbError",
    "BackendError",
    "TableNotFoundError",
    "NotFoundError",
    "EncodingError",
    "ReservedTableNameError",
    "ConstraintViolationError",
    "DatabaseLockedError",
    "DuplicateKeyError",
    "SchemaMismatchError",
    "Direction",
    "Node",
    "Edge",
    "Neighbor",
    "Path",
    "PathStep",
    "Hub",
    "SyncResult",
    "Properties",
    "PropertyValue",
]
