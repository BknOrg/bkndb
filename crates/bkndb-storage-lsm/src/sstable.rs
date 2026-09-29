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
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Bound;
use std::sync::Arc;

use bkndb_core::BknError;

use crate::bloom::BloomFilter;
use crate::memtable::LsmValue;

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
    mmap: Arc<memmap2::Mmap>,
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

fn encode_entry(out: &mut Vec<u8>, key: &[u8], value: &LsmValue) {
    out.extend_from_slice(&(key.len() as u32).to_be_bytes());
    out.extend_from_slice(key);
    match value {
        LsmValue::Tombstone => out.push(0),
        LsmValue::Value(bytes) => {
            out.push(1);
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
    }
}

/// `(key, value byte range or None for a tombstone, bytes consumed)`.
type DecodedEntry<'a> = (&'a [u8], Option<std::ops::Range<usize>>, usize);
/// Format v1's index section: `(min_key, max_key, [(key, data offset)])`.
type LegacyIndex = (Vec<u8>, Vec<u8>, Vec<(Vec<u8>, u64)>);

/// Decodes the entry at the start of `slice`: `(key, value_range, consumed)`,
/// with `value_range = None` for a tombstone. Values are left in place so a
/// point lookup copies only the one value it returns.
#[inline]
fn decode_entry(slice: &[u8]) -> Result<DecodedEntry<'_>, BknError> {
    let read_u32 = |at: usize| -> Result<usize, BknError> {
        slice
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes(b.try_into().unwrap()) as usize)
            .ok_or_else(|| corrupt("sstable entry truncated"))
    };
    let key_len = read_u32(0)?;
    let key = slice.get(4..4 + key_len).ok_or_else(|| corrupt("sstable entry truncated"))?;
    let tag = *slice.get(4 + key_len).ok_or_else(|| corrupt("sstable entry truncated"))?;
    let mut consumed = 4 + key_len + 1;
    let value = match tag {
        0 => None,
        1 => {
            let vlen = read_u32(consumed)?;
            consumed += 4;
            if slice.len() < consumed + vlen {
                return Err(corrupt("sstable entry truncated"));
            }
            let range = consumed..consumed + vlen;
            consumed += vlen;
            Some(range)
        }
        other => return Err(corrupt(format!("unknown sstable entry tag {other}"))),
    };
    Ok((key, value, consumed))
}

fn to_lsm_value(block: &[u8], value: Option<std::ops::Range<usize>>) -> LsmValue {
    match value {
        Some(r) => LsmValue::Value(block[r].to_vec()),
        None => LsmValue::Tombstone,
    }
}

fn below_start(key: &[u8], start: &Bound<Vec<u8>>) -> bool {
    match start {
        Bound::Included(b) => key < b.as_slice(),
        Bound::Excluded(b) => key <= b.as_slice(),
        Bound::Unbounded => false,
    }
}

fn past_end(key: &[u8], end: &Bound<Vec<u8>>) -> bool {
    match end {
        Bound::Included(b) => key > b.as_slice(),
        Bound::Excluded(b) => key >= b.as_slice(),
        Bound::Unbounded => false,
    }
}

/// Accumulates entries into blocks and writes each one out as it fills.
struct BlockWriter<'f> {
    out: BufWriter<&'f File>,
    opts: WriteOptions,
    raw: Vec<u8>,
    first_key: Vec<u8>,
    offset: u64,
    blocks: Vec<BlockMeta>,
}

impl BlockWriter<'_> {
    fn push(&mut self, key: &[u8], value: &LsmValue) -> Result<(), BknError> {
        if self.raw.is_empty() {
            self.first_key = key.to_vec();
        }
        encode_entry(&mut self.raw, key, value);
        if self.raw.len() >= self.opts.block_size.max(1) {
            self.finish_block()?;
        }
        Ok(())
    }

    fn finish_block(&mut self) -> Result<(), BknError> {
        if self.raw.is_empty() {
            return Ok(());
        }
        let compressed = if self.opts.compress {
            Some(lz4_flex::block::compress(&self.raw)).filter(|c| c.len() < self.raw.len())
        } else {
            None
        };
        let stored: &[u8] = compressed.as_deref().unwrap_or(&self.raw);
        let too_big = |n: usize| u32::try_from(n).map_err(|_| BknError::Backend(format!("sstable block too large ({n} bytes)")));
        self.out.write_all(stored).map_err(io_err)?;
        self.blocks.push(BlockMeta {
            first_key: std::mem::take(&mut self.first_key),
            offset: self.offset,
            stored_len: too_big(stored.len())?,
            raw_len: too_big(self.raw.len())?,
            compressed: compressed.is_some(),
            crc: crc32fast::hash(stored),
        });
        self.offset += stored.len() as u64;
        self.raw.clear();
        Ok(())
    }
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
    pub fn write<I>(file: &Arc<File>, generation: u64, entries: I, expected_items: usize, opts: WriteOptions) -> Result<Self, BknError>
    where
        I: IntoIterator<Item = Result<(Vec<u8>, LsmValue), BknError>>,
    {
        let base_offset = file.metadata().map_err(io_err)?.len();
        (&**file).seek(SeekFrom::Start(base_offset)).map_err(io_err)?;

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
            debug_assert!(min_key.is_none() || key.as_slice() > max_key.as_slice(), "sstable entries must be strictly ascending");
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
        let bloom_bytes = bincode::serialize(&bloom).map_err(|e| BknError::Encoding(e.to_string()))?;
        let index = IndexV2 {
            min_key: min_key.unwrap_or_default(),
            max_key,
            entry_count,
            blocks: w.blocks,
        };
        let index_bytes = bincode::serialize(&index).map_err(|e| BknError::Encoding(e.to_string()))?;
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
        out.write_all(&meta_crc.finalize().to_be_bytes()).map_err(io_err)?;
        out.write_all(&0u32.to_be_bytes()).map_err(io_err)?;
        out.write_all(MAGIC_V2).map_err(io_err)?;
        out.flush().map_err(io_err)?;
        drop(out);

        let blob_len = index_offset + index_len + FOOTER_V2_LEN;
        Ok(Self {
            _file: file.clone(),
            mmap: map_file(file)?,
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

    pub fn open(file: &Arc<File>, generation: u64, base_offset: u64, blob_len: u64) -> Result<Self, BknError> {
        let read_at = |offset: u64, len: u64| -> Result<Vec<u8>, BknError> {
            let mut buf = vec![0u8; len as usize];
            let mut f = &**file;
            f.seek(SeekFrom::Start(base_offset + offset)).map_err(io_err)?;
            f.read_exact(&mut buf).map_err(|e| corrupt(format!("sstable generation {generation} truncated: {e}")))?;
            Ok(buf)
        };
        if blob_len < FOOTER_V1_LEN {
            return Err(corrupt(format!("sstable generation {generation} is too small to contain a footer")));
        }
        let is_v2 = blob_len >= FOOTER_V2_LEN && read_at(blob_len - 8, 8)? == MAGIC_V2;
        let footer_len = if is_v2 { FOOTER_V2_LEN } else { FOOTER_V1_LEN };
        let footer = read_at(blob_len - footer_len, footer_len)?;
        let field = |i: usize| u64::from_be_bytes(footer[i * 8..i * 8 + 8].try_into().unwrap());
        let (bloom_offset, index_offset, index_len) = (field(0), field(1), field(2));
        if bloom_offset > index_offset || index_offset.checked_add(index_len).is_none_or(|end| end + footer_len > blob_len) {
            return Err(corrupt(format!("sstable generation {generation} has an inconsistent footer")));
        }

        let meta = read_at(bloom_offset, index_offset + index_len - bloom_offset)?;
        let (bloom_bytes, index_bytes) = meta.split_at((index_offset - bloom_offset) as usize);
        if is_v2 {
            let expected = u32::from_be_bytes(footer[24..28].try_into().unwrap());
            if crc32fast::hash(&meta) != expected {
                return Err(corrupt(format!("sstable generation {generation}: bloom/index checksum mismatch")));
            }
        }
        let decode_err = |e: bincode::Error| corrupt(format!("sstable generation {generation}: {e}"));
        let bloom: BloomFilter = bincode::deserialize(bloom_bytes).map_err(decode_err)?;

        let (blocks, entry_count, min_key, max_key) = if is_v2 {
            let index: IndexV2 = bincode::deserialize(index_bytes).map_err(decode_err)?;
            (index.blocks.into_iter().map(Block::from).collect(), Some(index.entry_count), index.min_key, index.max_key)
        } else {
            let (min_key, max_key, sparse): LegacyIndex = bincode::deserialize(index_bytes).map_err(decode_err)?;
            (legacy_blocks(sparse, bloom_offset), None, min_key, max_key)
        };
        if blocks.last().is_some_and(|b: &Block| b.offset + b.stored_len > bloom_offset) {
            return Err(corrupt(format!("sstable generation {generation}: block index points past the data section")));
        }

        Ok(Self {
            _file: file.clone(),
            mmap: map_file(file)?,
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
        let start = (self.base_offset + b.offset) as usize;
        let stored = self
            .mmap
            .get(start..start + b.stored_len as usize)
            .ok_or_else(|| corrupt(format!("sstable generation {} block {i} lies past the end of the file", self.generation)))?;
        if b.crc.is_some_and(|crc| crc32fast::hash(stored) != crc) {
            return Err(corrupt(format!("sstable generation {} block {i}: checksum mismatch", self.generation)));
        }
        if b.compressed {
            lz4_flex::block::decompress(stored, b.raw_len)
                .map(Cow::Owned)
                .map_err(|e| corrupt(format!("sstable generation {} block {i}: {e}", self.generation)))
        } else {
            Ok(Cow::Borrowed(stored))
        }
    }

    /// Index of the block that would contain `key`, if any block could.
    fn block_for(&self, key: &[u8]) -> Option<usize> {
        self.blocks.partition_point(|b| b.first_key.as_slice() <= key).checked_sub(1)
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<LsmValue>, BknError> {
        if self.blocks.is_empty() || key < self.min_key.as_slice() || key > self.max_key.as_slice() {
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
        for i in 0..self.blocks.len() {
            let block = self.block_bytes(i)?;
            let mut pos = 0;
            while pos < block.len() {
                pos += decode_entry(&block[pos..])?.2;
                counts.entries += 1;
            }
            if self.blocks[i].crc.is_some() {
                counts.blocks_verified += 1;
            } else {
                counts.blocks_unchecked += 1;
            }
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

impl From<BlockMeta> for Block {
    fn from(m: BlockMeta) -> Self {
        Block {
            first_key: m.first_key,
            offset: m.offset,
            stored_len: m.stored_len as u64,
            raw_len: m.raw_len as usize,
            compressed: m.compressed,
            crc: Some(m.crc),
        }
    }
}

/// A v1 sparse index marks every Nth entry; the runs between consecutive
/// marks become (unchecked, uncompressed) blocks.
fn legacy_blocks(sparse: Vec<(Vec<u8>, u64)>, data_end: u64) -> Vec<Block> {
    let ends: Vec<u64> = sparse.iter().skip(1).map(|(_, o)| *o).chain(std::iter::once(data_end)).collect();
    sparse
        .into_iter()
        .zip(ends)
        .map(|((first_key, offset), end)| Block {
            first_key,
            offset,
            stored_len: end - offset,
            raw_len: (end - offset) as usize,
            compressed: false,
            crc: None,
        })
        .collect()
}

fn map_file(file: &File) -> Result<Arc<memmap2::Mmap>, BknError> {
    // SAFETY: the container file is only ever appended to (or replaced
    // wholesale by rename) while mapped, never truncated below a live
    // SSTable, and every access is bounds-checked against the mapping.
    Ok(Arc::new(unsafe { memmap2::Mmap::map(file).map_err(io_err)? }))
}

/// Streaming range read over one SSTable: holds one decoded block at a time.
pub struct SstCursor {
    sst: Arc<SstableHandle>,
    next_block: usize,
    buf: VecDeque<(Vec<u8>, LsmValue)>,
    start: Bound<Vec<u8>>,
    end: Bound<Vec<u8>>,
    done: bool,
}

impl SstCursor {
    fn load_next_block(&mut self) -> Result<(), BknError> {
        let block = self.sst.block_bytes(self.next_block)?;
        self.next_block += 1;
        let mut pos = 0;
        while pos < block.len() {
            let (k, value, consumed) = decode_entry(&block[pos..])?;
            let value = value.map(|r| r.start + pos..r.end + pos);
            if !below_start(k, &self.start) {
                self.buf.push_back((k.to_vec(), to_lsm_value(&block, value)));
            }
            pos += consumed;
        }
        Ok(())
    }
}

impl Iterator for SstCursor {
    type Item = Result<(Vec<u8>, LsmValue), BknError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.done {
                return None;
            }
            if let Some((k, v)) = self.buf.pop_front() {
                if past_end(&k, &self.end) {
                    self.done = true;
                    return None;
                }
                return Some(Ok((k, v)));
            }
            if self.next_block >= self.sst.blocks.len() {
                self.done = true;
                return None;
            }
            if let Err(e) = self.load_next_block() {
                self.done = true;
                return Some(Err(e));
            }
        }
    }
}

/// Writes a blob exactly as format v1 did (see the module docs).
#[cfg(test)]
pub(crate) fn write_v1(file: &File, entries: &[(Vec<u8>, LsmValue)], interval: usize) -> (u64, u64) {
    let base = file.metadata().unwrap().len();
    let mut data = Vec::new();
    let mut sparse = Vec::new();
    for (i, (k, v)) in entries.iter().enumerate() {
        if i % interval == 0 {
            sparse.push((k.clone(), data.len() as u64));
        }
        encode_entry(&mut data, k, v);
    }
    let mut bloom = BloomFilter::new(entries.len(), 0.01);
    for (k, _) in entries {
        bloom.insert(k);
    }
    let bloom_bytes = bincode::serialize(&bloom).unwrap();
    let index = (entries[0].0.clone(), entries.last().unwrap().0.clone(), sparse);
    let index_bytes = bincode::serialize(&index).unwrap();
    let bloom_offset = data.len() as u64;
    let index_offset = bloom_offset + bloom_bytes.len() as u64;
    let mut blob = data;
    blob.extend_from_slice(&bloom_bytes);
    blob.extend_from_slice(&index_bytes);
    blob.extend_from_slice(&bloom_offset.to_be_bytes());
    blob.extend_from_slice(&index_offset.to_be_bytes());
    blob.extend_from_slice(&(index_bytes.len() as u64).to_be_bytes());
    let mut f = file;
    f.seek(SeekFrom::Start(base)).unwrap();
    f.write_all(&blob).unwrap();
    (base, blob.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: WriteOptions = WriteOptions { block_size: 64, compress: true };

    fn sample_entries() -> Vec<(Vec<u8>, LsmValue)> {
        (0u32..50)
            .map(|i| (i.to_be_bytes().to_vec(), LsmValue::Value(format!("value-{i}-{}", "x".repeat(20)).into_bytes())))
            .collect()
    }

    fn ok_entries(v: &[(Vec<u8>, LsmValue)]) -> impl Iterator<Item = Result<(Vec<u8>, LsmValue), BknError>> + '_ {
        v.iter().cloned().map(Ok)
    }

    fn temp_file() -> (tempfile::TempDir, Arc<File>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("container.bkndb");
        let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).unwrap();
        (dir, Arc::new(file))
    }

    fn collect(c: SstCursor) -> Vec<u32> {
        c.map(|r| u32::from_be_bytes(r.unwrap().0.as_slice().try_into().unwrap())).collect()
    }

    #[test]
    fn write_then_open_get_roundtrip() {
        for opts in [SMALL, WriteOptions { block_size: 4096, compress: false }] {
            let (_dir, file) = temp_file();
            let entries = sample_entries();
            let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), opts).unwrap();

            let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
            assert!(!handle.is_legacy());
            for (k, v) in &entries {
                assert_eq!(handle.get(k).unwrap().as_ref(), Some(v));
            }
            assert_eq!(handle.get(&999u32.to_be_bytes()).unwrap(), None);
            let counts = handle.verify().unwrap();
            assert_eq!(counts.entries, 50);
            assert_eq!(counts.blocks_unchecked, 0);
        }
    }

    #[test]
    fn small_blocks_compress_and_split() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let h = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
        assert!(h.blocks.len() > 10, "64-byte blocks must split 50 entries into many blocks");
        let raw = SstableHandle::write(&file, 2, ok_entries(&entries), entries.len(), WriteOptions { block_size: 4096, compress: false }).unwrap();
        let packed = SstableHandle::write(&file, 3, ok_entries(&entries), entries.len(), WriteOptions { block_size: 4096, compress: true }).unwrap();
        assert!(packed.blob_len < raw.blob_len, "repetitive values must compress");
    }

    #[test]
    fn cursor_respects_bounds_across_blocks() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let h = Arc::new(SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap());

        let key = |i: u32| i.to_be_bytes().to_vec();
        assert_eq!(collect(h.cursor(Bound::Included(key(10)), Bound::Excluded(key(15)))), vec![10, 11, 12, 13, 14]);
        assert_eq!(collect(h.cursor(Bound::Excluded(key(47)), Bound::Unbounded)), vec![48, 49]);
        assert_eq!(collect(h.cursor(Bound::Unbounded, Bound::Included(key(2)))), vec![0, 1, 2]);
        assert_eq!(collect(h.cursor(Bound::Included(key(60)), Bound::Unbounded)), Vec::<u32>::new());
        assert_eq!(collect(h.cursor(Bound::Unbounded, Bound::Unbounded)).len(), 50);
    }

    #[test]
    fn tombstones_round_trip() {
        let (_dir, file) = temp_file();
        let entries = vec![(b"a".to_vec(), LsmValue::Value(b"1".to_vec())), (b"b".to_vec(), LsmValue::Tombstone)];
        let written = SstableHandle::write(&file, 1, ok_entries(&entries), 2, SMALL).unwrap();
        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
        assert_eq!(handle.get(b"b").unwrap(), Some(LsmValue::Tombstone));
    }

    #[test]
    fn empty_sstable_is_valid() {
        let (_dir, file) = temp_file();
        let written = SstableHandle::write(&file, 1, std::iter::empty(), 0, SMALL).unwrap();
        let handle = Arc::new(SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap());
        assert_eq!(handle.get(b"a").unwrap(), None);
        assert_eq!(handle.cursor(Bound::Unbounded, Bound::Unbounded).count(), 0);
    }

    #[test]
    fn two_blobs_coexist_in_one_shared_file() {
        let (_dir, file) = temp_file();
        let first = vec![(b"a".to_vec(), LsmValue::Value(b"first-a".to_vec()))];
        let second = vec![(b"a".to_vec(), LsmValue::Value(b"second-a".to_vec()))];

        let w1 = SstableHandle::write(&file, 1, ok_entries(&first), 1, SMALL).unwrap();
        let w2 = SstableHandle::write(&file, 2, ok_entries(&second), 1, SMALL).unwrap();
        assert!(w2.base_offset > w1.base_offset, "the second blob must be appended after the first");

        let h1 = SstableHandle::open(&file, 1, w1.base_offset, w1.blob_len).unwrap();
        let h2 = SstableHandle::open(&file, 2, w2.base_offset, w2.blob_len).unwrap();
        assert_eq!(h1.get(b"a").unwrap(), Some(LsmValue::Value(b"first-a".to_vec())));
        assert_eq!(h2.get(b"a").unwrap(), Some(LsmValue::Value(b"second-a".to_vec())));
    }

    #[test]
    fn flipped_data_byte_is_reported_as_corruption() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
        let victim = written.base_offset + 3;
        let mut byte = [0u8];
        (&*file).seek(SeekFrom::Start(victim)).unwrap();
        (&*file).read_exact(&mut byte).unwrap();
        (&*file).seek(SeekFrom::Start(victim)).unwrap();
        (&*file).write_all(&[byte[0] ^ 0xFF]).unwrap();

        let handle = SstableHandle::open(&file, 1, written.base_offset, written.blob_len).unwrap();
        assert!(matches!(handle.get(&0u32.to_be_bytes()), Err(BknError::Corruption(_))));
        assert!(matches!(handle.verify(), Err(BknError::Corruption(_))));
        // Blocks that weren't touched still read fine.
        assert!(handle.get(&49u32.to_be_bytes()).unwrap().is_some());
    }

    #[test]
    fn flipped_index_byte_fails_open() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let written = SstableHandle::write(&file, 1, ok_entries(&entries), entries.len(), SMALL).unwrap();
        let victim = written.base_offset + written.blob_len - FOOTER_V2_LEN - 2;
        (&*file).seek(SeekFrom::Start(victim)).unwrap();
        (&*file).write_all(&[0xAB]).unwrap();
        assert!(matches!(SstableHandle::open(&file, 1, written.base_offset, written.blob_len), Err(BknError::Corruption(_))));
    }

    #[test]
    fn legacy_v1_blobs_remain_readable() {
        let (_dir, file) = temp_file();
        let entries = sample_entries();
        let (base, len) = write_v1(&file, &entries, 4);
        let handle = Arc::new(SstableHandle::open(&file, 1, base, len).unwrap());
        assert!(handle.is_legacy());
        for (k, v) in &entries {
            assert_eq!(handle.get(k).unwrap().as_ref(), Some(v));
        }
        let key = |i: u32| i.to_be_bytes().to_vec();
        assert_eq!(collect(handle.cursor(Bound::Included(key(5)), Bound::Excluded(key(9)))), vec![5, 6, 7, 8]);
        let counts = handle.verify().unwrap();
        assert_eq!((counts.entries, counts.blocks_verified), (50, 0));
    }
}
