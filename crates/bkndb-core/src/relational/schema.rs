/// Column value kind. Mirrors `crate::value::PropValue`'s variants 1:1 —
/// declared separately (rather than reusing `PropValue` itself as the type
/// tag) because a schema needs to name a *kind* independent of any concrete
/// value, e.g. to validate an inserted row's columns against the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnKind {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
}

#[derive(Debug, Clone, Copy)]
pub struct ColumnDef {
    pub name: &'static str,
    pub kind: ColumnKind,
}

/// Schema for a relational table. Deliberately distinct from the existing
/// `TableSpec` (a physical KV table name) and from a graph node's `label`
/// (a free-form string, no fixed columns) — a `RelSchema` describes a typed,
/// row-shaped table on top of the same KV substrate those use.
///
/// Every field is `&'static`: in practice a `RelSchema` is always a
/// source-level constant (e.g. `pub static FILES_SCHEMA: RelSchema = ...`),
/// never built from runtime/user input, so this is not a real limitation.
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
