use std::ops::Bound;

use crate::relational::codec::sortable_encode;
use crate::relational::db::{RelTable, Row};
use crate::relational::schema::RelSchema;
use crate::value::PropValue;
use crate::{BknError, StorageBackend};

#[derive(Clone)]
enum Predicate {
    Eq(String, PropValue),
    Range(String, Bound<PropValue>, Bound<PropValue>),
    Prefix(String, String),
}

impl Predicate {
    fn column(&self) -> &str {
        match self {
            Predicate::Eq(c, _) => c,
            Predicate::Range(c, _, _) => c,
            Predicate::Prefix(c, _) => c,
        }
    }
}

fn cmp_prop(a: &PropValue, b: &PropValue) -> Result<std::cmp::Ordering, BknError> {
    Ok(sortable_encode(a)?.cmp(&sortable_encode(b)?))
}

fn bound_ok(v: &PropValue, bound: &Bound<PropValue>, want_lower: bool) -> Result<bool, BknError> {
    Ok(match bound {
        Bound::Unbounded => true,
        Bound::Included(b) => {
            let ord = cmp_prop(v, b)?;
            if want_lower {
                ord != std::cmp::Ordering::Less
            } else {
                ord != std::cmp::Ordering::Greater
            }
        }
        Bound::Excluded(b) => {
            let ord = cmp_prop(v, b)?;
            if want_lower {
                ord == std::cmp::Ordering::Greater
            } else {
                ord == std::cmp::Ordering::Less
            }
        }
    })
}

fn predicate_matches(schema: &RelSchema, pred: &Predicate, row: &Row) -> Result<bool, BknError> {
    match pred {
        Predicate::Eq(col, val) => Ok(row.get(schema, col) == Some(val)),
        Predicate::Range(col, start, end) => match row.get(schema, col) {
            Some(v) => Ok(bound_ok(v, start, true)? && bound_ok(v, end, false)?),
            None => Ok(false),
        },
        Predicate::Prefix(col, prefix) => match row.get(schema, col) {
            Some(PropValue::Str(s)) => Ok(s.starts_with(prefix)),
            _ => Ok(false),
        },
    }
}

fn predicates_match(schema: &RelSchema, predicates: &[Predicate], row: &Row) -> Result<bool, BknError> {
    for p in predicates {
        if !predicate_matches(schema, p, row)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Picks candidate rows using the first predicate that names an indexed
/// column (exact-match predicates preferred over prefix or range predicates,
/// since an eq lookup is a tighter scan); falls back to a full table scan if no
/// predicate is indexed. The chosen predicate (and every other predicate)
/// is still re-applied as an in-Rust filter afterward — this is a
/// deliberately simple "pick one index, then filter" strategy, not a
/// cost-based planner.
fn candidate_rows<B: StorageBackend>(table: &RelTable<'_, B>, predicates: &[Predicate]) -> Result<Vec<Row>, BknError> {
    if let Some(p) = predicates
        .iter()
        .find(|p| matches!(p, Predicate::Eq(col, _) if table.schema.is_indexed(col)))
    {
        let Predicate::Eq(col, val) = p else { unreachable!() };
        return table.index_lookup_eq(col, val);
    }
    if let Some(p) = predicates
        .iter()
        .find(|p| matches!(p, Predicate::Prefix(col, _) if table.schema.is_indexed(col)))
    {
        let Predicate::Prefix(col, prefix) = p else { unreachable!() };
        return table.index_lookup_prefix(col, prefix);
    }
    if let Some(p) = predicates
        .iter()
        .find(|p| matches!(p, Predicate::Range(col, ..) if table.schema.is_indexed(col)))
    {
        let Predicate::Range(col, start, end) = p else { unreachable!() };
        return table.index_lookup_range(col, start, end);
    }
    table.scan_all()
}

pub struct SelectQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    predicates: Vec<Predicate>,
    limit: Option<usize>,
}

impl<'a, B: StorageBackend> SelectQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self {
            table,
            predicates: Vec::new(),
            limit: None,
        }
    }

    pub fn where_eq(mut self, column: &str, value: PropValue) -> Self {
        self.predicates.push(Predicate::Eq(column.to_string(), value));
        self
    }

    /// Prefix predicate on a string column — leverages secondary index range scan
    /// if the column is indexed, otherwise filters during table scan.
    pub fn where_prefix(mut self, column: &str, prefix: &str) -> Self {
        self.predicates.push(Predicate::Prefix(column.to_string(), prefix.to_string()));
        self
    }

    /// Range predicate over an indexed column only — `run()` returns
    /// `BknError::Encoding` if `column` isn't declared in the schema's
    /// `indexed_columns`.
    pub fn where_range(mut self, column: &str, start: Bound<PropValue>, end: Bound<PropValue>) -> Self {
        self.predicates.push(Predicate::Range(column.to_string(), start, end));
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    pub fn run(self) -> Result<Vec<Row>, BknError> {
        for p in &self.predicates {
            if matches!(p, Predicate::Range(..)) && !self.table.schema.is_indexed(p.column()) {
                return Err(BknError::Encoding(format!(
                    "where_range requires an indexed column, '{}' is not indexed",
                    p.column()
                )));
            }
        }
        let candidates = candidate_rows(&self.table, &self.predicates)?;
        let mut out = Vec::new();
        for row in candidates {
            if predicates_match(self.table.schema, &self.predicates, &row)? {
                out.push(row);
                if let Some(n) = self.limit {
                    if out.len() >= n {
                        break;
                    }
                }
            }
        }
        Ok(out)
    }
}

pub struct UpdateQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    predicates: Vec<Predicate>,
    sets: Vec<(String, PropValue)>,
}

impl<'a, B: StorageBackend> UpdateQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self {
            table,
            predicates: Vec::new(),
            sets: Vec::new(),
        }
    }

    pub fn where_eq(mut self, column: &str, value: PropValue) -> Self {
        self.predicates.push(Predicate::Eq(column.to_string(), value));
        self
    }

    pub fn where_prefix(mut self, column: &str, prefix: &str) -> Self {
        self.predicates.push(Predicate::Prefix(column.to_string(), prefix.to_string()));
        self
    }

    pub fn set(mut self, column: &str, value: PropValue) -> Self {
        self.sets.push((column.to_string(), value));
        self
    }

    /// Applies `.set(...)` mutations to every row matching the predicates,
    /// returning how many rows were changed. Matching zero rows is not an
    /// error — it returns `Ok(0)` — since this is a predicate-based bulk
    /// operation, not a point lookup by id.
    pub fn run(self) -> Result<usize, BknError> {
        if let Some((col, _)) = self.sets.iter().find(|(c, _)| c == self.table.schema.primary_key) {
            return Err(BknError::Encoding(format!("cannot set primary key column '{col}' via update")));
        }
        let candidates = candidate_rows(&self.table, &self.predicates)?;
        let mut count = 0;
        for row in candidates {
            if !predicates_match(self.table.schema, &self.predicates, &row)? {
                continue;
            }
            let sets = self.sets.clone();
            let changed = self.table.update_row(&row.pk, |values| {
                for (col, val) in sets {
                    values.insert(col, val);
                }
            })?;
            if changed {
                count += 1;
            }
        }
        Ok(count)
    }
}

pub struct DeleteQuery<'a, B: StorageBackend> {
    table: RelTable<'a, B>,
    predicates: Vec<Predicate>,
}

impl<'a, B: StorageBackend> DeleteQuery<'a, B> {
    pub(crate) fn new(table: RelTable<'a, B>) -> Self {
        Self {
            table,
            predicates: Vec::new(),
        }
    }

    pub fn where_eq(mut self, column: &str, value: PropValue) -> Self {
        self.predicates.push(Predicate::Eq(column.to_string(), value));
        self
    }

    pub fn where_prefix(mut self, column: &str, prefix: &str) -> Self {
        self.predicates.push(Predicate::Prefix(column.to_string(), prefix.to_string()));
        self
    }


    /// Deletes every row matching the predicates, returning how many rows
    /// were removed. Matching zero rows is not an error — see
    /// [`UpdateQuery::run`] for the same reasoning.
    pub fn run(self) -> Result<usize, BknError> {
        let candidates = candidate_rows(&self.table, &self.predicates)?;
        let mut count = 0;
        for row in candidates {
            if !predicates_match(self.table.schema, &self.predicates, &row)? {
                continue;
            }
            if self.table.delete_row(&row.pk)? {
                count += 1;
            }
        }
        Ok(count)
    }
}
