//! Parser: MATCH text to a [`MatchQuery`](super::MatchQuery).
use super::*;

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
pub(super) fn remaining_len(c: &Cursor<'_>) -> usize {
    c.src.len() - c.current_pos()
}

pub(super) fn parse_props(c: &mut Cursor<'_>) -> Result<Vec<(String, Operand)>, BknError> {
    if !c.is_sym("{") {
        return Ok(Vec::new());
    }
    match c.operand()? {
        Operand::Map(entries) => Ok(entries),
        _ => unreachable!("an operand starting with '{{' is a map"),
    }
}

pub(super) fn parse_node(c: &mut Cursor<'_>) -> Result<NodePattern, BknError> {
    c.expect_sym("(")?;
    let var = if matches!(c.peek(), Some(Tok::Ident(_) | Tok::QuotedIdent(_))) { Some(c.ident("a variable", RESERVED)?) } else { None };
    let label = if c.eat_sym(":") { Some(c.ident("a label", &[])?) } else { None };
    let props = parse_props(c)?;
    c.expect_sym(")")?;
    Ok(NodePattern { var, label, props })
}

pub(super) fn parse_rel(c: &mut Cursor<'_>) -> Result<Option<RelPattern>, BknError> {
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

pub(super) fn parse_pattern(c: &mut Cursor<'_>) -> Result<(Vec<NodePattern>, Vec<RelPattern>), BknError> {
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

pub(super) fn function_at(c: &Cursor<'_>) -> Option<String> {
    match (c.peek(), c.peek_at(1)) {
        (Some(Tok::Ident(w)), Some(Tok::Sym("("))) => Some(w.to_ascii_lowercase()),
        _ => None,
    }
}

pub(super) fn parse_expr(c: &mut Cursor<'_>) -> Result<GExpr, BknError> {
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

pub(super) fn parse_return_expr(c: &mut Cursor<'_>) -> Result<ReturnExpr, BknError> {
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

pub(super) fn parse_or(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
    let mut parts = vec![parse_and(c)?];
    while c.eat_kw("OR") {
        parts.push(parse_and(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { GCond::Or(parts) })
}

pub(super) fn parse_and(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
    let mut parts = vec![parse_not(c)?];
    while c.eat_kw("AND") {
        parts.push(parse_not(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { GCond::And(parts) })
}

pub(super) fn parse_not(c: &mut Cursor<'_>) -> Result<GCond, BknError> {
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
