//! The manifest: which SSTable byte ranges currently make up the live
//! database, and where the active WAL region starts. Pure encode/decode —
//! all the file I/O for where a manifest blob physically lives inside the
//! single container file is `engine.rs`'s responsibility (see
//! `container.rs`'s header, which points at the current manifest blob).
use bkndb_core::BknError;

fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SstableRef {
    pub generation: u64,
    /// Absolute byte offset of this SSTable's blob within the container file.
    pub offset: u64,
    /// Total blob length (data section + bloom filter + sparse index + footer).
    pub length: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub next_sstable_id: u64,
    pub sstables: Vec<SstableRef>,
    /// Absolute offset where the active WAL region begins. It has no
    /// explicit length: replay always reads from here to physical EOF,
    /// using the self-describing WAL frame format to know where to stop —
    /// safe because `flush()` (under the single-writer lock) always
    /// advances this past everything already appended before any new
    /// commit can append another frame, so `[wal_region_start, EOF)` is
    /// always pure, contiguous WAL frames.
    pub wal_region_start: u64,
}

impl Manifest {
    pub fn encode(&self) -> Result<Vec<u8>, BknError> {
        bincode::serialize(self).map_err(enc_err)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, BknError> {
        bincode::deserialize(bytes).map_err(enc_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let m = Manifest {
            next_sstable_id: 5,
            sstables: vec![
                SstableRef {
                    generation: 1,
                    offset: 128,
                    length: 500,
                },
                SstableRef {
                    generation: 2,
                    offset: 628,
                    length: 900,
                },
            ],
            wal_region_start: 1528,
        };
        let bytes = m.encode().unwrap();
        let decoded = Manifest::decode(&bytes).unwrap();
        assert_eq!(decoded.next_sstable_id, 5);
        assert_eq!(decoded.wal_region_start, 1528);
        assert_eq!(decoded.sstables.len(), 2);
        assert_eq!(decoded.sstables[1].offset, 628);
    }

    #[test]
    fn same_shaped_manifest_encodes_to_a_stable_length() {
        // engine.rs relies on this: it two-pass-encodes a placeholder
        // manifest to learn the byte length before it knows the real
        // `wal_region_start` value, then re-encodes with the real value at
        // the same length so the final blob's offset math stays correct.
        let a = Manifest {
            next_sstable_id: 1,
            sstables: vec![],
            wal_region_start: 0,
        };
        let b = Manifest {
            next_sstable_id: 1,
            sstables: vec![],
            wal_region_start: u64::MAX,
        };
        assert_eq!(a.encode().unwrap().len(), b.encode().unwrap().len());
    }
}
