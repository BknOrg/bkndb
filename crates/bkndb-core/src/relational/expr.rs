//! Filter expressions and aggregates for relational queries.
//!
//! ```ignore
//! use bkndb::relational::{col, Agg};
//! table.select()
//!     .filter(col("age").ge(18).and(col("city").is_in(["Jakarta", "Bandung"])))
//!     .order_by_desc("age")
//!     .limit(10)
//!     .run()?;
//! ```
//!
//! Comparisons against a missing or null column value are false (so are
//! comparisons between incomparable kinds, e.g. Str vs Int); use
//! [`Col::is_null`] / [`Col::is_not_null`] to test for null. `Int` and
//! `Float` compare numerically with each other.
use std::cmp::Ordering;

use crate::relational::db::Row;
use crate::relational::schema::{ColumnKind, TableSchema};
use crate::value::PropValue;
use crate::BknError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    fn holds(self, ord: Ordering) -> bool {
        match self {
            CmpOp::Eq => ord == Ordering::Equal,
            CmpOp::Ne => ord != Ordering::Equal,
            CmpOp::Lt => ord == Ordering::Less,
            CmpOp::Le => ord != Ordering::Greater,
            CmpOp::Gt => ord == Ordering::Greater,
            CmpOp::Ge => ord != Ordering::Less,
        }
    }
}

/// A boolean predicate over one row.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Cmp(String, CmpOp, PropValue),
    In(String, Vec<PropValue>),
    IsNull(String),
    IsNotNull(String),
    /// String column starts with the given prefix.
    Prefix(String, String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

/// Starts an expression on a column: `col("age").gt(30)`.
pub fn col(name: impl Into<String>) -> Col {
    Col(name.into())
}

/// A column reference, the left-hand side of an [`Expr`].
#[derive(Debug, Clone)]
pub struct Col(String);

impl Col {
    fn cmp(self, op: CmpOp, v: impl Into<PropValue>) -> Expr {
        Expr::Cmp(self.0, op, v.into())
    }
    pub fn eq(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Eq, v)
    }
    pub fn ne(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Ne, v)
    }
    pub fn lt(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Lt, v)
    }
    pub fn le(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Le, v)
    }
    pub fn gt(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Gt, v)
    }
    pub fn ge(self, v: impl Into<PropValue>) -> Expr {
        self.cmp(CmpOp::Ge, v)
    }
    /// Inclusive on both ends, like SQL `BETWEEN`.
    pub fn between(self, lo: impl Into<PropValue>, hi: impl Into<PropValue>) -> Expr {
        Expr::And(vec![self.clone().ge(lo), self.le(hi)])
    }
    pub fn is_in<V: Into<PropValue>>(self, values: impl IntoIterator<Item = V>) -> Expr {
        Expr::In(self.0, values.into_iter().map(Into::into).collect())
    }
    pub fn is_null(self) -> Expr {
        Expr::IsNull(self.0)
    }
    pub fn is_not_null(self) -> Expr {
        Expr::IsNotNull(self.0)
    }
    pub fn starts_with(self, prefix: impl Into<String>) -> Expr {
        Expr::Prefix(self.0, prefix.into())
    }
}

impl Expr {
    pub fn and(self, other: Expr) -> Expr {
        match self {
            Expr::And(mut v) => {
                v.push(other);
                Expr::And(v)
            }
            e => Expr::And(vec![e, other]),
        }
    }

    pub fn or(self, other: Expr) -> Expr {
        match self {
            Expr::Or(mut v) => {
                v.push(other);
                Expr::Or(v)
            }
            e => Expr::Or(vec![e, other]),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Expr {
        Expr::Not(Box::new(self))
    }

    pub(crate) fn eval(&self, schema: &TableSchema, row: &Row) -> bool {
        let value = |c: &str| row.get(schema, c).filter(|v| !matches!(v, PropValue::Null));
        match self {
            Expr::Cmp(c, op, PropValue::Null) => match op {
                // `eq(Null)` / `ne(Null)` read naturally as null tests.
                CmpOp::Eq => value(c).is_none(),
                CmpOp::Ne => value(c).is_some(),
                _ => false,
            },
            Expr::Cmp(c, op, rhs) => value(c)
                .and_then(|v| compare_values(v, rhs))
                .is_some_and(|ord| op.holds(ord)),
            Expr::In(c, list) => value(c)
                .is_some_and(|v| list.iter().any(|x| compare_values(v, x) == Some(Ordering::Equal))),
            Expr::IsNull(c) => value(c).is_none(),
            Expr::IsNotNull(c) => value(c).is_some(),
            Expr::Prefix(c, p) => matches!(value(c), Some(PropValue::Str(s)) if s.starts_with(p.as_str())),
            Expr::And(v) => v.iter().all(|e| e.eval(schema, row)),
            Expr::Or(v) => v.iter().any(|e| e.eval(schema, row)),
            Expr::Not(e) => !e.eval(schema, row),
        }
    }

    pub(crate) fn columns<'e>(&'e self, out: &mut Vec<&'e str>) {
        match self {
            Expr::Cmp(c, ..) | Expr::In(c, _) | Expr::IsNull(c) | Expr::IsNotNull(c) | Expr::Prefix(c, _) => {
                out.push(c)
            }
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.columns(out)),
            Expr::Not(e) => e.columns(out),
        }
    }
}

/// Ordering between two non-null values, or `None` if their kinds aren't
/// comparable. `Int`/`Float` compare numerically across kinds.
pub(crate) fn compare_values(a: &PropValue, b: &PropValue) -> Option<Ordering> {
    use PropValue::*;
    match (a, b) {
        (Int(x), Int(y)) => Some(x.cmp(y)),
        (Float(x), Float(y)) => x.partial_cmp(y),
        (Int(x), Float(y)) => (*x as f64).partial_cmp(y),
        (Float(x), Int(y)) => x.partial_cmp(&(*y as f64)),
        (Str(x), Str(y)) => Some(x.cmp(y)),
        (Bool(x), Bool(y)) => Some(x.cmp(y)),
        (Bytes(x), Bytes(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

/// A total order over optional values for ORDER BY and grouping: nulls
/// (and missing values) first, then by kind, then by value.
pub(crate) fn total_cmp(a: Option<&PropValue>, b: Option<&PropValue>) -> Ordering {
    fn rank(v: Option<&PropValue>) -> u8 {
        match v {
            None | Some(PropValue::Null) => 0,
            Some(PropValue::Bool(_)) => 1,
            Some(PropValue::Int(_) | PropValue::Float(_)) => 2,
            Some(PropValue::Str(_)) => 3,
            Some(PropValue::Bytes(_)) => 4,
        }
    }
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (Some(PropValue::Float(x)), Some(PropValue::Float(y))) => x.total_cmp(y),
        (Some(PropValue::Int(x)), Some(PropValue::Float(y))) => (*x as f64).total_cmp(y),
        (Some(PropValue::Float(x)), Some(PropValue::Int(y))) => x.total_cmp(&(*y as f64)),
        (Some(x), Some(y)) => compare_values(x, y).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Asc,
    Desc,
}

/// An aggregate function over the matched rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Agg {
    /// Number of rows.
    Count,
    /// Number of rows where the column is non-null.
    CountColumn(String),
    /// Sum of a numeric column (null if no non-null values). Int if every
    /// value is Int, otherwise Float.
    Sum(String),
    Min(String),
    Max(String),
    /// Average of a numeric column as Float (null if no non-null values).
    Avg(String),
}

impl Agg {
    pub fn count() -> Agg {
        Agg::Count
    }
    pub fn count_column(c: impl Into<String>) -> Agg {
        Agg::CountColumn(c.into())
    }
    pub fn sum(c: impl Into<String>) -> Agg {
        Agg::Sum(c.into())
    }
    pub fn min(c: impl Into<String>) -> Agg {
        Agg::Min(c.into())
    }
    pub fn max(c: impl Into<String>) -> Agg {
        Agg::Max(c.into())
    }
    pub fn avg(c: impl Into<String>) -> Agg {
        Agg::Avg(c.into())
    }

    pub(crate) fn column(&self) -> Option<&str> {
        match self {
            Agg::Count => None,
            Agg::CountColumn(c) | Agg::Sum(c) | Agg::Min(c) | Agg::Max(c) | Agg::Avg(c) => Some(c),
        }
    }

    pub(crate) fn check(&self, schema: &TableSchema) -> Result<(), BknError> {
        if let Agg::Sum(c) | Agg::Avg(c) = self {
            let kind = schema.column(c).map(|c| c.kind);
            if !matches!(kind, Some(ColumnKind::Int | ColumnKind::Float)) {
                return Err(BknError::SchemaMismatch {
                    table: schema.name().to_string(),
                    message: format!("SUM/AVG need a numeric column, '{c}' is {kind:?}"),
                });
            }
        }
        Ok(())
    }
}

/// One output row of a grouped aggregate: the group's key values (in
/// `group_by` order) and one value per requested [`Agg`].
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateRow {
    pub group: Vec<PropValue>,
    pub values: Vec<PropValue>,
}

#[derive(Debug, Clone)]
pub(crate) enum Acc {
    Count(u64),
    Sum { int: i128, float: f64, any_float: bool, seen: bool },
    Min(Option<PropValue>),
    Max(Option<PropValue>),
    Avg { sum: f64, n: u64 },
}

impl Acc {
    pub(crate) fn new(agg: &Agg) -> Acc {
        match agg {
            Agg::Count | Agg::CountColumn(_) => Acc::Count(0),
            Agg::Sum(_) => Acc::Sum { int: 0, float: 0.0, any_float: false, seen: false },
            Agg::Min(_) => Acc::Min(None),
            Agg::Max(_) => Acc::Max(None),
            Agg::Avg(_) => Acc::Avg { sum: 0.0, n: 0 },
        }
    }

    pub(crate) fn add(&mut self, agg: &Agg, schema: &TableSchema, row: &Row) {
        let v = agg
            .column()
            .and_then(|c| row.get(schema, c))
            .filter(|v| !matches!(v, PropValue::Null));
        match self {
            Acc::Count(n) => {
                if agg.column().is_none() || v.is_some() {
                    *n += 1
                }
            }
            Acc::Sum { int, float, any_float, seen } => match v {
                Some(PropValue::Int(i)) => {
                    *int += *i as i128;
                    *seen = true;
                }
                Some(PropValue::Float(f)) => {
                    *float += f;
                    *any_float = true;
                    *seen = true;
                }
                _ => {}
            },
            Acc::Min(cur) | Acc::Max(cur) => {
                let want = if matches!(agg, Agg::Min(_)) { Ordering::Less } else { Ordering::Greater };
                if let Some(v) = v
                    && cur.as_ref().is_none_or(|c| total_cmp(Some(v), Some(c)) == want)
                {
                    *cur = Some(v.clone());
                }
            }
            Acc::Avg { sum, n } => match v {
                Some(PropValue::Int(i)) => {
                    *sum += *i as f64;
                    *n += 1;
                }
                Some(PropValue::Float(f)) => {
                    *sum += f;
                    *n += 1;
                }
                _ => {}
            },
        }
    }

    pub(crate) fn finish(self, table: &str) -> Result<PropValue, BknError> {
        Ok(match self {
            Acc::Count(n) => PropValue::Int(n as i64),
            Acc::Sum { seen: false, .. } => PropValue::Null,
            Acc::Sum { int, float, any_float: true, .. } => PropValue::Float(int as f64 + float),
            Acc::Sum { int, .. } => PropValue::Int(i64::try_from(int).map_err(|_| BknError::ConstraintViolation {
                table: table.to_string(),
                message: "SUM overflows a 64-bit integer".to_string(),
            })?),
            Acc::Min(v) | Acc::Max(v) => v.unwrap_or(PropValue::Null),
            Acc::Avg { n: 0, .. } => PropValue::Null,
            Acc::Avg { sum, n } => PropValue::Float(sum / n as f64),
        })
    }
}
