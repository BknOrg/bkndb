//! Deterministic volatile/durable storage model for tests. This simulates loss of
//! unsynced data AND directory updates, unlike a subprocess kill on a live OS.
use crate::io::*;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use std::{io, path::Path, sync::Arc};
#[derive(Clone, Copy)]
pub enum Failure {
    Before,
    After,
    NoSpace,
}
#[derive(Default)]
struct Data {
    bytes: Mutex<Vec<u8>>,
    durable: Mutex<Vec<u8>>,
    locked: AtomicBool,
}
#[derive(Default)]
struct Shared {
    live: Mutex<BTreeMap<std::path::PathBuf, Arc<Data>>>,
    durable: Mutex<BTreeMap<std::path::PathBuf, Arc<Data>>>,
    fail: Mutex<Option<(u64, Failure)>>,
    events: AtomicU64,
    short: AtomicU64,
}
#[derive(Clone, Default)]
pub struct FaultIo(Arc<Shared>);
impl FaultIo {
    pub fn fail_at(&self, event: u64, mode: Failure) {
        self.0.events.store(0, Ordering::SeqCst);
        *self.0.fail.lock().unwrap() = Some((event, mode));
    }
    pub fn clear_failure(&self) {
        *self.0.fail.lock().unwrap() = None;
    }
    pub fn short_writes(&self, max: u64) {
        self.0.short.store(max, Ordering::SeqCst);
    }
    pub fn events(&self) -> u64 {
        self.0.events.load(Ordering::SeqCst)
    }
    /// Drop every Engine/transaction before calling this simulated power cut.
    pub fn power_loss(&self) {
        let stable = self.0.durable.lock().unwrap().clone();
        for data in stable.values() {
            *data.bytes.lock().unwrap() = data.durable.lock().unwrap().clone();
        }
        *self.0.live.lock().unwrap() = stable;
        self.clear_failure();
    }
    fn before(&self) -> io::Result<bool> {
        let n = self.0.events.fetch_add(1, Ordering::SeqCst) + 1;
        match *self.0.fail.lock().unwrap() {
            Some((at, Failure::NoSpace)) if n == at => Err(io::ErrorKind::StorageFull.into()),
            Some((at, Failure::Before)) if n == at => {
                Err(io::Error::other("injected failure before I/O"))
            }
            Some((at, Failure::After)) if n == at => Ok(true),
            _ => Ok(false),
        }
    }
    fn after(&self, fail: bool) -> io::Result<()> {
        if fail {
            Err(io::Error::other("injected failure after I/O"))
        } else {
            Ok(())
        }
    }
}
struct Handle {
    read_only: bool,
    data: Arc<Data>,
    io: FaultIo,
    holds_lock: AtomicBool,
}
impl Drop for Handle {
    fn drop(&mut self) {
        if self.holds_lock.load(Ordering::SeqCst) {
            self.data.locked.store(false, Ordering::SeqCst);
        }
    }
}
impl StorageFile for Handle {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let bytes = self.data.bytes.lock().unwrap();
        let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        if offset >= bytes.len() {
            return Ok(0);
        }
        let n = buf.len().min(bytes.len() - offset);
        buf[..n].copy_from_slice(&bytes[offset..offset + n]);
        Ok(n)
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        if self.read_only {return Err(io::ErrorKind::PermissionDenied.into());}
        let fail = self.io.before()?;
        let offset = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        let short = self.io.0.short.load(Ordering::SeqCst) as usize;
        let n = if short > 0 {
            short.min(buf.len())
        } else {
            buf.len()
        };
        let end = offset
            .checked_add(n)
            .filter(|n| *n <= 128 * 1024 * 1024)
            .ok_or(io::ErrorKind::StorageFull)?;
        let mut bytes = self.data.bytes.lock().unwrap();
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[offset..end].copy_from_slice(&buf[..n]);
        self.io.after(fail)?;
        Ok(n)
    }
    fn len(&self) -> io::Result<u64> {
        Ok(self.data.bytes.lock().unwrap().len() as u64)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        if self.read_only {return Err(io::ErrorKind::PermissionDenied.into());}
        let fail = self.io.before()?;
        let len = usize::try_from(len)
            .ok()
            .filter(|n| *n <= 128 * 1024 * 1024)
            .ok_or(io::ErrorKind::StorageFull)?;
        self.data.bytes.lock().unwrap().resize(len, 0);
        self.io.after(fail)
    }
    fn sync(&self) -> io::Result<()> {
        let fail = self.io.before()?;
        *self.data.durable.lock().unwrap() = self.data.bytes.lock().unwrap().clone();
        self.io.after(fail)
    }
    fn lock_exclusive(&self) -> io::Result<()> {
        if self.data.locked.swap(true, Ordering::SeqCst) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.holds_lock.store(true, Ordering::SeqCst);
        Ok(())
    }
}
impl StorageIo for FaultIo {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            durable_sync: true,
            atomic_replace: true,
            durable_directory: true,
            exclusive_lock: true,
            no_clobber_publish: true,
        }
    }
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<File> {
        let mut live = self.0.live.lock().unwrap();
        let exists = live.contains_key(path);
        if exists && matches!(mode, OpenMode::CreateNew) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        if !exists && matches!(mode, OpenMode::Existing | OpenMode::ReadOnly) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let data = live.entry(path.to_owned()).or_default().clone();
        Ok(File::new(Arc::new(Handle {
            read_only: matches!(mode, OpenMode::ReadOnly),
            data,
            io: self.clone(),
            holds_lock: AtomicBool::new(false),
        })))
    }
    fn create_dir_all(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn exists(&self, path: &Path) -> io::Result<bool> {
        Ok(self.0.live.lock().unwrap().contains_key(path))
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        let fail = self.before()?;
        self.0.live.lock().unwrap().remove(path);
        self.after(fail)
    }
    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let fail = self.before()?;
        let mut live = self.0.live.lock().unwrap();
        let file = live.remove(src).ok_or(io::ErrorKind::NotFound)?;
        live.insert(dst.to_owned(), file);
        self.after(fail)
    }
    fn publish(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let fail = self.before()?;
        let mut live = self.0.live.lock().unwrap();
        if live.contains_key(dst) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let file = live.remove(src).ok_or(io::ErrorKind::NotFound)?;
        live.insert(dst.to_owned(), file);
        self.after(fail)
    }
    fn sync_parent(&self, _: &Path) -> io::Result<()> {
        let fail = self.before()?;
        *self.0.durable.lock().unwrap() = self.0.live.lock().unwrap().clone();
        self.after(fail)
    }
}
