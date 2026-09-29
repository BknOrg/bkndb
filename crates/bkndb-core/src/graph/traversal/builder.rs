//! Fluent traversal builders and call-tree conversion.
use super::*;

/// Fluent traversal builder over a [`GraphDb`]. Opens one read transaction on `run()`.
pub struct TraversalBuilder<'a, B: StorageBackend> {
    pub(super) db: &'a GraphDb<B>,
    pub(super) start: Option<NodeId>,
    pub(super) direction: Direction,
    pub(super) edge_types: Option<Vec<String>>,
    pub(super) filter_label: Option<String>,
    pub(super) max_depth: u32,
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
    pub(super) rtx: &'a R,
    pub(super) start: Option<NodeId>,
    pub(super) direction: Direction,
    pub(super) edge_types: Option<Vec<String>>,
    pub(super) filter_label: Option<String>,
    pub(super) max_depth: u32,
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
