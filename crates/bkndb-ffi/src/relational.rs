//! FFI records for the relational layer: table schemas, rows, queries and
//! aggregates, plus their conversions to and from `bkndb_core` types.
use std::collections::HashMap;

use bkndb_core::relational::{
    Agg, AggregateRow, CmpOp, ColumnKind, ColumnSchema, Expr, Order, Query, Row, TableSchema,
};

use crate::error::FfiBknError;
use crate::types::{core_props_to_ffi, FfiPropValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiColumnKind {
    Bool,
    Int,
    Float,
    Str,
    Bytes,
}

impl From<FfiColumnKind> for ColumnKind {
    fn from(k: FfiColumnKind) -> Self {
        match k {
            FfiColumnKind::Bool => ColumnKind::Bool,
            FfiColumnKind::Int => ColumnKind::Int,
            FfiColumnKind::Float => ColumnKind::Float,
            FfiColumnKind::Str => ColumnKind::Str,
            FfiColumnKind::Bytes => ColumnKind::Bytes,
        }
    }
}

impl TryFrom<ColumnKind> for FfiColumnKind {
    type Error = FfiBknError;
    fn try_from(k: ColumnKind) -> Result<Self, FfiBknError> {
        Ok(match k {
            ColumnKind::Bool => FfiColumnKind::Bool,
            ColumnKind::Int => FfiColumnKind::Int,
            ColumnKind::Float => FfiColumnKind::Float,
            ColumnKind::Str => FfiColumnKind::Str,
            ColumnKind::Bytes => FfiColumnKind::Bytes,
            ColumnKind::Null => return Err(invalid("a column cannot have kind Null")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiColumn {
    pub name: String,
    pub kind: FfiColumnKind,
    #[uniffi(default = true)]
    pub nullable: bool,
    #[uniffi(default = false)]
    pub unique: bool,
    #[uniffi(default = None)]
    pub default_value: Option<FfiPropValue>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTableSchema {
    pub name: String,
    pub columns: Vec<FfiColumn>,
    pub primary_key: String,
    #[uniffi(default = false)]
    pub auto_increment: bool,
    /// Secondary indexes (UNIQUE columns are indexed automatically).
    #[uniffi(default = [])]
    pub indexed_columns: Vec<String>,
}

pub(crate) fn invalid(message: impl Into<String>) -> FfiBknError {
    FfiBknError::InvalidArgument { message: message.into() }
}

impl TryFrom<FfiTableSchema> for TableSchema {
    type Error = FfiBknError;
    fn try_from(s: FfiTableSchema) -> Result<Self, FfiBknError> {
        let mut b = TableSchema::builder(s.name).primary_key(s.primary_key);
        for c in s.columns {
            let mut col = ColumnSchema::new(c.name, c.kind.into());
            col.nullable = c.nullable;
            col.unique = c.unique;
            col.default = c.default_value.map(Into::into);
            b = b.column(col);
        }
        for i in s.indexed_columns {
            b = b.index(i);
        }
        if s.auto_increment {
            b = b.auto_increment();
        }
        Ok(b.build()?)
    }
}

impl TryFrom<&TableSchema> for FfiTableSchema {
    type Error = FfiBknError;
    fn try_from(s: &TableSchema) -> Result<Self, FfiBknError> {
        Ok(FfiTableSchema {
            name: s.name().to_string(),
            columns: s
                .columns()
                .iter()
                .map(|c| {
                    Ok(FfiColumn {
                        name: c.name.clone(),
                        kind: c.kind.try_into()?,
                        nullable: c.nullable,
                        unique: c.unique,
                        default_value: c.default.clone().map(Into::into),
                    })
                })
                .collect::<Result<_, FfiBknError>>()?,
            primary_key: s.primary_key().to_string(),
            auto_increment: s.auto_increment_pk(),
            indexed_columns: s.indexed_columns().to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRow {
    pub pk: FfiPropValue,
    pub values: HashMap<String, FfiPropValue>,
}

impl From<Row> for FfiRow {
    fn from(r: Row) -> Self {
        FfiRow { pk: r.pk.into(), values: core_props_to_ffi(r.values) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiExprOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `column` is one of `values`.
    In,
    IsNull,
    IsNotNull,
    /// `column` (a string) starts with `values[0]` (a string).
    StartsWith,
    /// All of `children`.
    And,
    /// Any of `children`.
    Or,
    /// Negation of `children[0]`.
    Not,
}

/// One node of a filter expression. A filter is a list of nodes in
/// post-order: each node's `children` are indices of *earlier* nodes, and
/// the last node is the root. Every node must be used exactly once.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiExprNode {
    pub op: FfiExprOp,
    #[uniffi(default = None)]
    pub column: Option<String>,
    #[uniffi(default = [])]
    pub values: Vec<FfiPropValue>,
    #[uniffi(default = [])]
    pub children: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiOrder {
    pub column: String,
    #[uniffi(default = false)]
    pub descending: bool,
}

#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct FfiQuery {
    #[uniffi(default = [])]
    pub filter: Vec<FfiExprNode>,
    #[uniffi(default = [])]
    pub order_by: Vec<FfiOrder>,
    #[uniffi(default = 0)]
    pub offset: u64,
    #[uniffi(default = None)]
    pub limit: Option<u64>,
    /// Projection; `None` returns every column.
    #[uniffi(default = None)]
    pub columns: Option<Vec<String>>,
}

fn build_expr(nodes: Vec<FfiExprNode>) -> Result<Option<Expr>, FfiBknError> {
    if nodes.is_empty() {
        return Ok(None);
    }
    let mut built: Vec<Option<Expr>> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.into_iter().enumerate() {
        let mut children = Vec::with_capacity(n.children.len());
        for &c in &n.children {
            let slot = built
                .get_mut(c as usize)
                .ok_or_else(|| invalid(format!("filter node {i} refers to child {c}, which is not an earlier node")))?;
            children.push(slot.take().ok_or_else(|| invalid(format!("filter node {c} is used more than once")))?);
        }
        let column = || n.column.clone().ok_or_else(|| invalid(format!("filter node {i} ({:?}) needs a column", n.op)));
        let one_value = || -> Result<_, FfiBknError> {
            match n.values.as_slice() {
                [v] => Ok(v.clone().into()),
                _ => Err(invalid(format!("filter node {i} ({:?}) needs exactly one value", n.op))),
            }
        };
        let cmp = |op| -> Result<Expr, FfiBknError> { Ok(Expr::Cmp(column()?, op, one_value()?)) };
        let expr = match n.op {
            FfiExprOp::Eq => cmp(CmpOp::Eq)?,
            FfiExprOp::Ne => cmp(CmpOp::Ne)?,
            FfiExprOp::Lt => cmp(CmpOp::Lt)?,
            FfiExprOp::Le => cmp(CmpOp::Le)?,
            FfiExprOp::Gt => cmp(CmpOp::Gt)?,
            FfiExprOp::Ge => cmp(CmpOp::Ge)?,
            FfiExprOp::In => Expr::In(column()?, n.values.iter().cloned().map(Into::into).collect()),
            FfiExprOp::IsNull => Expr::IsNull(column()?),
            FfiExprOp::IsNotNull => Expr::IsNotNull(column()?),
            FfiExprOp::StartsWith => match n.values.as_slice() {
                [FfiPropValue::Str(p)] => Expr::Prefix(column()?, p.clone()),
                _ => return Err(invalid(format!("filter node {i} (StartsWith) needs one string value"))),
            },
            FfiExprOp::And => Expr::And(children),
            FfiExprOp::Or => Expr::Or(children),
            FfiExprOp::Not => match <[Expr; 1]>::try_from(children) {
                Ok([e]) => Expr::Not(Box::new(e)),
                Err(_) => return Err(invalid(format!("filter node {i} (Not) needs exactly one child"))),
            },
        };
        built.push(Some(expr));
    }
    let root = built.pop().flatten();
    if built.iter().any(Option::is_some) {
        return Err(invalid("filter has nodes that are not reachable from the root (the last node)"));
    }
    Ok(root)
}

impl TryFrom<FfiQuery> for Query {
    type Error = FfiBknError;
    fn try_from(q: FfiQuery) -> Result<Self, FfiBknError> {
        let mut query = Query::new();
        if let Some(e) = build_expr(q.filter)? {
            query = query.filter(e);
        }
        for o in q.order_by {
            query = query.order_by(&o.column, if o.descending { Order::Desc } else { Order::Asc });
        }
        query = query.offset(usize::try_from(q.offset).unwrap_or(usize::MAX));
        if let Some(l) = q.limit {
            query = query.limit(usize::try_from(l).unwrap_or(usize::MAX));
        }
        if let Some(cols) = q.columns {
            query = query.columns(cols);
        }
        Ok(query)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAggFunc {
    /// Number of rows (no column).
    Count,
    /// Number of rows where the column is non-null.
    CountColumn,
    Sum,
    Min,
    Max,
    Avg,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiAgg {
    pub func: FfiAggFunc,
    #[uniffi(default = None)]
    pub column: Option<String>,
}

impl TryFrom<FfiAgg> for Agg {
    type Error = FfiBknError;
    fn try_from(a: FfiAgg) -> Result<Self, FfiBknError> {
        let col = || a.column.clone().ok_or_else(|| invalid(format!("aggregate {:?} needs a column", a.func)));
        Ok(match a.func {
            FfiAggFunc::Count => Agg::Count,
            FfiAggFunc::CountColumn => Agg::CountColumn(col()?),
            FfiAggFunc::Sum => Agg::Sum(col()?),
            FfiAggFunc::Min => Agg::Min(col()?),
            FfiAggFunc::Max => Agg::Max(col()?),
            FfiAggFunc::Avg => Agg::Avg(col()?),
        })
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiAggregateRow {
    pub group: Vec<FfiPropValue>,
    pub values: Vec<FfiPropValue>,
}

impl From<AggregateRow> for FfiAggregateRow {
    fn from(r: AggregateRow) -> Self {
        FfiAggregateRow {
            group: r.group.into_iter().map(Into::into).collect(),
            values: r.values.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(op: FfiExprOp, col: &str, v: i64) -> FfiExprNode {
        FfiExprNode { op, column: Some(col.into()), values: vec![FfiPropValue::Int(v)], children: vec![] }
    }

    #[test]
    fn post_order_nodes_build_the_expected_tree() {
        let nodes = vec![
            leaf(FfiExprOp::Ge, "a", 1),
            leaf(FfiExprOp::Lt, "a", 5),
            FfiExprNode { op: FfiExprOp::And, column: None, values: vec![], children: vec![0, 1] },
            FfiExprNode { op: FfiExprOp::Not, column: None, values: vec![], children: vec![2] },
        ];
        let e = build_expr(nodes).unwrap().unwrap();
        assert_eq!(
            e,
            Expr::Not(Box::new(Expr::And(vec![
                Expr::Cmp("a".into(), CmpOp::Ge, 1.into()),
                Expr::Cmp("a".into(), CmpOp::Lt, 5.into()),
            ])))
        );
    }

    #[test]
    fn malformed_trees_are_rejected() {
        let and_self = FfiExprNode { op: FfiExprOp::And, column: None, values: vec![], children: vec![0] };
        assert!(build_expr(vec![and_self]).is_err(), "forward/self reference");
        assert!(build_expr(vec![leaf(FfiExprOp::Eq, "a", 1), leaf(FfiExprOp::Eq, "b", 2)]).is_err(), "dangling node");
        let twice = FfiExprNode { op: FfiExprOp::Or, column: None, values: vec![], children: vec![0, 0] };
        assert!(build_expr(vec![leaf(FfiExprOp::Eq, "a", 1), twice]).is_err(), "reused node");
        let no_col = FfiExprNode { op: FfiExprOp::Eq, column: None, values: vec![FfiPropValue::Int(1)], children: vec![] };
        assert!(build_expr(vec![no_col]).is_err());
    }
}
