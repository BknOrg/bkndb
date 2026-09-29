"""Operational methods of :class:`bkndb.Database`: transactions, backup,
statistics, integrity checks and CSV/JSONL import/export."""
from __future__ import annotations

import csv
import json
import os
import typing

from . import _io, errors, types
from ._common import _call
from ._ops_core import Transaction
from ._ops_extra import _GraphTableOps
from .query import Where, _where_expr, col


class _IoOps(_GraphTableOps):
    """Transactions, maintenance, statistics and import/export (part of
    :class:`bkndb.Database`)."""

    def transaction(self) -> Transaction:
        """Opens an explicit write transaction (see :class:`Transaction`)."""
        return Transaction(_call(self._h.begin_transaction))

    def compact(self) -> None:
        """Rewrites the file with only live data: reclaims space from
        overwritten/deleted data and upgrades older on-disk formats. Blocks
        writers while it runs."""
        _call(self._h.compact)

    # ----- operations --------------------------------------------------------------

    def backup(self, path: "_io.PathLike") -> None:
        """Writes a consistent, compacted copy of everything committed so far
        to a new ``.bkndb`` file at `path` (which must not exist yet). Reads
        and writes carry on meanwhile; an open transaction's uncommitted
        writes are not included. On-disk databases only."""
        dest = os.fspath(path)
        _call(lambda: self._h.backup(dest))

    def stats(self) -> types.DbStats:
        """Node, edge and per-table row counts (from one snapshot), plus file
        figures for on-disk databases. Counting scans the data, so this is
        for monitoring and tooling rather than hot paths."""
        return types.DbStats._from_ffi(_call(self._h.stats))

    def verify_integrity(self) -> types.IntegrityReport:
        """Re-reads and checksums every stored byte. Raises
        :class:`bkndb.CorruptionError` naming the first damaged structure."""
        return types.IntegrityReport._from_ffi(_call(self._h.verify_integrity))

    def _schema(self, table: str) -> types.TableSchema:
        schema = self.table_schema(table)
        if schema is None:
            raise errors.TableNotFoundError(table)
        return schema

    def iter_rows(
        self,
        table: str,
        where: Where = None,
        *,
        batch_size: int = 1000,
        columns: typing.Optional[typing.Sequence[str]] = None,
    ) -> typing.Iterator[types.Row]:
        """Lazily yields the rows matching `where` in primary-key order,
        fetching ``batch_size`` rows at a time, so memory stays bounded however
        large the table is.

        Each batch is read from the latest committed state, so rows written
        while iterating may or may not show up; no row is yielded twice.
        """
        if batch_size < 1:
            raise ValueError("batch_size must be >= 1")
        pk = self._schema(table).primary_key
        base = _where_expr(where)
        last: typing.Optional[types.PropertyValue] = None
        while True:
            after = None if last is None else col(pk) > last
            if base is None:
                cond = after
            elif after is None:
                cond = base
            else:
                cond = base & after
            batch = self.select(table, cond, order_by=pk, limit=batch_size, columns=columns)
            yield from batch
            if len(batch) < batch_size:
                return
            last = batch[-1].pk

    def export_jsonl(self, table: str, path: "_io.PathLike", where: Where = None) -> int:
        """Writes the rows matching `where` to `path` as JSON Lines (one
        object per row, primary key included); returns how many."""
        schema = self._schema(table)
        n = 0
        with open(path, "w", encoding="utf-8", newline="\n") as f:
            for row in self.iter_rows(table, where):
                f.write(json.dumps(_io.row_to_json(schema, row), ensure_ascii=False))
                f.write("\n")
                n += 1
        return n

    def export_csv(self, table: str, path: "_io.PathLike", where: Where = None) -> int:
        """Writes the rows matching `where` to `path` as CSV with a header
        row (primary key first, then columns in schema order); returns how
        many rows."""
        schema = self._schema(table)
        header = _io.csv_header(schema)
        n = 0
        with open(path, "w", encoding="utf-8", newline="") as f:
            writer = csv.writer(f)
            writer.writerow(header)
            for row in self.iter_rows(table, where):
                writer.writerow(_io.row_to_csv(header, schema, row))
                n += 1
        return n

    def _import(
        self,
        table: str,
        rows: typing.Iterable[typing.Dict[str, types.PropertyValue]],
        mode: str,
        batch_size: int,
    ) -> int:
        if mode not in ("upsert", "insert"):
            raise ValueError("mode must be 'upsert' or 'insert'")
        if batch_size < 1:
            raise ValueError("batch_size must be >= 1")
        n = 0
        with self.transaction() as tx:
            write = tx.upsert_many if mode == "upsert" else tx.insert_many
            batch: typing.List[typing.Dict[str, types.PropertyValue]] = []
            for row in rows:
                batch.append(row)
                if len(batch) >= batch_size:
                    write(table, batch)
                    n += len(batch)
                    batch = []
            if batch:
                write(table, batch)
                n += len(batch)
        return n

    def import_jsonl(self, table: str, path: "_io.PathLike", *, mode: str = "upsert", batch_size: int = 1000) -> int:
        """Loads rows from a JSON Lines file (as written by
        :meth:`export_jsonl`) into an existing table, all in one transaction:
        nothing is written if any row fails. ``mode="upsert"`` (default)
        replaces rows whose primary key exists; ``"insert"`` raises
        :class:`bkndb.DuplicateKeyError` instead. Returns how many rows."""
        self._schema(table)

        def rows() -> typing.Iterator[typing.Dict[str, types.PropertyValue]]:
            with open(path, encoding="utf-8") as f:
                for line_no, line in enumerate(f, 1):
                    if not line.strip():
                        continue
                    try:
                        obj = json.loads(line)
                    except json.JSONDecodeError as exc:
                        raise errors.InvalidArgumentError(f"line {line_no}: invalid JSON: {exc}") from None
                    if not isinstance(obj, dict):
                        raise errors.InvalidArgumentError(f"line {line_no}: expected a JSON object")
                    yield {k: _io.from_json_value(v) for k, v in obj.items()}

        return self._import(table, rows(), mode, batch_size)

    def import_csv(self, table: str, path: "_io.PathLike", *, mode: str = "upsert", batch_size: int = 1000) -> int:
        """Loads rows from a CSV file with a header row of column names into
        an existing table, converting each cell to its column's type, all in
        one transaction. Empty cells are left out (so DEFAULT/NULL applies,
        and an auto-increment key is assigned). ``mode`` as for
        :meth:`import_jsonl`. Returns how many rows."""
        schema = self._schema(table)

        def rows() -> typing.Iterator[typing.Dict[str, types.PropertyValue]]:
            with open(path, encoding="utf-8", newline="") as f:
                reader = csv.reader(f)
                header = next(reader, None)
                if header is None:
                    return
                for record in reader:
                    if not record:
                        continue
                    yield _io.csv_record_to_row(schema, header, record, reader.line_num)

        return self._import(table, rows(), mode, batch_size)
