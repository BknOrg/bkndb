//! Parser for SELECT statements and WHERE conditions.
use super::*;

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

pub(super) fn eat_if_exists(c: &mut Cursor<'_>, not: bool) -> Result<bool, BknError> {
    if !c.eat_kw("IF") {
        return Ok(false);
    }
    if not {
        c.expect_kw("NOT")?;
    }
    c.expect_kw("EXISTS")?;
    Ok(true)
}

pub(super) fn table_and_column(c: &mut Cursor<'_>) -> Result<(String, String), BknError> {
    let table = c.ident("a table name", RESERVED)?;
    c.expect_sym("(")?;
    let column = c.ident("a column name", RESERVED)?;
    c.expect_sym(")")?;
    Ok((table, column))
}

pub(super) fn parse_select(c: &mut Cursor<'_>) -> Result<Select, BknError> {
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

pub(super) fn agg_func_at(c: &Cursor<'_>) -> Option<AggFunc> {
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

pub(super) fn parse_alias(c: &mut Cursor<'_>) -> Result<Option<String>, BknError> {
    if c.eat_kw("AS") {
        return Ok(Some(c.ident("an alias", &[])?));
    }
    match c.peek() {
        Some(Tok::Ident(w)) if !RESERVED.iter().any(|r| w.eq_ignore_ascii_case(r)) => Ok(Some(c.ident("an alias", &[])?)),
        Some(Tok::QuotedIdent(_)) => Ok(Some(c.ident("an alias", &[])?)),
        _ => Ok(None),
    }
}

pub(super) fn parse_where(c: &mut Cursor<'_>) -> Result<Option<Cond>, BknError> {
    if c.eat_kw("WHERE") {
        Ok(Some(parse_or(c)?))
    } else {
        Ok(None)
    }
}

pub(super) fn parse_or(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    let mut parts = vec![parse_and(c)?];
    while c.eat_kw("OR") {
        parts.push(parse_and(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::Or(parts) })
}

pub(super) fn parse_and(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
    let mut parts = vec![parse_not(c)?];
    while c.eat_kw("AND") {
        parts.push(parse_not(c)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::And(parts) })
}

pub(super) fn parse_not(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
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

pub(super) fn cmp_op(c: &mut Cursor<'_>) -> Option<CmpOp> {
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

pub(super) fn flip(op: CmpOp) -> CmpOp {
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
pub(super) fn at_value(c: &Cursor<'_>) -> bool {
    if (c.is_kw("TIMESTAMP") || c.is_kw("UUID")) && !matches!(c.peek_at(1), Some(Tok::Str(_))) {
        return false;
    }
    c.at_operand()
}

pub(super) fn parse_predicate(c: &mut Cursor<'_>) -> Result<Cond, BknError> {
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
