//! Query execution: filtering, aggregation, RETURN/ORDER BY/SKIP/LIMIT.
use super::*;

/// Aggregation state for one aggregate in one group.
#[derive(Debug, Clone)]
pub(super) enum AggState {
    Count(i64),
    Sum { int: i64, float: f64, all_int: bool, any: bool, overflow: bool },
    Avg { total: f64, n: u64 },
    Extreme(Option<PropValue>),
    Collect(Vec<PropValue>),
}

impl AggState {
    pub(super) fn new(f: GAgg) -> Self {
        match f {
            GAgg::Count => AggState::Count(0),
            GAgg::Sum => AggState::Sum { int: 0, float: 0.0, all_int: true, any: false, overflow: false },
            GAgg::Avg => AggState::Avg { total: 0.0, n: 0 },
            GAgg::Min | GAgg::Max => AggState::Extreme(None),
            GAgg::Collect => AggState::Collect(Vec::new()),
        }
    }

    pub(super) fn add(&mut self, f: GAgg, v: Option<PropValue>) -> Result<(), BknError> {
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

    pub(super) fn finish(self) -> PropValue {
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

pub(super) fn bind_count(op: &Option<Operand>, params: &Params, what: &str) -> Result<Option<usize>, BknError> {
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
