//! FFI records for the relational layer: table schemas, rows, queries and
//! aggregates, plus their conversions to and from `bkndb_core` types.
use std::collections::HashMap;

use bkndb_core::relational::{
    Agg, AggregateRow, CmpOp, ColumnKind, ColumnSchema, Expr, Order, Query, Row, TableSchema,
};

use crate::error::FfiBknError;
use crate::types::{core_props_to_ffi, FfiPropValue};

mod search;
mod expr;
#[cfg(test)]
mod tests;

pub use search::*;
pub(crate) use expr::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiColumnKind {
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    Timestamp,
    Uuid,
    List,
    Map,
}

impl From<FfiColumnKind> for ColumnKind {
    fn from(k: FfiColumnKind) -> Self {
        match k {
            FfiColumnKind::Bool => ColumnKind::Bool,
            FfiColumnKind::Int => ColumnKind::Int,
            FfiColumnKind::Float => ColumnKind::Float,
            FfiColumnKind::Str => ColumnKind::Str,
            FfiColumnKind::Bytes => ColumnKind::Bytes,
            FfiColumnKind::Timestamp => ColumnKind::Timestamp,
            FfiColumnKind::Uuid => ColumnKind::Uuid,
            FfiColumnKind::List => ColumnKind::List,
            FfiColumnKind::Map => ColumnKind::Map,
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
            ColumnKind::Timestamp => FfiColumnKind::Timestamp,
            ColumnKind::Uuid => FfiColumnKind::Uuid,
            ColumnKind::List => FfiColumnKind::List,
            ColumnKind::Map => FfiColumnKind::Map,
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
    /// List `column` has an element equal to `values[0]`, string `column`
    /// contains it as a substring, or map `column` has it as a key.
    Contains,
    /// String `column` matches the SQL LIKE pattern `values[0]`.
    Like,
    /// Case-insensitive `Like`.
    ILike,
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
