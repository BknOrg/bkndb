"""Plain Python values ⇄ bkndb's tagged ``FfiPropValue`` (see :mod:`bkndb.types`)."""
from __future__ import annotations

import datetime
import typing
import uuid

from ._native.bkndb_ffi import FfiPropValue


#: A property / column value: ``None``, ``str``, ``int``, ``float``,
#: ``bool``, ``bytes``, ``datetime.datetime`` (stored as UTC microseconds),
#: ``uuid.UUID``, a ``list`` of values or a ``dict`` of ``str`` -> value
#: (nesting freely, like JSON). ``bytearray`` and ``tuple`` are also
#: accepted on input.
PropertyValue = typing.Union[
    None,
    str,
    int,
    float,
    bool,
    bytes,
    datetime.datetime,
    uuid.UUID,
    typing.List[typing.Any],
    typing.Dict[str, typing.Any],
]

_EPOCH = datetime.datetime(1970, 1, 1, tzinfo=datetime.timezone.utc)
_MICRO = datetime.timedelta(microseconds=1)
_U64 = (1 << 64) - 1


def datetime_to_micros(value: datetime.datetime) -> int:
    """Microseconds since the epoch; a naive datetime is taken as UTC."""
    if value.tzinfo is None:
        value = value.replace(tzinfo=datetime.timezone.utc)
    return (value - _EPOCH) // _MICRO


def micros_to_datetime(micros: int) -> datetime.datetime:
    """An aware UTC datetime."""
    return _EPOCH + datetime.timedelta(microseconds=micros)
#: Properties as returned by bkndb.
Properties = typing.Dict[str, PropertyValue]
#: Properties as accepted by bkndb: any mapping (so ``dict[str, str]`` fits).
PropertiesLike = typing.Mapping[str, PropertyValue]


# UniFFI rebinds `FfiPropValue` to its tagged-union class at import time,
# which static checkers can't follow — hence `Any` at this boundary.
def to_ffi_value(value: typing.Union[PropertyValue, bytearray]) -> typing.Any:
    if value is None:
        return FfiPropValue.NULL()
    # `bool` must be checked before `int` — `bool` is an `int` subclass in
    # Python, so `isinstance(True, int)` is true and would otherwise shadow
    # the BOOL branch entirely.
    if isinstance(value, bool):
        return FfiPropValue.BOOL(value)
    if isinstance(value, int):
        return FfiPropValue.INT(value)
    if isinstance(value, float):
        return FfiPropValue.FLOAT(value)
    if isinstance(value, str):
        return FfiPropValue.STR(value)
    if isinstance(value, (bytes, bytearray)):
        return FfiPropValue.BYTES(bytes(value))
    if isinstance(value, datetime.datetime):
        return FfiPropValue.TIMESTAMP(datetime_to_micros(value))
    if isinstance(value, uuid.UUID):
        return FfiPropValue.UUID(hi=value.int >> 64, lo=value.int & _U64)
    if isinstance(value, (list, tuple)):
        return FfiPropValue.LIST([to_ffi_value(v) for v in value])
    if isinstance(value, typing.Mapping):
        items = {}
        for k, v in value.items():
            if not isinstance(k, str):
                raise TypeError(f"map keys must be str, got {type(k).__name__}")
            items[k] = to_ffi_value(v)
        return FfiPropValue.MAP(items)
    raise TypeError(f"unsupported property value type: {type(value).__name__}")


def from_ffi_value(value: typing.Any) -> PropertyValue:
    if value.is_null():
        return None
    if value.is_timestamp():
        return micros_to_datetime(value[0])
    if value.is_uuid():
        return uuid.UUID(int=(value.hi << 64) | value.lo)
    if value.is_list():
        return [from_ffi_value(v) for v in value[0]]
    if value.is_map():
        return {k: from_ffi_value(v) for k, v in value[0].items()}
    return value[0]


def to_ffi_properties(properties: typing.Mapping[str, PropertyValue]) -> typing.Dict[str, FfiPropValue]:
    return {key: to_ffi_value(val) for key, val in properties.items()}


def from_ffi_properties(properties: typing.Mapping[str, FfiPropValue]) -> Properties:
    return {key: from_ffi_value(val) for key, val in properties.items()}
