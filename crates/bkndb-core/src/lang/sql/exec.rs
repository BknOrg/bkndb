//! Binding parameters and executing parsed statements.
use super::*;

pub(super) fn bind_expr(cond: &Cond, params: &Params) -> Result<Expr, BknError> {
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

pub(super) fn bind_count(op: &Option<Operand>, params: &Params, what: &str) -> Result<Option<usize>, BknError> {
    match op.as_ref().map(|o| o.bind(params)).transpose()? {
        None => Ok(None),
        Some(PropValue::Int(n)) if n >= 0 => Ok(Some(n as usize)),
        Some(other) => Err(BknError::InvalidQuery(format!("{what} must be a non-negative integer, got {other:?}"))),
    }
}

pub(super) fn unknown_column(schema: &TableSchema, name: &str) -> BknError {
    BknError::SchemaMismatch { table: schema.name().to_string(), message: format!("unknown column '{name}'") }
}

pub(super) fn agg_name(func: AggFunc, column: Option<&str>) -> String {
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
pub(super) fn to_agg(func: AggFunc, column: Option<&String>) -> Agg {
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

pub(super) fn execute_select<R: StorageReadTx>(rtx: &R, sel: &Select, params: &Params) -> Result<SqlOutput, BknError> {
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
