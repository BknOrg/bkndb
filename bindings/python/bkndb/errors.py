"""Pythonic exception hierarchy mapping bkndb's Rust-side error variants.

Every error bkndb raises derives from :class:`BknDbError`. Application code
never sees the raw UniFFI ``FfiBknError`` type — every call in
:mod:`bkndb.database` routes its errors through :func:`wrap`.
"""
from __future__ import annotations

from ._native.bkndb_ffi import FfiBknError, InternalError


class BknDbError(Exception):
    """Base class for every error bkndb can raise."""


class BackendError(BknDbError):
    """The storage backend failed (I/O, corruption, ...)."""


class CorruptionError(BknDbError):
    """Stored data failed an integrity check (checksum mismatch, truncated
    or malformed file structure). Restore from a backup; see
    :meth:`bkndb.Database.verify_integrity` and :meth:`bkndb.Database.backup`."""


class QueryError(BknDbError, ValueError):
    """A SQL statement or graph pattern failed to parse or is invalid."""


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


class DatabaseClosedError(BknDbError):
    """The database has been closed."""


class DuplicateKeyError(BknDbError):
    """An insert collided with an existing primary key."""

    def __init__(self, table: str, key: str) -> None:
        super().__init__(f"duplicate primary key {key} in table '{table}'")
        self.table = table
        self.key = key


class SchemaMismatchError(BknDbError):
    """A value doesn't match its column's kind, a column is unknown, or a
    schema change isn't allowed."""

    def __init__(self, table: str, message: str) -> None:
        super().__init__(f"schema mismatch in table '{table}': {message}")
        self.table = table


class ConstraintViolationError(BknDbError):
    """A NOT NULL or UNIQUE constraint would be violated."""

    def __init__(self, table: str, message: str) -> None:
        super().__init__(f"constraint violation in table '{table}': {message}")
        self.table = table


class TransactionError(BknDbError):
    """Base class for transaction misuse."""


class TransactionInProgressError(TransactionError):
    """A transaction is open: write through it, or commit/roll it back first."""


class TransactionClosedError(TransactionError):
    """The transaction has already been committed or rolled back."""


class TransactionAbortedError(TransactionError):
    """An earlier operation in the transaction failed; it must be rolled back."""


class InvalidArgumentError(BknDbError, ValueError):
    """An argument is malformed (e.g. an integer outside the 64-bit range)."""


def translate(exc: FfiBknError) -> BknDbError:  # type: ignore[valid-type]
    """Converts a raw ``FfiBknError`` into the matching public exception."""
    E = FfiBknError
    if isinstance(exc, E.Backend):
        return BackendError(exc.message)
    if isinstance(exc, E.TableNotFound):
        return TableNotFoundError(exc.table)
    if isinstance(exc, E.NotFound):
        return NotFoundError("not found")
    if isinstance(exc, E.Encoding):
        return EncodingError(exc.message)
    if isinstance(exc, E.ReservedTableName):
        return ReservedTableNameError(exc.table)
    if isinstance(exc, E.DatabaseLocked):
        return DatabaseLockedError(exc.path)
    if isinstance(exc, E.DuplicateKey):
        return DuplicateKeyError(exc.table, exc.key)
    if isinstance(exc, E.SchemaMismatch):
        return SchemaMismatchError(exc.table, exc.message)
    if isinstance(exc, E.ConstraintViolation):
        return ConstraintViolationError(exc.table, exc.message)
    if isinstance(exc, E.Corruption):
        return CorruptionError(exc.message)
    if isinstance(exc, E.InvalidQuery):
        return QueryError(exc.message)
    if isinstance(exc, E.DatabaseClosed):
        return DatabaseClosedError("the database has been closed")
    if isinstance(exc, E.TransactionInProgress):
        return TransactionInProgressError(
            "a transaction is open on this database; use it, or commit/roll it back first"
        )
    if isinstance(exc, E.TransactionClosed):
        return TransactionClosedError("the transaction has already been committed or rolled back")
    if isinstance(exc, E.TransactionAborted):
        return TransactionAbortedError(f"an earlier operation failed, roll the transaction back: {exc.message}")
    if isinstance(exc, E.InvalidArgument):
        return InvalidArgumentError(exc.message)
    return BknDbError(str(exc))


def wrap(exc: BaseException) -> BaseException:
    """Maps any exception escaping the native layer to a public one."""
    if isinstance(exc, FfiBknError):  # type: ignore[arg-type]
        return translate(exc)
    if isinstance(exc, InternalError):
        return BknDbError(f"internal error in the native library: {exc}")
    if isinstance(exc, (ValueError, OverflowError)):
        # Raised by UniFFI while converting arguments, e.g. an int that
        # doesn't fit in 64 bits.
        return InvalidArgumentError(str(exc))
    return exc
