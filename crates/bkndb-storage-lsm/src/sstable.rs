//! SSTable blob format within the shared container file: a sorted run of
//! key/value entries flushed from a memtable, plus a Bloom filter and a
//! sparse index so `get`/`range` don't need to load the whole blob into
//! memory. Every offset here is relative to `base_offset` — the blob's own
//! start within the container file — which is what lets several SSTable
//! blobs coexist as separate byte ranges of one shared `File`.
//!
//! Layout (relative to `base_offset`):
//! ```text
//! [data entries, sorted by key]  key_len:u32 BE ++ key ++ tag:u8(0=tombstone,1=value) ++ [value_len:u32 BE ++ value]
//! [bloom filter section]         bincode(BloomFilter)
//! [index section]                bincode((min_key, max_key, sparse_index: Vec<(key, data_offset)>))
//! [footer, fixed 24 bytes]       bloom_offset:u64 BE ++ index_offset:u64 BE ++ index_len:u64 BE
//! ```
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::ops::Bound;
use std::sync::Arc;

use bkndb_core::BknError;

use crate::bloom::BloomFilter;
use crate::memtable::LsmValue;

const FOOTER_LEN: u64 = 24;

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

pub struct SstableHandle {
    file: Arc<File>,
    pub base_offset: u64,
    pub blob_len: u64,
    bloom: BloomFilter,
    /// Offsets here (and `data_end`) are relative to `base_offset`.
    sparse_index: Vec<(Vec<u8>, u64)>,
    data_end: u64,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub generation: u64,
}

fn write_entry<W: Write>(w: &mut W, key: &[u8], value: &LsmValue) -> Result<u64, BknError> {
    let key_len = key.len() as u32;
    w.write_all(&key_len.to_be_bytes()).map_err(io_err)?;
    w.write_all(key).map_err(io_err)?;
    let mut written = 4u64 + key.len() as u64;
    match value {
        LsmValue::Tombstone => {
            w.write_all(&[0u8]).map_err(io_err)?;
            written += 1;
        }
        LsmValue::Value(bytes) => {
            w.write_all(&[1u8]).map_err(io_err)?;
            let vlen = bytes.len() as u32;
            w.write_all(&vlen.to_be_bytes()).map_err(io_err)?;
            w.write_all(bytes).map_err(io_err)?;
            written += 1 + 4 + bytes.len() as u64;
        }
    }
    Ok(written)
}

fn read_entry<R: Read>(r: &mut R) -> Result<(Vec<u8>, LsmValue, u64), BknError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).map_err(io_err)?;
    let key_len = u32::from_be_bytes(len_buf) as usize;
    let mut key = vec![0u8; key_len];
    r.read_exact(&mut key).map_err(io_err)?;
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag).map_err(io_err)?;
    let mut consumed = 4u64 + key_len as u64 + 1;
    let value = if tag[0] == 0 {
        LsmValue::Tombstone
    } else {
        let mut vlen_buf = [0u8; 4];
        r.read_exact(&mut vlen_buf).map_err(io_err)?;
        let vlen = u32::from_be_bytes(vlen_buf) as usize;
        let mut bytes = vec![0u8; vlen];
        r.read_exact(&mut bytes).map_err(io_err)?;
        consumed += 4 + vlen as u64;
        LsmValue::Value(bytes)
    };
    Ok((key, value, consumed))
}

impl SstableHandle {
    /// Streams `entries` (must already be sorted ascending by key — a
    /// memtable/compaction merge iterator both guarantee this) to `file`,
    /// starting at `file`'s current end-of-file, building the Bloom filter
    /// and sparse index as it goes. Does **not** fsync — the caller
    /// (`flush`/`compact_all`) batches durability together with the
    /// manifest blob that will reference this SSTable, since a bare
    /// SSTable blob isn't meaningful on its own until something points at
    /// it.
    pub fn write<I>(file: &Arc<File>, generation: u64, entries: I, expected_items: usize, sparse_interval: usize) -> Result<Self, BknError>
    where
        I: IntoIterator<Item = (Vec<u8>, LsmValue)>,
    {
        let base_offset = file.metadata().map_err(io_err)?.len();
        (&**file).seek(SeekFrom::Start(base_offset)).map_err(io_err)?;

        let mut bloom = BloomFilter::new(expected_items.max(1), 0.01);
        let mut sparse_index = Vec::new();
        let mut min_key: Option<Vec<u8>> = None;
        let mut max_key: Option<Vec<u8>> = None;
        let mut offset: u64 = 0; // relative to base_offset
        let mut count = 0usize;
        let interval = sparse_interval.max(1);

        // `&File` implements `Write`/`Seek` directly (via `&*file`), so we
        // write straight through the shared handle at its current position
        // — no separate BufWriter needed for correctness, just slightly
        // more syscalls, acceptable at this project's scale.
        let mut w = &**file;
        for (key, value) in entries {
            if min_key.is_none() {
                min_key = Some(key.clone());
            }
            max_key = Some(key.clone());
            bloom.insert(&key);
            if count % interval == 0 {
                sparse_index.push((key.clone(), offset));
            }
            offset += write_entry(&mut w, &key, &value)?;
            count += 1;
        }

        let bloom_offset = offset;
        let bloom_bytes = bincode::serialize(&bloom).map_err(enc_err)?;
        w.write_all(&bloom_bytes).map_err(io_err)?;

        let index_offset = bloom_offset + bloom_bytes.len() as u64;
        let index_payload = (min_key.clone().unwrap_or_default(), max_key.clone().unwrap_or_default(), sparse_index.clone());
        let index_bytes = bincode::serialize(&index_payload).map_err(enc_err)?;
        w.write_all(&index_bytes).map_err(io_err)?;
        let index_len = index_bytes.len() as u64;

        w.write_all(&bloom_offset.to_be_bytes()).map_err(io_err)?;
        w.write_all(&index_offset.to_be_bytes()).map_err(io_err)?;
        w.write_all(&index_len.to_be_bytes()).map_err(io_err)?;

        let blob_len = index_offset + index_len + FOOTER_LEN;

        Ok(Self {
            file: file.clone(),
            base_offset,
            blob_len,
            bloom,
            sparse_index,
            data_end: bloom_offset,
            min_key: min_key.unwrap_or_default(),
            max_key: max_key.unwrap_or_default(),
            generation,
        })
    }

    pub fn open(file: &Arc<File>, generation: u64, base_offset: u64, blob_len: u64) -> Result<Self, BknError> {
        if blob_len < FOOTER_LEN {
            return Err(BknError::Backend("sstable blob too small to contain a footer".to_string()));
        }
        let mut f = &**file;
        f.seek(SeekFrom::Start(base_offset + blob_len - FOOTER_LEN)).map_err(io_err)?;
        let mut footer = [0u8; FOOTER_LEN as usize];
        f.read_exact(&mut footer).map_err(io_err)?;
        let bloom_offset = u64::from_be_bytes(footer[0..8].try_into().unwrap());
        let index_offset = u64::from_be_bytes(footer[8..16].try_into().unwrap());
        let index_len = u64::from_be_bytes(footer[16..24].try_into().unwrap());

        f.seek(SeekFrom::Start(base_offset + bloom_offset)).map_err(io_err)?;
        let mut bloom_bytes = vec![0u8; (index_offset - bloom_offset) as usize];
        f.read_exact(&mut bloom_bytes).map_err(io_err)?;
        let bloom: BloomFilter = bincode::deserialize(&bloom_bytes).map_err(enc_err)?;

        f.seek(SeekFrom::Start(base_offset + index_offset)).map_err(io_err)?;
        let mut index_bytes = vec![0u8; index_len as usize];
        f.read_exact(&mut index_bytes).map_err(io_err)?;
        let (min_key, max_key, sparse_index): (Vec<u8>, Vec<u8>, Vec<(Vec<u8>, u64)>) =
            bincode::deserialize(&index_bytes).map_err(enc_err)?;

        Ok(Self {
            file: file.clone(),
            base_offset,
            blob_len,
            bloom,
            sparse_index,
            data_end: bloom_offset,
            min_key,
            max_key,
            generation,
        })
    }

    fn seek_start_offset(&self, key: &[u8]) -> u64 {
        match self.sparse_index.binary_search_by(|(k, _)| k.as_slice().cmp(key)) {
            Ok(idx) => self.sparse_index[idx].1,
            Err(0) => 0,
            Err(idx) => self.sparse_index[idx - 1].1,
        }
    }

    /// Independent, positioned reader over this blob's own bytes. Never
    /// reopens by path: after a compaction rename, a path no longer refers
    /// to this blob's bytes at all — only this originally-captured `Arc<File>`
    /// still does (see `engine.rs`'s compaction doc comment).
    fn reader(&self) -> Result<BufReader<File>, BknError> {
        Ok(BufReader::new(self.file.try_clone().map_err(io_err)?))
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<LsmValue>, BknError> {
        if self.sparse_index.is_empty() || key < self.min_key.as_slice() || key > self.max_key.as_slice() {
            return Ok(None);
        }
        if !self.bloom.might_contain(key) {
            return Ok(None);
        }
        let mut reader = self.reader()?;
        let mut pos = self.seek_start_offset(key);
        reader.seek(SeekFrom::Start(self.base_offset + pos)).map_err(io_err)?;
        while pos < self.data_end {
            let (k, v, consumed) = read_entry(&mut reader)?;
            pos += consumed;
            match k.as_slice().cmp(key) {
                std::cmp::Ordering::Equal => return Ok(Some(v)),
                std::cmp::Ordering::Greater => return Ok(None),
                std::cmp::Ordering::Less => continue,
            }
        }
        Ok(None)
    }

    /// Ascending `(key, value)` pairs (including tombstones — callers merge
    /// and drop those) within `[start, end)` per the given bounds.
    pub fn range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Result<Vec<(Vec<u8>, LsmValue)>, BknError> {
        if self.sparse_index.is_empty() {
            return Ok(Vec::new());
        }
        // Cheap overlap check against this blob's key range before touching disk.
        if let Bound::Included(k) | Bound::Excluded(k) = end {
            if k < self.min_key.as_slice() {
                return Ok(Vec::new());
            }
        }
        if let Bound::Included(k) | Bound::Excluded(k) = start {
            if k > self.max_key.as_slice() {
                return Ok(Vec::new());
            }
        }

        let start_offset = match start {
            Bound::Included(k) | Bound::Excluded(k) => self.seek_start_offset(k),
            Bound::Unbounded => 0,
        };

        let mut reader = self.reader()?;
        reader.seek(SeekFrom::Start(self.base_offset + start_offset)).map_err(io_err)?;

        let mut out = Vec::new();
        let mut pos = start_offset;
        while pos < self.data_end {
            let (k, v, consumed) = read_entry(&mut reader)?;
            pos += consumed;

            let below_start = match start {
                Bound::Included(b) => k.as_slice() < b,
                Bound::Excluded(b) => k.as_slice() <= b,
                Bound::Unbounded => false,
            };
            if below_start {
                continue;
            }
            let past_end = match end {
                Bound::Included(b) => k.as_slice() > b,
                Bound::Excluded(b) => k.as_slice() >= b,
                Bound::Unbounded => false,
            };
            if past_end {
                break;
            }
            out.push((k, v));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<(Vec<u8>, LsmValue)> {
        (0u32..50)
            .map(|i| (i.to_be_bytes().to_vec(), LsmValue::Value(format!("v{i}").into_bytes())))
            .collect()
    }

    fn temp_file() -> (tempfile::TempDir, Arc<File>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("container.bkndb");
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).open(&path).unwrap();
        (dir, Arc::new(file))
    }

    #[test]
    fn write_then_open_get_roundtrip() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let written = SstableHandle::write(&file, 1, entries.clone(), entries.len(), 4).unwrap();

        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
        for (k, v) in &entries {
            assert_eq!(handle.get(k).unwrap().as_ref(), Some(v));
        }
        assert_eq!(handle.get(&999u32.to_be_bytes()).unwrap(), None);
    }

    #[test]
    fn range_scan_respects_bounds() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let written = SstableHandle::write(&file, 1, entries.clone(), entries.len(), 4).unwrap();
        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();

        let start = 10u32.to_be_bytes();
        let end = 15u32.to_be_bytes();
        let got = handle.range(Bound::Included(&start), Bound::Excluded(&end)).unwrap();
        let got_keys: Vec<u32> = got.iter().map(|(k, _)| u32::from_be_bytes(k.as_slice().try_into().unwrap())).collect();
        assert_eq!(got_keys, vec![10, 11, 12, 13, 14]);
    }

    #[test]
    fn tombstones_round_trip() {
        let (_dir, file) = temp_file();
        let entries = vec![
            (b"a".to_vec(), LsmValue::Value(b"1".to_vec())),
            (b"b".to_vec(), LsmValue::Tombstone),
        ];
        let written = SstableHandle::write(&file, 1, entries, 2, 4).unwrap();
        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
        assert_eq!(handle.get(b"b").unwrap(), Some(LsmValue::Tombstone));
    }

    #[test]
    fn two_blobs_coexist_in_one_shared_file() {
        let (_dir, file) = temp_file();
        let first = vec![(b"a".to_vec(), LsmValue::Value(b"first-a".to_vec()))];
        let second = vec![(b"a".to_vec(), LsmValue::Value(b"second-a".to_vec()))];

        let w1 = SstableHandle::write(&file, 1, first, 1, 4).unwrap();
        let w2 = SstableHandle::write(&file, 2, second, 1, 4).unwrap();
        assert!(w2.base_offset > w1.base_offset, "the second blob must be appended after the first");

        let h1 = SstableHandle::open(&file, 1, w1.base_offset, w1.blob_len).unwrap();
        let h2 = SstableHandle::open(&file, 2, w2.base_offset, w2.blob_len).unwrap();
        assert_eq!(h1.get(b"a").unwrap(), Some(LsmValue::Value(b"first-a".to_vec())));
        assert_eq!(h2.get(b"a").unwrap(), Some(LsmValue::Value(b"second-a".to_vec())));
    }
}
