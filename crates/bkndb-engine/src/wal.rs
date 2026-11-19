//! Format v3 frames: magic BKW3 + payload length:u32 BE + payload CRC:u32 BE
//! + CRC of those twelve header bytes:u32 BE + fixed-width bincode payload.
//! An empty record is a synced maintenance fence, never a user commit. Complete
//! corrupt headers/payloads fail normal recovery. Only an incomplete final frame
//! is an interrupted append; explicit repair can salvage a corrupt suffix.
use crate::io::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

use crate::BknError;

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
    if bincode::serialized_size(record).map_err(enc_err)? > MAX_FRAME_BYTES {
        return Err(BknError::ResourceLimit(
            "WAL frame exceeds 64 MiB limit".into(),
        ));
    }
    let payload = bincode::serialize(record).map_err(enc_err)?;
    let crc = crc32fast::hash(&payload);
    let len = u32::try_from(payload.len()).map_err(|_| {
        BknError::Backend(format!(
            "transaction too large for one WAL frame ({} bytes, max 4 GiB)",
            payload.len()
        ))
    })?;

    let mut buf = Vec::with_capacity(16 + payload.len());
    buf.extend_from_slice(b"BKW3");
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&crc.to_be_bytes());
    let header_crc = crc32fast::hash(&buf);
    buf.extend_from_slice(&header_crc.to_be_bytes());
    buf.extend_from_slice(&payload);

    let mut f = file;
    f.seek(SeekFrom::End(0)).map_err(io_err)?;
    f.write_all(&buf)
        .map_err(|e| BknError::CommitUnknown(e.to_string()))?;
    file.sync_data()
        .map_err(|e| BknError::CommitUnknown(e.to_string()))?;
    Ok(())
}

/// Reads every valid frame starting at `start_offset` in order, stopping
/// at an incomplete final frame or valid maintenance fence —
/// the byte range `[start_offset, EOF)` is always pure WAL frames (see
/// `manifest.rs`'s `wal_region_start` doc comment), so this never needs to
/// know where the region "ends" ahead of time.
///
/// Also returns the absolute offset just past the last valid frame. The
/// caller must truncate the file there before appending again: `append`
/// always writes at EOF, so any torn bytes left behind would sit between
/// the old frames and the new ones, and the *next* replay would stop at
/// them and drop every commit made after this recovery.
pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// A durable empty frame fences off uncommitted SSTable/manifest output.
/// No user commit may follow a fence until a new manifest is durable.
pub fn fence(file: &File) -> Result<(), BknError> {
    append(
        file,
        &WalRecord {
            entries: Vec::new(),
        },
    )
}

#[cfg(test)]
pub fn replay_from(file: &File, start_offset: u64) -> Result<(Vec<WalRecord>, u64), BknError> {
    replay_from_mode(file, start_offset, false)
}

#[cfg(test)]
pub fn replay_from_mode(
    file: &File,
    start_offset: u64,
    salvage: bool,
) -> Result<(Vec<WalRecord>, u64), BknError> {
    replay_version(file, start_offset, salvage, 3)
}
#[cfg(any(test, feature = "fuzzing"))]
pub fn replay_version(
    file: &File,
    start_offset: u64,
    salvage: bool,
    version: u32,
) -> Result<(Vec<WalRecord>, u64), BknError> {
    let mut records = Vec::new();
    let (_, end) = replay_visit(file, start_offset, salvage, version, |record| {
        records.push(record);
        Ok(())
    })?;
    Ok((records, end))
}
pub fn replay_visit(
    file: &File,
    start_offset: u64,
    salvage: bool,
    version: u32,
    mut visit: impl FnMut(WalRecord) -> Result<(), BknError>,
) -> Result<(u64, u64), BknError> {
    let file_len = file.metadata().map_err(io_err)?.len();
    if start_offset > file_len {
        return Err(BknError::Corruption("WAL starts past EOF".into()));
    }
    let clone = file.try_clone().map_err(io_err)?;
    let mut reader = BufReader::new(&clone);
    reader.seek(SeekFrom::Start(start_offset)).map_err(io_err)?;
    let mut records = 0u64;
    let mut valid_end = start_offset;
    let header_len = if version >= 3 { 16 } else { 8 };
    loop {
        let mut header = [0u8; 16];
        match reader.read_exact(&mut header[..header_len]) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(io_err(e)),
        }
        let (len, crc) = if version >= 3 {
            if &header[..4] != b"BKW3"
                || crc32fast::hash(&header[..12])
                    != u32::from_be_bytes(header[12..16].try_into().unwrap())
            {
                if salvage {
                    break;
                }
                return Err(BknError::Corruption(format!(
                    "invalid WAL header at {valid_end}"
                )));
            }
            (
                u32::from_be_bytes(header[4..8].try_into().unwrap()) as u64,
                u32::from_be_bytes(header[8..12].try_into().unwrap()),
            )
        } else {
            (
                u32::from_be_bytes(header[..4].try_into().unwrap()) as u64,
                u32::from_be_bytes(header[4..8].try_into().unwrap()),
            )
        };
        if len > MAX_FRAME_BYTES {
            return Err(BknError::ResourceLimit("WAL frame exceeds 64 MiB".into()));
        }
        if valid_end
            .checked_add(header_len as u64 + len)
            .is_none_or(|end| end > file_len)
        {
            break;
        }
        let mut payload = vec![0; len as usize];
        reader.read_exact(&mut payload).map_err(io_err)?;
        if crc32fast::hash(&payload) != crc {
            if salvage {
                break;
            }
            return Err(BknError::Corruption(format!(
                "WAL checksum mismatch at {valid_end}"
            )));
        }
        let record: WalRecord = match crate::decode::bounded(&payload) {
            Ok(r) => r,
            Err(e) => {
                if salvage {
                    break;
                }
                return Err(e);
            }
        };
        if version >= 3 && record.entries.is_empty() {
            break;
        }
        visit(record)?;
        records += 1;
        valid_end += header_len as u64 + len;
    }
    Ok((records, valid_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> (tempfile::TempDir, File) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("container.bkndb");
        let file = crate::container::create_container_file(&path).unwrap();
        (dir, file)
    }

    #[test]
    fn append_then_replay_roundtrip() {
        let (_dir, file) = temp_file();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"a".to_vec(), Some(b"1".to_vec()))],
            },
        )
        .unwrap();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"a".to_vec(), None)],
            },
        )
        .unwrap();

        let (records, _) = replay_from(&file, 0).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0].entries,
            vec![(b"a".to_vec(), Some(b"1".to_vec()))]
        );
        assert_eq!(records[1].entries, vec![(b"a".to_vec(), None)]);
    }

    #[test]
    fn replay_from_a_nonzero_offset_skips_prior_bytes() {
        let (_dir, file) = temp_file();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"before".to_vec(), Some(b"x".to_vec()))],
            },
        )
        .unwrap();
        let offset_after_first = file.metadata().unwrap().len();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"after".to_vec(), Some(b"y".to_vec()))],
            },
        )
        .unwrap();

        let (records, _) = replay_from(&file, offset_after_first).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].entries,
            vec![(b"after".to_vec(), Some(b"y".to_vec()))]
        );
    }

    #[test]
    fn truncated_trailing_frame_is_discarded_not_erroring() {
        let (_dir, file) = temp_file();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"good".to_vec(), Some(b"1".to_vec()))],
            },
        )
        .unwrap();

        // Simulate a crash mid-write: append a truncated frame header with
        // no payload behind it.
        (&file).seek(SeekFrom::End(0)).unwrap();
        let mut header = b"BKW3".to_vec();
        header.extend_from_slice(&999u32.to_be_bytes());
        header.extend_from_slice(&0u32.to_be_bytes());
        let crc = crc32fast::hash(&header);
        header.extend_from_slice(&crc.to_be_bytes());
        (&file).write_all(&header).unwrap();
        (&file).write_all(b"short").unwrap(); // far less than 999 bytes

        let good_end = 16
            + bincode::serialize(&WalRecord {
                entries: vec![(b"good".to_vec(), Some(b"1".to_vec()))],
            })
            .unwrap()
            .len() as u64;
        let (records, valid_end) = replay_from(&file, 0).unwrap();
        assert_eq!(
            valid_end, good_end,
            "valid_end must point just past the last complete frame"
        );
        assert_eq!(
            records.len(),
            1,
            "only the first, complete frame should survive replay"
        );
        assert_eq!(
            records[0].entries,
            vec![(b"good".to_vec(), Some(b"1".to_vec()))]
        );
    }

    #[test]
    fn corrupted_crc_requires_explicit_salvage() {
        let (_dir, file) = temp_file();
        append(
            &file,
            &WalRecord {
                entries: vec![(b"good".to_vec(), Some(b"1".to_vec()))],
            },
        )
        .unwrap();

        let payload = bincode::serialize(&WalRecord {
            entries: vec![(b"bad".to_vec(), Some(b"2".to_vec()))],
        })
        .unwrap();
        (&file).seek(SeekFrom::End(0)).unwrap();
        let mut header = b"BKW3".to_vec();
        header.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        header.extend_from_slice(&0xDEADBEEFu32.to_be_bytes());
        let crc = crc32fast::hash(&header);
        header.extend_from_slice(&crc.to_be_bytes());
        (&file).write_all(&header).unwrap();
        (&file).write_all(&payload).unwrap();

        assert!(matches!(
            replay_from(&file, 0),
            Err(BknError::Corruption(_))
        ));
        let (records, _) = replay_from_mode(&file, 0, true).unwrap();
        assert_eq!(records.len(), 1);
    }
}
