//! The single-file `.bkndb` container: a fixed double-buffered header at
//! the start of the file, followed by a body region where WAL frames,
//! SSTable blobs, and manifest blobs are appended at growing offsets.
//!
//! The header is the database's root pointer — it's what lets `open()`
//! find "the current manifest" without a separate named OS file to open
//! directly. A torn write to it would be unrecoverable (unlike a torn WAL
//! frame, which only loses the last uncommitted batch), so it's double
//! buffered: two fixed slots, always writing into the currently-inactive
//! one, so a crash mid-write to one slot leaves the other slot — still
//! pointing at the previous, fully-valid manifest — intact.
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use bkndb_core::BknError;

const MAGIC: &[u8; 8] = b"BKNDBLS1";
const FORMAT_VERSION: u32 = 1;
pub const HEADER_SLOT_LEN: u64 = 64;
pub const BODY_START: u64 = HEADER_SLOT_LEN * 2;

fn io_err(e: std::io::Error) -> BknError {
    BknError::Backend(e.to_string())
}

#[derive(Debug, Clone, Copy)]
pub struct HeaderSlot {
    pub slot: u8,
    pub seq: u64,
    pub manifest_offset: u64,
    pub manifest_len: u64,
}

fn encode_slot(seq: u64, manifest_offset: u64, manifest_len: u64) -> [u8; HEADER_SLOT_LEN as usize] {
    let mut buf = [0u8; HEADER_SLOT_LEN as usize];
    buf[0..8].copy_from_slice(MAGIC);
    buf[8..12].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
    buf[12..20].copy_from_slice(&seq.to_be_bytes());
    buf[20..28].copy_from_slice(&manifest_offset.to_be_bytes());
    buf[28..36].copy_from_slice(&manifest_len.to_be_bytes());
    let crc = crc32fast::hash(&buf[0..36]);
    buf[36..40].copy_from_slice(&crc.to_be_bytes());
    // bytes [40..64) stay zero-filled ("reserved").
    buf
}

fn decode_slot(slot: u8, buf: &[u8; HEADER_SLOT_LEN as usize]) -> Result<Option<HeaderSlot>, BknError> {
    if &buf[0..8] != MAGIC {
        return Ok(None); // uninitialized/foreign slot, not an error — the other slot may still be valid
    }
    let version = u32::from_be_bytes(buf[8..12].try_into().unwrap());
    if version != FORMAT_VERSION {
        return Err(BknError::Backend(format!(
            "unsupported .bkndb format version {version}, expected {FORMAT_VERSION}"
        )));
    }
    let expected_crc = u32::from_be_bytes(buf[36..40].try_into().unwrap());
    if crc32fast::hash(&buf[0..36]) != expected_crc {
        return Ok(None); // torn/corrupt write to this slot — the other slot is the fallback
    }
    let seq = u64::from_be_bytes(buf[12..20].try_into().unwrap());
    let manifest_offset = u64::from_be_bytes(buf[20..28].try_into().unwrap());
    let manifest_len = u64::from_be_bytes(buf[28..36].try_into().unwrap());
    Ok(Some(HeaderSlot {
        slot,
        seq,
        manifest_offset,
        manifest_len,
    }))
}

/// Reads both header slots and returns the valid one with the higher
/// `seq` (a slot is "valid" iff its magic and crc check out and its
/// format version is recognized). `Ok(None)` means the file is too short
/// to contain a header at all — a brand-new, empty database. An
/// unrecognized-but-well-formed format version is a hard error, never
/// silently misinterpreted.
pub fn read_header(file: &File) -> Result<Option<HeaderSlot>, BknError> {
    let len = file.metadata().map_err(io_err)?.len();
    if len < BODY_START {
        return Ok(None);
    }
    let mut buf = vec![0u8; BODY_START as usize];
    (&*file).seek(SeekFrom::Start(0)).map_err(io_err)?;
    (&*file).read_exact(&mut buf).map_err(io_err)?;

    let mut slot0 = [0u8; HEADER_SLOT_LEN as usize];
    slot0.copy_from_slice(&buf[0..HEADER_SLOT_LEN as usize]);
    let mut slot1 = [0u8; HEADER_SLOT_LEN as usize];
    slot1.copy_from_slice(&buf[HEADER_SLOT_LEN as usize..(2 * HEADER_SLOT_LEN) as usize]);

    let a = decode_slot(0, &slot0)?;
    let b = decode_slot(1, &slot1)?;
    Ok(match (a, b) {
        (Some(a), Some(b)) => Some(if a.seq >= b.seq { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => {
            if len == 0 {
                None
            } else {
                return Err(BknError::Backend(
                    "corrupt or unrecognized .bkndb header: neither slot is valid".to_string(),
                ));
            }
        }
    })
}

/// Writes a new header slot (the inactive one relative to `prev_slot`,
/// with `seq = prev_seq + 1`) and fsyncs it — this fsync is the actual
/// commit point for whatever manifest it points at. Returns the new
/// `(seq, slot)` for the caller to remember for the next write.
pub fn write_header(file: &File, prev_seq: u64, prev_slot: u8, manifest_offset: u64, manifest_len: u64) -> Result<(u64, u8), BknError> {
    let new_slot = 1 - prev_slot;
    let new_seq = prev_seq + 1;
    let buf = encode_slot(new_seq, manifest_offset, manifest_len);
    let offset = new_slot as u64 * HEADER_SLOT_LEN;
    (&*file).seek(SeekFrom::Start(offset)).map_err(io_err)?;
    (&*file).write_all(&buf).map_err(io_err)?;
    file.sync_data().map_err(io_err)?;
    Ok((new_seq, new_slot))
}

pub fn write_blob_at(file: &File, offset: u64, bytes: &[u8]) -> Result<(), BknError> {
    (&*file).seek(SeekFrom::Start(offset)).map_err(io_err)?;
    (&*file).write_all(bytes).map_err(io_err)?;
    Ok(())
}

pub fn read_blob(file: &File, offset: u64, len: u64) -> Result<Vec<u8>, BknError> {
    let mut buf = vec![0u8; len as usize];
    (&*file).seek(SeekFrom::Start(offset)).map_err(io_err)?;
    (&*file).read_exact(&mut buf).map_err(io_err)?;
    Ok(buf)
}

/// Opens (creating if absent) the container file at `path`. On Windows,
/// adds `FILE_SHARE_DELETE` so a later compaction's `fs::rename` over this
/// path can succeed while this handle (or a `try_clone()` of it, as every
/// `SstableHandle` holds) is still open and being read from — POSIX allows
/// rename/unlink-while-open transparently, but Windows/NTFS needs this
/// flag for the same effect.
pub fn open_container_file(path: &Path) -> Result<File, BknError> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true);
    apply_share_mode(&mut opts);
    opts.open(path).map_err(io_err)
}

/// Same sharing behavior, but always creates fresh (truncating any
/// existing file) — used for compaction's temp file.
pub fn create_container_file(path: &Path) -> Result<File, BknError> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(true);
    apply_share_mode(&mut opts);
    opts.open(path).map_err(io_err)
}

#[cfg(windows)]
fn apply_share_mode(opts: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
}

#[cfg(not(windows))]
fn apply_share_mode(_opts: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_file_has_no_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bkndb");
        let file = create_container_file(&path).unwrap();
        assert!(read_header(&file).unwrap().is_none());
    }

    #[test]
    fn write_then_read_header_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bkndb");
        let file = open_container_file(&path).unwrap();
        file.set_len(BODY_START).unwrap();
        let (seq, slot) = write_header(&file, 0, 1, 128, 42).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(slot, 0);

        let read = read_header(&file).unwrap().unwrap();
        assert_eq!(read.seq, 1);
        assert_eq!(read.manifest_offset, 128);
        assert_eq!(read.manifest_len, 42);
    }

    #[test]
    fn higher_seq_slot_wins_and_alternates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bkndb");
        let file = open_container_file(&path).unwrap();
        file.set_len(BODY_START).unwrap();

        let (seq1, slot1) = write_header(&file, 0, 1, 100, 10).unwrap();
        let (seq2, slot2) = write_header(&file, seq1, slot1, 200, 20).unwrap();
        assert_ne!(slot1, slot2, "consecutive writes must alternate slots");

        let read = read_header(&file).unwrap().unwrap();
        assert_eq!(read.seq, seq2);
        assert_eq!(read.manifest_offset, 200);
    }

    #[test]
    fn torn_slot_falls_back_to_the_other_valid_slot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bkndb");
        let file = open_container_file(&path).unwrap();
        file.set_len(BODY_START).unwrap();

        let (seq1, slot1) = write_header(&file, 0, 1, 100, 10).unwrap();
        let (_seq2, slot2) = write_header(&file, seq1, slot1, 200, 20).unwrap();

        // Corrupt the newer slot's bytes directly, simulating a crash
        // mid-write to it.
        let offset = slot2 as u64 * HEADER_SLOT_LEN;
        write_blob_at(&file, offset, &[0xFFu8; HEADER_SLOT_LEN as usize]).unwrap();

        let read = read_header(&file).unwrap().unwrap();
        assert_eq!(read.slot, slot1, "must fall back to the older, still-valid slot");
        assert_eq!(read.manifest_offset, 100);
    }

    #[test]
    fn blob_write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bkndb");
        let file = open_container_file(&path).unwrap();
        write_blob_at(&file, BODY_START, b"hello world").unwrap();
        let read = read_blob(&file, BODY_START, 11).unwrap();
        assert_eq!(read, b"hello world");
    }
}
