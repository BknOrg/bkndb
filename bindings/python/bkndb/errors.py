"""Pythonic exception hierarchy mapping bkndb's Rust-side error variants.

Wraps the raw ``FfiBknError`` UniFFI exception (see
:mod:`bkndb._native.bkndb_ffi`) so callers catch ordinary Python
exceptions instead of reaching into a Rust-flavored tagged-variant type.
Application code should never import ``FfiBknError`` directly — every
:mod:`bkndb.database` call already routes its errors through
:func:`translate`.
"""
from __future__ import annotations

from ._native.bkndb_ffi import FfiBknError


class BknDbError(Exception):
    """Base class for every error bkndb can raise."""


class BackendError(BknDbError):
    """The storage backend failed (I/O, corruption, ...)."""


class TableNotFoundError(BknDbError):
    def __init__(self, table: str) -> None:
        super().__init__(f"table not found: {table}")
        self.table = table


class NotFoundError(BknDbError):
    """The requested node, edge, or row does not exist."""


class EncodingError(BknDbError):
    """A value failed to encode or decode."""


class ReservedTableNameError(BknDbError):
    """A table name collides with one of bkndb's internal reserved names."""

    def __init__(self, table: str) -> None:
        super().__init__(f"table name '{table}' is reserved for bkndb's internal use")
        self.table = table


class DatabaseLockedError(BknDbError):
    """The database file is already open by another handle or process."""

    def __init__(self, path: str) -> None:
        super().__init__(f"database '{path}' is already open by another handle or process")
        self.path = path


class DuplicateKeyError(BknDbError):
    """An insert collided with an existing primary key."""

    def __init__(self, table: str, key: str) -> None:
        super().__init__(f"duplicate primary key {key} in table '{table}'")
        self.table = table
        self.key = key


class SchemaMismatchError(BknDbError):
    """A value doesn't match its column's declared kind, or names an undeclared column."""

    def __init__(self, table: str, message: str) -> None:
        super().__init__(f"schema mismatch in table '{table}': {message}")
        self.table = table


class ConstraintViolationError(BknDbError):
    """A NOT NULL or UNIQUE constraint would be violated."""

    def __init__(self, table: str, message: str) -> None:
        super().__init__(f"constraint violation in table '{table}': {message}")
        self.table = table


def translate(exc: FfiBknError) -> BknDbError:
    """Converts a raw ``FfiBknError`` into the matching public exception."""
    if isinstance(exc, FfiBknError.Backend):
        return BackendError(exc.message)
    if isinstance(exc, FfiBknError.TableNotFound):
        return TableNotFoundError(exc.table)
    if isinstance(exc, FfiBknError.NotFound):
        return NotFoundError("key not found")
    if isinstance(exc, FfiBknError.Encoding):
        return EncodingError(exc.message)
    if isinstance(exc, FfiBknError.ReservedTableName):
        return ReservedTableNameError(exc.table)
    if isinstance(exc, FfiBknError.DatabaseLocked):
        return DatabaseLockedError(exc.path)
    if isinstance(exc, FfiBknError.DuplicateKey):
        return DuplicateKeyError(exc.table, exc.key)
    if isinstance(exc, FfiBknError.SchemaMismatch):
        return SchemaMismatchError(exc.table, exc.message)
    if isinstance(exc, FfiBknError.ConstraintViolation):
        return ConstraintViolationError(exc.table, exc.message)
    return BknDbError(str(exc))
