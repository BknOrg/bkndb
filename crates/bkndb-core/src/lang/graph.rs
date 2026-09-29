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

pub fn parse(text: &str) -> Result<MatchQuery, BknError> {
    let mut c = Cursor::new(text, Dialect::Graph)?;
    if !c.eat_kw("MATCH") {
        return Err(c.err("expected MATCH (only read queries are supported; use the graph API to write)"));
    }
    let (nodes, rels) = parse_pattern(&mut c)?;
    let filter = if c.eat_kw("WHERE") { Some(parse_or(&mut c)?) } else { None };
    c.expect_kw("RETURN")?;
    let distinct = c.eat_kw("DISTINCT");
    let mut returns = Vec::new();
    if !c.eat_sym("*") {
        loop {
            let start = c.src.len() - remaining_len(&c);
            let expr = parse_return_expr(&mut c)?;
            let end = c.src.len() - remaining_len(&c);
            let name = if c.eat_kw("AS") {
                c.ident("an alias", &[])?
            } else {
                c.src[start..end].trim().to_string()
            };
            returns.push(ReturnItem { expr, name });
            if !c.eat_sym(",") {
                break;
            }
        }
    }
    let mut order = Vec::new();
    if c.eat_kw("ORDER") {
        c.expect_kw("BY")?;
        loop {
            let start = c.src.len() - remaining_len(&c);
            let expr = parse_expr(&mut c)?;
            let end = c.src.len() - remaining_len(&c);
            let text = c.src[start..end].trim();
            let key = match returns.iter().position(|r| r.name == text) {
                Some(i) => OrderKey::Column(i),
                None => OrderKey::Expr(expr),
            };
            let dir = if c.eat_kw("DESC") {
                SortOrder::Desc
            } else {
                c.eat_kw("ASC");
                SortOrder::Asc
            };
            order.push((key, dir));
            if !c.eat_sym(",") {
                break;
            }
        }
    }
    let (mut skip, mut limit) = (None, None);
    loop {
        if skip.is_none() && (c.eat_kw("SKIP") || c.eat_kw("OFFSET")) {
            skip = Some(c.operand()?);
        } else if limit.is_none() && c.eat_kw("LIMIT") {
            limit = Some(c.operand()?);
        } else {
            break;
        }
    }
    c.finish()?;
    let q = MatchQuery { nodes, rels, filter, distinct, returns, order, skip, limit };
    validate(&q)?;
    Ok(q)
}

/// Bytes of source left from the cursor's current token on (for slicing
/// the original text of a RETURN item as its default column name).
fn remaining_len(c: &Cursor<'_>) -> usize {
    c.src.len() - c.current_pos()
}

fn parse_props(c: &mut Cursor<'_>) -> Result<Vec<(String, Operand)>, BknError> {
    if !c.is_sym("{") {
        return Ok(Vec::new());
    }
    match c.operand()? {
        Operand::Map(entries) => Ok(entries),
        _ => unreachable!("an operand starting with '{{' is a map"),
    }
}

fn parse_node(c: &mut Cursor<'_>) -> Result<NodePattern, BknError> {
    c.expect_sym("(")?;
    let var = if matches!(c.peek(), Some(Tok::Ident(_) | Tok::QuotedIdent(_))) { Some(c.ident("a variable", RESERVED)?) } else { None };
    let label = if c.eat_sym(":") { Some(c.ident("a label", &[])?) } else { None };
    let props = parse_props(c)?;
    c.expect_sym(")")?;
    Ok(NodePattern { var, label, props })
}

fn parse_rel(c: &mut Cursor<'_>) -> Result<Option<RelPattern>, BknError> {
    // Opening: `<-` or `-`. (`-->` lexes as `-` `->`, `<--` as `<-` `-`.)
    let left_arrow = c.eat_sym("<-");
    if !left_arrow && !c.eat_sym("-") {
        return Ok(None);
    }
    let mut rel = RelPattern { var: None, types: Vec::new(), direction: RelDirection::Either, props: Vec::new(), hops: None };
    if c.eat_sym("[") {
        if matches!(c.peek(), Some(Tok::Ident(_) | Tok::QuotedIdent(_))) {
            rel.var = Some(c.ident("a variable", RESERVED)?);
        }
        if c.eat_sym(":") {
            loop {
                rel.types.push(c.ident("a relationship type", &[])?);
                if !c.eat_sym("|") {
                    break;
                }
                c.eat_sym(":");
            }
        }
        if c.eat_sym("*") {
            let min = if matches!(c.peek(), Some(Tok::Int(_))) { Some(c.usize_lit("a hop count")?) } else { None };
            let (min, max) = if c.is_sym(".") {
                c.expect_sym(".")?;
                c.expect_sym(".")?;
                let max = if matches!(c.peek(), Some(Tok::Int(_))) { c.usize_lit("a hop count")? } else { MAX_HOPS };
                (min.unwrap_or(1), max)
            } else {
                let n = min.unwrap_or(1);
                (n, if min.is_some() { n } else { MAX_HOPS })
            };
            if max < min || max > MAX_HOPS {
                return Err(c.err(format!("hop range must satisfy min <= max <= {MAX_HOPS}")));
            }
            rel.hops = Some((min, max));
        }
        rel.props = parse_props(c)?;
        c.expect_sym("]")?;
    }
    // Closing: `-` (left arrow or undirected) or `->`.
    if left_arrow {
        if c.is_sym("->") {
            return Err(c.err("a relationship can't point both ways"));
        }
        c.expect_sym("-")?;
        rel.direction = RelDirection::Left;
    } else if c.eat_sym("->") {
        rel.direction = RelDirection::Right;
    } else {
        c.expect_sym("-")?;
    }
    Ok(Some(rel))
}

fn parse_pattern(c: &mut Cursor<'_>) -> Result<(Vec<NodePattern>, Vec<RelPattern>), BknError> {
    let mut nodes = vec![parse_node(c)?];
    let mut rels = Vec::new();
    while let Some(rel) = parse_rel(c)? {
        rels.push(rel);
        nodes.push(parse_node(c)?);
    }
    if c.is_sym(",") {
        return Err(c.err("only a single path pattern is supported (no comma-separated patterns)"));
    }
    Ok((nodes, rels))
}

fn function_at(c: &Cursor<'_>) -> Option<String> {
    match (c.peek(), c.peek_at(1)) {
        (Some(Tok::Ident(w)), Some(Tok::Sym("("))) => Some(w.to_ascii_lowercase()),
        _ => None,
    }
}

fn parse_expr(c: &mut Cursor<'_>) -> Result<GExpr, BknError> {
    if let Some(f) = function_at(c) {
        if matches!(f.as_str(), "id" | "label" | "labels" | "type") {
            c.next();
            c.expect_sym("(")?;
            let var = c.ident("a variable", RESERVED)?;
            c.expect_sym(")")?;
            return Ok(match f.as_str() {
                "id" => GExpr::Id(var),
                "type" => GExpr::Type(var),
                _ => GExpr::Label(var),
            });
        }
        return Err(c.err(format!("unknown function {f}() here")));
    }
    if c.at_operand() && !(c.is_kw("TIMESTAMP") || c.is_kw("UUID")) || matches!(c.peek(), Some(Tok::Str(_))) {
        return Ok(GExpr::Value(c.operand()?));
    }
    if (c.is_kw("TIMESTAMP") || c.is_kw("UUID")) && matches!(c.peek_at(1), Some(Tok::Str(_))) {
        return Ok(GExpr::Value(c.operand()?));
    }
    let var = c.ident("a variable or value", RESERVED)?;
    let mut path = Vec::new();
    while c.eat_sym(".") {
        match c.next() {
            Some(Tok::Ident(w) | Tok::QuotedIdent(w)) => path.push(w),
            Some(Tok::Int(i)) if i >= 0 => path.push(i.to_string()),
            _ => return Err(c.err("expected a property name after '.'")),
        }
    }
    Ok(if path.is_empty() { GExpr::Var(var) } else { GExpr::Prop(var, path) })
}

fn parse_return_expr(c: &mut Cursor<'_>) -> Result<ReturnExpr, BknError> {
    let agg = match function_at(c).as_deref() {
        Some("count") => Some(GAgg::Count),
        Some("sum") => Some(GAgg::Sum),
        Some("avg") => Some(GAgg::Avg),
        Some("min") => Some(GAgg::Min),
        Some("max") => Some(GAgg::Max),
        Some("collect") => Some(GAgg::Collect),
        _ => None,
    };
    let Some(agg) = agg else {
        return Ok(ReturnExpr::Expr(parse_expr(c)?));
    };
    c.next();
    c.expect_sym("(")?;
    let arg = if agg == GAgg::Count && c.eat_sym("*") { None } else { Some(parse_expr(c)?) };
    c.expect_sym(")")?;
    Ok(ReturnExpr::Agg(agg, arg))
}

fn parse_or(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
    let mut parts = vec![parse_and(c)?];
    while c.eat_kw("OR") {
        parts.push(parse_and(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { GCond::Or(parts) })
}

fn parse_and(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
    let mut parts = vec![parse_not(c)?];
    while c.eat_kw("AND") {
        parts.push(parse_not(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { GCond::And(parts) })
}

fn parse_not(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
    if c.eat_kw("NOT") {
        return Ok(GCond::Not(Box::new(parse_not(c)?)));
    }
    if c.eat_sym("(") {
        let inner = parse_or(c)?;
        c.expect_sym(")")?;
        return Ok(inner);
    }
    // `var:Label`
    if matches!(c.peek(), Some(Tok::Ident(_))) && matches!(c.peek_at(1), Some(Tok::Sym(":"))) {
        let var = c.ident("a variable", RESERVED)?;
        c.expect_sym(":")?;
        return Ok(GCond::HasLabel(var, c.ident("a label", &[])?));
    }
    let lhs = parse_expr(c)?;
    let op = match c.peek() {
        Some(Tok::Sym("=" | "==")) => Some(CmpOp::Eq),
        Some(Tok::Sym("<>" | "!=")) => Some(CmpOp::Ne),
        Some(Tok::Sym("<")) => Some(CmpOp::Lt),
        Some(Tok::Sym("<=")) => Some(CmpOp::Le),
        Some(Tok::Sym(">")) => Some(CmpOp::Gt),
        Some(Tok::Sym(">=")) => Some(CmpOp::Ge),
        Some(Tok::Sym("<-")) => {
            // `x <-1` lexes as `<-` `1`: read it as `x < -1`.
            c.next();
            let rhs = match parse_expr(c)? {
                GExpr::Value(Operand::Lit(PropValue::Int(i))) => PropValue::Int(-i),
                GExpr::Value(Operand::Lit(PropValue::Float(f))) => PropValue::Float(-f),
                _ => return Err(c.err("expected a number after '<-'")),
            };
            return Ok(GCond::Cmp(lhs, CmpOp::Lt, GExpr::Value(Operand::Lit(rhs))));
        }
        _ => None,
    };
    if let Some(op) = op {
        c.next();
        return Ok(GCond::Cmp(lhs, op, parse_expr(c)?));
    }
    if c.eat_kw("IS") {
        let negated = c.eat_kw("NOT");
        c.expect_kw("NULL")?;
        return Ok(GCond::IsNull(lhs, negated));
    }
    if c.eat_kw("CONTAINS") {
        return Ok(GCond::Contains(lhs, parse_expr(c)?));
    }
    if c.eat_kw("STARTS") {
        c.expect_kw("WITH")?;
        return Ok(GCond::StartsWith(lhs, parse_expr(c)?));
    }
    if c.eat_kw("ENDS") {
        c.expect_kw("WITH")?;
        return Ok(GCond::EndsWith(lhs, parse_expr(c)?));
    }
    let negated = c.eat_kw("NOT");
    if c.eat_kw("IN") {
        return Ok(GCond::In(lhs, parse_expr(c)?, negated));
    }
    if c.is_kw("LIKE") || c.is_kw("ILIKE") {
        let case_insensitive = c.is_kw("ILIKE");
        c.next();
        return Ok(GCond::Like { expr: lhs, pattern: parse_expr(c)?, case_insensitive, negated });
    }
    Err(c.err("expected a comparison, IN, IS NULL, LIKE, CONTAINS, STARTS WITH or ENDS WITH"))
}

/// What a variable is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarKind {
    Node,
    Rel,
    Path,
}

fn var_kinds(q: &MatchQuery) -> Result<BTreeMap<String, VarKind>, BknError> {
    let mut vars = BTreeMap::new();
    for n in &q.nodes {
        if let Some(v) = &n.var {
            vars.insert(v.clone(), VarKind::Node);
        }
    }
    for r in &q.rels {
        if let Some(v) = &r.var {
            let kind = if r.hops.is_some() { VarKind::Path } else { VarKind::Rel };
            if vars.insert(v.clone(), kind).is_some() {
                return Err(BknError::InvalidQuery(format!("variable '{v}' is bound twice")));
            }
        }
    }
    Ok(vars)
}

fn validate(q: &MatchQuery) -> Result<(), BknError> {
    let vars = var_kinds(q)?;
    let check = |e: &GExpr| -> Result<(), BknError> {
        let (var, need) = match e {
            GExpr::Var(v) | GExpr::Prop(v, _) => (v, None),
            GExpr::Id(v) => (v, None),
            GExpr::Label(v) => (v, Some(VarKind::Node)),
            GExpr::Type(v) => (v, Some(VarKind::Rel)),
            GExpr::Value(_) => return Ok(()),
        };
        match vars.get(var) {
            None => Err(BknError::InvalidQuery(format!("variable '{var}' is not defined in the pattern"))),
            Some(k) if need.is_some_and(|n| n != *k) => {
                Err(BknError::InvalidQuery(format!("'{var}' is not a {}", if need == Some(VarKind::Node) { "node" } else { "single relationship" })))
            }
            Some(VarKind::Path) if matches!(e, GExpr::Prop(..) | GExpr::Id(_)) => {
                Err(BknError::InvalidQuery(format!("'{var}' is a variable-length relationship (a list of edges)")))
            }
            _ => Ok(()),
        }
    };
    fn walk(c: &GCond, f: &dyn Fn(&GExpr) -> Result<(), BknError>) -> Result<(), BknError> {
        match c {
            GCond::Cmp(a, _, b) | GCond::In(a, b, _) | GCond::Contains(a, b) | GCond::StartsWith(a, b) | GCond::EndsWith(a, b) => {
                f(a)?;
                f(b)
            }
            GCond::IsNull(a, _) => f(a),
            GCond::Like { expr, pattern, .. } => {
                f(expr)?;
                f(pattern)
            }
            GCond::HasLabel(v, _) => f(&GExpr::Label(v.clone())),
            GCond::And(v) | GCond::Or(v) => v.iter().try_for_each(|c| walk(c, f)),
            GCond::Not(c) => walk(c, f),
        }
    }
    if let Some(filter) = &q.filter {
        walk(filter, &check)?;
    }
    for r in &q.returns {
        match &r.expr {
            ReturnExpr::Expr(e) | ReturnExpr::Agg(_, Some(e)) => check(e)?,
            ReturnExpr::Agg(_, None) => {}
        }
    }
    if q.returns.is_empty() && vars.is_empty() {
        return Err(BknError::InvalidQuery("RETURN * needs at least one named variable".into()));
    }
    let aggregating = q.returns.iter().any(|r| matches!(r.expr, ReturnExpr::Agg(..)));
    for (key, _) in &q.order {
        if let OrderKey::Expr(e) = key {
            if aggregating {
                return Err(BknError::InvalidQuery("ORDER BY in an aggregate query must name a RETURN column".into()));
            }
            check(e)?;
        }
    }
    Ok(())
}

// ============================================================================
// Execution
// ============================================================================

#[derive(Debug, Clone)]
enum Bound {
    Node(NodeId),
    Rel(EdgeId),
    Path(Vec<EdgeId>),
}

struct Exec<'r, R: StorageReadTx> {
    rtx: &'r R,
    q: &'r MatchQuery,
    params: &'r Params,
    node_props: Vec<Vec<(String, PropValue)>>,
    rel_props: Vec<Vec<(String, PropValue)>>,
    nodes: HashMap<NodeId, Option<NodeRecord>>,
    edges: HashMap<EdgeId, Option<EdgeRecord>>,
}

impl<R: StorageReadTx> Exec<'_, R> {
    fn node(&mut self, id: NodeId) -> Result<Option<&NodeRecord>, BknError> {
        if !self.nodes.contains_key(&id) {
            let rec = get_node_in(self.rtx, id)?;
            self.nodes.insert(id, rec);
        }
        Ok(self.nodes[&id].as_ref())
    }

    fn edge(&mut self, id: EdgeId) -> Result<Option<&EdgeRecord>, BknError> {
        if !self.edges.contains_key(&id) {
            let rec = get_edge_in(self.rtx, id)?;
            self.edges.insert(id, rec);
        }
        Ok(self.edges[&id].as_ref())
    }

    fn node_matches(&mut self, i: usize, id: NodeId) -> Result<bool, BknError> {
        let label = self.q.nodes[i].label.clone();
        let props = self.node_props[i].clone();
        let Some(rec) = self.node(id)? else { return Ok(false) };
        Ok(label.is_none_or(|l| rec.label == l)
            && props.iter().all(|(k, v)| rec.properties.get(k).is_some_and(|x| compare_values(x, v) == Some(std::cmp::Ordering::Equal))))
    }

    fn edge_matches(&mut self, i: usize, id: EdgeId) -> Result<bool, BknError> {
        let props = self.rel_props[i].clone();
        if props.is_empty() {
            return Ok(true);
        }
        let Some(rec) = self.edge(id)? else { return Ok(false) };
        Ok(props.iter().all(|(k, v)| rec.properties.get(k).is_some_and(|x| compare_values(x, v) == Some(std::cmp::Ordering::Equal))))
    }

    /// One-hop neighbors of `node` along relationship `i`, travelling
    /// left-to-right (`forward`) or right-to-left through the pattern.
    fn step(&self, i: usize, node: NodeId, forward: bool) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        let rel = &self.q.rels[i];
        let (out, inn) = match (rel.direction, forward) {
            (RelDirection::Right, true) | (RelDirection::Left, false) => (true, false),
            (RelDirection::Left, true) | (RelDirection::Right, false) => (false, true),
            (RelDirection::Either, _) => (true, true),
        };
        let mut hits = Vec::new();
        for (enabled, table) in [(out, ADJ_OUT), (inn, ADJ_IN)] {
            if !enabled {
                continue;
            }
            if rel.types.is_empty() {
                hits.extend(neighbors_any_in(self.rtx, table, node)?.into_iter().map(|(_, n, e)| (n, e)));
            } else {
                for t in &rel.types {
                    hits.extend(if table == ADJ_OUT { neighbors_out_in(self.rtx, node, t)? } else { neighbors_in_in(self.rtx, node, t)? });
                }
            }
        }
        // A self-loop shows up in both lists of an undirected step.
        if out && inn {
            let mut seen = HashSet::new();
            hits.retain(|(_, e)| seen.insert(*e));
        }
        Ok(hits)
    }

    /// Every `(end node, edges)` reachable from `start` over relationship
    /// `i` (one hop, or `min..=max` hops), without reusing `used` edges.
    fn expand(&mut self, i: usize, start: NodeId, forward: bool, used: &HashSet<EdgeId>) -> Result<Vec<(NodeId, Vec<EdgeId>)>, BknError> {
        let (min, max) = self.q.rels[i].hops.unwrap_or((1, 1));
        let mut out = Vec::new();
        if min == 0 {
            out.push((start, Vec::new()));
        }
        let mut frontier = vec![(start, Vec::<EdgeId>::new())];
        for depth in 1..=max {
            let mut next = Vec::new();
            for (node, path) in frontier {
                for (n, e) in self.step(i, node, forward)? {
                    if used.contains(&e) || path.contains(&e) || !self.edge_matches(i, e)? {
                        continue;
                    }
                    let mut p = path.clone();
                    p.push(e);
                    if depth >= min {
                        out.push((n, p.clone()));
                    }
                    next.push((n, p));
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(out)
    }

    /// Candidate ids for node pattern `i` as the starting point.
    fn anchor_candidates(&mut self, i: usize, pinned: Option<PropValue>, indexed: Option<(String, PropValue)>) -> Result<Vec<NodeId>, BknError> {
        if let Some(v) = pinned {
            return Ok(match v {
                PropValue::Int(id) if id >= 0 => vec![NodeId(id as u64)],
                _ => Vec::new(),
            });
        }
        let label = self.q.nodes[i].label.clone();
        match (label, indexed) {
            (Some(l), Some((p, v))) => find_nodes_in(self.rtx, &l, &p, &v),
            (Some(l), None) => nodes_by_label_in(self.rtx, &l),
            (None, _) => Ok(scan_all_nodes(self.rtx)?
                .into_iter()
                .map(|(id, rec)| {
                    self.nodes.insert(id, Some(rec));
                    id
                })
                .collect()),
        }
    }
}

/// `id(var) = value` and `var.prop = value` conjuncts at the top level of
/// the WHERE clause, for choosing the start node.
fn top_level_equalities(filter: Option<&GCond>, params: &Params) -> Result<Vec<(GExpr, PropValue)>, BknError> {
    let mut out = Vec::new();
    let mut stack: Vec<&GCond> = filter.into_iter().collect();
    while let Some(c) = stack.pop() {
        match c {
            GCond::And(parts) => stack.extend(parts),
            GCond::Cmp(a, CmpOp::Eq, b) => match (a, b) {
                (e @ (GExpr::Id(_) | GExpr::Prop(..)), GExpr::Value(v)) | (GExpr::Value(v), e @ (GExpr::Id(_) | GExpr::Prop(..))) => {
                    out.push((e.clone(), v.bind(params)?));
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(out)
}

fn node_value(id: NodeId, rec: &NodeRecord) -> PropValue {
    let mut m = BTreeMap::new();
    m.insert("id".to_string(), PropValue::Int(id.0 as i64));
    m.insert("label".to_string(), PropValue::Str(rec.label.clone()));
    m.insert("properties".to_string(), PropValue::Map(rec.properties.clone()));
    PropValue::Map(m)
}

fn edge_value(id: EdgeId, rec: &EdgeRecord) -> PropValue {
    let mut m = BTreeMap::new();
    m.insert("id".to_string(), PropValue::Int(id.0 as i64));
    m.insert("type".to_string(), PropValue::Str(rec.edge_type.clone()));
    m.insert("from".to_string(), PropValue::Int(rec.from.0 as i64));
    m.insert("to".to_string(), PropValue::Int(rec.to.0 as i64));
    m.insert("properties".to_string(), PropValue::Map(rec.properties.clone()));
    PropValue::Map(m)
}

type Binding = BTreeMap<String, Bound>;

impl<R: StorageReadTx> Exec<'_, R> {
    fn eval(&mut self, e: &GExpr, b: &Binding) -> Result<PropValue, BknError> {
        let var = |v: &str| b.get(v).cloned();
        Ok(match e {
            GExpr::Value(op) => op.bind(self.params)?,
            GExpr::Id(v) => match var(v) {
                Some(Bound::Node(n)) => PropValue::Int(n.0 as i64),
                Some(Bound::Rel(r)) => PropValue::Int(r.0 as i64),
                _ => PropValue::Null,
            },
            GExpr::Label(v) => match var(v) {
                Some(Bound::Node(n)) => self.node(n)?.map_or(PropValue::Null, |r| PropValue::Str(r.label.clone())),
                _ => PropValue::Null,
            },
            GExpr::Type(v) => match var(v) {
                Some(Bound::Rel(r)) => self.edge(r)?.map_or(PropValue::Null, |r| PropValue::Str(r.edge_type.clone())),
                _ => PropValue::Null,
            },
            GExpr::Prop(v, path) => {
                let props = match var(v) {
                    Some(Bound::Node(n)) => self.node(n)?.map(|r| &r.properties),
                    Some(Bound::Rel(r)) => self.edge(r)?.map(|r| &r.properties),
                    _ => None,
                };
                props
                    .and_then(|p| p.get(&path[0]))
                    .and_then(|v| v.get_path(&path[1..]))
                    .cloned()
                    .unwrap_or(PropValue::Null)
            }
            GExpr::Var(v) => match var(v) {
                Some(Bound::Node(n)) => self.node(n)?.map_or(PropValue::Null, |r| node_value(n, r)),
                Some(Bound::Rel(r)) => self.edge(r)?.map_or(PropValue::Null, |rec| edge_value(r, rec)),
                Some(Bound::Path(edges)) => {
                    let mut list = Vec::with_capacity(edges.len());
                    for e in edges {
                        list.push(self.edge(e)?.map_or(PropValue::Null, |rec| edge_value(e, rec)));
                    }
                    PropValue::List(list)
                }
                None => PropValue::Null,
            },
        })
    }

    fn test(&mut self, c: &GCond, b: &Binding) -> Result<bool, BknError> {
        use std::cmp::Ordering::*;
        let str_pair = |a: PropValue, p: PropValue| match (a, p) {
            (PropValue::Str(a), PropValue::Str(p)) => Some((a, p)),
            _ => None,
        };
        Ok(match c {
            GCond::Cmp(a, op, rhs) => {
                let (x, y) = (self.eval(a, b)?, self.eval(rhs, b)?);
                if matches!(x, PropValue::Null) || matches!(y, PropValue::Null) {
                    // Like the relational engine: `= null` / `<> null` test nullness.
                    match op {
                        CmpOp::Eq => matches!(x, PropValue::Null) && matches!(y, PropValue::Null),
                        CmpOp::Ne => !(matches!(x, PropValue::Null) && matches!(y, PropValue::Null)),
                        _ => false,
                    }
                } else {
                    compare_values(&x, &y).is_some_and(|o| match op {
                        CmpOp::Eq => o == Equal,
                        CmpOp::Ne => o != Equal,
                        CmpOp::Lt => o == Less,
                        CmpOp::Le => o != Greater,
                        CmpOp::Gt => o == Greater,
                        CmpOp::Ge => o != Less,
                    })
                }
            }
            GCond::In(a, list, negated) => {
                let x = self.eval(a, b)?;
                let found = match self.eval(list, b)? {
                    PropValue::List(items) => !matches!(x, PropValue::Null) && items.iter().any(|i| compare_values(&x, i) == Some(Equal)),
                    _ => return Err(BknError::InvalidQuery("IN needs a list on its right".into())),
                };
                found != *negated
            }
            GCond::IsNull(a, negated) => matches!(self.eval(a, b)?, PropValue::Null) != *negated,
            GCond::Like { expr, pattern, case_insensitive, negated } => {
                let (x, p) = (self.eval(expr, b)?, self.eval(pattern, b)?);
                match str_pair(x, p) {
                    Some((s, p)) => like_matches(&s, &p, *case_insensitive) != *negated,
                    None => false,
                }
            }
            GCond::Contains(a, n) => match (self.eval(a, b)?, self.eval(n, b)?) {
                (PropValue::List(items), n) => items.iter().any(|i| compare_values(i, &n) == Some(Equal)),
                (PropValue::Str(s), PropValue::Str(n)) => s.contains(&n),
                (PropValue::Map(m), PropValue::Str(k)) => m.contains_key(&k),
                _ => false,
            },
            GCond::StartsWith(a, p) => str_pair(self.eval(a, b)?, self.eval(p, b)?).is_some_and(|(s, p)| s.starts_with(&p)),
            GCond::EndsWith(a, p) => str_pair(self.eval(a, b)?, self.eval(p, b)?).is_some_and(|(s, p)| s.ends_with(&p)),
            GCond::HasLabel(v, label) => match b.get(v) {
                Some(Bound::Node(n)) => self.node(*n)?.is_some_and(|r| &r.label == label),
                _ => false,
            },
            GCond::And(parts) => {
                for p in parts {
                    if !self.test(p, b)? {
                        return Ok(false);
                    }
                }
                true
            }
            GCond::Or(parts) => {
                for p in parts {
                    if self.test(p, b)? {
                        return Ok(true);
                    }
                }
                false
            }
            GCond::Not(inner) => !self.test(inner, b)?,
        })
    }
}

/// Aggregation state for one aggregate in one group.
#[derive(Debug, Clone)]
enum AggState {
    Count(i64),
    Sum { int: i64, float: f64, all_int: bool, any: bool, overflow: bool },
    Avg { total: f64, n: u64 },
    Extreme(Option<PropValue>),
    Collect(Vec<PropValue>),
}

impl AggState {
    fn new(f: GAgg) -> Self {
        match f {
            GAgg::Count => AggState::Count(0),
            GAgg::Sum => AggState::Sum { int: 0, float: 0.0, all_int: true, any: false, overflow: false },
            GAgg::Avg => AggState::Avg { total: 0.0, n: 0 },
            GAgg::Min | GAgg::Max => AggState::Extreme(None),
            GAgg::Collect => AggState::Collect(Vec::new()),
        }
    }

    fn add(&mut self, f: GAgg, v: Option<PropValue>) -> Result<(), BknError> {
        let Some(v) = v else {
            if let AggState::Count(n) = self {
                *n += 1;
            }
            return Ok(());
        };
        if matches!(v, PropValue::Null) {
            return Ok(());
        }
        let numeric = |v: &PropValue| match v {
            PropValue::Int(i) => Ok(*i as f64),
            PropValue::Float(x) => Ok(*x),
            other => Err(BknError::InvalidQuery(format!("sum/avg need numbers, got {}", other.kind_name()))),
        };
        match self {
            AggState::Count(n) => *n += 1,
            AggState::Sum { int, float, all_int, any, overflow } => {
                *any = true;
                *float += numeric(&v)?;
                match (&v, int.checked_add(if let PropValue::Int(i) = v { i } else { 0 })) {
                    (PropValue::Int(_), Some(s)) => *int = s,
                    (PropValue::Int(_), None) => *overflow = true,
                    _ => *all_int = false,
                }
            }
            AggState::Avg { total, n } => {
                *total += numeric(&v)?;
                *n += 1;
            }
            AggState::Extreme(cur) => {
                let replace = match cur {
                    None => true,
                    Some(c) => {
                        let o = total_cmp(Some(&v), Some(c));
                        if f == GAgg::Min { o.is_lt() } else { o.is_gt() }
                    }
                };
                if replace {
                    *cur = Some(v);
                }
            }
            AggState::Collect(items) => items.push(v),
        }
        Ok(())
    }

    fn finish(self) -> PropValue {
        match self {
            AggState::Count(n) => PropValue::Int(n),
            AggState::Sum { any: false, .. } => PropValue::Null,
            AggState::Sum { int, all_int: true, overflow: false, .. } => PropValue::Int(int),
            AggState::Sum { float, .. } => PropValue::Float(float),
            AggState::Avg { n: 0, .. } => PropValue::Null,
            AggState::Avg { total, n } => PropValue::Float(total / n as f64),
            AggState::Extreme(v) => v.unwrap_or(PropValue::Null),
            AggState::Collect(items) => PropValue::List(items),
        }
    }
}

fn bind_count(op: &Option<Operand>, params: &Params, what: &str) -> Result<Option<usize>, BknError> {
    match op.as_ref().map(|o| o.bind(params)).transpose()? {
        None => Ok(None),
        Some(PropValue::Int(n)) if n >= 0 => Ok(Some(n as usize)),
        Some(other) => Err(BknError::InvalidQuery(format!("{what} must be a non-negative integer, got {other:?}"))),
    }
}

/// Runs a parsed query against a read (or write) transaction.
pub(crate) fn execute<R: StorageReadTx>(rtx: &R, q: &MatchQuery, params: &Params) -> Result<QueryResult, BknError> {
    let bind_props = |props: &[(String, Operand)]| -> Result<Vec<(String, PropValue)>, BknError> {
        props.iter().map(|(k, v)| Ok((k.clone(), v.bind(params)?))).collect()
    };
    let mut ex = Exec {
        rtx,
        q,
        params,
        node_props: q.nodes.iter().map(|n| bind_props(&n.props)).collect::<Result<_, _>>()?,
        rel_props: q.rels.iter().map(|r| bind_props(&r.props)).collect::<Result<_, _>>()?,
        nodes: HashMap::new(),
        edges: HashMap::new(),
    };
    let skip = bind_count(&q.skip, params, "SKIP")?.unwrap_or(0);
    let limit = bind_count(&q.limit, params, "LIMIT")?;

    // --- choose the start node ---
    let equalities = top_level_equalities(q.filter.as_ref(), params)?;
    let indexes: HashSet<(String, String)> = property_indexes_in(rtx)?.into_iter().collect();
    let mut best: (u8, usize, Option<PropValue>, Option<(String, PropValue)>) = (0, 0, None, None);
    for (i, n) in q.nodes.iter().enumerate() {
        let pinned = n.var.as_ref().and_then(|v| {
            equalities.iter().find_map(|(e, val)| matches!(e, GExpr::Id(x) if x == v).then(|| val.clone()))
        });
        let indexed = n.label.as_ref().and_then(|l| {
            let inline = n.props.iter().zip(&ex.node_props[i]).map(|((k, _), (_, v))| (k.clone(), v.clone()));
            let from_where = equalities.iter().filter_map(|(e, val)| match e {
                GExpr::Prop(x, path) if Some(x) == n.var.as_ref() && path.len() == 1 => Some((path[0].clone(), val.clone())),
                _ => None,
            });
            inline.chain(from_where).find(|(p, _)| indexes.contains(&(l.clone(), p.clone())))
        });
        let score = if pinned.is_some() {
            4
        } else if indexed.is_some() {
            3
        } else if n.label.is_some() {
            2
        } else {
            1
        };
        if score > best.0 {
            best = (score, i, pinned, indexed);
        }
    }
    let (_, anchor, pinned, indexed) = best;

    // Traversal order: from the anchor rightwards, then leftwards.
    let steps: Vec<(usize, bool)> = (anchor..q.rels.len()).map(|i| (i, true)).chain((0..anchor).rev().map(|i| (i, false))).collect();

    let aggregating = q.returns.iter().any(|r| matches!(r.expr, ReturnExpr::Agg(..)));
    let streaming_limit = if !aggregating && !q.distinct && q.order.is_empty() { limit.map(|l| l + skip) } else { None };

    let mut matches: Vec<Binding> = Vec::new();
    'anchors: for start in ex.anchor_candidates(anchor, pinned, indexed)? {
        if !ex.node_matches(anchor, start)? {
            continue;
        }
        // Depth-first over the steps, each frame = (binding, node ids by pattern position, used edges).
        let mut positions = vec![None; q.nodes.len()];
        positions[anchor] = Some(start);
        let mut stack = vec![(0usize, positions, HashSet::<EdgeId>::new(), Vec::<(usize, Vec<EdgeId>)>::new())];
        while let Some((depth, positions, used, rels)) = stack.pop() {
            if depth == steps.len() {
                // Complete: build the variable binding, checking repeated variables.
                let mut b = Binding::new();
                let mut consistent = true;
                for (i, n) in q.nodes.iter().enumerate() {
                    if let Some(v) = &n.var {
                        let id = positions[i].expect("all positions bound");
                        match b.get(v) {
                            Some(Bound::Node(prev)) if *prev != id => consistent = false,
                            _ => {
                                b.insert(v.clone(), Bound::Node(id));
                            }
                        }
                    }
                }
                if !consistent {
                    continue;
                }
                for (i, edges) in &rels {
                    if let Some(v) = &q.rels[*i].var {
                        let bound = if q.rels[*i].hops.is_some() { Bound::Path(edges.clone()) } else { Bound::Rel(edges[0]) };
                        b.insert(v.clone(), bound);
                    }
                }
                if let Some(f) = &q.filter
                    && !ex.test(f, &b)?
                {
                    continue;
                }
                matches.push(b);
                if streaming_limit.is_some_and(|l| matches.len() >= l) {
                    break 'anchors;
                }
                continue;
            }
            let (i, forward) = steps[depth];
            let (from, to) = if forward { (i, i + 1) } else { (i + 1, i) };
            let origin = positions[from].expect("steps extend from a bound node");
            let mut expansions = ex.expand(i, origin, forward, &used)?;
            // Push in reverse so the stack visits them in adjacency order.
            expansions.reverse();
            for (target, edges) in expansions {
                if !ex.node_matches(to, target)? {
                    continue;
                }
                let mut positions = positions.clone();
                positions[to] = Some(target);
                let mut used = used.clone();
                used.extend(edges.iter().copied());
                let mut rels = rels.clone();
                rels.push((i, edges));
                stack.push((depth + 1, positions, used, rels));
            }
        }
    }

    // --- projection ---
    let items: Vec<ReturnItem> = if q.returns.is_empty() {
        var_kinds(q)?.into_keys().map(|v| ReturnItem { expr: ReturnExpr::Expr(GExpr::Var(v.clone())), name: v }).collect()
    } else {
        q.returns.clone()
    };
    let columns: Vec<String> = items.iter().map(|i| i.name.clone()).collect();

    let mut rows: Vec<(Vec<PropValue>, Vec<PropValue>)> = Vec::new(); // (row, extra sort keys)
    if aggregating {
        let mut groups: Vec<(Vec<PropValue>, Vec<AggState>)> = Vec::new();
        let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
        for b in &matches {
            let mut key = Vec::new();
            for item in &items {
                if let ReturnExpr::Expr(e) = &item.expr {
                    key.push(ex.eval(e, b)?);
                }
            }
            let encoded = bincode::serialize(&key).map_err(|e| BknError::Encoding(e.to_string()))?;
            let slot = *index.entry(encoded).or_insert_with(|| {
                let states = items
                    .iter()
                    .filter_map(|i| match i.expr {
                        ReturnExpr::Agg(f, _) => Some(AggState::new(f)),
                        _ => None,
                    })
                    .collect();
                groups.push((key, states));
                groups.len() - 1
            });
            let mut k = 0;
            for item in &items {
                if let ReturnExpr::Agg(f, arg) = &item.expr {
                    let v = match arg {
                        Some(e) => Some(ex.eval(e, b)?),
                        None => None,
                    };
                    groups[slot].1[k].add(*f, v)?;
                    k += 1;
                }
            }
        }
        if groups.is_empty() && items.iter().all(|i| matches!(i.expr, ReturnExpr::Agg(..))) {
            // Aggregates over no matches still produce one row (count = 0).
            let states = items.iter().filter_map(|i| if let ReturnExpr::Agg(f, _) = i.expr { Some(AggState::new(f)) } else { None }).collect();
            groups.push((Vec::new(), states));
        }
        for (key, states) in groups {
            let (mut key, mut states) = (key.into_iter(), states.into_iter());
            let row = items
                .iter()
                .map(|i| match i.expr {
                    ReturnExpr::Expr(_) => key.next().unwrap(),
                    ReturnExpr::Agg(..) => states.next().unwrap().finish(),
                })
                .collect();
            rows.push((row, Vec::new()));
        }
    } else {
        for b in &matches {
            let mut row = Vec::with_capacity(items.len());
            for item in &items {
                let ReturnExpr::Expr(e) = &item.expr else { unreachable!() };
                row.push(ex.eval(e, b)?);
            }
            let mut keys = Vec::new();
            for (key, _) in &q.order {
                if let OrderKey::Expr(e) = key {
                    keys.push(ex.eval(e, b)?);
                }
            }
            rows.push((row, keys));
        }
    }
    if q.distinct {
        let mut seen = HashSet::new();
        rows.retain(|(row, _)| seen.insert(bincode::serialize(row).unwrap_or_default()));
    }
    if !q.order.is_empty() {
        rows.sort_by(|(ra, ka), (rb, kb)| {
            let mut extra = 0;
            for (key, dir) in &q.order {
                let o = match key {
                    OrderKey::Column(i) => total_cmp(Some(&ra[*i]), Some(&rb[*i])),
                    OrderKey::Expr(_) => {
                        let o = total_cmp(Some(&ka[extra]), Some(&kb[extra]));
                        extra += 1;
                        o
                    }
                };
                let o = if *dir == SortOrder::Desc { o.reverse() } else { o };
                if o.is_ne() {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    let rows = rows.into_iter().skip(skip).take(limit.unwrap_or(usize::MAX)).map(|(r, _)| r).collect();
    Ok(QueryResult { columns, rows, affected: 0 })
}

/// Parses and runs `text` against a read (or write) transaction.
pub(crate) fn run<R: StorageReadTx>(rtx: &R, text: &str, params: &Params) -> Result<QueryResult, BknError> {
    execute(rtx, &parse(text)?, params)
}
