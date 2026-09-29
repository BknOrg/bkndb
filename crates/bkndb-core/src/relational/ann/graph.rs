//! The HNSW graph: insertion, removal and layered best-first search.
use super::*;

/// One index, loaded for the duration of a single operation.
pub(super) struct Hnsw {
    pub(super) name: String,
    pub(super) meta_key: Vec<u8>,
    pub(super) nodes: TableSpec,
    pub(super) links: TableSpec,
    pub(super) pks: TableSpec,
    pub(super) meta: AnnMeta,
    pub(super) metric: VectorMetric,
    /// Nodes read so far (`None`: deleted). Nodes never change once written.
    pub(super) cache: HashMap<u64, Option<Rc<Node>>>,
}

impl Hnsw {
    pub(super) fn load<R: StorageReadTx>(rtx: &R, table: &str, column: &str) -> Result<Option<Self>, BknError> {
        let key = meta_key(table, column);
        let Some(bytes) = rtx.get(meta_table(), &key)? else { return Ok(None) };
        let meta: AnnMeta = bincode::deserialize(&bytes).map_err(enc_err)?;
        Ok(Some(Self {
            name: format!("{table}.{column}"),
            meta_key: key,
            nodes: nodes_table(table, column),
            links: links_table(table, column),
            pks: pks_table(table, column),
            metric: metric_from(meta.metric)?,
            meta,
            cache: HashMap::new(),
        }))
    }

    pub(super) fn save<W: StorageWriteTx>(&self, wtx: &mut W) -> Result<(), BknError> {
        wtx.put(meta_table(), &self.meta_key, &bincode::serialize(&self.meta).map_err(enc_err)?)
    }

    pub(super) fn m(&self) -> usize {
        self.meta.m as usize
    }

    /// Most links a node keeps on `layer`.
    pub(super) fn cap(&self, layer: u8) -> usize {
        if layer == 0 { self.m() * 2 } else { self.m() }
    }

    pub(super) fn node<R: StorageReadTx>(&mut self, rtx: &R, id: u64) -> Result<Option<Rc<Node>>, BknError> {
        if let Some(n) = self.cache.get(&id) {
            return Ok(n.clone());
        }
        let n = match rtx.get(self.nodes, &id.to_be_bytes())? {
            Some(b) => Some(Rc::new(decode_node(&b)?)),
            None => None,
        };
        self.cache.insert(id, n.clone());
        Ok(n)
    }

    pub(super) fn links<R: StorageReadTx>(&self, rtx: &R, id: u64, layer: u8) -> Result<Vec<u64>, BknError> {
        Ok(match rtx.get(self.links, &link_key(id, layer))? {
            Some(b) => b.as_chunks::<8>().0.iter().map(|c| u64::from_be_bytes(*c)).collect(),
            None => Vec::new(),
        })
    }

    pub(super) fn put_links<W: StorageWriteTx>(&self, wtx: &mut W, id: u64, layer: u8, ids: &[u64]) -> Result<(), BknError> {
        let bytes: Vec<u8> = ids.iter().flat_map(|i| i.to_be_bytes()).collect();
        wtx.put(self.links, &link_key(id, layer), &bytes)
    }

    /// Lower = closer. Cosine vectors are stored normalized.
    pub(super) fn dist(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.metric {
            VectorMetric::Cosine => 1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>(),
            VectorMetric::Dot => -a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>(),
            VectorMetric::Euclidean => a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>(),
        }
    }

    /// The form a vector is indexed/searched in; `None` = can't match
    /// (a zero vector under cosine, as in the exact scan).
    pub(super) fn prepare(&self, mut v: Vec<f32>) -> Option<Vec<f32>> {
        if self.metric == VectorMetric::Cosine {
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm == 0.0 || !norm.is_finite() {
                return None;
            }
            v.iter_mut().for_each(|x| *x /= norm);
        }
        Some(v)
    }

    pub(super) fn cand<R: StorageReadTx>(&mut self, rtx: &R, q: &[f32], id: u64) -> Result<Cand, BknError> {
        let node = self.node(rtx, id)?.ok_or_else(|| BknError::Corruption(format!("vector index '{}' lost its entry point", self.name)))?;
        Ok(Cand { d: self.dist(q, &node.vector), id })
    }

    /// Best-first search of one layer from `eps`, keeping the `ef` closest
    /// nodes; returned nearest first.
    pub(super) fn search_layer<R: StorageReadTx>(&mut self, rtx: &R, q: &[f32], eps: &[Cand], ef: usize, layer: u8) -> Result<Vec<Cand>, BknError> {
        let mut visited: HashSet<u64> = eps.iter().map(|c| c.id).collect();
        let mut frontier: BinaryHeap<Reverse<Cand>> = eps.iter().copied().map(Reverse).collect();
        let mut best: BinaryHeap<Cand> = eps.iter().copied().collect();
        while best.len() > ef {
            best.pop();
        }
        while let Some(Reverse(c)) = frontier.pop() {
            if best.len() >= ef && best.peek().is_some_and(|w| c.d > w.d) {
                break;
            }
            for n in self.links(rtx, c.id, layer)? {
                if !visited.insert(n) {
                    continue;
                }
                let Some(node) = self.node(rtx, n)? else { continue };
                let d = self.dist(q, &node.vector);
                if best.len() < ef || best.peek().is_some_and(|w| d < w.d) {
                    frontier.push(Reverse(Cand { d, id: n }));
                    best.push(Cand { d, id: n });
                    if best.len() > ef {
                        best.pop();
                    }
                }
            }
        }
        Ok(best.into_sorted_vec())
    }

    /// Greedy descent from the entry point to layer `down_to` (exclusive of
    /// nothing: the returned entry points are for `down_to`).
    pub(super) fn descend<R: StorageReadTx>(&mut self, rtx: &R, q: &[f32], down_to: u8) -> Result<Vec<Cand>, BknError> {
        let entry = self.meta.entry.expect("descend on an empty index");
        let mut eps = vec![self.cand(rtx, q, entry)?];
        let mut layer = self.meta.max_level;
        while layer > down_to {
            eps = self.search_layer(rtx, q, &eps, 1, layer)?;
            layer -= 1;
        }
        Ok(eps)
    }

    /// The neighbour-selection heuristic: walks `candidates` (nearest
    /// first) keeping one only if it's closer to the base than to every kept
    /// node, which keeps links spread out so the graph stays navigable.
    pub(super) fn select<R: StorageReadTx>(&mut self, rtx: &R, candidates: &[Cand], m: usize) -> Result<Vec<u64>, BknError> {
        let mut kept: Vec<Rc<Node>> = Vec::with_capacity(m);
        let mut out = Vec::with_capacity(m);
        for c in candidates {
            if out.len() >= m {
                break;
            }
            let Some(node) = self.node(rtx, c.id)? else { continue };
            if kept.iter().all(|k| self.dist(&node.vector, &k.vector) > c.d) {
                out.push(c.id);
                kept.push(node);
            }
        }
        Ok(out)
    }

    /// `ids` as `owner`'s links on `layer`: dead ids dropped and, when more
    /// than the layer allows, narrowed down by [`Self::select`].
    pub(super) fn shrink<R: StorageReadTx>(&mut self, rtx: &R, owner: &Node, ids: Vec<u64>, layer: u8) -> Result<Vec<u64>, BknError> {
        let mut cands = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(n) = self.node(rtx, id)? {
                cands.push(Cand { d: self.dist(&owner.vector, &n.vector), id });
            }
        }
        let cap = self.cap(layer);
        if cands.len() <= cap {
            return Ok(cands.into_iter().map(|c| c.id).collect());
        }
        cands.sort();
        self.select(rtx, &cands, cap)
    }

    /// Indexes `vector` for the row `pk` (not yet in the index).
    pub(super) fn add<W: StorageWriteTx>(&mut self, wtx: &mut W, pk: &[u8], vector: Vec<f32>) -> Result<(), BknError> {
        if vector.is_empty() {
            return Ok(());
        }
        if self.meta.count == 0 {
            self.meta.dim = vector.len() as u32;
        } else if vector.len() != self.meta.dim as usize {
            return Err(BknError::InvalidQuery(format!(
                "vector index '{}' holds {}-dimensional vectors; this row has {}",
                self.name,
                self.meta.dim,
                vector.len()
            )));
        }
        let Some(vector) = self.prepare(vector) else { return Ok(()) };

        let id = self.meta.next_id;
        self.meta.next_id += 1;
        let level = level_for(pk, self.m());
        wtx.put(self.nodes, &id.to_be_bytes(), &encode_node(level, pk, &vector))?;
        wtx.put(self.pks, pk, &id.to_be_bytes())?;
        self.meta.count += 1;
        let node = Rc::new(Node { level, pk: pk.to_vec(), vector });
        self.cache.insert(id, Some(node.clone()));

        if self.meta.entry.is_none() {
            self.meta.entry = Some(id);
            self.meta.max_level = level;
            return Ok(());
        }
        let top = level.min(self.meta.max_level);
        let mut eps = self.descend(&*wtx, &node.vector, top)?;
        let ef = (self.meta.ef_construction as usize).max(self.m());
        for layer in (0..=top).rev() {
            let found = self.search_layer(&*wtx, &node.vector, &eps, ef, layer)?;
            let neighbours = self.select(&*wtx, &found, self.m())?;
            self.put_links(wtx, id, layer, &neighbours)?;
            for n in neighbours {
                let Some(other) = self.node(&*wtx, n)? else { continue };
                let mut list = self.links(&*wtx, n, layer)?;
                list.push(id);
                let list = self.shrink(&*wtx, &other, list, layer)?;
                self.put_links(wtx, n, layer, &list)?;
            }
            eps = found;
        }
        if level > self.meta.max_level {
            self.meta.entry = Some(id);
            self.meta.max_level = level;
        }
        Ok(())
    }

    /// Removes the row `pk` from the index (a no-op if it isn't in it).
    pub(super) fn remove<W: StorageWriteTx>(&mut self, wtx: &mut W, pk: &[u8]) -> Result<(), BknError> {
        let Some(raw) = wtx.get(self.pks, pk)? else { return Ok(()) };
        let id = u64::from_be_bytes(raw.as_slice().try_into().map_err(|_| BknError::Corruption("malformed vector index id".into()))?);
        wtx.delete(self.pks, pk)?;
        let node = self.node(&*wtx, id)?;
        wtx.delete(self.nodes, &id.to_be_bytes())?;
        self.cache.insert(id, None);
        let Some(node) = node else { return Ok(()) };
        self.meta.count = self.meta.count.saturating_sub(1);

        // Re-link every neighbour that linked back, offering it the removed
        // node's other neighbours as replacements.
        let mut by_layer = Vec::with_capacity(node.level as usize + 1);
        for layer in 0..=node.level {
            let list = self.links(&*wtx, id, layer)?;
            wtx.delete(self.links, &link_key(id, layer))?;
            for &n in &list {
                let Some(other) = self.node(&*wtx, n)? else { continue };
                let mut theirs = self.links(&*wtx, n, layer)?;
                let before = theirs.len();
                theirs.retain(|&x| x != id);
                if theirs.len() == before {
                    continue;
                }
                for &x in &list {
                    if x != n && !theirs.contains(&x) {
                        theirs.push(x);
                    }
                }
                let theirs = self.shrink(&*wtx, &other, theirs, layer)?;
                self.put_links(wtx, n, layer, &theirs)?;
            }
            by_layer.push(list);
        }

        if self.meta.entry == Some(id) {
            self.meta.entry = None;
            self.meta.max_level = 0;
            // The best surviving neighbour on the highest layer that has one.
            for list in by_layer.iter().rev() {
                let mut best: Option<(u8, u64)> = None;
                for &n in list {
                    if let Some(other) = self.node(&*wtx, n)?
                        && best.is_none_or(|(l, _)| other.level > l)
                    {
                        best = Some((other.level, n));
                    }
                }
                if let Some((level, n)) = best {
                    self.meta.entry = Some(n);
                    self.meta.max_level = level;
                    break;
                }
            }
            if self.meta.entry.is_none() && self.meta.count > 0 {
                // An isolated entry point: fall back to the highest node.
                let mut best: Option<(u8, u64)> = None;
                for kv in wtx.scan(self.nodes, Bound::Unbounded, Bound::Unbounded)? {
                    let (k, v) = kv?;
                    let level = *v.first().ok_or_else(|| BknError::Corruption("malformed vector index node".into()))?;
                    if best.is_none_or(|(l, _)| level > l) {
                        let id = u64::from_be_bytes(k.as_slice().try_into().map_err(|_| BknError::Corruption("malformed vector index id".into()))?);
                        best = Some((level, id));
                    }
                }
                if let Some((level, n)) = best {
                    self.meta.entry = Some(n);
                    self.meta.max_level = level;
                }
            }
        }
        Ok(())
    }

    /// The sortable pks of up to `ef` indexed rows nearest to the prepared
    /// query, nearest first.
    pub(super) fn search<R: StorageReadTx>(&mut self, rtx: &R, q: &[f32], ef: usize) -> Result<Vec<Vec<u8>>, BknError> {
        if self.meta.entry.is_none() {
            return Ok(Vec::new());
        }
        let eps = self.descend(rtx, q, 0)?;
        let found = self.search_layer(rtx, q, &eps, ef, 0)?;
        let mut out = Vec::with_capacity(found.len());
        for c in found {
            if let Some(n) = self.node(rtx, c.id)? {
                out.push(n.pk.clone());
            }
        }
        Ok(out)
    }
}
