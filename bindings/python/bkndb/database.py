"""Pythonic wrapper around the raw UniFFI-generated ``BknDbEngine``.

This is the one hand-maintained adapter layer between bkndb's native
extension and its public Python API. It exists so the underlying Rust
surface can grow without breaking callers — application code should only
ever import from :mod:`bkndb`, never reach into :mod:`bkndb._native`.
"""
from __future__ import annotations

import os
import typing

from . import _io, errors
from ._common import _call
from ._native.bkndb_ffi import BknDbEngine, FfiLsmOptions
from ._ops_core import Table, Transaction
from ._ops_io import _IoOps
from ._ops_search import _SearchOps

__all__ = ["Database", "Table", "Transaction"]


class Database(_SearchOps, _IoOps):
    """An embedded, single-file (or in-memory) bkndb database.

    Construct via :meth:`open`/:meth:`in_memory` (or the top-level
    ``bkndb.open``/``bkndb.in_memory`` shortcuts), ideally as a context
    manager so the file lock is released deterministically::

        with bkndb.open("my.bkndb") as db:
            doc = db.create_node("Document", {"title": "..."})
    """

    def __init__(self, engine: BknDbEngine, location: str = ":memory:") -> None:
        self._engine: typing.Optional[BknDbEngine] = engine
        self._location = location

    @classmethod
    def open(
        cls,
        path: "_io.PathLike",
        *,
        memtable_flush_bytes: typing.Optional[int] = None,
        compaction_trigger_files: typing.Optional[int] = None,
        block_size: typing.Optional[int] = None,
        compression: typing.Optional[bool] = None,
    ) -> "Database":
        """Opens or creates an on-disk single-file database at `path`.

        Raises :class:`bkndb.DatabaseLockedError` if another handle or process
        has it open, and :class:`bkndb.CorruptionError` if its structure is
        damaged. The keyword options tune the storage engine:
        ``memtable_flush_bytes`` (write buffer size, default 16 MiB),
        ``compaction_trigger_files`` (default 16), ``block_size`` (bytes per
        on-disk block, default 4096) and ``compression`` (lz4, default on).
        """
        path = os.fspath(path)
        tuned = (memtable_flush_bytes, compaction_trigger_files, block_size, compression)
        if all(o is None for o in tuned):
            return cls(_call(lambda: BknDbEngine.open(path)), path)
        options = FfiLsmOptions(
            memtable_flush_bytes=memtable_flush_bytes if memtable_flush_bytes is not None else 16 * 1024 * 1024,
            compaction_trigger_files=compaction_trigger_files if compaction_trigger_files is not None else 16,
            block_size_bytes=block_size,
            compression=compression,
        )
        return cls(_call(lambda: BknDbEngine.open_with_options(path, options)), path)

    @classmethod
    def in_memory(cls) -> "Database":
        """Creates an ephemeral in-memory database."""
        return cls(_call(BknDbEngine.in_memory))

    @property
    def _h(self) -> BknDbEngine:
        if self._engine is None:
            raise errors.DatabaseClosedError("this Database has been closed")
        return self._engine

    @property
    def closed(self) -> bool:
        return self._engine is None

    def close(self) -> None:
        """Closes the database and releases its file lock. Safe to call more
        than once. Raises :class:`bkndb.TransactionInProgressError` while a
        transaction is open."""
        if self._engine is None:
            return
        _call(self._engine.close)
        self._engine = None

    def __enter__(self) -> "Database":
        return self

    def __exit__(self, *_exc_info: object) -> None:
        self.close()

    def __repr__(self) -> str:
        return f"<bkndb.Database {self._location!r} {'closed' if self.closed else 'open'}>"
