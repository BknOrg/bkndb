//! Validate graph records and their mandatory adjacency/counter structures in
//! one read snapshot. Optional property indexes have a separate audit scope.
use super::*;

#[derive(Debug, Clone, serde::Serialize)]
pub struct GraphIntegrityReport {
    pub nodes_checked: u64,
    pub edges_checked: u64,
    pub adjacency_entries_checked: u64,
}

impl<B: StorageBackend> GraphDb<B> {
    pub fn verify_integrity(&self) -> Result<GraphIntegrityReport, BknError> {
        let tx = self.backend.begin_read()?;
        let mut report = GraphIntegrityReport {
            nodes_checked: 0,
            edges_checked: 0,
            adjacency_entries_checked: 0,
        };
        let mut max_node = None;
        let mut max_edge = None;
        for item in tx.scan(NODES, Bound::Unbounded, Bound::Unbounded)? {
            let (key, bytes) = item?;
            let id = crate::graph::codec::decode_node_id(&key)?;
            let _: NodeRecord = decode(&bytes)?;
            max_node = Some(id.0);
            report.nodes_checked += 1;
        }
        for item in tx.scan(EDGES, Bound::Unbounded, Bound::Unbounded)? {
            let (key, bytes) = item?;
            let id =
                EdgeId(u64::from_be_bytes(key.as_slice().try_into().map_err(
                    |_| BknError::Corruption("invalid graph edge key".into()),
                )?));
            let edge: EdgeRecord = decode(&bytes)?;
            validate_edge_type(&edge.edge_type).map_err(|_| {
                BknError::Corruption("stored edge type exceeds format limit".into())
            })?;
            if tx.get(NODES, &node_key(edge.from))?.is_none()
                || tx.get(NODES, &node_key(edge.to))?.is_none()
            {
                return Err(BknError::Corruption(format!(
                    "edge {} has a missing endpoint",
                    id.0
                )));
            }
            for (table, key) in [
                (
                    ADJ_OUT,
                    adj_out_key(edge.from, &edge.edge_type, edge.to, id),
                ),
                (ADJ_IN, adj_in_key(edge.to, &edge.edge_type, edge.from, id)),
            ] {
                if tx.get(table, &key)?.as_deref() != Some(&[]) {
                    return Err(BknError::Corruption(format!(
                        "edge {} has missing/invalid adjacency",
                        id.0
                    )));
                }
            }
            max_edge = Some(id.0);
            report.edges_checked += 1;
        }
        for (table, outgoing) in [(ADJ_OUT, true), (ADJ_IN, false)] {
            for item in tx.scan(table, Bound::Unbounded, Bound::Unbounded)? {
                let (key, value) = item?;
                let (first, edge_type, second, edge_id) = decode_adj_key(&key)?;
                let edge = get_edge_in(&tx, edge_id)?
                    .ok_or_else(|| BknError::Corruption("orphan graph adjacency".into()))?;
                let (from, to) = if outgoing {
                    (first, second)
                } else {
                    (second, first)
                };
                if !value.is_empty()
                    || edge.from.0 != from
                    || edge.to.0 != to
                    || edge.edge_type != edge_type
                {
                    return Err(BknError::Corruption(
                        "graph adjacency disagrees with edge record".into(),
                    ));
                }
                report.adjacency_entries_checked += 1;
            }
        }
        for (counter, maximum) in [(NEXT_NODE_ID_KEY, max_node), (NEXT_EDGE_ID_KEY, max_edge)] {
            let bytes = tx.get(META, counter)?;
            let next = match bytes {
                Some(bytes) => u64::from_be_bytes(
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| BknError::Corruption("invalid graph ID counter".into()))?,
                ),
                None => 1,
            };
            if next == 0 || maximum.is_some_and(|max| next <= max) {
                return Err(BknError::Corruption(
                    "graph ID counter can reuse an existing ID".into(),
                ));
            }
        }
        Ok(report)
    }
}
