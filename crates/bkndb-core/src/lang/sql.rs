//! A SQL subset over registered relational tables.
//!
//! ```text
//! SELECT * | item [, item ...] FROM table
//!     [WHERE cond] [GROUP BY path, ...] [ORDER BY path [ASC|DESC], ...]
//!     [LIMIT n] [OFFSET n]
//!   item: path [[AS] alias] | COUNT(*) | COUNT(col) | SUM|AVG|MIN|MAX(col) [[AS] alias]
//! INSERT [OR REPLACE] INTO table [(col, ...)] VALUES (v, ...) [, (v, ...)]
//! UPSERT INTO table ...                          -- same as INSERT OR REPLACE
//! UPDATE table SET col = v [, ...] [WHERE cond]
//! DELETE FROM table [WHERE cond]
//! CREATE TABLE [IF NOT EXISTS] table (col TYPE [constraints], ... [, PRIMARY KEY (col)] [, UNIQUE (col)] [, INDEX (col)])
//!   TYPE: INT|INTEGER|BIGINT, FLOAT|REAL|DOUBLE, TEXT|STRING|VARCHAR, BOOL|BOOLEAN,
//!         BYTES|BLOB, TIMESTAMP|DATETIME, UUID, LIST|ARRAY, MAP|JSON
//!   constraints: PRIMARY KEY, AUTOINCREMENT, NOT NULL, UNIQUE, DEFAULT literal
//! DROP TABLE [IF EXISTS] table
//! CREATE INDEX [IF NOT EXISTS] [name] ON table (col)
//! DROP INDEX [IF EXISTS] ON table (col)
//! ALTER TABLE table ADD [COLUMN] col TYPE [constraints]
//! ALTER TABLE table DROP [COLUMN] col
//!
//! cond: cond AND cond | cond OR cond | NOT cond | (cond)
//!     | path (= | == | != | <> | < | <= | > | >=) v   (either side may be the value)
//!     | path [NOT] IN (v, ...) | path IS [NOT] NULL | path [NOT] BETWEEN v AND v
//!     | path [NOT] LIKE v | path [NOT] ILIKE v | path CONTAINS v
//! v: 42, -1.5, 'text', x'00ff', TRUE, FALSE, NULL, TIMESTAMP '2026-01-31T12:00:00Z',
//!    UUID '…', [v, ...], {key: v, ...}, or a parameter: ?, ?N, $N, :name
//! ```
//!
//! A `path` is a column, or a dotted path into a list/map column
//! (`meta.author`, `tags.0`). Comparisons are column-vs-value only (no
//! column-vs-column comparisons, arithmetic or joins). `x = NULL` means
//! `x IS NULL`. ORDER BY in an aggregate query sorts the output by one of
//! its columns (a group column or an alias).
use std::collections::HashMap;

use crate::lang::lexer::{Dialect, Tok};
use crate::lang::{Cursor, Operand, Params, QueryResult};
use crate::relational::catalog;
use crate::relational::db::{insert_bulk_in, OnConflict};
use crate::relational::expr::{resolve, root_column, total_cmp};
use crate::relational::query::{aggregate_in, delete_in, select_in, update_in};
use crate::relational::{Agg, CmpOp, ColumnKind, ColumnSchema, Expr, Order, Query, TableSchema};
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageReadTx, StorageWriteTx};

/// A parsed statement — see the module docs for the grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    Select(Select),
    Insert { table: String, columns: Option<Vec<String>>, rows: Vec<Vec<Operand>>, upsert: bool },
    Update { table: String, sets: Vec<(String, Operand)>, filter: Option<Cond> },
    Delete { table: String, filter: Option<Cond> },
    CreateTable { schema: TableSchema, if_not_exists: bool },
    DropTable { table: String, if_exists: bool },
    CreateIndex { table: String, column: String, if_not_exists: bool },
    DropIndex { table: String, column: String, if_exists: bool },
    AddColumn { table: String, column: ColumnSchema },
    DropColumn { table: String, column: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Select {
    pub table: String,
    pub items: Vec<SelectItem>,
    pub filter: Option<Cond>,
    pub group_by: Vec<String>,
    pub order: Vec<(String, Order)>,
    pub limit: Option<Operand>,
    pub offset: Option<Operand>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    Star,
    Column { path: String, alias: Option<String> },
    Aggregate { func: AggFunc, column: Option<String>, alias: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// A WHERE condition, with values still unbound.
#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Cmp(String, CmpOp, Operand),
    In { path: String, values: Vec<Operand>, negated: bool },
    IsNull { path: String, negated: bool },
    Between { path: String, low: Operand, high: Operand, negated: bool },
    Like { path: String, pattern: Operand, case_insensitive: bool, negated: bool },
    Contains(String, Operand),
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(Box<Cond>),
}

/// What a SQL statement produced — see [`QueryResult`]. INSERT returns the
/// inserted primary keys as a one-column result.
pub type SqlOutput = QueryResult;

impl Statement {
    /// Whether the statement only reads (and can run in a read transaction).
    pub fn is_read_only(&self) -> bool {
        matches!(self, Statement::Select(_))
    }
}

// ============================================================================
// Parsing
// ============================================================================

/// Words that can't be used as unquoted column names where they'd be ambiguous.
const RESERVED: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "GROUP", "ORDER", "BY", "LIMIT", "OFFSET", "AS", "IS", "IN", "BETWEEN",
    "LIKE", "ILIKE", "CONTAINS", "NULL", "TRUE", "FALSE", "SET", "VALUES", "ASC", "DESC", "ON",
];

/// Parses one statement (a trailing `;` is allowed).
pub fn parse(sql: &str) -> Result<Statement, BknError> {
    let mut c = Cursor::new(sql, Dialect::Sql)?;
    let stmt = if c.eat_kw("SELECT") {
        Statement::Select(parse_select(&mut c)?)
    } else if c.is_kw("INSERT") || c.is_kw("UPSERT") {
        parse_insert(&mut c)?
    } else if c.eat_kw("UPDATE") {
        let table = c.ident("a table name", RESERVED)?;
        c.expect_kw("SET")?;
        let mut sets = Vec::new();
        loop {
            let col = c.ident("a column name", RESERVED)?;
            if !c.eat_sym("=") {
                c.expect_sym("==")?;
            }
            sets.push((col, c.operand()?));
            if !c.eat_sym(",") {
                break;
            }
        }
        let filter = parse_where(&mut c)?;
        Statement::Update { table, sets, filter }
    } else if c.eat_kw("DELETE") {
        c.expect_kw("FROM")?;
        let table = c.ident("a table name", RESERVED)?;
        Statement::Delete { table, filter: parse_where(&mut c)? }
    } else if c.eat_kw("CREATE") {
        if c.eat_kw("TABLE") {
            parse_create_table(&mut c)?
        } else if c.eat_kw("INDEX") {
            let if_not_exists = eat_if_exists(&mut c, true)?;
            if !c.is_kw("ON") {
                c.ident("an index name", RESERVED)?; // names are accepted but not stored
            }
            c.expect_kw("ON")?;
            let (table, column) = table_and_column(&mut c)?;
            Statement::CreateIndex { table, column, if_not_exists }
        } else {
            return Err(c.err("expected TABLE or INDEX after CREATE"));
        }
    } else if c.eat_kw("DROP") {
        if c.eat_kw("TABLE") {
            let if_exists = eat_if_exists(&mut c, false)?;
            Statement::DropTable { table: c.ident("a table name", RESERVED)?, if_exists }
        } else if c.eat_kw("INDEX") {
            let if_exists = eat_if_exists(&mut c, false)?;
            c.expect_kw("ON")?;
            let (table, column) = table_and_column(&mut c)?;
            Statement::DropIndex { table, column, if_exists }
        } else {
            return Err(c.err("expected TABLE or INDEX after DROP"));
        }
    } else if c.eat_kw("ALTER") {
        c.expect_kw("TABLE")?;
        let table = c.ident("a table name", RESERVED)?;
        if c.eat_kw("ADD") {
            c.eat_kw("COLUMN");
            let def = parse_column_def(&mut c)?;
            if def.primary_key || def.auto_increment {
                return Err(c.err("ALTER TABLE ... ADD COLUMN cannot add a primary key"));
            }
            Statement::AddColumn { table, column: def.column }
        } else if c.eat_kw("DROP") {
            c.eat_kw("COLUMN");
            Statement::DropColumn { table, column: c.ident("a column name", RESERVED)? }
        } else {
            return Err(c.err("expected ADD or DROP"));
        }
    } else {
        return Err(c.err("expected SELECT, INSERT, UPSERT, UPDATE, DELETE, CREATE, DROP or ALTER"));
    };
    c.finish()?;
    Ok(stmt)
}

fn eat_if_exists(c: &mut Cursor<'_>, not: bool) -> Result<bool, BknError> {
    if !c.eat_kw("IF") {
        return Ok(false);
    }
    if not {
        c.expect_kw("NOT")?;
    }
    c.expect_kw("EXISTS")?;
    Ok(true)
}

fn table_and_column(c: &mut Cursor<'_>) -> Result<(String, String), BknError> {
    let table = c.ident("a table name", RESERVED)?;
    c.expect_sym("(")?;
    let column = c.ident("a column name", RESERVED)?;
    c.expect_sym(")")?;
    Ok((table, column))
}

fn parse_select(c: &mut Cursor<'_>) -> Result<Select, BknError> {
    let mut items = Vec::new();
    loop {
        if c.eat_sym("*") {
            items.push(SelectItem::Star);
        } else if let Some(func) = agg_func_at(c) {
            c.next();
            c.expect_sym("(")?;
            let column = if c.eat_sym("*") {
                if func != AggFunc::Count {
                    return Err(c.err("only COUNT accepts '*'"));
                }
                None
            } else {
                Some(c.ident("a column name", RESERVED)?)
            };
            c.expect_sym(")")?;
            items.push(SelectItem::Aggregate { func, column, alias: parse_alias(c)? });
        } else {
            let path = c.path("a column name", RESERVED)?;
            items.push(SelectItem::Column { path, alias: parse_alias(c)? });
        }
        if !c.eat_sym(",") {
            break;
        }
    }
    c.expect_kw("FROM")?;
    let table = c.ident("a table name", RESERVED)?;
    let filter = parse_where(c)?;
    let mut group_by = Vec::new();
    if c.eat_kw("GROUP") {
        c.expect_kw("BY")?;
        loop {
            group_by.push(c.path("a column name", RESERVED)?);
            if !c.eat_sym(",") {
                break;
            }
        }
    }
    let mut order = Vec::new();
    if c.eat_kw("ORDER") {
        c.expect_kw("BY")?;
        loop {
            let path = c.path("a column name", RESERVED)?;
            let dir = if c.eat_kw("DESC") {
                Order::Desc
            } else {
                c.eat_kw("ASC");
                Order::Asc
            };
            order.push((path, dir));
            if !c.eat_sym(",") {
                break;
            }
        }
    }
    let (mut limit, mut offset) = (None, None);
    loop {
        if limit.is_none() && c.eat_kw("LIMIT") {
            limit = Some(c.operand()?);
        } else if offset.is_none() && c.eat_kw("OFFSET") {
            offset = Some(c.operand()?);
        } else {
            break;
        }
    }
    Ok(Select { table, items, filter, group_by, order, limit, offset })
}

fn agg_func_at(c: &Cursor<'_>) -> Option<AggFunc> {
    let Some(Tok::Ident(w)) = c.peek() else { return None };
    if !matches!(c.peek_at(1), Some(Tok::Sym("("))) {
        return None;
    }
    Some(match w.to_ascii_uppercase().as_str() {
        "COUNT" => AggFunc::Count,
        "SUM" => AggFunc::Sum,
        "AVG" => AggFunc::Avg,
        "MIN" => AggFunc::Min,
        "MAX" => AggFunc::Max,
        _ => return None,
    })
}

fn parse_alias(c: &mut Cursor<'_>) -> Result<Option<String>, BknError> {
    if c.eat_kw("AS") {
        return Ok(Some(c.ident("an alias", &[])?));
    }
    match c.peek() {
        Some(Tok::Ident(w)) if !RESERVED.iter().any(|r| w.eq_ignore_ascii_case(r)) => Ok(Some(c.ident("an alias", &[])?)),
        Some(Tok::QuotedIdent(_)) => Ok(Some(c.ident("an alias", &[])?)),
        _ => Ok(None),
    }
}

fn parse_where(c: &mut Cursor<'_>) -> Result<Option<Cond>, BknError> {
    if c.eat_kw("WHERE") {
        Ok(Some(parse_or(c)?))
    } else {
        Ok(None)
    }
}

fn parse_or(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    let mut parts = vec![parse_and(c)?];
    while c.eat_kw("OR") {
        parts.push(parse_and(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::Or(parts) })
}

fn parse_and(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    let mut parts = vec![parse_not(c)?];
    while c.eat_kw("AND") {
        parts.push(parse_not(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::And(parts) })
}

fn parse_not(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    if c.eat_kw("NOT") {
        return Ok(Cond::Not(Box::new(parse_not(c)?)));
    }
    if c.eat_sym("(") {
        let inner = parse_or(c)?;
        c.expect_sym(")")?;
        return Ok(inner);
    }
    parse_predicate(c)
}

fn cmp_op(c: &mut Cursor<'_>) -> Option<CmpOp> {
    let op = match c.peek() {
        Some(Tok::Sym("=" | "==")) => CmpOp::Eq,
        Some(Tok::Sym("!=" | "<>")) => CmpOp::Ne,
        Some(Tok::Sym("<")) => CmpOp::Lt,
        Some(Tok::Sym("<=")) => CmpOp::Le,
        Some(Tok::Sym(">")) => CmpOp::Gt,
        Some(Tok::Sym(">=")) => CmpOp::Ge,
        _ => return None,
    };
    c.next();
    Some(op)
}

fn flip(op: CmpOp) -> CmpOp {
    match op {
        CmpOp::Lt => CmpOp::Gt,
        CmpOp::Le => CmpOp::Ge,
        CmpOp::Gt => CmpOp::Lt,
        CmpOp::Ge => CmpOp::Le,
        other => other,
    }
}

/// Whether the cursor sits on a value rather than a column name — `TIMESTAMP`
/// and `UUID` count only when a quoted literal follows (so columns can be
/// named that).
fn at_value(c: &Cursor<'_>) -> bool {
    if (c.is_kw("TIMESTAMP") || c.is_kw("UUID")) && !matches!(c.peek_at(1), Some(Tok::Str(_))) {
        return false;
    }
    c.at_operand()
}

fn parse_predicate(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    if at_value(c) {
        let value = c.operand()?;
        let op = cmp_op(c).ok_or_else(|| c.err("expected a comparison operator"))?;
        let path = c.path("a column name", RESERVED)?;
        return Ok(Cond::Cmp(path, flip(op), value));
    }
    let path = c.path("a column name", RESERVED)?;
    if let Some(op) = cmp_op(c) {
        return Ok(Cond::Cmp(path, op, c.operand()?));
    }
    if c.eat_kw("IS") {
        let negated = c.eat_kw("NOT");
        c.expect_kw("NULL")?;
        return Ok(Cond::IsNull { path, negated });
    }
    if c.eat_kw("CONTAINS") {
        return Ok(Cond::Contains(path, c.operand()?));
    }
    let negated = c.eat_kw("NOT");
    if c.eat_kw("IN") {
        c.expect_sym("(")?;
        let mut values = Vec::new();
        if !c.eat_sym(")") {
            loop {
                values.push(c.operand()?);
                if c.eat_sym(")") {
                    break;
                }
                c.expect_sym(",")?;
            }
        }
        return Ok(Cond::In { path, values, negated });
    }
    if c.eat_kw("BETWEEN") {
        let low = c.operand()?;
        c.expect_kw("AND")?;
        let high = c.operand()?;
        return Ok(Cond::Between { path, low, high, negated });
    }
    if c.is_kw("LIKE") || c.is_kw("ILIKE") {
        let case_insensitive = c.is_kw("ILIKE");
        c.next();
        return Ok(Cond::Like { path, pattern: c.operand()?, case_insensitive, negated });
    }
    Err(c.err(if negated {
        "expected IN, BETWEEN, LIKE or ILIKE after NOT"
    } else {
        "expected a comparison, IN, IS NULL, BETWEEN, LIKE, ILIKE or CONTAINS"
    }))
}

fn parse_insert(c: &mut Cursor<'_>) -> Result<Statement, BknError> {
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

struct ColumnDefinition {
    column: ColumnSchema,
    primary_key: bool,
    auto_increment: bool,
}

fn parse_type(c: &mut Cursor<'_>) -> Result<ColumnKind, BknError> {
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

fn parse_column_def(c: &mut Cursor<'_>) -> Result<ColumnDefinition, BknError> {
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

fn parse_create_table(c: &mut Cursor<'_>) -> Result<Statement, BknError> {
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

fn bind_expr(cond: &Cond, params: &Params) -> Result<Expr, BknError> {
    Ok(match cond {
        Cond::Cmp(p, op, v) => Expr::Cmp(p.clone(), *op, v.bind(params)?),
        Cond::In { path, values, negated } => {
            let e = Expr::In(path.clone(), values.iter().map(|v| v.bind(params)).collect::<Result<_, _>>()?);
            if *negated { e.not() } else { e }
        }
        Cond::IsNull { path, negated } => {
            if *negated {
                Expr::IsNotNull(path.clone())
            } else {
                Expr::IsNull(path.clone())
            }
        }
        Cond::Between { path, low, high, negated } => {
            let e = Expr::And(vec![
                Expr::Cmp(path.clone(), CmpOp::Ge, low.bind(params)?),
                Expr::Cmp(path.clone(), CmpOp::Le, high.bind(params)?),
            ]);
            if *negated { e.not() } else { e }
        }
        Cond::Like { path, pattern, case_insensitive, negated } => {
            let PropValue::Str(pattern) = pattern.bind(params)? else {
                return Err(BknError::InvalidQuery(format!("the LIKE pattern for '{path}' must be a string")));
            };
            // `'abc%'` is a plain prefix match, which an index can serve.
            let plain_prefix = pattern.strip_suffix('%').filter(|p| !p.contains(['%', '_']));
            let e = match plain_prefix {
                Some(prefix) if !case_insensitive => Expr::Prefix(path.clone(), prefix.to_string()),
                _ => Expr::Like(path.clone(), pattern, *case_insensitive),
            };
            if *negated { e.not() } else { e }
        }
        Cond::Contains(p, v) => Expr::Contains(p.clone(), v.bind(params)?),
        Cond::And(parts) => Expr::And(parts.iter().map(|c| bind_expr(c, params)).collect::<Result<_, _>>()?),
        Cond::Or(parts) => Expr::Or(parts.iter().map(|c| bind_expr(c, params)).collect::<Result<_, _>>()?),
        Cond::Not(inner) => bind_expr(inner, params)?.not(),
    })
}

fn bind_count(op: &Option<Operand>, params: &Params, what: &str) -> Result<Option<usize>, BknError> {
    match op.as_ref().map(|o| o.bind(params)).transpose()? {
        None => Ok(None),
        Some(PropValue::Int(n)) if n >= 0 => Ok(Some(n as usize)),
        Some(other) => Err(BknError::InvalidQuery(format!("{what} must be a non-negative integer, got {other:?}"))),
    }
}

fn unknown_column(schema: &TableSchema, name: &str) -> BknError {
    BknError::SchemaMismatch { table: schema.name().to_string(), message: format!("unknown column '{name}'") }
}

fn agg_name(func: AggFunc, column: Option<&str>) -> String {
    let f = match func {
        AggFunc::Count => "count",
        AggFunc::Sum => "sum",
        AggFunc::Avg => "avg",
        AggFunc::Min => "min",
        AggFunc::Max => "max",
    };
    format!("{f}({})", column.unwrap_or("*"))
}

/// `column` is `None` only for `COUNT(*)` (the parser guarantees it).
fn to_agg(func: AggFunc, column: Option<&String>) -> Agg {
    let Some(c) = column.cloned() else {
        return Agg::Count;
    };
    match func {
        AggFunc::Count => Agg::CountColumn(c),
        AggFunc::Sum => Agg::Sum(c),
        AggFunc::Avg => Agg::Avg(c),
        AggFunc::Min => Agg::Min(c),
        AggFunc::Max => Agg::Max(c),
    }
}

fn execute_select<R: StorageReadTx>(rtx: &R, sel: &Select, params: &Params) -> Result<SqlOutput, BknError> {
    let schema = catalog::require_schema_in(rtx, &sel.table)?;
    let mut query = Query::new();
    if let Some(f) = &sel.filter {
        query = query.filter(bind_expr(f, params)?);
    }
    let limit = bind_count(&sel.limit, params, "LIMIT")?;
    let offset = bind_count(&sel.offset, params, "OFFSET")?.unwrap_or(0);

    let aggregating = !sel.group_by.is_empty() || sel.items.iter().any(|i| matches!(i, SelectItem::Aggregate { .. }));
    if !aggregating {
        query.order = sel.order.clone();
        query.offset = offset;
        query.limit = limit;
        // Output columns and how to read each from a row.
        let mut columns = Vec::new();
        let mut paths = Vec::new();
        for item in &sel.items {
            match item {
                SelectItem::Star => {
                    columns.push(schema.primary_key().to_string());
                    paths.push(schema.primary_key().to_string());
                    for col in schema.columns().iter().filter(|c| c.name != schema.primary_key()) {
                        columns.push(col.name.clone());
                        paths.push(col.name.clone());
                    }
                }
                SelectItem::Column { path, alias } => {
                    if root_column(&schema, path).is_none() {
                        return Err(unknown_column(&schema, path));
                    }
                    columns.push(alias.clone().unwrap_or_else(|| path.clone()));
                    paths.push(path.clone());
                }
                SelectItem::Aggregate { .. } => unreachable!(),
            }
        }
        let rows = select_in(rtx, &schema, &query)?
            .into_iter()
            .map(|row| paths.iter().map(|p| resolve(&schema, &row, p).cloned().unwrap_or(PropValue::Null)).collect())
            .collect();
        return Ok(SqlOutput { columns, rows, affected: 0 });
    }

    // Aggregate query.
    let mut aggs = Vec::new();
    let mut columns = Vec::new();
    // For each output column: Ok(index into group) or Err(index into aggs).
    let mut sources: Vec<Result<usize, usize>> = Vec::new();
    for item in &sel.items {
        match item {
            SelectItem::Star => return Err(BknError::InvalidQuery("SELECT * can't be combined with GROUP BY or aggregates".into())),
            SelectItem::Column { path, alias } => {
                let idx = sel.group_by.iter().position(|g| g == path).ok_or_else(|| {
                    BknError::InvalidQuery(format!("column '{path}' must appear in GROUP BY or be inside an aggregate"))
                })?;
                columns.push(alias.clone().unwrap_or_else(|| path.clone()));
                sources.push(Ok(idx));
            }
            SelectItem::Aggregate { func, column, alias } => {
                if column.is_none() && *func != AggFunc::Count {
                    return Err(BknError::InvalidQuery("only COUNT accepts '*'".into()));
                }
                columns.push(alias.clone().unwrap_or_else(|| agg_name(*func, column.as_deref())));
                sources.push(Err(aggs.len()));
                aggs.push(to_agg(*func, column.as_ref()));
            }
        }
    }
    for g in &sel.group_by {
        if root_column(&schema, g).is_none() {
            return Err(unknown_column(&schema, g));
        }
    }
    let grouped = aggregate_in(rtx, &schema, &query, &sel.group_by, &aggs)?;
    let mut rows: Vec<Vec<PropValue>> = grouped
        .into_iter()
        .map(|g| {
            sources
                .iter()
                .map(|s| match s {
                    Ok(i) => g.group[*i].clone(),
                    Err(i) => g.values[*i].clone(),
                })
                .collect()
        })
        .collect();
    if !sel.order.is_empty() {
        let mut keys = Vec::new();
        for (name, dir) in &sel.order {
            let idx = columns
                .iter()
                .position(|c| c == name)
                .or_else(|| {
                    sel.items.iter().position(|i| matches!(i, SelectItem::Column { path, .. } if path == name))
                })
                .ok_or_else(|| BknError::InvalidQuery(format!("ORDER BY '{name}' must name an output column of the aggregate query")))?;
            keys.push((idx, *dir));
        }
        rows.sort_by(|a, b| {
            keys.iter()
                .map(|(i, dir)| {
                    let o = total_cmp(Some(&a[*i]), Some(&b[*i]));
                    if *dir == Order::Desc { o.reverse() } else { o }
                })
                .find(|o| o.is_ne())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
    let rows: Vec<_> = rows.into_iter().skip(offset).take(limit.unwrap_or(usize::MAX)).collect();
    Ok(SqlOutput { columns, rows, affected: 0 })
}

/// Runs a read-only statement in a read transaction.
pub(crate) fn execute_read<R: StorageReadTx>(rtx: &R, stmt: &Statement, params: &Params) -> Result<SqlOutput, BknError> {
    match stmt {
        Statement::Select(sel) => execute_select(rtx, sel, params),
        _ => Err(BknError::InvalidQuery("this statement modifies data; run it in a write transaction".into())),
    }
}

/// Runs any statement in a write transaction.
pub(crate) fn execute<W: StorageWriteTx>(wtx: &mut W, stmt: &Statement, params: &Params) -> Result<SqlOutput, BknError> {
    let affected = |n: usize| Ok(SqlOutput { affected: n as u64, ..SqlOutput::default() });
    match stmt {
        Statement::Select(sel) => execute_select(&*wtx, sel, params),
        Statement::Insert { table, columns, rows, upsert } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            let names: Vec<String> = match columns {
                Some(cols) => cols.clone(),
                None => schema.columns().iter().map(|c| c.name.clone()).collect(),
            };
            let mut seen = HashMap::new();
            for n in &names {
                if seen.insert(n.as_str(), ()).is_some() {
                    return Err(BknError::InvalidQuery(format!("column '{n}' is listed twice")));
                }
            }
            let mut batch = Vec::with_capacity(rows.len());
            for (i, row) in rows.iter().enumerate() {
                if row.len() != names.len() {
                    return Err(BknError::InvalidQuery(format!(
                        "row {} has {} values for {} columns",
                        i + 1,
                        row.len(),
                        names.len()
                    )));
                }
                let mut props = Properties::new();
                for (name, op) in names.iter().zip(row) {
                    props.insert(name.clone(), op.bind(params)?);
                }
                batch.push(props);
            }
            let on_conflict = if *upsert { OnConflict::Replace } else { OnConflict::Error };
            let pks = insert_bulk_in(wtx, &schema, batch, on_conflict)?;
            Ok(SqlOutput {
                columns: vec![schema.primary_key().to_string()],
                affected: pks.len() as u64,
                rows: pks.into_iter().map(|pk| vec![pk]).collect(),
            })
        }
        Statement::Update { table, sets, filter } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            let mut query = Query::new();
            if let Some(f) = filter {
                query = query.filter(bind_expr(f, params)?);
            }
            let sets: Vec<(String, PropValue)> =
                sets.iter().map(|(c, v)| Ok((c.clone(), v.bind(params)?))).collect::<Result<_, BknError>>()?;
            if let Some((c, _)) = sets.iter().find(|(c, _)| schema.column(c).is_none()) {
                return Err(unknown_column(&schema, c));
            }
            affected(update_in(wtx, &schema, &query, &sets)?)
        }
        Statement::Delete { table, filter } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            let mut query = Query::new();
            if let Some(f) = filter {
                query = query.filter(bind_expr(f, params)?);
            }
            affected(delete_in(wtx, &schema, &query)?)
        }
        Statement::CreateTable { schema, if_not_exists } => {
            if catalog::load_schema_in(&*wtx, schema.name())?.is_some() {
                return if *if_not_exists {
                    affected(0)
                } else {
                    Err(BknError::InvalidQuery(format!("table '{}' already exists", schema.name())))
                };
            }
            catalog::create_table_in(wtx, schema)?;
            affected(0)
        }
        Statement::DropTable { table, if_exists } => {
            if !catalog::drop_table_in(wtx, table)? && !if_exists {
                return Err(BknError::TableNotFound(table.clone()));
            }
            affected(0)
        }
        Statement::CreateIndex { table, column, if_not_exists } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            if schema.is_indexed(column) && !if_not_exists {
                return Err(BknError::InvalidQuery(format!("column '{column}' of '{table}' is already indexed")));
            }
            catalog::ensure_table_in(wtx, &schema.to_builder().index(column.clone()).build()?)?;
            affected(0)
        }
        Statement::DropIndex { table, column, if_exists } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            if !schema.is_indexed(column) {
                return if *if_exists {
                    affected(0)
                } else {
                    Err(BknError::InvalidQuery(format!("column '{column}' of '{table}' is not indexed")))
                };
            }
            catalog::ensure_table_in(wtx, &schema.to_builder().drop_index(column).build()?)?;
            affected(0)
        }
        Statement::AddColumn { table, column } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            if schema.column(&column.name).is_some() {
                return Err(BknError::InvalidQuery(format!("column '{}' already exists in '{table}'", column.name)));
            }
            catalog::ensure_table_in(wtx, &schema.to_builder().column(column.clone()).build()?)?;
            affected(0)
        }
        Statement::DropColumn { table, column } => {
            let schema = catalog::require_schema_in(&*wtx, table)?;
            if schema.column(column).is_none() {
                return Err(unknown_column(&schema, column));
            }
            if column == schema.primary_key() {
                return Err(BknError::InvalidQuery("cannot drop the primary key column".into()));
            }
            catalog::ensure_table_in(wtx, &schema.to_builder().drop_column(column).build()?)?;
            affected(0)
        }
    }
}
