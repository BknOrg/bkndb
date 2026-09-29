//! Streaming range cursor over one SSTable.
use super::*;

/// Streaming range read over one SSTable: holds one decoded block at a time.
pub struct SstCursor {
    pub(super) sst: Arc<SstableHandle>,
    pub(super) next_block: usize,
    pub(super) buf: VecDeque<(Vec<u8>, LsmValue)>,
    pub(super) start: Bound<Vec<u8>>,
    pub(super) end: Bound<Vec<u8>>,
    pub(super) done: bool,
}

impl SstCursor {
    pub(super) fn load_next_block(&mut self) -> Result<(), BknError> {
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
