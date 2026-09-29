//! Full-text indexing (tokenizer, postings, maintenance) and BM25 search.
use super::*;

// ---------------------------------------------------------------------------
// Storage layout
// ---------------------------------------------------------------------------

pub(super) fn registry_key(table: &str) -> Vec<u8> {
    format!("relfts:{table}").into_bytes()
}

pub(super) fn stats_key(table: &str, column: &str) -> Vec<u8> {
    format!("relftsstat:{table}:{column}").into_bytes()
}

pub(super) fn postings_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__fts_{column}")))
}

pub(super) fn docs_table(table: &str, column: &str) -> TableSpec {
    TableSpec(intern(&format!("{table}__ftsdoc_{column}")))
}

pub(super) fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

/// Columns of `table` that have a full-text index.
pub(crate) fn fulltext_columns_in<R: StorageReadTx>(rtx: &R, table: &str) -> Result<Vec<String>, BknError> {
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

/// `(documents, total terms)` indexed for one column.
pub(super) fn load_stats<R: StorageReadTx>(rtx: &R, table: &str, column: &str) -> Result<(u64, u64), BknError> {
    match rtx.get(meta_table(), &stats_key(table, column))? {
        Some(b) if b.len() == 16 => Ok((u64::from_be_bytes(b[..8].try_into().unwrap()), u64::from_be_bytes(b[8..].try_into().unwrap()))),
        Some(_) => Err(BknError::Corruption(format!("full-text statistics for '{table}.{column}' are malformed"))),
        None => Ok((0, 0)),
    }
}

pub(super) fn save_stats<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str, docs: u64, terms: u64) -> Result<(), BknError> {
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

pub(super) fn postings_key(term: &str, pk: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(term.len() + 1 + pk.len());
    k.extend_from_slice(term.as_bytes());
    k.push(0);
    k.extend_from_slice(pk);
    k
}

pub(super) fn text_of<'p>(values: Option<&'p Properties>, column: &str) -> Option<&'p str> {
    match values?.get(column)? {
        PropValue::Str(s) => Some(s),
        _ => None,
    }
}

pub(super) fn index_text<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str, pk: &[u8], text: &str, sign: i64) -> Result<(), BknError> {
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

/// Keeps every full-text and vector index of `schema`'s table in step with
/// one row change (`old`/`new` are the row's non-pk values before/after;
/// `None` = absent). Called by every relational write path.
pub(crate) fn on_row_change<W: StorageWriteTx>(
    wtx: &mut W,
    schema: &TableSchema,
    pk: &PropValue,
    old: Option<&Properties>,
    new: Option<&Properties>,
) -> Result<(), BknError> {
    let columns = fulltext_columns_in(&*wtx, schema.name())?;
    let has_ann = !ann::columns_in(&*wtx, schema.name())?.is_empty();
    if columns.is_empty() && !has_ann {
        return Ok(());
    }
    let pk = sortable_encode(pk)?;
    if has_ann {
        ann::on_row_change(wtx, schema, &pk, old, new)?;
    }
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

pub(crate) fn clear<W: StorageWriteTx>(wtx: &mut W, table: TableSpec) -> Result<(), BknError> {
    for (k, _) in wtx.range(table, Bound::Unbounded, Bound::Unbounded)? {
        wtx.delete(table, &k)?;
    }
    Ok(())
}

pub(super) fn drop_index_data<W: StorageWriteTx>(wtx: &mut W, table: &str, column: &str) -> Result<(), BknError> {
    clear(wtx, postings_table(table, column))?;
    clear(wtx, docs_table(table, column))?;
    wtx.delete(meta_table(), &stats_key(table, column))
}

/// Drops the full-text and vector indexes of columns `schema` no longer has
/// (after a migration), or of every column (`schema = None`, table dropped).
pub(crate) fn retain_valid_in<W: StorageWriteTx>(wtx: &mut W, table: &str, schema: Option<&TableSchema>) -> Result<(), BknError> {
    ann::retain_valid_in(wtx, table, schema)?;
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

pub(super) const BM25_K1: f64 = 1.2;
pub(super) const BM25_B: f64 = 0.75;

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
