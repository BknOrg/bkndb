use std::sync::Arc;

use crate::value::PropValue;
use crate::{BknError, TableSpec};

/// Column value kind. Mirrors `crate::value::PropValue`'s variants 1:1 —
/// declared separately (rather than reusing `PropValue` itself as the type
/// tag) because a schema needs to name a *kind* independent of any concrete
/// value, e.g. to validate an inserted row's columns against the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ColumnKind {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    // Appended (catalog entries are serialized by variant index).
    Timestamp,
    Uuid,
    List,
    Map,
}

impl ColumnKind {
    /// The kind of a concrete value.
    pub fn of(value: &PropValue) -> ColumnKind {
        match value {
            PropValue::Null => ColumnKind::Null,
            PropValue::Bool(_) => ColumnKind::Bool,
            PropValue::Int(_) => ColumnKind::Int,
            PropValue::Float(_) => ColumnKind::Float,
            PropValue::Str(_) => ColumnKind::Str,
            PropValue::Bytes(_) => ColumnKind::Bytes,
            PropValue::Timestamp(_) => ColumnKind::Timestamp,
            PropValue::Uuid(_) => ColumnKind::Uuid,
            PropValue::List(_) => ColumnKind::List,
            PropValue::Map(_) => ColumnKind::Map,
        }
    }

    /// Whether values of this kind can be a primary key or be indexed
    /// (only these have a sortable byte encoding).
    pub fn is_keyable(self) -> bool {
        matches!(self, ColumnKind::Int | ColumnKind::Str | ColumnKind::Timestamp | ColumnKind::Uuid)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnDef {
    pub name: &'static str,
    pub kind: ColumnKind,
}

/// Compile-time schema for a relational table, written as a `static`:
///
/// ```ignore
/// static USERS: RelSchema = RelSchema { name: "users", columns: &[..], .. };
/// ```
///
/// Every API that takes a schema accepts `&RelSchema` and converts it into a
/// [`TableSchema`], the owned runtime form that additionally supports
/// constraints (NOT NULL, UNIQUE, DEFAULT) and can be stored in the catalog.
/// A `RelSchema`'s columns are all nullable, non-unique and default-less.
#[derive(Debug, Clone, Copy)]
pub struct RelSchema {
    pub name: &'static str,
    pub columns: &'static [ColumnDef],
    pub primary_key: &'static str,
    pub auto_increment_pk: bool,
    pub indexed_columns: &'static [&'static str],
}

impl RelSchema {
    pub fn column(&self, name: &str) -> Option<&'static ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn primary_key_column(&self) -> &'static ColumnDef {
        self.column(self.primary_key)
            .expect("RelSchema.primary_key must name one of RelSchema.columns")
    }

    pub fn is_indexed(&self, column: &str) -> bool {
        self.indexed_columns.contains(&column)
    }
}

/// Anything that can tell a [`crate::relational::Row`] which column is its
/// primary key — implemented by both schema forms so `Row::get` accepts
/// either.
pub trait HasPrimaryKey {
    fn primary_key_name(&self) -> &str;
}

impl HasPrimaryKey for RelSchema {
    fn primary_key_name(&self) -> &str {
        self.primary_key
    }
}

impl HasPrimaryKey for TableSchema {
    fn primary_key_name(&self) -> &str {
        self.primary_key()
    }
}

/// One column of a [`TableSchema`], with its constraints.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    pub kind: ColumnKind,
    /// `false` = NOT NULL: the column must be present with a non-null value.
    pub nullable: bool,
    /// UNIQUE: no two rows may share a non-null value. Implies an index.
    pub unique: bool,
    /// Filled in on insert/upsert when the column is absent.
    pub default: Option<PropValue>,
}

impl ColumnSchema {
    /// A nullable, non-unique column with no default.
    pub fn new(name: impl Into<String>, kind: ColumnKind) -> Self {
        Self {
            name: name.into(),
            kind,
            nullable: true,
            unique: false,
            default: None,
        }
    }

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    pub fn default_value(mut self, value: impl Into<PropValue>) -> Self {
        self.default = Some(value.into());
        self
    }
}

/// The serialized, catalog-stored part of a [`TableSchema`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct TableDef {
    pub name: String,
    pub columns: Vec<ColumnSchema>,
    pub primary_key: String,
    pub auto_increment_pk: bool,
    /// Every secondary-indexed column, including UNIQUE ones; never the pk.
    pub indexed_columns: Vec<String>,
}

/// Owned runtime schema for a relational table: built with
/// [`TableSchema::builder`], loaded from the catalog, or converted from a
/// static [`RelSchema`]. Cloning is cheap (one `Arc` bump).
#[derive(Clone)]
pub struct TableSchema {
    def: Arc<TableDef>,
    base: TableSpec,
}

impl std::fmt::Debug for TableSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.def.fmt(f)
    }
}

impl PartialEq for TableSchema {
    fn eq(&self, other: &Self) -> bool {
        self.def == other.def
    }
}

impl TableSchema {
    pub fn builder(name: impl Into<String>) -> TableSchemaBuilder {
        TableSchemaBuilder {
            name: name.into(),
            columns: Vec::new(),
            primary_key: None,
            auto_increment_pk: false,
            indexed_columns: Vec::new(),
        }
    }

    /// Starts a builder pre-filled with this schema — the way to derive a
    /// modified schema (e.g. with one more column) for
    /// [`crate::relational::RelationalDb::ensure_table`].
    pub fn to_builder(&self) -> TableSchemaBuilder {
        TableSchemaBuilder {
            name: self.def.name.clone(),
            columns: self.def.columns.clone(),
            primary_key: Some(self.def.primary_key.clone()),
            auto_increment_pk: self.def.auto_increment_pk,
            indexed_columns: self.def.indexed_columns.clone(),
        }
    }

    pub(crate) fn from_def(def: TableDef) -> Self {
        let base = TableSpec(crate::relational::codec::intern(&def.name));
        Self { def: Arc::new(def), base }
    }

    pub(crate) fn def(&self) -> &TableDef {
        &self.def
    }

    pub fn name(&self) -> &str {
        &self.def.name
    }

    pub fn columns(&self) -> &[ColumnSchema] {
        &self.def.columns
    }

    pub fn column(&self, name: &str) -> Option<&ColumnSchema> {
        self.def.columns.iter().find(|c| c.name == name)
    }

    pub fn primary_key(&self) -> &str {
        &self.def.primary_key
    }

    pub fn primary_key_column(&self) -> &ColumnSchema {
        self.column(&self.def.primary_key)
            .expect("TableSchema.primary_key must name one of its columns")
    }

    pub fn auto_increment_pk(&self) -> bool {
        self.def.auto_increment_pk
    }

    pub fn indexed_columns(&self) -> &[String] {
        &self.def.indexed_columns
    }

    pub fn is_indexed(&self, column: &str) -> bool {
        self.def.indexed_columns.iter().any(|c| c == column)
    }

    /// The physical KV table holding this table's rows.
    pub(crate) fn base_table(&self) -> TableSpec {
        self.base
    }
}

impl From<&RelSchema> for TableSchema {
    fn from(s: &RelSchema) -> Self {
        let def = TableDef {
            name: s.name.to_string(),
            columns: s.columns.iter().map(|c| ColumnSchema::new(c.name, c.kind)).collect(),
            primary_key: s.primary_key.to_string(),
            auto_increment_pk: s.auto_increment_pk,
            indexed_columns: s.indexed_columns.iter().map(|c| c.to_string()).collect(),
        };
        // `s.name` is already `'static`: no interning needed.
        Self {
            def: Arc::new(def),
            base: TableSpec(s.name),
        }
    }
}

impl From<&TableSchema> for TableSchema {
    fn from(s: &TableSchema) -> Self {
        s.clone()
    }
}

/// Longest table or column name accepted by [`TableSchemaBuilder::build`],
/// chosen so a derived index table name (`<table>__idx_<column>`) always
/// fits the storage layer's 255-byte table-name limit.
pub const MAX_IDENTIFIER_LEN: usize = 100;

fn check_identifier(what: &str, name: &str) -> Result<(), String> {
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
    name: String,
    columns: Vec<ColumnSchema>,
    primary_key: Option<String>,
    auto_increment_pk: bool,
    indexed_columns: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> TableSchemaBuilder {
        TableSchema::builder("users")
            .column(ColumnSchema::new("id", ColumnKind::Int))
            .column(ColumnSchema::new("email", ColumnKind::Str).not_null().unique())
            .column(ColumnSchema::new("age", ColumnKind::Int).default_value(0))
            .primary_key("id")
            .auto_increment()
    }

    #[test]
    fn builder_indexes_unique_columns_and_normalizes_the_pk() {
        let s = users().index("id").build().unwrap();
        assert_eq!(s.indexed_columns(), &["email".to_string()]);
        assert!(!s.primary_key_column().nullable);
        assert_eq!(s.base_table().0, "users");
    }

    #[test]
    fn builder_rejects_invalid_definitions() {
        assert!(TableSchema::builder("bad name").column(ColumnSchema::new("id", ColumnKind::Int)).primary_key("id").build().is_err());
        assert!(TableSchema::builder("a__b").column(ColumnSchema::new("id", ColumnKind::Int)).primary_key("id").build().is_err());
        assert!(matches!(
            TableSchema::builder("meta").column(ColumnSchema::new("id", ColumnKind::Int)).primary_key("id").build(),
            Err(BknError::ReservedTableName(_))
        ));
        assert!(users().primary_key("nope").build().is_err());
        assert!(users().index("missing").build().is_err());
        assert!(users().column(ColumnSchema::new("f", ColumnKind::Float)).index("f").build().is_err());
        assert!(users().column(ColumnSchema::new("n", ColumnKind::Int).default_value("x")).build().is_err());
        assert!(TableSchema::builder("t")
            .column(ColumnSchema::new("id", ColumnKind::Str))
            .primary_key("id")
            .auto_increment()
            .build()
            .is_err());
    }

    #[test]
    fn static_schema_converts_losslessly() {
        static S: RelSchema = RelSchema {
            name: "files",
            columns: &[ColumnDef { name: "id", kind: ColumnKind::Int }, ColumnDef { name: "path", kind: ColumnKind::Str }],
            primary_key: "id",
            auto_increment_pk: true,
            indexed_columns: &["path"],
        };
        let t = TableSchema::from(&S);
        assert_eq!(t.name(), "files");
        assert!(t.is_indexed("path"));
        assert!(t.column("path").unwrap().nullable);
        assert_eq!(t.base_table().0, "files");
    }
}
