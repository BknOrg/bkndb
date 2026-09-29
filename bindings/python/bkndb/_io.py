"""Row (de)serialization for :meth:`bkndb.Database.export_csv` and friends.

Formats:

* **JSONL** — one JSON object per line, ``{column: value}`` including the
  primary key. Lists and dicts are written as JSON arrays/objects; the
  types JSON lacks are tagged one-key objects: ``{"$bytes": "<base64>"}``,
  ``{"$datetime": "<ISO 8601, UTC>"}``, ``{"$uuid": "<hex>"}``. Types
  round-trip exactly (except ``float`` NaN/infinity, which Python's
  ``json`` writes as ``NaN``/``Infinity``).
* **CSV** — a header row of column names (schema order), then one row per
  record. An empty cell means "not given": on import the column is left
  out, so its DEFAULT (or NULL) applies. Booleans are ``true``/``false``,
  bytes are base64, datetimes ISO 8601, UUIDs hex, lists/dicts JSON text.
  On import each cell is converted to its column's type.
"""
from __future__ import annotations

import base64
import datetime
import json
import os
import typing
import uuid

from . import errors, types

PathLike = typing.Union[str, "os.PathLike[str]"]

_BYTES_KEY = "$bytes"
_DATETIME_KEY = "$datetime"
_UUID_KEY = "$uuid"


def _iso(dt: datetime.datetime) -> str:
    micros = types.datetime_to_micros(dt)
    return types.micros_to_datetime(micros).isoformat().replace("+00:00", "Z")


def _parse_iso(text: str) -> datetime.datetime:
    # `fromisoformat` only accepts a trailing "Z" from Python 3.11 on.
    if text.endswith(("Z", "z")):
        text = text[:-1] + "+00:00"
    return datetime.datetime.fromisoformat(text)


def to_json_value(v: types.PropertyValue) -> typing.Any:
    if isinstance(v, (bytes, bytearray)):
        return {_BYTES_KEY: base64.b64encode(bytes(v)).decode("ascii")}
    if isinstance(v, datetime.datetime):
        return {_DATETIME_KEY: _iso(v)}
    if isinstance(v, uuid.UUID):
        return {_UUID_KEY: str(v)}
    if isinstance(v, (list, tuple)):
        return [to_json_value(x) for x in v]
    if isinstance(v, dict):
        return {k: to_json_value(x) for k, x in v.items()}
    return v


def from_json_value(v: typing.Any) -> types.PropertyValue:
    if isinstance(v, dict):
        if len(v) == 1:
            ((key, inner),) = v.items()
            try:
                if key == _BYTES_KEY:
                    return base64.b64decode(inner, validate=True)
                if key == _DATETIME_KEY:
                    return _parse_iso(inner)
                if key == _UUID_KEY:
                    return uuid.UUID(inner)
            except (ValueError, TypeError) as exc:
                raise errors.InvalidArgumentError(f"bad {key} value {inner!r}: {exc}") from None
        return {k: from_json_value(x) for k, x in v.items()}
    if isinstance(v, list):
        return [from_json_value(x) for x in v]
    return v


def row_to_json(schema: types.TableSchema, row: types.Row) -> typing.Dict[str, typing.Any]:
    out: typing.Dict[str, typing.Any] = {schema.primary_key: to_json_value(row.pk)}
    for k, v in row.values.items():
        out[k] = to_json_value(v)
    return out


def _kind(column: types.Column) -> str:
    t = column.type
    return t if isinstance(t, str) else t.__name__


def to_csv_cell(v: types.PropertyValue) -> str:
    if v is None:
        return ""
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (bytes, bytearray)):
        return base64.b64encode(bytes(v)).decode("ascii")
    if isinstance(v, float):
        return repr(v)
    if isinstance(v, datetime.datetime):
        return _iso(v)
    if isinstance(v, (list, tuple, dict)):
        return json.dumps(to_json_value(v), ensure_ascii=False)
    return str(v)


_TRUE = {"true", "t", "1", "yes", "y"}
_FALSE = {"false", "f", "0", "no", "n"}


def from_csv_cell(column: types.Column, cell: str) -> types.PropertyValue:
    kind = _kind(column)
    try:
        if kind == "str":
            return cell
        if kind == "int":
            return int(cell)
        if kind == "float":
            return float(cell)
        if kind == "bool":
            low = cell.strip().lower()
            if low in _TRUE:
                return True
            if low in _FALSE:
                return False
            raise ValueError(f"not a boolean: {cell!r}")
        if kind == "bytes":
            return base64.b64decode(cell, validate=True)
        if kind in ("timestamp", "datetime"):
            return _parse_iso(cell)
        if kind in ("uuid", "UUID"):
            return uuid.UUID(cell)
        if kind in ("list", "map", "dict"):
            value = from_json_value(json.loads(cell))
            if not isinstance(value, list if kind == "list" else dict):
                raise ValueError(f"expected a JSON {'array' if kind == 'list' else 'object'}")
            return value
    except ValueError as exc:
        raise errors.InvalidArgumentError(f"column '{column.name}': cannot read {cell!r} as {kind}: {exc}") from None
    raise errors.InvalidArgumentError(f"column '{column.name}' has unsupported type {kind!r}")


def csv_header(schema: types.TableSchema) -> typing.List[str]:
    names = [c.name for c in schema.columns]
    if schema.primary_key in names:
        names.remove(schema.primary_key)
    return [schema.primary_key, *names]


def row_to_csv(header: typing.Sequence[str], schema: types.TableSchema, row: types.Row) -> typing.List[str]:
    return [to_csv_cell(row.pk if name == schema.primary_key else row.values.get(name)) for name in header]


def csv_record_to_row(
    schema: types.TableSchema, header: typing.Sequence[str], record: typing.Sequence[str], line: int
) -> typing.Dict[str, types.PropertyValue]:
    if len(record) != len(header):
        raise errors.InvalidArgumentError(f"CSV line {line}: expected {len(header)} fields, got {len(record)}")
    row: typing.Dict[str, types.PropertyValue] = {}
    for name, cell in zip(header, record):
        if cell == "":
            continue
        column = schema.column(name)
        if column is None:
            raise errors.SchemaMismatchError(schema.name, f"CSV column '{name}' is not in the table")
        row[name] = from_csv_cell(column, cell)
    return row
