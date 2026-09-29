//! [`TableSchemaBuilder`](super::TableSchemaBuilder) and identifier validation.
use super::*;

/// Longest table or column name accepted by [`TableSchemaBuilder::build`],
/// chosen so a derived index table name (`<table>__idx_<column>`) always
/// fits the storage layer's 255-byte table-name limit.
pub const MAX_IDENTIFIER_LEN: usize = 100;

pub(super) fn check_identifier(what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_IDENTIFIER_LEN {
        return Err(format!("{what} name must be 1..={MAX_IDENTIFIER_LEN} bytes, got '{name}'"));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!("{what} name '{name}' may only contain ASCII letters, digits and '_'"));
    }
    if name.contains("__") {
        // Reserves `__` for derived physical names like `<t>__idx_<c>`, so a
        // user table can never alias another table's index.
        return Err(format!("{what} name '{name}' may not contain '__'"));
    }
    Ok(())
}

/// Builder for a validated [`TableSchema`].
#[derive(Debug, Clone)]
pub struct TableSchemaBuilder {
    pub(super) name: String,
    pub(super) columns: Vec<ColumnSchema>,
    pub(super) primary_key: Option<String>,
    pub(super) auto_increment_pk: bool,
    pub(super) indexed_columns: Vec<String>,
}

impl TableSchemaBuilder {
    /// Adds a column, replacing any existing column with the same name.
    pub fn column(mut self, column: ColumnSchema) -> Self {
        self.columns.retain(|c| c.name != column.name);
        self.columns.push(column);
        self
    }

    pub fn drop_column(mut self, name: &str) -> Self {
        self.columns.retain(|c| c.name != name);
        self.indexed_columns.retain(|c| c != name);
        self
    }

    pub fn primary_key(mut self, column: impl Into<String>) -> Self {
        self.primary_key = Some(column.into());
        self
    }

    pub fn auto_increment(mut self) -> Self {
        self.auto_increment_pk = true;
        self
    }

    pub fn index(mut self, column: impl Into<String>) -> Self {
        let column = column.into();
        if !self.indexed_columns.contains(&column) {
            self.indexed_columns.push(column);
        }
        self
    }

    pub fn drop_index(mut self, column: &str) -> Self {
        self.indexed_columns.retain(|c| c != column);
        for c in &mut self.columns {
            if c.name == column {
                c.unique = false;
            }
        }
        self
    }

    pub fn build(self) -> Result<TableSchema, BknError> {
        let table = self.name.clone();
        let err = |message: String| BknError::SchemaMismatch { table: table.clone(), message };

        check_identifier("table", &self.name).map_err(err)?;
        if crate::is_reserved(&self.name) {
            return Err(BknError::ReservedTableName(self.name));
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.columns {
            check_identifier("column", &c.name).map_err(err)?;
            if !seen.insert(c.name.as_str()) {
                return Err(err(format!("duplicate column '{}'", c.name)));
            }
            if c.kind == ColumnKind::Null {
                return Err(err(format!("column '{}' cannot have kind Null", c.name)));
            }
            if c.unique && !c.kind.is_keyable() {
                return Err(err(format!("UNIQUE column '{}' must be Int or Str, got {:?}", c.name, c.kind)));
            }
            match &c.default {
                Some(PropValue::Null) if !c.nullable => {
                    return Err(err(format!("NOT NULL column '{}' cannot default to null", c.name)));
                }
                Some(v) if !matches!(v, PropValue::Null) && ColumnKind::of(v) != c.kind => {
                    return Err(err(format!("default for column '{}' must be {:?}, got {:?}", c.name, c.kind, ColumnKind::of(v))));
                }
                _ => {}
            }
        }

        let pk = self.primary_key.ok_or_else(|| err("no primary key declared".to_string()))?;
        let mut columns = self.columns;
        let pk_col = columns
            .iter_mut()
            .find(|c| c.name == pk)
            .ok_or_else(|| err(format!("primary key '{pk}' is not a declared column")))?;
        if !pk_col.kind.is_keyable() {
            return Err(err(format!("primary key '{pk}' must be Int or Str, got {:?}", pk_col.kind)));
        }
        if self.auto_increment_pk && pk_col.kind != ColumnKind::Int {
            return Err(err(format!("auto-increment primary key '{pk}' must be Int")));
        }
        // The pk is inherently unique and non-null, and lives in the row key
        // rather than an index.
        pk_col.nullable = false;
        pk_col.unique = false;
        pk_col.default = None;

        let mut indexed: Vec<String> = Vec::new();
        for name in self.indexed_columns.iter().chain(columns.iter().filter(|c| c.unique).map(|c| &c.name)) {
            if *name == pk || indexed.contains(name) {
                continue;
            }
            let col = columns
                .iter()
                .find(|c| &c.name == name)
                .ok_or_else(|| err(format!("indexed column '{name}' is not a declared column")))?;
            if !col.kind.is_keyable() {
                return Err(err(format!("indexed column '{name}' must be Int or Str, got {:?}", col.kind)));
            }
            indexed.push(name.clone());
        }

        Ok(TableSchema::from_def(TableDef {
            name: self.name,
            columns,
            primary_key: pk,
            auto_increment_pk: self.auto_increment_pk,
            indexed_columns: indexed,
        }))
    }
}
