//! SSTable blob format within the shared container file: a sorted run of
//! key/value entries flushed from a memtable (or merged by compaction),
//! plus a Bloom filter and a block index so `get`/scans only touch the
//! blocks they need. Every offset here is relative to `base_offset` — the
//! blob's own start within the container file — which is what lets several
//! SSTable blobs coexist as separate byte ranges of one shared `File`.
//!
//! Entry encoding (inside a block, after decompression):
//! `key_len:u32 BE ++ key ++ tag:u8(0=tombstone,1=value) ++ [value_len:u32 BE ++ value]`
//!
//! **Format v2** (written by this version):
//! ```text
//! [block]*          entries packed up to ~block_size bytes, each block
//!                   optionally lz4-compressed, with its own crc32
//! [bloom section]   bincode(BloomFilter)
//! [index section]   bincode(IndexV2 { min_key, max_key, entry_count, blocks })
//! [footer, 40 B]    bloom_offset:u64 ++ index_offset:u64 ++ index_len:u64
//!                   ++ meta_crc:u32 (bloom + index bytes) ++ reserved:u32
//!                   ++ magic "BKNSST02"
//! ```
//! Block checksums are verified every time a block is read, and the
//! bloom/index checksum once when the SSTable is opened, so on-disk
//! corruption surfaces as [`BknError::Corruption`] instead of wrong data.
//!
//! **Format v1** (still readable; rewritten as v2 by the next compaction):
//! uncompressed entries with no checksums, a sparse index of
//! `(key, offset)` every N entries, and a 24-byte footer without the magic.
//! Its sparse-index runs are treated as unchecked, uncompressed blocks.
use crate::io::File;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::ops::Bound;
use std::sync::Arc;

use crate::BknError;

use crate::bloom::BloomFilter;
use crate::memtable::LsmValue;

mod cursor;
mod format;
#[cfg(test)]
mod tests;

pub use cursor::*;
#[cfg(test)]
pub(crate) use format::write_v1;
use format::*;

const FOOTER_V1_LEN: u64 = 24;
const FOOTER_V2_LEN: u64 = 40;
const MAGIC_V2: &[u8; 8] = b"BKNSST02";

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

fn corrupt(msg: impl std::fmt::Display) -> BknError {
    BknError::Corruption(msg.to_string())
}

/// How SSTables written from now on store their blocks. Reading always
/// handles both, per block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    /// Target uncompressed size of one block; a block closes as soon as it
    /// reaches this many bytes (so one oversized entry makes one block).
    pub block_size: usize,
    /// lz4-compress each block, keeping the raw bytes whenever compression
    /// doesn't actually make that block smaller.
    pub compress: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BlockMeta {
    first_key: Vec<u8>,
    offset: u64,
    stored_len: u32,
    raw_len: u32,
    compressed: bool,
    crc: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct IndexV2 {
    min_key: Vec<u8>,
    max_key: Vec<u8>,
    entry_count: u64,
    blocks: Vec<BlockMeta>,
}

/// One block as the reader sees it, whatever format it came from.
struct Block {
    first_key: Vec<u8>,
    offset: u64,
    stored_len: u64,
    raw_len: usize,
    compressed: bool,
    /// `None` for legacy v1 runs, which carry no checksum.
    crc: Option<u32>,
}

/// What [`SstableHandle::verify`] found.
#[derive(Debug, Default, Clone, Copy)]
pub struct VerifyCounts {
    pub blocks_verified: u64,
    /// Legacy (v1) blocks: fully decoded, but there's no checksum to compare.
    pub blocks_unchecked: u64,
    pub entries: u64,
}

pub struct SstableHandle {
    _file: Arc<File>,
    pub base_offset: u64,
    pub blob_len: u64,
    bloom: BloomFilter,
    blocks: Vec<Block>,
    /// Exact for v2; `None` for v1, which never recorded it.
    entry_count: Option<u64>,
    pub min_key: Vec<u8>,
    pub max_key: Vec<u8>,
    pub generation: u64,
}

impl SstableHandle {
    /// Streams `entries` (must already be sorted ascending by key, with no
    /// duplicates — memtables and the compaction merge both guarantee this)
    /// to `file`, starting at its current end, as a v2 SSTable blob. Memory
    /// use is one block plus the index, regardless of how many entries
    /// stream through. `expected_items` sizes the Bloom filter.
    ///
    /// Does **not** fsync — the caller (`flush`/compaction/backup) batches
    /// durability with the manifest blob that will reference this SSTable,
    /// since a bare SSTable blob means nothing until something points at it.
    pub fn write<I>(
        file: &Arc<File>,
        generation: u64,
        entries: I,
        expected_items: usize,
        opts: WriteOptions,
    ) -> Result<Self, BknError>
    where
        I: IntoIterator<Item = Result<(Vec<u8>, LsmValue), BknError>>,
    {
        let base_offset = file.metadata().map_err(io_err)?.len();
        (&**file)
            .seek(SeekFrom::Start(base_offset))
            .map_err(io_err)?;

        let mut bloom = BloomFilter::new(expected_items.max(1), 0.01);
        let mut min_key: Option<Vec<u8>> = None;
        let mut max_key: Vec<u8> = Vec::new();
        let mut entry_count = 0u64;
        let mut w = BlockWriter {
            out: BufWriter::with_capacity(64 * 1024, &**file),
            opts,
            raw: Vec::with_capacity(opts.block_size + 64),
            first_key: Vec::new(),
            offset: 0,
            blocks: Vec::new(),
        };

        for entry in entries {
            let (key, value) = entry?;
            if min_key.is_some() && key <= max_key {
                return Err(corrupt("SSTable input not strictly ascending"));
            }
            if min_key.is_none() {
                min_key = Some(key.clone());
            }
            bloom.insert(&key);
            w.push(&key, &value)?;
            max_key = key;
            entry_count += 1;
        }
        w.finish_block()?;

        let bloom_offset = w.offset;
        let bloom_bytes =
            bincode::serialize(&bloom).map_err(|e| BknError::Encoding(e.to_string()))?;
        let index = IndexV2 {
            min_key: min_key.unwrap_or_default(),
            max_key,
            entry_count,
            blocks: w.blocks,
        };
        let index_size =
            bincode::serialized_size(&index).map_err(|e| BknError::Encoding(e.to_string()))?;
        if index_size
            .checked_add(bloom_bytes.len() as u64)
            .is_none_or(|n| n > 64 * 1024 * 1024)
        {
            return Err(BknError::ResourceLimit(
                "SSTable metadata exceeds 64 MiB".into(),
            ));
        }
        let index_bytes =
            bincode::serialize(&index).map_err(|e| BknError::Encoding(e.to_string()))?;
        let index_offset = bloom_offset + bloom_bytes.len() as u64;
        let index_len = index_bytes.len() as u64;
        let mut meta_crc = crc32fast::Hasher::new();
        meta_crc.update(&bloom_bytes);
        meta_crc.update(&index_bytes);

        let mut out = w.out;
        out.write_all(&bloom_bytes).map_err(io_err)?;
        out.write_all(&index_bytes).map_err(io_err)?;
        out.write_all(&bloom_offset.to_be_bytes()).map_err(io_err)?;
        out.write_all(&index_offset.to_be_bytes()).map_err(io_err)?;
        out.write_all(&index_len.to_be_bytes()).map_err(io_err)?;
        out.write_all(&meta_crc.finalize().to_be_bytes())
            .map_err(io_err)?;
        out.write_all(&0u32.to_be_bytes()).map_err(io_err)?;
        out.write_all(MAGIC_V2).map_err(io_err)?;
        out.flush().map_err(io_err)?;
        drop(out);

        let blob_len = index_offset + index_len + FOOTER_V2_LEN;
        Ok(Self {
            _file: file.clone(),
            base_offset,
            blob_len,
            bloom,
            blocks: index.blocks.into_iter().map(Block::from).collect(),
            entry_count: Some(entry_count),
            min_key: index.min_key,
            max_key: index.max_key,
            generation,
        })
    }

    pub fn open(
        file: &Arc<File>,
        generation: u64,
        base_offset: u64,
        blob_len: u64,
    ) -> Result<Self, BknError> {
        let read_at = |offset: u64, len: u64| -> Result<Vec<u8>, BknError> {
            if len > 64 * 1024 * 1024 {
                return Err(BknError::ResourceLimit(
                    "SSTable metadata exceeds 64 MiB".into(),
                ));
            }
            if offset.checked_add(len).is_none_or(|end| end > blob_len) {
                return Err(corrupt("SSTable read outside blob"));
            }
            let size = usize::try_from(len)
                .map_err(|_| corrupt("SSTable metadata exceeds address space"))?;
            let mut buf = vec![0u8; size];
            let absolute = base_offset
                .checked_add(offset)
                .ok_or_else(|| corrupt("SSTable offset overflow"))?;
            file.read_exact_at(absolute, &mut buf)
                .map_err(|e| corrupt(format!("sstable generation {generation} truncated: {e}")))?;
            Ok(buf)
        };
        if base_offset
            .checked_add(blob_len)
            .is_none_or(|end| end > file.metadata().map(|m| m.len()).unwrap_or(0))
        {
            return Err(corrupt("SSTable range outside file"));
        }
        if blob_len < FOOTER_V1_LEN {
            return Err(corrupt(format!(
                "sstable generation {generation} is too small to contain a footer"
            )));
        }
        let is_v2 = blob_len >= FOOTER_V2_LEN && read_at(blob_len - 8, 8)? == MAGIC_V2;
        let footer_len = if is_v2 { FOOTER_V2_LEN } else { FOOTER_V1_LEN };
        let footer = read_at(blob_len - footer_len, footer_len)?;
        let field = |i: usize| u64::from_be_bytes(footer[i * 8..i * 8 + 8].try_into().unwrap());
        let (bloom_offset, index_offset, index_len) = (field(0), field(1), field(2));
        if bloom_offset > index_offset
            || index_offset
                .checked_add(index_len)
                .is_none_or(|end| end.checked_add(footer_len) != Some(blob_len))
        {
            return Err(corrupt(format!(
                "sstable generation {generation} has an inconsistent footer"
            )));
        }

        let meta = read_at(bloom_offset, index_offset + index_len - bloom_offset)?;
        let (bloom_bytes, index_bytes) = meta.split_at((index_offset - bloom_offset) as usize);
        if is_v2 {
            let expected = u32::from_be_bytes(footer[24..28].try_into().unwrap());
            if crc32fast::hash(&meta) != expected {
                return Err(corrupt(format!(
                    "sstable generation {generation}: bloom/index checksum mismatch"
                )));
            }
        }
        let bloom = BloomFilter::decode(bloom_bytes)?;
        bloom.validate()?;

        let (blocks, entry_count, min_key, max_key) = if is_v2 {
            let index: IndexV2 = crate::decode::bounded(index_bytes)?;
            if index.entry_count > bloom_offset / 5
                || (!index.blocks.is_empty() && index.entry_count == 0)
            {
                return Err(corrupt("invalid SSTable entry count"));
            }
            (
                index.blocks.into_iter().map(Block::from).collect(),
                Some(index.entry_count),
                index.min_key,
                index.max_key,
            )
        } else {
            let (min_key, max_key, sparse): LegacyIndex = crate::decode::bounded(index_bytes)?;
            (legacy_blocks(sparse, bloom_offset)?, None, min_key, max_key)
        };
        let mut previous_end = 0;
        let mut previous_key: Option<&[u8]> = None;
        for b in &blocks {
            if b.offset != previous_end
                || b.stored_len == 0
                || b.stored_len > 64 * 1024 * 1024
                || b.raw_len == 0
                || b.raw_len > 64 * 1024 * 1024
                || (!b.compressed && b.raw_len as u64 != b.stored_len)
            {
                return Err(corrupt("invalid SSTable block shape"));
            }
            previous_end = b
                .offset
                .checked_add(b.stored_len)
                .ok_or_else(|| corrupt("block range overflow"))?;
            if previous_end > bloom_offset
                || previous_key.is_some_and(|k| k >= b.first_key.as_slice())
            {
                return Err(corrupt("invalid block index ordering"));
            }
            previous_key = Some(&b.first_key);
        }
        if previous_end != bloom_offset
            || blocks.first().is_some_and(|b| b.first_key != min_key)
            || (!blocks.is_empty() && min_key > max_key)
            || (blocks.is_empty()
                && (entry_count.is_some_and(|n| n != 0)
                    || !min_key.is_empty()
                    || !max_key.is_empty()))
        {
            return Err(corrupt("SSTable index bounds disagree"));
        }

        Ok(Self {
            _file: file.clone(),
            base_offset,
            blob_len,
            bloom,
            blocks,
            entry_count,
            min_key,
            max_key,
            generation,
        })
    }

    /// Number of entries (tombstones included), or an estimate for legacy
    /// SSTables that never recorded it — only used to size Bloom filters.
    pub fn estimated_entries(&self) -> u64 {
        self.entry_count.unwrap_or(self.blocks.len() as u64 * 16)
    }

    pub fn is_legacy(&self) -> bool {
        self.entry_count.is_none()
    }

    /// The (checksum-verified, decompressed) bytes of block `i`.
    fn block_bytes(&self, i: usize) -> Result<Cow<'_, [u8]>, BknError> {
        let b = &self.blocks[i];
        let start = self
            .base_offset
            .checked_add(b.offset)
            .ok_or_else(|| corrupt("block offset overflow"))?;
        let len =
            usize::try_from(b.stored_len).map_err(|_| corrupt("block exceeds address space"))?;
        let mut stored = vec![0; len];
        self._file
            .read_exact_at(start, &mut stored)
            .map_err(|e| corrupt(format!("block read: {e}")))?;
        if b.crc.is_some_and(|crc| crc32fast::hash(&stored) != crc) {
            return Err(corrupt(format!(
                "sstable generation {} block {i}: checksum mismatch",
                self.generation
            )));
        }
        if b.compressed {
            lz4_flex::block::decompress(&stored, b.raw_len)
                .map(Cow::Owned)
                .map_err(|e| {
                    corrupt(format!(
                        "sstable generation {} block {i}: {e}",
                        self.generation
                    ))
                })
        } else {
            Ok(Cow::Owned(stored))
        }
    }

    /// Index of the block that would contain `key`, if any block could.
    fn block_for(&self, key: &[u8]) -> Option<usize> {
        self.blocks
            .partition_point(|b| b.first_key.as_slice() <= key)
            .checked_sub(1)
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<LsmValue>, BknError> {
        if self.blocks.is_empty() || key < self.min_key.as_slice() || key > self.max_key.as_slice()
        {
            return Ok(None);
        }
        if !self.bloom.might_contain(key) {
            return Ok(None);
        }
        let Some(i) = self.block_for(key) else {
            return Ok(None);
        };
        let block = self.block_bytes(i)?;
        let mut pos = 0;
        while pos < block.len() {
            let (k, value, consumed) = decode_entry(&block[pos..])?;
            match k.cmp(key) {
                std::cmp::Ordering::Equal => {
                    let value = value.map(|r| r.start + pos..r.end + pos);
                    return Ok(Some(to_lsm_value(&block, value)));
                }
                std::cmp::Ordering::Greater => return Ok(None),
                std::cmp::Ordering::Less => pos += consumed,
            }
        }
        Ok(None)
    }

    /// Ascending entries (tombstones included — callers merge and drop
    /// them) within the given bounds, decoded one block at a time.
    pub fn cursor(self: &Arc<Self>, start: Bound<Vec<u8>>, end: Bound<Vec<u8>>) -> SstCursor {
        let empty = self.blocks.is_empty()
            || matches!(&end, Bound::Included(k) | Bound::Excluded(k) if k.as_slice() < self.min_key.as_slice())
            || matches!(&start, Bound::Included(k) | Bound::Excluded(k) if k.as_slice() > self.max_key.as_slice());
        let next_block = match &start {
            Bound::Included(k) | Bound::Excluded(k) => self.block_for(k).unwrap_or(0),
            Bound::Unbounded => 0,
        };
        SstCursor {
            sst: self.clone(),
            next_block: if empty { self.blocks.len() } else { next_block },
            buf: VecDeque::new(),
            start,
            end,
            done: empty,
        }
    }

    /// Reads and decodes every block, checking every checksum.
    pub fn verify(&self) -> Result<VerifyCounts, BknError> {
        let mut counts = VerifyCounts::default();
        let mut previous: Option<Vec<u8>> = None;
        for i in 0..self.blocks.len() {
            let block = self.block_bytes(i)?;
            let mut pos = 0;
            while pos < block.len() {
                let (key, _, consumed) = decode_entry(&block[pos..])?;
                if previous.as_ref().is_some_and(|p| p.as_slice() >= key)
                    || (pos == 0 && key != self.blocks[i].first_key)
                    || !self.bloom.might_contain(key)
                {
                    return Err(corrupt("SSTable key ordering or Bloom mismatch"));
                }
                previous = Some(key.to_vec());
                pos += consumed;
                counts.entries += 1;
            }
            if self.blocks[i].crc.is_some() {
                counts.blocks_verified += 1;
            } else {
                counts.blocks_unchecked += 1;
            }
        }
        if previous.as_ref().is_some_and(|p| p != &self.max_key) {
            return Err(corrupt("SSTable maximum key mismatch"));
        }
        if let Some(n) = self.entry_count.filter(|&n| n != counts.entries) {
            return Err(corrupt(format!(
                "sstable generation {}: index records {n} entries but the blocks hold {}",
                self.generation, counts.entries
            )));
        }
        Ok(counts)
    }
}

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_entry(input: &[u8]) {
    let _ = format::decode_entry(input);
}
