use bkndb_storage_lsm::io::{self, NativeIo, OpenMode, StorageFile, StorageIo};
use std::{
    io as stdio,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
pub struct CrashIo {
    pub counter: Arc<AtomicU64>,
    pub at: u64,
    pub before: bool,
}
impl CrashIo {
    pub fn point(&self, before: bool) {
        if self.before != before {
            return;
        }
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.at {
            use stdio::Write;
            println!("KILL");
            stdio::stdout().flush().unwrap();
            let mut line = String::new();
            stdio::stdin().read_line(&mut line).unwrap();
            panic!("parent should kill the child");
        }
    }
}
struct CrashFile {
    file: io::File,
    io: Arc<CrashIo>,
}
impl StorageFile for CrashFile {
    fn read_at(&self, off: u64, buf: &mut [u8]) -> stdio::Result<usize> {
        let len = self.file.metadata()?.len();
        let n = buf.len().min(len.saturating_sub(off) as usize);
        self.file.read_exact_at(off, &mut buf[..n])?;
        Ok(n)
    }
    fn write_at(&self, off: u64, buf: &[u8]) -> stdio::Result<usize> {
        self.io.point(true);
        self.file.write_all_at(off, buf)?;
        self.io.point(false);
        Ok(buf.len())
    }
    fn len(&self) -> stdio::Result<u64> {
        Ok(self.file.metadata()?.len())
    }
    fn set_len(&self, n: u64) -> stdio::Result<()> {
        self.io.point(true);
        self.file.set_len(n)?;
        self.io.point(false);
        Ok(())
    }
    fn sync(&self) -> stdio::Result<()> {
        self.io.point(true);
        self.file.sync_all()?;
        self.io.point(false);
        Ok(())
    }
    fn lock_exclusive(&self) -> stdio::Result<()> {
        self.file.try_lock()
    }
}
pub struct Host(pub Arc<CrashIo>);
impl StorageIo for Host {
    fn capabilities(&self) -> io::Capabilities {
        NativeIo.capabilities()
    }
    fn database_path(&self, p: &Path) -> stdio::Result<std::path::PathBuf> {
        NativeIo.database_path(p)
    }
    fn open(&self, p: &Path, m: OpenMode) -> stdio::Result<io::File> {
        Ok(io::File::new(Arc::new(CrashFile {
            file: NativeIo.open(p, m)?,
            io: self.0.clone(),
        })))
    }
    fn create_dir_all(&self, p: &Path) -> stdio::Result<()> {
        NativeIo.create_dir_all(p)
    }
    fn exists(&self, p: &Path) -> stdio::Result<bool> {
        NativeIo.exists(p)
    }
    fn remove(&self, p: &Path) -> stdio::Result<()> {
        self.0.point(true);
        let r = NativeIo.remove(p);
        self.0.point(false);
        r
    }
    fn replace(&self, s: &Path, d: &Path) -> stdio::Result<()> {
        self.0.point(true);
        NativeIo.replace(s, d)?;
        self.0.point(false);
        Ok(())
    }
    fn publish(&self, s: &Path, d: &Path) -> stdio::Result<()> {
        self.0.point(true);
        NativeIo.publish(s, d)?;
        self.0.point(false);
        Ok(())
    }
    fn sync_parent(&self, p: &Path) -> stdio::Result<()> {
        self.0.point(true);
        NativeIo.sync_parent(p)?;
        self.0.point(false);
        Ok(())
    }
}
