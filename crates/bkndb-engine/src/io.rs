//! Capability-based positioned I/O. Implementations must preserve the documented
//! sync and atomic-replacement semantics; an engine never silently weakens them.
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug)]
pub enum OpenMode {
    OpenOrCreate,
    Existing,
    ReadOnly,
    CreateNew,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Capabilities {
    pub durable_sync: bool,
    pub atomic_replace: bool,
    pub durable_directory: bool,
    pub exclusive_lock: bool,
    pub no_clobber_publish: bool,
}
impl Capabilities {
    pub fn complete(self) -> bool {
        self.durable_sync
            && self.atomic_replace
            && self.durable_directory
            && self.exclusive_lock
            && self.no_clobber_publish
    }
}
pub trait StorageFile: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<usize>;
    fn len(&self) -> io::Result<u64>;
    fn set_len(&self, len: u64) -> io::Result<()>;
    fn sync(&self) -> io::Result<()>;
    /// Nonblocking, retained until the last handle is dropped.
    fn lock_exclusive(&self) -> io::Result<()>;
}
pub trait StorageIo: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    fn database_path(&self, path: &Path) -> io::Result<std::path::PathBuf> {
        Ok(path.to_owned())
    }
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<File>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> io::Result<bool>;
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Atomic replacement of dst; existing open handles must retain their bytes.
    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()>;
    /// Atomic publication that fails if dst exists.
    fn publish(&self, src: &Path, dst: &Path) -> io::Result<()>;
    fn sync_parent(&self, path: &Path) -> io::Result<()>;
}

/// A cursor is local to this wrapper, never the OS file description. Clones get
/// independent cursors; random reads do not change any cursor.
pub struct File {
    inner: Arc<dyn StorageFile>,
    cursor: Mutex<u64>,
}
pub struct Metadata(u64);
impl Metadata {
    pub fn len(&self) -> u64 {
        self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}
impl File {
    pub fn new(inner: Arc<dyn StorageFile>) -> Self {
        Self {
            inner,
            cursor: Mutex::new(0),
        }
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        let pos = *self.cursor.lock().map_err(poison)?;
        Ok(Self {
            inner: self.inner.clone(),
            cursor: Mutex::new(pos),
        })
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.inner.len().map(Metadata)
    }
    pub fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
    pub fn sync_data(&self) -> io::Result<()> {
        self.inner.sync()
    }
    pub fn sync_all(&self) -> io::Result<()> {
        self.inner.sync()
    }
    pub fn try_lock(&self) -> io::Result<()> {
        self.inner.lock_exclusive()
    }
    pub fn read_exact_at(&self, mut offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.inner.read_at(offset, buf) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) if n <= buf.len() => {
                    offset = offset
                        .checked_add(n as u64)
                        .ok_or(io::ErrorKind::InvalidInput)?;
                    buf = &mut buf[n..];
                }
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    pub fn write_all_at(&self, mut offset: u64, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.inner.write_at(offset, buf) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) if n <= buf.len() => {
                    offset = offset
                        .checked_add(n as u64)
                        .ok_or(io::ErrorKind::InvalidInput)?;
                    buf = &buf[n..];
                }
                Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
fn poison<T>(_: std::sync::PoisonError<T>) -> io::Error {
    io::Error::other("poisoned I/O cursor")
}
impl Read for &File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut pos = self.cursor.lock().map_err(poison)?;
        let n = self.inner.read_at(*pos, buf)?;
        if n > buf.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        *pos = pos
            .checked_add(n as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(n)
    }
}
impl Write for &File {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut pos = self.cursor.lock().map_err(poison)?;
        let n = self.inner.write_at(*pos, buf)?;
        if n > buf.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        *pos = pos
            .checked_add(n as u64)
            .ok_or(io::ErrorKind::InvalidInput)?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for &File {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        let mut pos = self.cursor.lock().map_err(poison)?;
        let value = match seek {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(n) => pos
                .checked_add_signed(n)
                .ok_or(io::ErrorKind::InvalidInput)?,
            SeekFrom::End(n) => self
                .inner
                .len()?
                .checked_add_signed(n)
                .ok_or(io::ErrorKind::InvalidInput)?,
        };
        *pos = value;
        Ok(value)
    }
}
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::NativeIo;
