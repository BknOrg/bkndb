//! Pattern matching: anchor selection and relationship expansion.
use super::*;

#[derive(Debug, Clone)]
pub(super) enum Bound {
    Node(NodeId),
    Rel(EdgeId),
    Path(Vec<EdgeId>),
}

pub(super) struct Exec<'r, R: StorageReadTx> {
    pub(super) rtx: &'r R,
    pub(super) q: &'r MatchQuery,
    pub(super) params: &'r Params,
    pub(super) node_props: Vec<Vec<(String, PropValue)>>,
    pub(super) rel_props: Vec<Vec<(String, PropValue)>>,
    pub(super) nodes: HashMap<NodeId, Option<NodeRecord>>,
    pub(super) edges: HashMap<EdgeId, Option<EdgeRecord>>,
}

impl<R: StorageReadTx> Exec<'_, R> {
    pub(super) fn node(&mut self, id: NodeId) -> Result<Option<&NodeRecord>, BknError> {
        if !self.nodes.contains_key(&id) {
            let rec = get_node_in(self.rtx, id)?;
            self.nodes.insert(id, rec);
        }
        Ok(self.nodes[&id].as_ref())
    }

    pub(super) fn edge(&mut self, id: EdgeId) -> Result<Option<&EdgeRecord>, BknError> {
        if !self.edges.contains_key(&id) {
            let rec = get_edge_in(self.rtx, id)?;
            self.edges.insert(id, rec);
        }
        Ok(self.edges[&id].as_ref())
    }

    pub(super) fn node_matches(&mut self, i: usize, id: NodeId) -> Result<bool, BknError> {
        let label = self.q.nodes[i].label.clone();
        let props = self.node_props[i].clone();
        let Some(rec) = self.node(id)? else { return Ok(false) };
        Ok(label.is_none_or(|l| rec.label == l)
            && props.iter().all(|(k, v)| rec.properties.get(k).is_some_and(|x| compare_values(x, v) == Some(std::cmp::Ordering::Equal))))
    }

    pub(super) fn edge_matches(&mut self, i: usize, id: EdgeId) -> Result<bool, BknError> {
        let props = self.rel_props[i].clone();
        if props.is_empty() {
            return Ok(true);
        }
        let Some(rec) = self.edge(id)? else { return Ok(false) };
        Ok(props.iter().all(|(k, v)| rec.properties.get(k).is_some_and(|x| compare_values(x, v) == Some(std::cmp::Ordering::Equal))))
    }

    /// One-hop neighbors of `node` along relationship `i`, travelling
    /// left-to-right (`forward`) or right-to-left through the pattern.
    pub(super) fn step(&self, i: usize, node: NodeId, forward: bool) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
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
    pub(super) fn expand(&mut self, i: usize, start: NodeId, forward: bool, used: &HashSet<EdgeId>) -> Result<Vec<(NodeId, Vec<EdgeId>)>, BknError> {
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
    pub(super) fn anchor_candidates(&mut self, i: usize, pinned: Option<PropValue>, indexed: Option<(String, PropValue)>) -> Result<Vec<NodeId>, BknError> {
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
pub(super) fn top_level_equalities(filter: Option<&GCond>, params: &Params) -> Result<Vec<(GExpr, PropValue)>, BknError> {
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

pub(super) fn node_value(id: NodeId, rec: &NodeRecord) -> PropValue {
    let mut m = BTreeMap::new();
    m.insert("id".to_string(), PropValue::Int(id.0 as i64));
    m.insert("label".to_string(), PropValue::Str(rec.label.clone()));
    m.insert("properties".to_string(), PropValue::Map(rec.properties.clone()));
    PropValue::Map(m)
}

pub(super) fn edge_value(id: EdgeId, rec: &EdgeRecord) -> PropValue {
    let mut m = BTreeMap::new();
    m.insert("id".to_string(), PropValue::Int(id.0 as i64));
    m.insert("type".to_string(), PropValue::Str(rec.edge_type.clone()));
    m.insert("from".to_string(), PropValue::Int(rec.from.0 as i64));
    m.insert("to".to_string(), PropValue::Int(rec.to.0 as i64));
    m.insert("properties".to_string(), PropValue::Map(rec.properties.clone()));
    PropValue::Map(m)
}

pub(super) type Binding = BTreeMap<String, Bound>;

impl<R: StorageReadTx> Exec<'_, R> {
    pub(super) fn eval(&mut self, e: &GExpr, b: &Binding) -> Result<PropValue, BknError> {
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

    pub(super) fn test(&mut self, c: &GCond, b: &Binding) -> Result<bool, BknError> {
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
