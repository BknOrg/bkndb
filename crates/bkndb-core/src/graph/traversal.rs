use std::collections::{HashSet, VecDeque};

use crate::graph::db::GraphDb;
use crate::graph::model::{EdgeId, NodeId};
use crate::{BknError, StorageBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversedNode {
    pub node: NodeId,
    pub depth: u32,
    pub via_edge: Option<EdgeId>,
    pub parent: Option<NodeId>,
}

pub struct TraversalBuilder<'a, B: StorageBackend> {
    db: &'a GraphDb<B>,
    start: Option<NodeId>,
    direction: Direction,
    edge_type: Option<String>,
    max_depth: u32,
}

impl<'a, B: StorageBackend> TraversalBuilder<'a, B> {
    pub(crate) fn new(db: &'a GraphDb<B>) -> Self {
        Self {
            db,
            start: None,
            direction: Direction::Out,
            edge_type: None,
            max_depth: 10,
        }
    }

    pub fn start(mut self, id: NodeId) -> Self {
        self.start = Some(id);
        self
    }

    pub fn outgoing(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::Out;
        self.edge_type = Some(edge_type.into());
        self
    }

    pub fn incoming(mut self, edge_type: impl Into<String>) -> Self {
        self.direction = Direction::In;
        self.edge_type = Some(edge_type.into());
        self
    }

    /// Traverse edges of any type in the currently selected direction.
    pub fn any_type(mut self) -> Self {
        self.edge_type = None;
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
            if depth >= self.max_depth {
                continue;
            }
            let neighbors = self.neighbors(current)?;
            for (next, edge_id) in neighbors {
                // `HashSet::insert` returns true only on first insertion —
                // this is the visited-set cycle check: a node already seen
                // is never re-queued, so a cyclic graph still terminates.
                if visited.insert(next) {
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

    fn neighbors(&self, node: NodeId) -> Result<Vec<(NodeId, EdgeId)>, BknError> {
        match (&self.direction, &self.edge_type) {
            (Direction::Out, Some(t)) => self.db.neighbors_out(node, t),
            (Direction::In, Some(t)) => self.db.neighbors_in(node, t),
            (Direction::Out, None) => Ok(self
                .db
                .neighbors_out_any(node)?
                .into_iter()
                .map(|(_, n, e)| (n, e))
                .collect()),
            (Direction::In, None) => Ok(self
                .db
                .neighbors_in_any(node)?
                .into_iter()
                .map(|(_, n, e)| (n, e))
                .collect()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTreeNode {
    pub node: NodeId,
    pub via_edge: Option<EdgeId>,
    pub children: Vec<CallTreeNode>,
}

/// Groups a flat, depth-ordered BFS result by `parent` into a nested view —
/// useful for `impact`-style consumers that want a call tree rather than a
/// flat list. Kept as a separate pure function over `TraversedNode` (rather
/// than built into `run()`) so BFS itself stays free of recursive-ownership
/// complications.
pub fn to_tree(nodes: &[TraversedNode]) -> Vec<CallTreeNode> {
    use std::collections::HashMap;

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
