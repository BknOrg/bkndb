//! Full-text and vector search over relational tables (the `search`
//! feature).
//!
//! **Full-text.** [`RelationalDb::create_fulltext_index`] builds an inverted
//! index over a `Str` column, kept up to date by every insert, update and
//! delete from then on (in the same transaction). Text is split into
//! lowercase runs of Unicode letters and digits — no stemming or stop words,
//! so it's language-neutral. [`RelationalDb::search_text`] ranks rows with
//! BM25; a query word ending in `*` matches as a prefix.
//!
//! Storage: `<table>__fts_<column>` holds postings
//! `term ++ 0x00 ++ sortable(pk)` → term frequency, `<table>__ftsdoc_<column>`
//! holds `sortable(pk)` → document length, and the shared `meta` table holds
//! the registry (`relfts:<table>`) and corpus totals
//! (`relftsstat:<table>:<column>`).
//!
//! **Vectors.** [`RelationalDb::search_vector`] is exact k-nearest-neighbour
//! search over a column holding embeddings — either a `List` of numbers, or
//! `Bytes` of packed little-endian `f32`s (4 bytes per dimension, far more
//! compact). It streams the table (or only the rows an optional filter
//! selects, using the query planner) keeping the best `limit` in a heap:
//! linear time, fine up to a few hundred thousand rows; there's no
//! approximate (HNSW-style) index.
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::ops::Bound;

use crate::relational::catalog;
use crate::relational::codec::{decode_sortable, intern, meta_table, sortable_encode};
use crate::relational::db::{get_in, scan_all_in, RelationalDb, Row};
use crate::relational::query::{matching_rows, Query};
use crate::relational::schema::{ColumnKind, TableSchema};
use crate::relational::Expr;
use crate::value::{PropValue, Properties};
use crate::{BknError, StorageBackend, StorageReadTx, StorageWriteTx, TableSpec};

// ---------------------------------------------------------------------------
// Storage layout
// ---------------------------------------------------------------------------

fn registry_key(table: &str) -> Vec<u8> {
    format!("relfts:{table}").into_bytes()
}

fn stats_key(table: &str, column: &str) -> Vec<u8> {
    format!("relftsstat:{table}:{column}").into_bytes()
}

fn postings_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__fts_{column}")))
}

fn docs_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__ftsdoc_{column}")))
}

fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

/// Columns of `table` that have a full-text index.
pub(crate) fn fulltext_columns_in<R: StorageReadTx>(rtx: &R, table: &str) -> Result<Vec<String>, BknError> {
    match rtx.get(meta_table(), &registry_key(table))? {
        Some(bytes) => bincode::deserialize(&bytes).map_err(enc_err),
        None => Ok(Vec::new()),
    }
}

fn save_columns<W: StorageWriteTx>(wtx: &mut W, table: &str, columns: &[String]) -> Result<(), BknError> {
    if columns.is_empty() {
        wtx.delete(meta_table(), &registry_key(table))
    } else {
        wtx.put(meta_table(), &registry_key(table), &bincode::serialize(columns).map_err(enc_err)?)
    }
}

/// `(documents, total terms)` indexed for one column.
fn load_stats<R: StorageReadTx>(rtx: &R, table: &str, column: &str) -> Result<(u64, u64), BknError> {
    match rtx.get(meta_table(), &stats_key(table, column))? {
        Some(b) if b.len() == 16 => Ok((u64::from_be_bytes(b[..8].try_into().unwrap()), u64::from_be_bytes(b[8..].try_into().unwrap()))),
        Some(_) => Err(BknError::Corruption(format!("full-text statistics for '{table}.{column}' are malformed"))),
        None => Ok((0, 0)),
    }
}

fn save_stats<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str, docs: u64, terms: u64) -> Result<(), BknError> {
    let mut b = docs.to_be_bytes().to_vec();
    b.extend_from_slice(&terms.to_be_bytes());
    wtx.put(meta_table(), &stats_key(table, column), &b)
}

// ---------------------------------------------------------------------------
// Tokenizing and index maintenance
// ---------------------------------------------------------------------------

/// Splits text into lowercase runs of Unicode letters/digits.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).map(str::to_lowercase).collect()
}

fn postings_key(term: &str, pk: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(term.len() + 1 + pk.len());
    k.extend_from_slice(term.as_bytes());
    k.push(0);
    k.extend_from_slice(pk);
    k
}

fn text_of<'p>(values: Option<&'p Properties>, column: &str) -> Option<&'p str> {
    match values?.get(column)? {
        PropValue::Str(s) => Some(s),
        _ => None,
    }
}

fn index_text<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str, pk: &[u8], text: &str, sign: i64) -> Result<(), BknError> {
    let tokens = tokenize(text);
    let mut tf: BTreeMap<&str, u32> = BTreeMap::new();
    for t in &tokens {
        *tf.entry(t).or_default() += 1;
    }
    let postings = postings_table(table, column);
    for (term, n) in &tf {
        if sign > 0 {
            wtx.put(postings, &postings_key(term, pk), &n.to_be_bytes())?;
        } else {
            wtx.delete(postings, &postings_key(term, pk))?;
        }
    }
    if sign > 0 {
        wtx.put(docs_table(table, column), pk, &(tokens.len() as u32).to_be_bytes())?;
    } else {
        wtx.delete(docs_table(table, column), pk)?;
    }
    let (docs, terms) = load_stats(&*wtx, table, column)?;
    let len = tokens.len() as u64;
    let (docs, terms) = if sign > 0 { (docs + 1, terms + len) } else { (docs.saturating_sub(1), terms.saturating_sub(len)) };
    save_stats(wtx, table, column, docs, terms)
}

/// Keeps every full-text index of `schema`'s table in step with one row
/// change (`old`/`new` are the row's non-pk values before/after; `None` =
/// absent). Called by every relational write path.
pub(crate) fn on_row_change<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &PropValue,
    old: Option<&Properties>,
    new: Option<&Properties>,
) -> Result<(), BknError> {
    let columns = fulltext_columns_in(&*wtx, schema.name())?;
    if columns.is_empty() {
        return Ok(());
    }
    let pk = sortable_encode(pk)?;
    for column in &columns {
        let (before, after) = (text_of(old, column), text_of(new, column));
        if before == after {
            continue;
        }
        if let Some(t) = before {
            index_text(wtx, schema.name(), column, &pk, t, -1)?;
        }
        if let Some(t) = after {
            index_text(wtx, schema.name(), column, &pk, t, 1)?;
        }
    }
    Ok(())
}

fn clear<W: StorageWriteTx>(wtx: &mut W, table: TableSpec) -> Result<(), BknError> {
    for (k, _) in wtx.range(table, Bound::Unbounded, Bound::Unbounded)? {
        wtx.delete(table, &k)?;
    }
    Ok(())
}

fn drop_index_data<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<(), BknError> {
    clear(wtx, postings_table(table, column))?;
    clear(wtx, docs_table(table, column))?;
    wtx.delete(meta_table(), &stats_key(table, column))
}

/// Drops the full-text indexes of columns `schema` no longer has as `Str`
/// (after a migration), or of every column (`schema = None`, table dropped).
pub(crate) fn retain_valid_in<W: StorageWriteTx>(wtx: &mut W, table: &str, schema: Option<&TableSchema>) -> Result<(), BknError> {
    let columns = fulltext_columns_in(&*wtx, table)?;
    let (keep, gone): (Vec<String>, Vec<String>) = columns
        .into_iter()
        .partition(|c| schema.is_some_and(|s| s.column(c).is_some_and(|col| col.kind == ColumnKind::Str)));
    if gone.is_empty() {
        return Ok(());
    }
    for c in &gone {
        drop_index_data(wtx, table, c)?;
    }
    save_columns(wtx, table, &keep)
}

pub(crate) fn create_fulltext_index_in<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<bool, BknError> {
    let schema = catalog::require_schema_in(&*wtx, table)?;
    match schema.column(column) {
        Some(c) if c.kind == ColumnKind::Str && column != schema.primary_key() => {}
        Some(_) => {
            return Err(BknError::SchemaMismatch {
                table: table.to_string(),
                message: format!("full-text indexes need a non-key Str column; '{column}' isn't one"),
            });
        }
        None => {
            return Err(BknError::SchemaMismatch { table: table.to_string(), message: format!("unknown column '{column}'") });
        }
    }
    let mut columns = fulltext_columns_in(&*wtx, table)?;
    if columns.iter().any(|c| c == column) {
        return Ok(false);
    }
    // Start from a clean slate, then backfill.
    drop_index_data(wtx, table, column)?;
    columns.push(column.to_string());
    save_columns(wtx, table, &columns)?;
    for row in scan_all_in(&*wtx, &schema)? {
        if let Some(text) = text_of(Some(&row.values), column) {
            let text = text.to_string();
            index_text(wtx, table, column, &sortable_encode(&row.pk)?, &text, 1)?;
        }
    }
    Ok(true)
}

pub(crate) fn drop_fulltext_index_in<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<bool, BknError> {
    let mut columns = fulltext_columns_in(&*wtx, table)?;
    let Some(pos) = columns.iter().position(|c| c == column) else {
        return Ok(false);
    };
    columns.remove(pos);
    drop_index_data(wtx, table, column)?;
    save_columns(wtx, table, &columns)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// A search hit: the row and its score (BM25 for text; for vectors the
/// cosine similarity, dot product, or Euclidean distance).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredRow {
    pub row: Row,
    pub score: f64,
}

fn validated_filter(schema: &TableSchema, filter: Option<&Expr>) -> Result<Option<Query>, BknError> {
    let Some(f) = filter else { return Ok(None) };
    let q = Query::new().filter(f.clone());
    q.check_columns(schema, [])?;
    Ok(Some(q))
}

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// BM25-ranked rows whose `column` matches `query`.
///
/// Every query word adds to the score of the rows containing it; with
/// `match_all`, only rows containing every word qualify. `word*` matches any
/// term starting with `word`. `filter` further restricts the rows.
pub(crate) fn search_text_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    query: &str,
    limit: usize,
    match_all: bool,
    filter: Option<&Expr>,
) -> Result<Vec<ScoredRow>, BknError> {
    let table = schema.name();
    if !fulltext_columns_in(rtx, table)?.iter().any(|c| c == column) {
        return Err(BknError::InvalidQuery(format!("'{table}.{column}' has no full-text index (create one first)")));
    }
    let filter = validated_filter(schema, filter)?;
    // Query terms, each (text, is_prefix). A `*` applies to the word's last token.
    let mut terms: Vec<(String, bool)> = Vec::new();
    for word in query.split_whitespace() {
        let prefix = word.ends_with('*');
        let tokens = tokenize(word.trim_end_matches('*'));
        let n = tokens.len();
        for (i, t) in tokens.into_iter().enumerate() {
            let entry = (t, prefix && i + 1 == n);
            if !terms.contains(&entry) {
                terms.push(entry);
            }
        }
    }
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let (docs, total) = load_stats(rtx, table, column)?;
    let avgdl = if docs == 0 { 1.0 } else { (total as f64 / docs as f64).max(1.0) };
    let postings = postings_table(table, column);
    let doc_lens = docs_table(table, column);
    let mut lens: HashMap<Vec<u8>, f64> = HashMap::new();
    // pk bytes -> (score, bitmask of query terms matched)
    let mut hits: HashMap<Vec<u8>, (f64, u64)> = HashMap::new();
    for (i, (term, prefix)) in terms.iter().enumerate() {
        let lo = if *prefix { term.as_bytes().to_vec() } else { postings_key(term, &[]) };
        let mut hi = lo.clone();
        // Exclusive upper bound: everything starting with `lo`.
        while let Some(last) = hi.pop() {
            if last < 0xFF {
                hi.push(last + 1);
                break;
            }
        }
        let hi_bound = if hi.is_empty() { Bound::Unbounded } else { Bound::Excluded(hi.as_slice()) };
        // Group postings by exact term (a prefix may expand to many terms).
        let mut by_term: BTreeMap<Vec<u8>, Vec<(Vec<u8>, u32)>> = BTreeMap::new();
        for kv in rtx.scan(postings, Bound::Included(lo.as_slice()), hi_bound)? {
            let (k, v) = kv?;
            let split = k.iter().position(|&b| b == 0).ok_or_else(|| BknError::Corruption("malformed full-text posting".into()))?;
            let tf = u32::from_be_bytes(v.as_slice().try_into().map_err(|_| BknError::Corruption("malformed term frequency".into()))?);
            by_term.entry(k[..split].to_vec()).or_default().push((k[split + 1..].to_vec(), tf));
        }
        for list in by_term.into_values() {
            let df = list.len() as f64;
            let idf = (1.0 + (docs as f64 - df + 0.5) / (df + 0.5)).ln();
            for (pk, tf) in list {
                let dl = match lens.get(&pk) {
                    Some(l) => *l,
                    None => {
                        let l = rtx.get(doc_lens, &pk)?.and_then(|b| b.try_into().ok()).map_or(avgdl, |b: [u8; 4]| u32::from_be_bytes(b) as f64);
                        lens.insert(pk.clone(), l);
                        l
                    }
                };
                let tf = tf as f64;
                let s = idf * tf * (BM25_K1 + 1.0) / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * dl / avgdl));
                let e = hits.entry(pk).or_insert((0.0, 0));
                e.0 += s;
                e.1 |= 1u64 << (i.min(63));
            }
        }
    }
    let all_mask = if terms.len() >= 64 { u64::MAX } else { (1u64 << terms.len()) - 1 };
    let mut ranked: Vec<(Vec<u8>, f64)> =
        hits.into_iter().filter(|(_, (_, mask))| !match_all || *mask == all_mask).map(|(pk, (s, _))| (pk, s)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let pk_kind = schema.primary_key_column().kind;
    let mut out = Vec::new();
    for (pk, score) in ranked {
        let Some(row) = get_in(rtx, schema, &decode_sortable(pk_kind, &pk)?)? else { continue };
        if filter.as_ref().is_some_and(|q| !q.filters.iter().all(|f| f.eval(schema, &row))) {
            continue;
        }
        out.push(ScoredRow { row, score });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Distance/similarity used by [`RelationalDb::search_vector`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorMetric {
    /// Cosine similarity, highest first (zero-length vectors never match).
    Cosine,
    /// Dot product, highest first.
    Dot,
    /// Euclidean distance, lowest first.
    Euclidean,
}

/// A stored embedding as `f32`s: a `List` of numbers, or `Bytes` holding
/// little-endian `f32`s.
pub fn vector_of(value: &PropValue) -> Option<Vec<f32>> {
    match value {
        PropValue::List(items) => items
            .iter()
            .map(|v| match v {
                PropValue::Float(f) => Some(*f as f32),
                PropValue::Int(i) => Some(*i as f32),
                _ => None,
            })
            .collect(),
        PropValue::Bytes(b) if b.len() % 4 == 0 => Some(b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()),
        _ => None,
    }
}

/// Packs a vector as `Bytes` of little-endian `f32`s — the compact storage
/// form [`RelationalDb::search_vector`] reads.
pub fn pack_vector(v: &[f32]) -> PropValue {
    PropValue::Bytes(v.iter().flat_map(|x| x.to_le_bytes()).collect())
}

struct Candidate {
    /// Higher = better, whatever the metric.
    goodness: f64,
    seq: usize,
    score: f64,
    row: Row,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Candidate {}
impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Candidate {
    /// Reversed, so a `BinaryHeap` pops the *worst* kept candidate first.
    fn cmp(&self, other: &Self) -> Ordering {
        other.goodness.total_cmp(&self.goodness).then_with(|| self.seq.cmp(&other.seq))
    }
}

pub(crate) fn search_vector_in<R: StorageReadTx>(
    rtx: &R,
    schema: &TableSchema,
    column: &str,
    query: &[f32],
    limit: usize,
    metric: VectorMetric,
    filter: Option<&Expr>,
) -> Result<Vec<ScoredRow>, BknError> {
    match schema.column(column).map(|c| c.kind) {
        Some(ColumnKind::List | ColumnKind::Bytes) => {}
        Some(_) => {
            return Err(BknError::SchemaMismatch {
                table: schema.name().to_string(),
                message: format!("vector search needs a List or Bytes column; '{column}' isn't one"),
            });
        }
        None => {
            return Err(BknError::SchemaMismatch { table: schema.name().to_string(), message: format!("unknown column '{column}'") });
        }
    }
    if query.is_empty() {
        return Err(BknError::InvalidQuery("the query vector is empty".into()));
    }
    let q = validated_filter(schema, filter)?.unwrap_or_default();
    let qnorm = query.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(limit + 1);
    if limit == 0 {
        return Ok(Vec::new());
    }
    for (seq, row) in matching_rows(rtx, schema, &q)?.enumerate() {
        let row = row?;
        let Some(v) = row.values.get(column).and_then(vector_of) else { continue };
        if v.len() != query.len() {
            return Err(BknError::InvalidQuery(format!(
                "row {:?} has a {}-dimensional vector, the query has {}",
                row.pk,
                v.len(),
                query.len()
            )));
        }
        let dot: f64 = v.iter().zip(query).map(|(a, b)| *a as f64 * *b as f64).sum();
        let (score, goodness) = match metric {
            VectorMetric::Dot => (dot, dot),
            VectorMetric::Cosine => {
                let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
                if norm == 0.0 || qnorm == 0.0 {
                    continue;
                }
                let c = dot / (norm * qnorm);
                (c, c)
            }
            VectorMetric::Euclidean => {
                let d = v.iter().zip(query).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum::<f64>().sqrt();
                (d, -d)
            }
        };
        heap.push(Candidate { goodness, seq, score, row });
        if heap.len() > limit {
            heap.pop();
        }
    }
    let mut best = heap.into_vec();
    best.sort_by(|a, b| b.goodness.total_cmp(&a.goodness).then_with(|| a.seq.cmp(&b.seq)));
    Ok(best.into_iter().map(|c| ScoredRow { row: c.row, score: c.score }).collect())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl<B: StorageBackend> RelationalDb<B> {
    /// Builds a full-text index over a `Str` column of a registered table
    /// (backfilling existing rows). Returns `false` if it already exists.
    pub fn create_fulltext_index(&self, table: &str, column: &str) -> Result<bool, BknError> {
        self.write(|wtx| create_fulltext_index_in(wtx, table, column))
    }

    /// Removes a full-text index; `false` if there was none.
    pub fn drop_fulltext_index(&self, table: &str, column: &str) -> Result<bool, BknError> {
        self.write(|wtx| drop_fulltext_index_in(wtx, table, column))
    }

    /// Columns of `table` with a full-text index.
    pub fn fulltext_indexes(&self, table: &str) -> Result<Vec<String>, BknError> {
        fulltext_columns_in(&self.backend.begin_read()?, table)
    }

    /// Up to `limit` rows whose `column` best matches `query` (BM25). See
    /// the [module docs](self).
    pub fn search_text(
        &self,
        table: &str,
        column: &str,
        query: &str,
        limit: usize,
        match_all: bool,
        filter: Option<&Expr>,
    ) -> Result<Vec<ScoredRow>, BknError> {
        let rtx = self.backend.begin_read()?;
        let schema = catalog::require_schema_in(&rtx, table)?;
        search_text_in(&rtx, &schema, column, query, limit, match_all, filter)
    }

    /// The `limit` rows whose embedding in `column` is nearest to `query`.
    /// See the [module docs](self).
    pub fn search_vector(
        &self,
        table: &str,
        column: &str,
        query: &[f32],
        limit: usize,
        metric: VectorMetric,
        filter: Option<&Expr>,
    ) -> Result<Vec<ScoredRow>, BknError> {
        let rtx = self.backend.begin_read()?;
        let schema = catalog::require_schema_in(&rtx, table)?;
        search_vector_in(&rtx, &schema, column, query, limit, metric, filter)
    }
}
