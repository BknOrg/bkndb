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

mod parse;
mod parse_ddl;
mod exec;

pub use parse::*;
use parse_ddl::*;
pub(crate) use exec::*;

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
