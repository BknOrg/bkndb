//! The guest sees five named roles, never a host path or general filesystem.
use bkndb_storage_lsm::io::{Capabilities, File, OpenMode, StorageFile, StorageIo};
use std::{io, path::Path, sync::Arc};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "bkndb_host_v1")]
unsafe extern "C" {
    fn io_call(op: u32, handle: u32, offset: u64, ptr: u32, len: u32) -> i64;
}
fn call(op: u32, handle: u32, offset: u64, ptr: u32, len: u32) -> io::Result<u64> {
    #[cfg(target_arch = "wasm32")]
    // SAFETY: slices passed by the methods below remain valid for the whole
    // synchronous host call. Go validates every range before touching memory.
    let result = unsafe { io_call(op, handle, offset, ptr, len) };
    #[cfg(not(target_arch = "wasm32"))]
    let result = {
        let _ = (op, handle, offset, ptr, len);
        -1
    };
    if result >= 0 {
        return Ok(result as u64);
    }
    Err(match result {
        -2 => io::ErrorKind::WouldBlock,
        -3 => io::ErrorKind::AlreadyExists,
        -4 => io::ErrorKind::NotFound,
        -5 => io::ErrorKind::InvalidInput,
        _ => io::ErrorKind::Other,
    }
    .into())
}
fn role(path: &Path) -> io::Result<u32> {
    match path.to_str() {
        Some("database") => Ok(1),
        Some("database.lock") => Ok(2),
        Some("database.compact.tmp") => Ok(3),
        Some("backup") => Ok(4),
        Some("backup.backup.tmp") => Ok(5),
        _ => Err(io::ErrorKind::PermissionDenied.into()),
    }
}
pub struct HostIo;
struct HostFile(u32);
impl Drop for HostFile {
    fn drop(&mut self) {
        let _ = call(2, self.0, 0, 0, 0);
    }
}
impl StorageFile for HostFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        call(
            3,
            self.0,
            offset,
            buf.as_mut_ptr() as u32,
            buf.len()
                .try_into()
                .map_err(|_| io::ErrorKind::InvalidInput)?,
        )
        .map(|n| n as usize)
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        call(
            4,
            self.0,
            offset,
            buf.as_ptr() as u32,
            buf.len()
                .try_into()
                .map_err(|_| io::ErrorKind::InvalidInput)?,
        )
        .map(|n| n as usize)
    }
    fn len(&self) -> io::Result<u64> {
        call(5, self.0, 0, 0, 0)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        call(6, self.0, len, 0, 0).map(|_| ())
    }
    fn sync(&self) -> io::Result<()> {
        call(7, self.0, 0, 0, 0).map(|_| ())
    }
    fn lock_exclusive(&self) -> io::Result<()> {
        call(8, self.0, 0, 0, 0).map(|_| ())
    }
}
impl StorageIo for HostIo {
    fn capabilities(&self) -> Capabilities {
        let c = call(0, 0, 0, 0, 0).unwrap_or(0);
        Capabilities {
            durable_sync: c & 1 != 0,
            atomic_replace: c & 2 != 0,
            durable_directory: c & 4 != 0,
            exclusive_lock: c & 8 != 0,
            no_clobber_publish: c & 16 != 0,
        }
    }
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<File> {
        let mode = match mode {
            OpenMode::OpenOrCreate => 0,
            OpenMode::Existing => 1,
            OpenMode::CreateNew => 2,
            OpenMode::ReadOnly => return Err(io::ErrorKind::Unsupported.into()),
        };
        let handle = u32::try_from(call(1, role(path)?, mode, 0, 0)?)
            .map_err(|_| io::ErrorKind::InvalidData)?;
        Ok(File::new(Arc::new(HostFile(handle))))
    }
    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
    fn exists(&self, path: &Path) -> io::Result<bool> {
        call(9, role(path)?, 0, 0, 0).map(|n| n != 0)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        call(10, role(path)?, 0, 0, 0).map(|_| ())
    }
    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        call(11, role(src)?, role(dst)? as u64, 0, 0).map(|_| ())
    }
    fn publish(&self, src: &Path, dst: &Path) -> io::Result<()> {
        call(12, role(src)?, role(dst)? as u64, 0, 0).map(|_| ())
    }
    fn sync_parent(&self, path: &Path) -> io::Result<()> {
        call(13, role(path)?, 0, 0, 0).map(|_| ())
    }
}
