//! Parser for INSERT and CREATE TABLE (column types and definitions).
use super::*;

pub(super) fn parse_insert(c: &mut Cursor<'_>) -> Result<Statement, BknError> {
    let upsert = if c.eat_kw("UPSERT") {
        true
    } else {
        c.expect_kw("INSERT")?;
        if c.eat_kw("OR") {
            c.expect_kw("REPLACE")?;
            true
        } else {
            false
        }
    };
    c.expect_kw("INTO")?;
    let table = c.ident("a table name", RESERVED)?;
    let columns = if c.eat_sym("(") {
        let mut cols = Vec::new();
        loop {
            cols.push(c.ident("a column name", RESERVED)?);
            if c.eat_sym(")") {
                break;
            }
            c.expect_sym(",")?;
        }
        Some(cols)
    } else {
        None
    };
    c.expect_kw("VALUES")?;
    let mut rows = Vec::new();
    loop {
        c.expect_sym("(")?;
        let mut row = Vec::new();
        loop {
            row.push(c.operand()?);
            if c.eat_sym(")") {
                break;
            }
            c.expect_sym(",")?;
        }
        rows.push(row);
        if !c.eat_sym(",") {
            break;
        }
    }
    Ok(Statement::Insert { table, columns, rows, upsert })
}

pub(super) struct ColumnDefinition {
    pub(super) column: ColumnSchema,
    pub(super) primary_key: bool,
    pub(super) auto_increment: bool,
}

pub(super) fn parse_type(c: &mut Cursor<'_>) -> Result<ColumnKind, BknError> {
    let name = c.ident("a column type", &[])?;
    let kind = match name.to_ascii_uppercase().as_str() {
        "INT" | "INTEGER" | "BIGINT" | "SMALLINT" => ColumnKind::Int,
        "FLOAT" | "REAL" | "DOUBLE" | "NUMERIC" | "DECIMAL" => ColumnKind::Float,
        "TEXT" | "STRING" | "VARCHAR" | "CHAR" => ColumnKind::Str,
        "BOOL" | "BOOLEAN" => ColumnKind::Bool,
        "BYTES" | "BLOB" | "BYTEA" => ColumnKind::Bytes,
        "TIMESTAMP" | "DATETIME" | "TIMESTAMPTZ" => ColumnKind::Timestamp,
        "UUID" => ColumnKind::Uuid,
        "LIST" | "ARRAY" => ColumnKind::List,
        "MAP" | "JSON" | "JSONB" => ColumnKind::Map,
        other => return Err(c.err(format!("unknown column type {other}"))),
    };
    // Size arguments (`VARCHAR(255)`, `DECIMAL(10, 2)`) are accepted and ignored.
    if c.eat_sym("(") {
        c.usize_lit("a size")?;
        if c.eat_sym(",") {
            c.usize_lit("a size")?;
        }
        c.expect_sym(")")?;
    }
    Ok(kind)
}

pub(super) fn parse_column_def(c: &mut Cursor<'_>) -> Result<ColumnDefinition, BknError> {
    let name = c.ident("a column name", RESERVED)?;
    let kind = parse_type(c)?;
    let mut def = ColumnDefinition { column: ColumnSchema::new(name, kind), primary_key: false, auto_increment: false };
    loop {
        if c.eat_kw("PRIMARY") {
            c.expect_kw("KEY")?;
            def.primary_key = true;
        } else if c.eat_kw("AUTOINCREMENT") || c.eat_kw("AUTO_INCREMENT") {
            def.auto_increment = true;
        } else if c.eat_kw("NOT") {
            c.expect_kw("NULL")?;
            def.column = def.column.not_null();
        } else if c.eat_kw("NULL") {
        } else if c.eat_kw("UNIQUE") {
            def.column = def.column.unique();
        } else if c.eat_kw("DEFAULT") {
            let value = match c.operand()? {
                Operand::Param(_) => return Err(c.err("DEFAULT must be a literal, not a parameter")),
                op => op.bind(&Params::none())?,
            };
            def.column = def.column.default_value(value);
        } else {
            break;
        }
    }
    Ok(def)
}

pub(super) fn parse_create_table(c: &mut Cursor<'_>) -> Result<Statement, BknError> {
    let if_not_exists = eat_if_exists(c, true)?;
    let name = c.ident("a table name", RESERVED)?;
    c.expect_sym("(")?;
    let mut columns: Vec<ColumnSchema> = Vec::new();
    let mut primary_key: Option<String> = None;
    let mut auto_increment = false;
    let mut indexes = Vec::new();
    let mut uniques = Vec::new();
    loop {
        let table_constraint = |c: &mut Cursor<'_>| -> Result<String, BknError> {
            c.expect_sym("(")?;
            let col = c.ident("a column name", RESERVED)?;
            c.expect_sym(")")?;
            Ok(col)
        };
        if c.is_kw("PRIMARY") && c.is_kw_at(1, "KEY") && matches!(c.peek_at(2), Some(Tok::Sym("("))) {
            c.next();
            c.next();
            let col = table_constraint(c)?;
            if primary_key.replace(col).is_some() {
                return Err(c.err("more than one primary key"));
            }
        } else if c.is_kw("UNIQUE") && matches!(c.peek_at(1), Some(Tok::Sym("("))) {
            c.next();
            uniques.push(table_constraint(c)?);
        } else if c.is_kw("INDEX") && matches!(c.peek_at(1), Some(Tok::Sym("("))) {
            c.next();
            indexes.push(table_constraint(c)?);
        } else {
            let def = parse_column_def(c)?;
            if def.primary_key && primary_key.replace(def.column.name.clone()).is_some() {
                return Err(c.err("more than one primary key"));
            }
            auto_increment |= def.auto_increment;
            columns.push(def.column);
        }
        if c.eat_sym(")") {
            break;
        }
        c.expect_sym(",")?;
    }
    let pk = primary_key.ok_or_else(|| c.err("CREATE TABLE needs a PRIMARY KEY column"))?;
    for name in uniques.iter().chain(&indexes).chain(std::iter::once(&pk)) {
        if !columns.iter().any(|col| &col.name == name) {
            return Err(BknError::InvalidQuery(format!("constraint names unknown column '{name}'")));
        }
    }
    let mut builder = TableSchema::builder(name).primary_key(pk);
    for col in columns {
        let unique = uniques.contains(&col.name);
        builder = builder.column(if unique { col.unique() } else { col });
    }
    if auto_increment {
        builder = builder.auto_increment();
    }
    for col in indexes {
        builder = builder.index(col);
    }
    Ok(Statement::CreateTable { schema: builder.build()?, if_not_exists })
}

// ============================================================================
// Execution
// ============================================================================
