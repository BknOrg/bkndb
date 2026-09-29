//! Static checks of a parsed query (variables, aggregates, clauses).
use super::*;

/// What a variable is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VarKind {
    Node,
    Rel,
    Path,
}

pub(super) fn var_kinds(q: &MatchQuery) -> Result<BTreeMap<String, VarKind>, BknError> {
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

pub(super) fn validate(q: &MatchQuery) -> Result<(), BknError> {
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
