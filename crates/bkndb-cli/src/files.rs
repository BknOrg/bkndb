//! Offline source ownership and no-clobber publication. Only temporary files
//! created in our private directories are eligible for cleanup.
use crate::{
    args::{Backend as Kind, Source},
    backend::Backend,
    checkpoint,
    error::{Error, Result},
};
use bkndb::{Db, LsmStorageBackend, RedbStorageBackend};
use bkndb_engine::io::{NativeIo, StorageIo};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub struct Database {
    pub db: Db<Backend>,
    pub kind: Kind,
    pub path: PathBuf,
    pub source_bytes: u64,
    pub recovery_required: bool,
    _guard: Option<File>,
    _scratch: Option<tempfile::TempDir>,
}
pub fn detect(path: &Path) -> Result<Kind> {
    let mut f = File::open(path)?;
    if !f.metadata()?.is_file() {
        return Err(Error::input("source must be a regular file"));
    }
    let mut prefix = [0u8; 128];
    let n = f.read(&mut prefix)?;
    if n >= 9 && prefix[..9] == *b"redb\x1a\x0a\xa9\x0d\x0a" {
        return Ok(Kind::Redb);
    }
    if (n >= 8 && &prefix[..8] == b"BKNDBLS1") || (n >= 72 && &prefix[64..72] == b"BKNDBLS1") {
        return Ok(Kind::Lsm);
    }
    Err(Error::new(
        6,
        "unrecognized database signature (empty or damaged headers are never initialized automatically)",
    ))
}
fn lock(file: &File) -> Result<()> {
    file.try_lock().map_err(|e| match e {
        fs::TryLockError::WouldBlock => Error::new(
            4,
            "database is in use; close the application before offline administration",
        ),
        fs::TryLockError::Error(e) => e.into(),
    })
}
pub fn copy_file(source: &mut File, dest: &Path) -> Result<()> {
    source.seek(SeekFrom::Start(0))?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        checkpoint()?;
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        output.write_all(&buffer[..n])?;
    }
    output.sync_all()?;
    Ok(())
}
impl Database {
    pub fn open(source: &Source, write: bool) -> Result<Self> {
        checkpoint()?;
        let path = NativeIo.database_path(&source.file)?;
        let kind = detect(&path)?;
        if source.backend.is_some_and(|b| b != kind) {
            return Err(Error::input("--backend does not match the file signature"));
        }
        let size = fs::metadata(&path)?.len();
        match kind {
            Kind::Lsm => {
                let b = LsmStorageBackend::open_existing(&path, !write)?;
                let recovery_required = b.engine().recovery_required();
                Ok(Self {
                    db: Db::new(Backend::Lsm(b)),
                    kind,
                    path,
                    source_bytes: size,
                    recovery_required,
                    _guard: None,
                    _scratch: None,
                })
            }
            Kind::Redb if write => {
                let b = RedbStorageBackend::open_existing(&path)?;
                Ok(Self {
                    db: Db::new(Backend::Redb(b)),
                    kind,
                    path,
                    source_bytes: size,
                    recovery_required: false,
                    _guard: None,
                    _scratch: None,
                })
            }
            Kind::Redb => {
                let mut file = File::open(&path)?;
                lock(&file)?;
                // Capture the upstream v2/v3 recovery marker before opening our
                // copy. Opening Redb itself can recover and update its header.
                let mut header = [0u8; 10];
                file.read_exact(&mut header)?;
                let dirty = header[9] & 2 != 0;
                let scratch = tempfile::Builder::new()
                    .prefix("bkndb-inspect-")
                    .tempdir()?;
                let copy = scratch.path().join("snapshot.bkndb");
                copy_file(&mut file, &copy)?;
                let b = RedbStorageBackend::open_existing(&copy)?;
                Ok(Self {
                    db: Db::new(Backend::Redb(b)),
                    kind,
                    path,
                    source_bytes: size,
                    recovery_required: dirty,
                    _guard: Some(file),
                    _scratch: Some(scratch),
                })
            }
        }
    }
}
pub fn source(path: &Path) -> Source {
    Source {
        file: path.to_owned(),
        backend: None,
    }
}

pub struct Stage {
    pub path: PathBuf,
    dest: PathBuf,
    _directory: tempfile::TempDir,
}
impl Stage {
    pub fn new(dest: &Path) -> Result<Self> {
        if fs::symlink_metadata(dest).is_ok() {
            return Err(Error::input(
                "destination already exists; overwriting is disabled",
            ));
        }
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // Parent creation must be explicit so a typo doesn't create a tree.
        if !parent.is_dir() {
            return Err(Error::input("destination parent directory must exist"));
        }
        let dir = tempfile::Builder::new()
            .prefix(".bkndb-stage-")
            .tempdir_in(parent)?;
        Ok(Self {
            path: dir.path().join("data"),
            dest: dest.to_owned(),
            _directory: dir,
        })
    }
    pub fn publish(self) -> Result<()> {
        checkpoint()?;
        File::open(&self.path)?.sync_all()?;
        NativeIo.sync_parent(&self.path)?;
        crate::failpoint("before_publish");
        atomic_publish(&self.path, &self.dest)?;
        crate::failpoint("after_publish");
        NativeIo.sync_parent(&self.dest).map_err(|e|Error::new(7,format!("output published at {}; directory sync failed: {e}. Inspect it before retrying",self.dest.display())))?;
        NativeIo.sync_parent(&self.path).map_err(|e| {
            Error::new(
                7,
                format!(
                    "output published at {}; staging directory sync failed: {e}",
                    self.dest.display()
                ),
            )
        })?;
        Ok(())
    }
}
pub fn create(path: &Path, kind: Kind) -> Result<()> {
    match kind {
        Kind::Redb => {
            drop(RedbStorageBackend::open(path)?);
        }
        Kind::Lsm => {
            drop(LsmStorageBackend::open(path)?);
        }
    }
    Ok(())
}

// A hard-link+unlink sequence can leave two names after a crash, conflicting
// with the engine's rejection of hard-linked database files. Publish with one
// atomic no-replace rename on the five supported platforms instead.
fn atomic_publish(source: &Path, dest: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let s =
            CString::new(source.as_os_str().as_bytes()).map_err(|_| Error::input("NUL in path"))?;
        let d =
            CString::new(dest.as_os_str().as_bytes()).map_err(|_| Error::input("NUL in path"))?;
        #[cfg(target_os = "linux")]
        // SAFETY: both paths are live NUL-terminated strings. NOREPLACE is
        // mandatory; unsupported filesystems produce an error, never fallback.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                s.as_ptr(),
                libc::AT_FDCWD,
                d.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        // SAFETY: same path buffer contract; RENAME_EXCL rejects existing names.
        let result = unsafe { libc::renamex_np(s.as_ptr(), d.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(Error::unsupported(
            "atomic CLI publication supports Linux, macOS and Windows",
        ));
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        NativeIo.publish(source, dest)?;
        Ok(())
    }
}
