"""Filter expressions and aggregates for relational queries.

Build filters with :func:`col` and Python operators::

    from bkndb import col

    adults_in_java = (col("age") >= 18) & col("city").is_in(["Jakarta", "Bandung"])
    no_email = col("email").is_null()
    not_admin = ~(col("role") == "admin")

Comparisons against a missing or null value are false; use
:meth:`Col.is_null` to test for null (``col("x") == None`` works too).
``&``/``|``/``~`` bind tighter than comparisons in Python, so wrap each
comparison in parentheses.
"""
from __future__ import annotations

import typing

from ._native.bkndb_ffi import FfiAgg, FfiAggFunc, FfiExprNode, FfiExprOp, FfiOrder, FfiQuery
from .types import PropertyValue, to_ffi_value

Where = typing.Union["Expr", typing.Mapping[str, PropertyValue], None]
OrderBy = typing.Union[str, typing.Sequence[str], None]


class Expr:
    """A boolean filter over one row. Combine with ``&``, ``|`` and ``~``."""

    __slots__ = ()

    def __and__(self, other: "Expr") -> "Expr":
        return _Logic(FfiExprOp.AND, _flatten_same(FfiExprOp.AND, self, other))

    def __or__(self, other: "Expr") -> "Expr":
        return _Logic(FfiExprOp.OR, _flatten_same(FfiExprOp.OR, self, other))

    def __invert__(self) -> "Expr":
        return _Logic(FfiExprOp.NOT, (self,))

    def __bool__(self) -> bool:
        raise TypeError(
            "a bkndb filter expression has no truth value — combine filters with & | ~ "
            "(not and/or/not), and parenthesize each comparison"
        )

    def _emit(self, nodes: typing.List[FfiExprNode]) -> int:
        raise NotImplementedError


class _Leaf(Expr):
    __slots__ = ("op", "column", "values")

    def __init__(self, op: FfiExprOp, column: str, values: typing.Sequence[PropertyValue] = ()) -> None:
        self.op = op
        self.column = column
        self.values = tuple(values)

    def _emit(self, nodes: typing.List[FfiExprNode]) -> int:
        nodes.append(FfiExprNode(op=self.op, column=self.column, values=[to_ffi_value(v) for v in self.values], children=[]))
        return len(nodes) - 1

    def __repr__(self) -> str:
        return f"<{self.op.name} {self.column} {list(self.values)!r}>"


class _Logic(Expr):
    __slots__ = ("op", "children")

    def __init__(self, op: FfiExprOp, children: typing.Sequence[Expr]) -> None:
        for c in children:
            if not isinstance(c, Expr):
                raise TypeError(f"expected a filter expression, got {type(c).__name__}")
        self.op = op
        self.children = tuple(children)

    def _emit(self, nodes: typing.List[FfiExprNode]) -> int:
        indices = [c._emit(nodes) for c in self.children]
        nodes.append(FfiExprNode(op=self.op, column=None, values=[], children=indices))
        return len(nodes) - 1

    def __repr__(self) -> str:
        return f"<{self.op.name} {list(self.children)!r}>"


def _flatten_same(op: FfiExprOp, a: Expr, b: Expr) -> typing.Tuple[Expr, ...]:
    parts: typing.List[Expr] = []
    for e in (a, b):
        if isinstance(e, _Logic) and e.op == op:
            parts.extend(e.children)
        else:
            parts.append(e)
    return tuple(parts)


class Col:
    """A column reference — the left-hand side of a filter comparison."""

    __slots__ = ("name",)
    __hash__ = None  # type: ignore[assignment]  # `==` builds a filter, not a bool

    def __init__(self, name: str) -> None:
        self.name = name

    def _cmp(self, op: FfiExprOp, value: PropertyValue) -> Expr:
        return _Leaf(op, self.name, (value,))

    def __eq__(self, value: PropertyValue) -> Expr:  # type: ignore[override]
        return self._cmp(FfiExprOp.EQ, value)

    def __ne__(self, value: PropertyValue) -> Expr:  # type: ignore[override]
        return self._cmp(FfiExprOp.NE, value)

    def __lt__(self, value: PropertyValue) -> Expr:
        return self._cmp(FfiExprOp.LT, value)

    def __le__(self, value: PropertyValue) -> Expr:
        return self._cmp(FfiExprOp.LE, value)

    def __gt__(self, value: PropertyValue) -> Expr:
        return self._cmp(FfiExprOp.GT, value)

    def __ge__(self, value: PropertyValue) -> Expr:
        return self._cmp(FfiExprOp.GE, value)

    def is_in(self, values: typing.Iterable[PropertyValue]) -> Expr:
        return _Leaf(FfiExprOp.IN, self.name, tuple(values))

    def is_null(self) -> Expr:
        return _Leaf(FfiExprOp.IS_NULL, self.name)

    def is_not_null(self) -> Expr:
        return _Leaf(FfiExprOp.IS_NOT_NULL, self.name)

    def startswith(self, prefix: str) -> Expr:
        return _Leaf(FfiExprOp.STARTS_WITH, self.name, (prefix,))

    def contains(self, value: PropertyValue) -> Expr:
        """List column has an element equal to `value`; str column contains
        it as a substring; dict column has it as a key."""
        return _Leaf(FfiExprOp.CONTAINS, self.name, (value,))

    def like(self, pattern: str) -> Expr:
        """SQL ``LIKE``: ``%`` matches any run of characters, ``_`` one."""
        return _Leaf(FfiExprOp.LIKE, self.name, (pattern,))

    def ilike(self, pattern: str) -> Expr:
        """Case-insensitive :meth:`like`."""
        return _Leaf(FfiExprOp.I_LIKE, self.name, (pattern,))

    def __getitem__(self, key: typing.Union[str, int]) -> "Col":
        """A path into a dict/list column: ``col("meta")["author"]`` is
        ``col("meta.author")``, ``col("tags")[0]`` is ``col("tags.0")``."""
        return Col(f"{self.name}.{key}")

    def between(self, low: PropertyValue, high: PropertyValue) -> Expr:
        """Inclusive on both ends, like SQL ``BETWEEN``."""
        return (self >= low) & (self <= high)

    def __repr__(self) -> str:
        return f"col({self.name!r})"


def col(name: str) -> Col:
    """Starts a filter on a column: ``col("age") >= 18``."""
    return Col(name)


def _where_expr(where: Where) -> typing.Optional[Expr]:
    if where is None:
        return None
    if isinstance(where, Expr):
        return where
    if isinstance(where, typing.Mapping):
        exprs = [col(k) == v for k, v in where.items()]
        if not exprs:
            return None
        combined = exprs[0]
        for e in exprs[1:]:
            combined = combined & e
        return combined
    raise TypeError(f"`where` must be a filter expression or a dict of column: value, got {type(where).__name__}")


def _order(order_by: OrderBy) -> typing.List[FfiOrder]:
    if order_by is None:
        return []
    items = [order_by] if isinstance(order_by, str) else list(order_by)
    out = []
    for item in items:
        descending = item.startswith("-")
        out.append(FfiOrder(column=item[1:] if descending else item, descending=descending))
    return out


def to_ffi_filter(where: Where) -> typing.List[FfiExprNode]:
    """A standalone filter as post-order FFI nodes (empty = no filter)."""
    nodes: typing.List[FfiExprNode] = []
    expr = _where_expr(where)
    if expr is not None:
        expr._emit(nodes)
    return nodes


def to_ffi_query(
    where: Where = None,
    *,
    order_by: OrderBy = None,
    limit: typing.Optional[int] = None,
    offset: int = 0,
    columns: typing.Optional[typing.Sequence[str]] = None,
) -> FfiQuery:
    """``order_by`` takes column names; prefix one with ``-`` for descending."""
    nodes: typing.List[FfiExprNode] = []
    expr = _where_expr(where)
    if expr is not None:
        expr._emit(nodes)
    if limit is not None and limit < 0:
        raise ValueError("limit must be >= 0")
    if offset < 0:
        raise ValueError("offset must be >= 0")
    return FfiQuery(
        filter=nodes,
        order_by=_order(order_by),
        offset=offset,
        limit=limit,
        columns=list(columns) if columns is not None else None,
    )


class Agg:
    """Aggregate functions for :meth:`bkndb.Database.aggregate`::

        db.aggregate("orders", [Agg.count(), Agg.sum("total")], group_by=["status"])
    """

    __slots__ = ("func", "column")

    def __init__(self, func: FfiAggFunc, column: typing.Optional[str] = None) -> None:
        self.func = func
        self.column = column

    @staticmethod
    def count(column: typing.Optional[str] = None) -> "Agg":
        """Number of rows, or of rows where ``column`` is non-null."""
        return Agg(FfiAggFunc.COUNT_COLUMN, column) if column else Agg(FfiAggFunc.COUNT)

    @staticmethod
    def sum(column: str) -> "Agg":
        return Agg(FfiAggFunc.SUM, column)

    @staticmethod
    def min(column: str) -> "Agg":
        return Agg(FfiAggFunc.MIN, column)

    @staticmethod
    def max(column: str) -> "Agg":
        return Agg(FfiAggFunc.MAX, column)

    @staticmethod
    def avg(column: str) -> "Agg":
        return Agg(FfiAggFunc.AVG, column)

    def _to_ffi(self) -> FfiAgg:
        return FfiAgg(func=self.func, column=self.column)

    def __repr__(self) -> str:
        return f"Agg.{self.func.name.lower()}({self.column!r})" if self.column else f"Agg.{self.func.name.lower()}()"
