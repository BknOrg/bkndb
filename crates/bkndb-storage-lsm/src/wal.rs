//! Write-ahead log frames within the shared container file: one frame per
//! `commit()`, so a transaction's whole batch of writes becomes durable
//! (or not) as a single atomic unit.
//!
//! `frame = payload_len:u32 BE ++ crc32:u32 BE ++ payload`, where
//! `payload = bincode(WalRecord)`. On recovery, a frame whose declared
//! length runs past EOF, or whose CRC doesn't match, is where replay stops
//! — that frame and everything after it is discarded. This is exactly the
//! "uncommitted write vanishes" guarantee `StorageWriteTx::commit` promises:
//! an in-flight frame that never finished being written to disk is
//! indistinguishable from one that never happened.
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

use bkndb_core::BknError;

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

fn enc_err(e: impl std::fmt::Display) -> BknError {
    BknError::Encoding(e.to_string())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalRecord {
    /// `None` value = a delete (tombstone) for that key.
    pub entries: Vec<(Vec<u8>, Option<Vec<u8>>)>,
}

/// Appends one frame at `file`'s current end and fsyncs before returning —
/// this fsync is what makes `commit()` durable before it returns `Ok` to
/// the caller.
pub fn append(file: &File, record: &WalRecord) -> Result<(), BknError> {
    let payload = bincode::serialize(record).map_err(enc_err)?;
    let crc = crc32fast::hash(&payload);
    let len = payload.len() as u32;

    (&*file).seek(SeekFrom::End(0)).map_err(io_err)?;
    (&*file).write_all(&len.to_be_bytes()).map_err(io_err)?;
    (&*file).write_all(&crc.to_be_bytes()).map_err(io_err)?;
    (&*file).write_all(&payload).map_err(io_err)?;
    file.sync_data().map_err(io_err)?;
    Ok(())
}

/// Reads every valid frame starting at `start_offset` in order, stopping
/// (without error) at the first truncated or checksum-mismatched frame —
/// the byte range `[start_offset, EOF)` is always pure WAL frames (see
/// `manifest.rs`'s `wal_region_start` doc comment), so this never needs to
/// know where the region "ends" ahead of time.
pub fn replay_from(file: &File, start_offset: u64) -> Result<Vec<WalRecord>, BknError> {
    let mut reader = BufReader::new(file.try_clone().map_err(io_err)?);
    reader.seek(SeekFrom::Start(start_offset)).map_err(io_err)?;
    let mut records = Vec::new();

    loop {
        let mut header = [0u8; 8];
        if reader.read_exact(&mut header).is_err() {
            break; // truncated length/crc header: stop here, nothing further to discard
        }
        let len = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
        let expected_crc = u32::from_be_bytes(header[4..8].try_into().unwrap());

        let mut payload = vec![0u8; len];
        if reader.read_exact(&mut payload).is_err() {
            break; // declared length runs past EOF: an in-flight, never-finished write
        }
        if crc32fast::hash(&payload) != expected_crc {
            break; // corrupted frame: treat exactly like a truncated one
        }
        let record: WalRecord = match bincode::deserialize(&payload) {
            Ok(r) => r,
            Err(_) => break,
        };
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> (tempfile::TempDir, File) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("container.bkndb");
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).open(&path).unwrap();
        (dir, file)
    }

    #[test]
    fn append_then_replay_roundtrip() {
        let (_dir, file) = temp_file();
        append(&file, &WalRecord { entries: vec![(b"a".to_vec(), Some(b"1".to_vec()))] }).unwrap();
        append(&file, &WalRecord { entries: vec![(b"a".to_vec(), None)] }).unwrap();

        let records = replay_from(&file, 0).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].entries, vec![(b"a".to_vec(), Some(b"1".to_vec()))]);
        assert_eq!(records[1].entries, vec![(b"a".to_vec(), None)]);
    }

    #[test]
    fn replay_from_a_nonzero_offset_skips_prior_bytes() {
        let (_dir, file) = temp_file();
        append(&file, &WalRecord { entries: vec![(b"before".to_vec(), Some(b"x".to_vec()))] }).unwrap();
        let offset_after_first = file.metadata().unwrap().len();
        append(&file, &WalRecord { entries: vec![(b"after".to_vec(), Some(b"y".to_vec()))] }).unwrap();

        let records = replay_from(&file, offset_after_first).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entries, vec![(b"after".to_vec(), Some(b"y".to_vec()))]);
    }

    #[test]
    fn truncated_trailing_frame_is_discarded_not_erroring() {
        let (_dir, file) = temp_file();
        append(&file, &WalRecord { entries: vec![(b"good".to_vec(), Some(b"1".to_vec()))] }).unwrap();

        // Simulate a crash mid-write: append a truncated frame header with
        // no payload behind it.
        (&file).seek(SeekFrom::End(0)).unwrap();
        (&file).write_all(&999u32.to_be_bytes()).unwrap();
        (&file).write_all(&0u32.to_be_bytes()).unwrap();
        (&file).write_all(b"short").unwrap(); // far less than 999 bytes

        let records = replay_from(&file, 0).unwrap();
        assert_eq!(records.len(), 1, "only the first, complete frame should survive replay");
        assert_eq!(records[0].entries, vec![(b"good".to_vec(), Some(b"1".to_vec()))]);
    }

    #[test]
    fn corrupted_crc_is_discarded() {
        let (_dir, file) = temp_file();
        append(&file, &WalRecord { entries: vec![(b"good".to_vec(), Some(b"1".to_vec()))] }).unwrap();

        let payload = bincode::serialize(&WalRecord { entries: vec![(b"bad".to_vec(), Some(b"2".to_vec()))] }).unwrap();
        (&file).seek(SeekFrom::End(0)).unwrap();
        (&file).write_all(&(payload.len() as u32).to_be_bytes()).unwrap();
        (&file).write_all(&0xDEADBEEFu32.to_be_bytes()).unwrap(); // wrong crc
        (&file).write_all(&payload).unwrap();

        let records = replay_from(&file, 0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].entries, vec![(b"good".to_vec(), Some(b"1".to_vec()))]);
    }
}
