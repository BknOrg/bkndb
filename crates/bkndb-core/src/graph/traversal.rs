use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::codec::{ADJ_IN, ADJ_OUT};
use crate::graph::db::{get_node_in, neighbors_any_in, neighbors_in_in, neighbors_out_in, GraphDb};
use crate::graph::model::{EdgeId, NodeId};
use crate::{BknError, StorageBackend, StorageReadTx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversedNode {
    pub node: NodeId,
    pub depth: u32,
    pub via_edge: Option<EdgeId>,
    pub parent: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub node: NodeId,
    pub via_edge: Option<EdgeId>,
    pub edge_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathResult {
    pub steps: Vec<PathStep>,
}

impl PathResult {
    pub fn nodes(&self) -> Vec<NodeId> {
        self.steps.iter().map(|s| s.node).collect()
    }

    pub fn edges(&self) -> Vec<EdgeId> {
        self.steps.iter().filter_map(|s| s.via_edge).collect()
    }
}

pub(crate) fn neighbors_for_traversal<R: StorageReadTx>(
    rtx: &R,
    node: NodeId,
    direction: Direction,
    edge_types: Option<&[&str]>,
) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
    match direction {
        Direction::Out => match edge_types {
            Some(types) => {
                let mut res = Vec::new();
                for &t in types {
                    res.extend(neighbors_out_in(rtx, node, t)?);
                }
                Ok(res)
            }
            None => Ok(neighbors_any_in(rtx, ADJ_OUT, node)?
                .into_iter()
                .map(|(_, n, e)| (n, e))
                .collect()),
        },
        Direction::In => match edge_types {
            Some(types) => {
                let mut res = Vec::new();
                for &t in types {
                    res.extend(neighbors_in_in(rtx, node, t)?);
                }
                Ok(res)
            }
            None => Ok(neighbors_any_in(rtx, ADJ_IN, node)?
                .into_iter()
                .map(|(_, n, e)| (n, e))
                .collect()),
        },
        Direction::Both => {
            let mut out = neighbors_for_traversal(rtx, node, Direction::Out, edge_types)?;
            let incoming = neighbors_for_traversal(rtx, node, Direction::In, edge_types)?;
            out.extend(incoming);
            Ok(out)
        }
    }
}

pub(crate) fn traverse_in<R: StorageReadTx>(
    rtx: &R,
    start: NodeId,
    direction: Direction,
    edge_types: Option<&[&str]>,
    filter_label: Option<&str>,
    max_depth: u32,
) -> Result<Vec<TraversedNode>, BknError> {
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<(NodeId, u32)> = VecDeque::new();
    let mut out = Vec::new();

    visited.insert(start);
    queue.push_back((start, 0));
    out.push(TraversedNode {
        node: start,
        depth: 0,
        via_edge: None,
        parent: None,
    });

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let neighbors = neighbors_for_traversal(rtx, current, direction, edge_types)?;
        for (next, edge_id) in neighbors {
            if visited.insert(next) {
                if let Some(target_label) = filter_label {
                    if let Some(record) = get_node_in(rtx, next)? {
                        if record.label != target_label {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }

                queue.push_back((next, depth + 1));
                out.push(TraversedNode {
                    node: next,
                    depth: depth + 1,
                    via_edge: Some(edge_id),
                    parent: Some(current),
                });
            }
        }
    }

    Ok(out)
}

fn neighbors_for_path<R: StorageReadTx>(
    rtx: &R,
    node: NodeId,
    direction: Direction,
    edge_types: Option<&[&str]>,
) -> Result<Vec<(NodeId, EdgeId, String)>, BknError> {
    match direction {
        Direction::Out => {
            let all = neighbors_any_in(rtx, ADJ_OUT, node)?;
            Ok(all
                .into_iter()
                .filter(|(t, _, _)| edge_types.map_or(true, |types| types.contains(&t.as_str())))
                .map(|(t, n, e)| (n, e, t))
                .collect())
        }
        Direction::In => {
            let all = neighbors_any_in(rtx, ADJ_IN, node)?;
            Ok(all
                .into_iter()
                .filter(|(t, _, _)| edge_types.map_or(true, |types| types.contains(&t.as_str())))
                .map(|(t, n, e)| (n, e, t))
                .collect())
        }
        Direction::Both => {
            let mut out = neighbors_for_path(rtx, node, Direction::Out, edge_types)?;
            let incoming = neighbors_for_path(rtx, node, Direction::In, edge_types)?;
            out.extend(incoming);
            Ok(out)
        }
    }
}

pub(crate) fn find_shortest_path_in<R: StorageReadTx>(
    rtx: &R,
    start: NodeId,
    target: NodeId,
    direction: Direction,
    edge_types: Option<&[&str]>,
) -> Result<Option<PathResult>, BknError> {
    if start == target {
        return Ok(Some(PathResult {
            steps: vec![PathStep {
                node: start,
                via_edge: None,
                edge_type: None,
            }],
        }));
    }

    let mut parent_map: HashMap<NodeId, (NodeId, EdgeId, String)> = HashMap::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut queue: VecDeque<NodeId> = VecDeque::new();

    visited.insert(start);
    queue.push_back(start);

    let mut found = false;

    'outer: while let Some(current) = queue.pop_front() {
        let neighbors = neighbors_for_path(rtx, current, direction, edge_types)?;
        for (next, edge_id, edge_type) in neighbors {
            if visited.insert(next) {
                parent_map.insert(next, (current, edge_id, edge_type));
                if next == target {
                    found = true;
                    break 'outer;
                }
                queue.push_back(next);
            }
        }
    }

    if !found {
        return Ok(None);
    }

    let mut steps = Vec::new();
    let mut curr = target;
    while curr != start {
        let (prev, edge_id, edge_type) = parent_map.get(&curr).cloned().expect("parent exists");
        steps.push(PathStep {
            node: curr,
            via_edge: Some(edge_id),
            edge_type: Some(edge_type),
        });
        curr = prev;
    }
    steps.push(PathStep {
        node: start,
        via_edge: None,
        edge_type: None,
    });
    steps.reverse();

    Ok(Some(PathResult { steps }))
}

/// Fluent traversal builder over a [`GraphDb`]. Opens one read transaction on `run()`.
pub struct TraversalBuilder<'a, B: StorageBackend> {
    db: &'a GraphDb<B>,
    start: Option<NodeId>,
    direction: Direction,
    edge_types: Option<Vec<String>>,
    filter_label: Option<String>,
    max_depth: u32,
}

impl<'a, B: StorageBackend> TraversalBuilder<'a, B> {
    pub(crate) fn new(db: &'a GraphDb<B>) -> Self {
        Self {
            db,
            start: None,
            direction: Direction::Out,
            edge_types: None,
            filter_label: None,
            max_depth: 10,
        }
    }

    pub fn start(mut self, id: NodeId) -> Self {
        self.start = Some(id);
        self
    }

    pub fn outgoing(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::Out;
        self.edge_types = Some(vec![edge_type.into()]);
        self
    }

    pub fn incoming(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::In;
        self.edge_types = Some(vec![edge_type.into()]);
        self
    }

    pub fn filter_edge_types(mut self, types: &[&str]) -> Self {
        self.edge_types = Some(types.iter().map(|s| s.to_string()).collect());
        self
    }

    pub fn filter_node_label(mut self, label: impl Into<String>) -> Self {
        self.filter_label = Some(label.into());
        self
    }

    pub fn any_type(mut self) -> Self {
        self.edge_types = None;
        self
    }

    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    pub fn max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn run(self) -> Result<Vec<TraversedNode>, BknError> {
        let start = self
            .start
            .ok_or_else(|| BknError::Encoding("traversal requires .start(id)".to_string()))?;
        let rtx = self.db.backend.begin_read()?;
        let type_refs: Option<Vec<&str>> = self
            .edge_types
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        traverse_in(
            &rtx,
            start,
            self.direction,
            type_refs.as_deref(),
            self.filter_label.as_deref(),
            self.max_depth,
        )
    }
}

/// Fluent traversal builder operating directly on an already-open [`StorageReadTx`].
pub struct TraversalOnTx<'a, R: StorageReadTx> {
    rtx: &'a R,
    start: Option<NodeId>,
    direction: Direction,
    edge_types: Option<Vec<String>>,
    filter_label: Option<String>,
    max_depth: u32,
}

impl<'a, R: StorageReadTx> TraversalOnTx<'a, R> {
    pub(crate) fn new(rtx: &'a R) -> Self {
        Self {
            rtx,
            start: None,
            direction: Direction::Out,
            edge_types: None,
            filter_label: None,
            max_depth: 10,
        }
    }

    pub fn start(mut self, id: NodeId) -> Self {
        self.start = Some(id);
        self
    }

    pub fn outgoing(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::Out;
        self.edge_types = Some(vec![edge_type.into()]);
        self
    }

    pub fn incoming(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::In;
        self.edge_types = Some(vec![edge_type.into()]);
        self
    }

    pub fn filter_edge_types(mut self, types: &[&str]) -> Self {
        self.edge_types = Some(types.iter().map(|s| s.to_string()).collect());
        self
    }

    pub fn filter_node_label(mut self, label: impl Into<String>) -> Self {
        self.filter_label = Some(label.into());
        self
    }

    pub fn any_type(mut self) -> Self {
        self.edge_types = None;
        self
    }

    pub fn direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    pub fn max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    pub fn run(self) -> Result<Vec<TraversedNode>, BknError> {
        let start = self
            .start
            .ok_or_else(|| BknError::Encoding("traversal requires .start(id)".to_string()))?;
        let type_refs: Option<Vec<&str>> = self
            .edge_types
            .as_ref()
            .map(|v| v.iter().map(|s| s.as_str()).collect());
        traverse_in(
            self.rtx,
            start,
            self.direction,
            type_refs.as_deref(),
            self.filter_label.as_deref(),
            self.max_depth,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTreeNode {
    pub node: NodeId,
    pub via_edge: Option<EdgeId>,
    pub children: Vec<CallTreeNode>,
}

/// Groups a flat, depth-ordered BFS result by `parent` into a nested view —
/// useful for `impact`-style consumers that want a call tree rather than a flat list.
pub fn to_tree(nodes: &[TraversedNode]) -> Vec<CallTreeNode> {
    let mut children_of: HashMap<Option<NodeId>, Vec<&TraversedNode>> = HashMap::new();
    for n in nodes {
        children_of.entry(n.parent).or_default().push(n);
    }

    fn build(node: &TraversedNode, children_of: &HashMap<Option<NodeId>, Vec<&TraversedNode>>) -> CallTreeNode {
        let children = children_of
            .get(&Some(node.node))
            .map(|kids| kids.iter().map(|k| build(k, children_of)).collect())
            .unwrap_or_default();
        CallTreeNode {
            node: node.node,
            via_edge: node.via_edge,
            children,
        }
    }

    children_of
        .get(&None)
        .map(|roots| roots.iter().map(|r| build(r, &children_of)).collect())
        .unwrap_or_default()
}
