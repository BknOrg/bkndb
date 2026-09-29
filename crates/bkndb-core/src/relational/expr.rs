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
//!
//! A column reference may be a dotted path into a `List`/`Map` column —
//! `col("meta.author.name")`, `col("tags.0")` — in filters, ordering and
//! grouping. A column whose name literally contains the dots wins.
use std::cmp::Ordering;

use crate::relational::db::Row;
use crate::relational::schema::{ColumnKind, TableSchema};
pub(crate) use crate::value::{compare_values, like_matches, total_cmp};
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
    /// List column has an element equal to the value; Str column contains
    /// the value as a substring; Map column has the value (a Str) as a key.
    Contains(String, PropValue),
    /// SQL `LIKE`: `%` matches any run of characters, `_` exactly one.
    /// The flag makes it case-insensitive (`ILIKE`).
    Like(String, String, bool),
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
    pub fn contains(self, v: impl Into<PropValue>) -> Expr {
        Expr::Contains(self.0, v.into())
    }
    pub fn like(self, pattern: impl Into<String>) -> Expr {
        Expr::Like(self.0, pattern.into(), false)
    }
    pub fn ilike(self, pattern: impl Into<String>) -> Expr {
        Expr::Like(self.0, pattern.into(), true)
    }
}

/// The value a column reference names in `row`: a column, or a dotted path
/// into a `List`/`Map` column (see the module docs).
pub(crate) fn resolve<'r>(schema: &TableSchema, row: &'r Row, name: &str) -> Option<&'r PropValue> {
    if schema.column(name).is_some() {
        return row.get(schema, name);
    }
    let (head, rest) = name.split_once('.')?;
    let segments: Vec<&str> = rest.split('.').collect();
    row.get(schema, head)?.get_path(&segments)
}

/// The schema column a reference is rooted at, if any.
pub(crate) fn root_column<'n>(schema: &TableSchema, name: &'n str) -> Option<&'n str> {
    if schema.column(name).is_some() {
        return Some(name);
    }
    let (head, _) = name.split_once('.')?;
    schema.column(head).map(|_| head)
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
        let value = |c: &str| resolve(schema, row, c).filter(|v| !matches!(v, PropValue::Null));
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
            Expr::Contains(c, needle) => match (value(c), needle) {
                (Some(PropValue::List(items)), n) => items.iter().any(|x| compare_values(x, n) == Some(Ordering::Equal)),
                (Some(PropValue::Str(s)), PropValue::Str(n)) => s.contains(n.as_str()),
                (Some(PropValue::Map(m)), PropValue::Str(k)) => m.contains_key(k),
                _ => false,
            },
            Expr::Like(c, pattern, ci) => matches!(value(c), Some(PropValue::Str(s)) if like_matches(s, pattern, *ci)),
            Expr::And(v) => v.iter().all(|e| e.eval(schema, row)),
            Expr::Or(v) => v.iter().any(|e| e.eval(schema, row)),
            Expr::Not(e) => !e.eval(schema, row),
        }
    }

    pub(crate) fn columns<'e>(&'e self, out: &mut Vec<&'e str>) {
        match self {
            Expr::Cmp(c, ..)
            | Expr::In(c, _)
            | Expr::IsNull(c)
            | Expr::IsNotNull(c)
            | Expr::Prefix(c, _)
            | Expr::Contains(c, _)
            | Expr::Like(c, ..) => out.push(c),
            Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| e.columns(out)),
            Expr::Not(e) => e.columns(out),
        }
    }
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

#[cfg(test)]
mod like_tests {
    use super::like_matches;

    #[test]
    fn like_patterns() {
        for (text, pattern, expected) in [
            ("hello", "hello", true),
            ("hello", "h%", true),
            ("hello", "%llo", true),
            ("hello", "%l%", true),
            ("hello", "h_llo", true),
            ("hello", "h_lo", false),
            ("hello", "%", true),
            ("", "%", true),
            ("", "_", false),
            ("abcabc", "%abc", true),
            ("aXbXc", "a%b%c", true),
            ("aXbXd", "a%b%c", false),
            ("naïve", "na_ve", true),
        ] {
            assert_eq!(like_matches(text, pattern, false), expected, "{text:?} LIKE {pattern:?}");
        }
        assert!(like_matches("HeLLo", "hel%", true));
        assert!(!like_matches("HeLLo", "hel%", false));
    }
}
