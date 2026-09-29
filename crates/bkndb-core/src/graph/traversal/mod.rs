use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::codec::{ADJ_IN, ADJ_OUT};
use crate::graph::db::{get_node_in, neighbors_any_in, neighbors_in_in, neighbors_out_in, GraphDb};
use crate::graph::model::{EdgeId, NodeId};
use crate::{BknError, StorageBackend, StorageReadTx};

mod builder;

pub use builder::*;

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
                .filter(|(t, _, _)| edge_types.is_none_or(|types| types.contains(&t.as_str())))
                .map(|(t, n, e)| (n, e, t))
                .collect())
        }
        Direction::In => {
            let all = neighbors_any_in(rtx, ADJ_IN, node)?;
            Ok(all
                .into_iter()
                .filter(|(t, _, _)| edge_types.is_none_or(|types| types.contains(&t.as_str())))
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
        // A node trivially reaches itself — but only if it exists.
        if get_node_in(rtx, start)?.is_none() {
            return Ok(None);
        }
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

/// A lowest-cost path and its total cost.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedPath {
    pub path: PathResult,
    pub cost: f64,
}

/// `f64` ordered by `total_cmp`, for the Dijkstra priority queue.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cost(f64);
impl Eq for Cost {}
impl PartialOrd for Cost {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Cost {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Dijkstra's lowest-cost path from `start` to `target`. Each edge costs its
/// numeric (`Int`/`Float`) `weight_property`, or `default_weight` if the edge
/// has no such numeric property. Weights must be non-negative.
pub(crate) fn find_weighted_path_in<R: StorageReadTx>(
    rtx: &R,
    start: NodeId,
    target: NodeId,
    direction: Direction,
    edge_types: Option<&[&str]>,
    weight_property: &str,
    default_weight: f64,
) -> Result<Option<WeightedPath>, BknError> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let invalid = |w: f64| !(w.is_finite() && w >= 0.0);
    if invalid(default_weight) {
        return Err(BknError::Encoding(format!("default edge weight must be finite and >= 0, got {default_weight}")));
    }
    if get_node_in(rtx, start)?.is_none() || get_node_in(rtx, target)?.is_none() {
        return Ok(None);
    }

    let mut dist: HashMap<NodeId, f64> = HashMap::from([(start, 0.0)]);
    let mut parent: HashMap<NodeId, (NodeId, EdgeId, String)> = HashMap::new();
    let mut heap = BinaryHeap::from([Reverse((Cost(0.0), start))]);

    while let Some(Reverse((Cost(d), node))) = heap.pop() {
        if node == target {
            break;
        }
        if d > dist.get(&node).copied().unwrap_or(f64::INFINITY) {
            continue; // stale queue entry
        }
        for (next, edge_id, edge_type) in neighbors_for_path(rtx, node, direction, edge_types)? {
            let weight = match crate::graph::db::get_edge_in(rtx, edge_id)?
                .and_then(|e| e.properties.get(weight_property).cloned())
            {
                Some(crate::value::PropValue::Int(i)) => i as f64,
                Some(crate::value::PropValue::Float(f)) => f,
                _ => default_weight,
            };
            if invalid(weight) {
                return Err(BknError::Encoding(format!(
                    "edge {} has invalid weight {weight} (must be finite and >= 0)",
                    edge_id.0
                )));
            }
            let candidate = d + weight;
            if candidate < dist.get(&next).copied().unwrap_or(f64::INFINITY) {
                dist.insert(next, candidate);
                parent.insert(next, (node, edge_id, edge_type));
                heap.push(Reverse((Cost(candidate), next)));
            }
        }
    }

    let Some(&cost) = dist.get(&target) else {
        return Ok(None);
    };
    let mut steps = vec![];
    let mut curr = target;
    while curr != start {
        let (prev, edge, edge_type) = parent.get(&curr).cloned().expect("every reached node but start has a parent");
        steps.push(PathStep { node: curr, via_edge: Some(edge), edge_type: Some(edge_type) });
        curr = prev;
    }
    steps.push(PathStep { node: start, via_edge: None, edge_type: None });
    steps.reverse();
    Ok(Some(WeightedPath { path: PathResult { steps }, cost }))
}
