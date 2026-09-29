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
