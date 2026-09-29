//! On-disk layout of a vector index: keys, metadata, node and link encoding.
use super::*;

pub(super) fn registry_key(table: &str) -> Vec<u8> {
    format!("relann:{table}").into_bytes()
}

pub(super) fn meta_key(table: &str, column: &str) -> Vec<u8> {
    format!("relannmeta:{table}:{column}").into_bytes()
}

pub(super) fn nodes_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__ann_{column}")))
}

pub(super) fn links_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__annlnk_{column}")))
}

pub(super) fn pks_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__annpk_{column}")))
}

pub(super) fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AnnMeta {
    pub(super) metric: u8,
    pub(super) m: u32,
    pub(super) ef_construction: u32,
    /// 0 while no vector has been indexed.
    pub(super) dim: u32,
    pub(super) entry: Option<u64>,
    pub(super) max_level: u8,
    pub(super) next_id: u64,
    pub(super) count: u64,
}

pub(super) fn metric_code(metric: VectorMetric) -> u8 {
    match metric {
        VectorMetric::Cosine => 0,
        VectorMetric::Dot => 1,
        VectorMetric::Euclidean => 2,
    }
}

pub(super) fn metric_from(code: u8) -> Result<VectorMetric, BknError> {
    match code {
        0 => Ok(VectorMetric::Cosine),
        1 => Ok(VectorMetric::Dot),
        2 => Ok(VectorMetric::Euclidean),
        other => Err(BknError::Corruption(format!("unknown vector index metric {other}"))),
    }
}

/// Columns of `table` that have a vector index.
pub(crate) fn columns_in<R: StorageReadTx>(rtx: &R, table: &str) -> Result<Vec<String>, BknError> {
    match rtx.get(meta_table(), &registry_key(table))? {
        Some(bytes) => bincode::deserialize(&bytes).map_err(enc_err),
        None => Ok(Vec::new()),
    }
}

pub(super) fn save_columns<W: StorageWriteTx>(wtx: &mut W, table: &str, columns: &[String]) -> Result<(), BknError> {
    if columns.is_empty() {
        wtx.delete(meta_table(), &registry_key(table))
    } else {
        wtx.put(meta_table(), &registry_key(table), &bincode::serialize(columns).map_err(enc_err)?)
    }
}

pub(super) struct Node {
    pub(super) level: u8,
    pub(super) pk: Vec<u8>,
    pub(super) vector: Vec<f32>,
}

pub(super) fn encode_node(level: u8, pk: &[u8], vector: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + pk.len() + vector.len() * 4);
    out.push(level);
    out.extend_from_slice(&(pk.len() as u32).to_le_bytes());
    out.extend_from_slice(pk);
    for x in vector {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub(super) fn decode_node(bytes: &[u8]) -> Result<Node, BknError> {
    let bad = || BknError::Corruption("malformed vector index node".into());
    let (&level, rest) = bytes.split_first().ok_or_else(bad)?;
    let (len, rest) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
    let len = u32::from_le_bytes(*len) as usize;
    if rest.len() < len || !(rest.len() - len).is_multiple_of(4) {
        return Err(bad());
    }
    let (pk, vector) = rest.split_at(len);
    Ok(Node { level, pk: pk.to_vec(), vector: vector.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect() })
}

pub(super) fn link_key(id: u64, layer: u8) -> [u8; 9] {
    let mut k = [0u8; 9];
    k[..8].copy_from_slice(&id.to_be_bytes());
    k[8] = layer;
    k
}

/// Deterministic level for a new node: the usual `floor(-ln(U) / ln(m))`
/// with `U` drawn from a hash of the pk, so a rebuild reproduces the graph.
pub(super) fn level_for(pk: &[u8], m: usize) -> u8 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in pk {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    // splitmix64 finalizer
    h = h.wrapping_add(0x9e37_79b9_7f4a_7c15);
    h = (h ^ (h >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^= h >> 31;
    let u = ((h >> 11) as f64 + 1.0) / (1u64 << 53) as f64; // (0, 1]
    let level = (-u.ln() / (m as f64).ln()).floor();
    (level as u64).min(MAX_LEVEL as u64) as u8
}

/// A node and its distance to whatever is being searched for, ordered by
/// distance (ties by id).
#[derive(Debug, Clone, Copy)]
pub(super) struct Cand {
    pub(super) d: f32,
    pub(super) id: u64,
}

impl PartialEq for Cand {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Cand {}
impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Cand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.d.total_cmp(&other.d).then_with(|| self.id.cmp(&other.id))
    }
}
