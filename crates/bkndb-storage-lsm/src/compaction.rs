//! K-way merge shared by `range()` reads and full compaction: several
//! already-sorted `(key, LsmValue)` sources are combined into one ascending
//! stream, keeping only the most-recent entry per key and dropping
//! tombstones from the output entirely.
use crate::memtable::LsmValue;

/// One sorted source to merge, tagged by recency: rank 0 is always the most
/// recent (e.g. the active memtable, or the newest SSTable generation);
/// higher ranks are progressively older. When several sources share a key,
/// the lowest-rank entry wins.
pub struct MergeSource {
    pub rank: u32,
    pub entries: Vec<(Vec<u8>, LsmValue)>,
}

/// Implemented as sort + dedupe rather than a streaming k-way merge
/// (`BinaryHeap` over per-source iterators) — simpler to get right, and
/// correct regardless of source count or size. The trade-off is holding
/// every source's entries in memory for the duration of one merge, which is
/// acceptable at this project's scale: a merge only ever spans one flush's
/// memtable or one compaction batch's SSTables, never the whole database.
pub fn merge_sources(sources: Vec<MergeSource>) -> Vec<(Vec<u8>, LsmValue)> {
    let mut all: Vec<(Vec<u8>, u32, LsmValue)> = Vec::new();
    for src in sources {
        for (k, v) in src.entries {
            all.push((k, src.rank, v));
        }
    }
    // Sort by key first, then by rank ascending, so each key's most-recent
    // entry (lowest rank) is always the first one seen in its group.
    all.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut out = Vec::new();
    let mut i = 0;
    while i < all.len() {
        let key = all[i].0.clone();
        if !matches!(all[i].2, LsmValue::Tombstone) {
            out.push((key.clone(), all[i].2.clone()));
        }
        i += 1;
        while i < all.len() && all[i].0 == key {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_rank_shadows_older_for_the_same_key() {
        let sources = vec![
            MergeSource {
                rank: 0,
                entries: vec![(b"a".to_vec(), LsmValue::Value(b"new".to_vec()))],
            },
            MergeSource {
                rank: 1,
                entries: vec![(b"a".to_vec(), LsmValue::Value(b"old".to_vec()))],
            },
        ];
        let merged = merge_sources(sources);
        assert_eq!(merged, vec![(b"a".to_vec(), LsmValue::Value(b"new".to_vec()))]);
    }

    #[test]
    fn tombstone_shadows_and_is_then_dropped() {
        let sources = vec![
            MergeSource {
                rank: 0,
                entries: vec![(b"a".to_vec(), LsmValue::Tombstone)],
            },
            MergeSource {
                rank: 1,
                entries: vec![(b"a".to_vec(), LsmValue::Value(b"old".to_vec()))],
            },
        ];
        let merged = merge_sources(sources);
        assert!(merged.is_empty(), "a tombstone must suppress the older value, and not appear in the output itself");
    }

    #[test]
    fn distinct_keys_all_survive_in_sorted_order() {
        let sources = vec![
            MergeSource {
                rank: 0,
                entries: vec![(b"c".to_vec(), LsmValue::Value(b"3".to_vec())), (b"a".to_vec(), LsmValue::Value(b"1".to_vec()))],
            },
            MergeSource {
                rank: 1,
                entries: vec![(b"b".to_vec(), LsmValue::Value(b"2".to_vec()))],
            },
        ];
        let merged = merge_sources(sources);
        let keys: Vec<Vec<u8>> = merged.into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }
}
