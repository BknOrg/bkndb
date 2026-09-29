//! Graph pattern queries — a read-only subset of Cypher:
//!
//! ```text
//! MATCH (a:Person {name: $name})-[r:KNOWS|LIKES*1..3]->(b)<-[:WORKS_AT]-(c:Company)
//! WHERE b.age >= 30 AND NOT c.name STARTS WITH 'Acme'
//! RETURN [DISTINCT] b.name AS friend, c, count(*) AS n
//! ORDER BY n DESC, friend
//! SKIP 0 LIMIT 10
//! ```
//!
//! - **Pattern**: one path of node patterns `(var:Label {prop: value})`
//!   joined by relationships `-[var:TYPE|TYPE2 {prop: value}]->`, `<-[...]-`
//!   or undirected `-[...]-` (also `-->`, `<--`, `--`). Every part is
//!   optional. `*`, `*n`, `*min..max`, `*..max` make a relationship
//!   variable-length (default bounds 1..[`MAX_HOPS`]); its variable is
//!   then bound to the list of edges. A variable repeated in the pattern
//!   must bind the same node (`(a)-->(b)-->(a)` finds 2-cycles). Within one
//!   match, no edge is used twice.
//! - **WHERE**: `expr op expr` for `= <> != < <= > >=`, `IN [...]`,
//!   `IS [NOT] NULL`, `[NOT] LIKE/ILIKE`, `CONTAINS`, `STARTS WITH`,
//!   `ENDS WITH`, `var:Label`, combined with `AND`/`OR`/`NOT`/parentheses.
//! - **Expressions**: `var`, `var.prop[.key...]`, `id(var)`, `label(var)`
//!   (alias `labels`), `type(rel)`, literals and parameters (`$name`, `$1`,
//!   `?`).
//! - **RETURN**: expressions and aggregates `count(*)`, `count(e)`,
//!   `sum(e)`, `avg(e)`, `min(e)`, `max(e)`, `collect(e)`; non-aggregate
//!   items are the grouping keys. `RETURN *` returns every named variable.
//!   A node is returned as `{id, label, properties}`, an edge as
//!   `{id, type, from, to, properties}`.
//!
//! The start node is chosen automatically: a node pinned by `id(v) = value`,
//! else one with a label and an equality on an indexed property (see
//! [`crate::graph::GraphDb::create_property_index`]), else a labelled node
//! (label index), else a full node scan. The path is then expanded from it
//! in both directions along adjacency lists.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::graph::codec::{ADJ_IN, ADJ_OUT};
use crate::graph::db::{get_edge_in, get_node_in, neighbors_any_in, neighbors_in_in, neighbors_out_in};
use crate::graph::index::{find_nodes_in, nodes_by_label_in, property_indexes_in, scan_all_nodes};
use crate::graph::{EdgeId, EdgeRecord, NodeId, NodeRecord};
use crate::lang::lexer::{Dialect, Tok};
use crate::lang::{Cursor, Operand, Params, QueryResult};
use crate::value::{compare_values, like_matches, total_cmp, PropValue};
use crate::{BknError, StorageReadTx};

mod parse;
mod validate;
mod exec;
mod project;

pub use parse::*;
use validate::*;
use exec::*;
pub(crate) use project::*;

/// Upper bound for an unbounded variable-length relationship (`*`, `*2..`),
/// which keeps path enumeration finite.
pub const MAX_HOPS: usize = 16;

// ============================================================================
// AST
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    pub var: Option<String>,
    pub label: Option<String>,
    pub props: Vec<(String, Operand)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelDirection {
    /// `-[]->`: from the left node to the right one.
    Right,
    /// `<-[]-`
    Left,
    /// `-[]-`: either way.
    Either,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelPattern {
    pub var: Option<String>,
    /// Any of these types; empty = any type.
    pub types: Vec<String>,
    pub direction: RelDirection,
    pub props: Vec<(String, Operand)>,
    /// `Some((min, max))` for a variable-length relationship.
    pub hops: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GExpr {
    Var(String),
    Prop(String, Vec<String>),
    Id(String),
    Label(String),
    Type(String),
    Value(Operand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GCond {
    Cmp(GExpr, CmpOp, GExpr),
    In(GExpr, GExpr, bool),
    IsNull(GExpr, bool),
    Like { expr: GExpr, pattern: GExpr, case_insensitive: bool, negated: bool },
    Contains(GExpr, GExpr),
    StartsWith(GExpr, GExpr),
    EndsWith(GExpr, GExpr),
    HasLabel(String, String),
    And(Vec<GCond>),
    Or(Vec<GCond>),
    Not(Box<GCond>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GAgg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Collect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReturnExpr {
    Expr(GExpr),
    /// `None` argument only for `count(*)`.
    Agg(GAgg, Option<GExpr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnItem {
    pub expr: ReturnExpr,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchQuery {
    pub nodes: Vec<NodePattern>,
    pub rels: Vec<RelPattern>,
    pub filter: Option<GCond>,
    pub distinct: bool,
    /// Empty = `RETURN *`.
    pub returns: Vec<ReturnItem>,
    /// Output column name, or an expression (non-aggregate queries only).
    pub order: Vec<(OrderKey, SortOrder)>,
    pub skip: Option<Operand>,
    pub limit: Option<Operand>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OrderKey {
    Column(usize),
    Expr(GExpr),
}

// ============================================================================
// Parsing
// ============================================================================

const RESERVED: &[&str] = &[
    "MATCH", "WHERE", "RETURN", "AND", "OR", "NOT", "IN", "IS", "NULL", "TRUE", "FALSE", "ORDER", "BY", "SKIP", "LIMIT",
    "AS", "ASC", "DESC", "DISTINCT", "LIKE", "ILIKE", "CONTAINS", "STARTS", "ENDS", "WITH",
];

/// Parses and runs `text` against a read (or write) transaction.
pub(crate) fn run<R: StorageReadTx>(rtx: &R, text: &str, params: &Params) -> Result<QueryResult, BknError> {
    execute(rtx, &parse(text)?, params)
}
