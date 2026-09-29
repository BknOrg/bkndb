"""Type aliases and small helpers shared by the Database / Transaction wrappers."""
from __future__ import annotations

import typing

from . import errors, types
from ._native.bkndb_ffi import FfiNodeRef, FfiVectorMetric


T = typing.TypeVar("T")

NodeSpec = typing.Tuple[str, types.PropertiesLike]
EdgeSpec = typing.Tuple[int, str, int, types.PropertiesLike]
NodeRefLike = typing.Union[int, types.NewNode]
SyncEdgeSpec = typing.Tuple[NodeRefLike, str, NodeRefLike, types.PropertiesLike]


def _call(fn: typing.Callable[[], T]) -> T:
    """Runs `fn`, translating anything the native layer raises into a public exception."""
    try:
        return fn()
    except BaseException as exc:
        mapped = errors.wrap(exc)
        if mapped is exc:
            raise
        raise mapped from exc


def _node_ref(r: "NodeRefLike") -> typing.Any:
    if isinstance(r, types.NewNode):
        return FfiNodeRef.NEW(r.index)
    return FfiNodeRef.EXISTING(r)


def _props(p: typing.Optional[typing.Mapping[str, types.PropertyValue]]) -> dict:
    return types.to_ffi_properties(p or {})


Params = typing.Union[typing.Sequence[types.PropertyValue], typing.Mapping[str, types.PropertyValue], None]


def _params(params: Params) -> typing.Tuple[list, typing.Optional[dict]]:
    if params is None:
        return [], None
    if isinstance(params, typing.Mapping):
        return [], {str(k): types.to_ffi_value(v) for k, v in params.items()}
    if isinstance(params, (str, bytes)):
        raise TypeError("params must be a list/tuple or a dict, not a single value")
    return [types.to_ffi_value(v) for v in params], None


def _row_or_none(record) -> typing.Optional[types.Row]:
    return types.Row._from_ffi(record) if record is not None else None


_METRICS = {"cosine": FfiVectorMetric.COSINE, "dot": FfiVectorMetric.DOT, "euclidean": FfiVectorMetric.EUCLIDEAN}


def _metric(name: str) -> FfiVectorMetric:
    try:
        return _METRICS[name]
    except KeyError:
        raise ValueError(f"metric must be one of {sorted(_METRICS)}") from None
