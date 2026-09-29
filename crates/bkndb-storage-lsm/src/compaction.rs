//! Streaming k-way merge shared by range scans, full compaction and backup:
//! several already-sorted `(key, LsmValue)` sources are combined into one
//! ascending stream, keeping only the most recent entry per key and
//! dropping tombstones from the output entirely.
//!
//! Memory use is one pending entry per source (plus whatever each source
//! buffers itself — one block for an SSTable cursor), independent of how
//! much data flows through, which is what lets compaction rewrite a
//! database far larger than RAM.
use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bkndb_core::BknError;

use crate::memtable::LsmValue;

pub type EntryIter<'a> = Box<dyn Iterator<Item = Result<(Vec<u8>, LsmValue), BknError>> + 'a>;

/// Merges `sources`, which must be ordered newest first: when several
/// sources hold the same key, the one with the lowest index wins.
pub struct MergeIter<'a> {
    sources: Vec<EntryIter<'a>>,
    /// The next unconsumed value of each source, parked here while its key
    /// sits in `heap`.
    heads: Vec<Option<LsmValue>>,
    /// Min-heap on `(key, source index)`, so among equal keys the newest
    /// source pops first.
    heap: BinaryHeap<Reverse<(Vec<u8>, usize)>>,
    error: Option<BknError>,
}

impl<'a> MergeIter<'a> {
    pub fn new(sources: Vec<EntryIter<'a>>) -> Self {
        let mut this = Self {
            heads: sources.iter().map(|_| None).collect(),
            sources,
            heap: BinaryHeap::new(),
            error: None,
        };
        for i in 0..this.sources.len() {
            this.advance(i);
        }
        this
    }

    /// Pulls source `i`'s next entry into the heap (or records its error).
    fn advance(&mut self, i: usize) {
        match self.sources[i].next() {
            Some(Ok((key, value))) => {
                self.heads[i] = Some(value);
                self.heap.push(Reverse((key, i)));
            }
            Some(Err(e)) if self.error.is_none() => self.error = Some(e),
            Some(Err(_)) | None => {}
        }
    }
}

impl Iterator for MergeIter<'_> {
    /// Live values only — tombstones have done their job of shadowing.
    type Item = Result<(Vec<u8>, Vec<u8>), BknError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(e) = self.error.take() {
                self.heap.clear();
                return Some(Err(e));
            }
            let Reverse((key, i)) = self.heap.pop()?;
            let value = self.heads[i].take().expect("every heap entry has a parked value");
            self.advance(i);
            // Older versions of the same key: consume and discard.
            while let Some(Reverse((k, j))) = self.heap.peek() {
                if *k != key {
                    break;
                }
                let j = *j;
                self.heap.pop();
                self.heads[j] = None;
                self.advance(j);
            }
            if let LsmValue::Value(v) = value {
                return Some(Ok((key, v)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(entries: Vec<(&str, Option<&str>)>) -> EntryIter<'static> {
        let owned: Vec<_> = entries
            .into_iter()
            .map(|(k, v)| {
                Ok((
                    k.as_bytes().to_vec(),
                    match v {
                        Some(v) => LsmValue::Value(v.as_bytes().to_vec()),
                        None => LsmValue::Tombstone,
                    },
                ))
            })
            .collect();
        Box::new(owned.into_iter())
    }

    fn run(sources: Vec<EntryIter<'static>>) -> Vec<(String, String)> {
        MergeIter::new(sources)
            .map(|r| {
                let (k, v) = r.unwrap();
                (String::from_utf8(k).unwrap(), String::from_utf8(v).unwrap())
            })
            .collect()
    }

    #[test]
    fn newer_source_shadows_older_for_the_same_key() {
        let merged = run(vec![src(vec![("a", Some("new"))]), src(vec![("a", Some("old"))])]);
        assert_eq!(merged, vec![("a".into(), "new".into())]);
    }

    #[test]
    fn tombstone_shadows_and_is_then_dropped() {
        let merged = run(vec![src(vec![("a", None)]), src(vec![("a", Some("old")), ("b", Some("keep"))])]);
        assert_eq!(merged, vec![("b".into(), "keep".into())]);
    }

    #[test]
    fn distinct_keys_all_survive_in_sorted_order() {
        let merged = run(vec![
            src(vec![("a", Some("1")), ("c", Some("3"))]),
            src(vec![("b", Some("2")), ("d", Some("4"))]),
            src(vec![]),
            src(vec![("c", Some("old")), ("e", Some("5"))]),
        ]);
        let keys: Vec<&str> = merged.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c", "d", "e"]);
        assert_eq!(merged[2].1, "3");
    }

    #[test]
    fn source_errors_are_surfaced() {
        let failing: EntryIter<'static> = Box::new(vec![Err(BknError::Corruption("boom".into()))].into_iter());
        let mut it = MergeIter::new(vec![src(vec![("a", Some("1"))]), failing]);
        assert!(matches!(it.next(), Some(Err(BknError::Corruption(_)))));
        assert!(it.next().is_none());
    }
}
