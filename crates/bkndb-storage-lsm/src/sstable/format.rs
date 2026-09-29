//! Entry and block encoding, legacy (v1) block index support.
use super::*;

pub(super) fn encode_entry(out: &mut Vec<u8>, key: &[u8], value: &LsmValue) {
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
pub(super) type DecodedEntry<'a> = (&'a [u8], Option<std::ops::Range<usize>>, usize);
/// Format v1's index section: `(min_key, max_key, [(key, data offset)])`.
pub(super) type LegacyIndex = (Vec<u8>, Vec<u8>, Vec<(Vec<u8>, u64)>);

/// Decodes the entry at the start of `slice`: `(key, value_range, consumed)`,
/// with `value_range = None` for a tombstone. Values are left in place so a
/// point lookup copies only the one value it returns.
#[inline]
pub(super) fn decode_entry(slice: &[u8]) -> Result<DecodedEntry<'_>, BknError> {
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

pub(super) fn to_lsm_value(block: &[u8], value: Option<std::ops::Range<usize>>) -> LsmValue {
    match value {
        Some(r) => LsmValue::Value(block[r].to_vec()),
        None => LsmValue::Tombstone,
    }
}

pub(super) fn below_start(key: &[u8], start: &Bound<Vec<u8>>) -> bool {
    match start {
        Bound::Included(b) => key < b.as_slice(),
        Bound::Excluded(b) => key <= b.as_slice(),
        Bound::Unbounded => false,
    }
}

pub(super) fn past_end(key: &[u8], end: &Bound<Vec<u8>>) -> bool {
    match end {
        Bound::Included(b) => key > b.as_slice(),
        Bound::Excluded(b) => key >= b.as_slice(),
        Bound::Unbounded => false,
    }
}

/// Accumulates entries into blocks and writes each one out as it fills.
pub(super) struct BlockWriter<'f> {
    pub(super) out: BufWriter<&'f File>,
    pub(super) opts: WriteOptions,
    pub(super) raw: Vec<u8>,
    pub(super) first_key: Vec<u8>,
    pub(super) offset: u64,
    pub(super) blocks: Vec<BlockMeta>,
}

impl BlockWriter<'_> {
    pub(super) fn push(&mut self, key: &[u8], value: &LsmValue) -> Result<(), BknError> {
        if self.raw.is_empty() {
            self.first_key = key.to_vec();
        }
        encode_entry(&mut self.raw, key, value);
        if self.raw.len() >= self.opts.block_size.max(1) {
            self.finish_block()?;
        }
        Ok(())
    }

    pub(super) fn finish_block(&mut self) -> Result<(), BknError> {
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
pub(super) fn legacy_blocks(sparse: Vec<(Vec<u8>, u64)>, data_end: u64) -> Vec<Block> {
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

pub(super) fn map_file(file: &File) -> Result<Arc<memmap2::Mmap>, BknError> {
    // SAFETY: the container file is only ever appended to (or replaced
    // wholesale by rename) while mapped, never truncated below a live
    // SSTable, and every access is bounds-checked against the mapping.
    Ok(Arc::new(unsafe { memmap2::Mmap::map(file).map_err(io_err)? }))
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
