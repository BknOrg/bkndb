//! Bounded parser entry points for libFuzzer; never touches the native filesystem.
use crate::{
    io::{OpenMode, StorageIo},
    testing::FaultIo,
};
use std::{path::Path, sync::Arc};
pub fn decode(kind: u8, input: &[u8]) {
    if input.len() > 1024 * 1024 {
        return;
    }
    let io = FaultIo::default();
    let file = Arc::new(io.open(Path::new("fuzz"), OpenMode::CreateNew).unwrap());
    file.write_all_at(0, input).unwrap();
    match kind % 6 {
        0 => {
            let _ = crate::container::read_header(&file);
        }
        1 => {
            let _ = crate::manifest::Manifest::decode(input)
                .and_then(|m| m.validate(128, input.len() as u64, 1024 * 1024));
        }
        2 => {
            let _ = crate::wal::replay_version(&file, 0, false, 3);
        }
        3 => {
            if let Ok(s) = crate::sstable::SstableHandle::open(&file, 1, 0, input.len() as u64) {
                let _ = s.verify();
            }
        }
        4 => {
            if let Ok(b) = crate::bloom::BloomFilter::decode(input) {
                if b.validate().is_ok() {
                    let _ = b.might_contain(b"probe");
                }
            }
        }
        _ => crate::sstable::fuzz_entry(input),
    }
}
