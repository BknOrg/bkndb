use super::*;
use std::fs::{self, OpenOptions};
#[derive(Default)]
pub struct NativeIo;
struct NativeFile(std::fs::File);
impl StorageFile for NativeFile {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_at(&self.0, buf, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_read(&self.0, buf, offset)
        }
    }
    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::write_at(&self.0, buf, offset)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::FileExt::seek_write(&self.0, buf, offset)
        }
    }
    fn len(&self) -> io::Result<u64> {
        self.0.metadata().map(|m| m.len())
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }
    fn sync(&self) -> io::Result<()> {
        self.0.sync_all()
    }
    fn lock_exclusive(&self) -> io::Result<()> {
        self.0.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
            fs::TryLockError::Error(e) => e,
        })
    }
}
impl StorageIo for NativeIo {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            durable_sync: true,
            atomic_replace: true,
            durable_directory: true,
            exclusive_lock: true,
            no_clobber_publish: true,
        }
    }
    fn database_path(&self, path: &Path) -> io::Result<std::path::PathBuf> {
        if path.try_exists()? {
            let metadata = fs::metadata(path)?;
            if !metadata.is_file() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 {
                    return Err(io::Error::other("hard-linked databases are unsupported"));
                }
            }
            #[cfg(windows)]
            if windows_link_count(path)? != 1 {
                return Err(io::Error::other("hard-linked databases are unsupported"));
            }
            path.canonicalize()
        } else {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(parent
                .canonicalize()?
                .join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?))
        }
    }
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<File> {
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut o = OpenOptions::new();
        o.read(true).write(!matches!(mode, OpenMode::ReadOnly));
        match mode {
            OpenMode::OpenOrCreate => {
                o.create(true);
            }
            OpenMode::CreateNew => {
                o.create_new(true);
            }
            OpenMode::Existing | OpenMode::ReadOnly => {}
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            o.share_mode(7);
        }
        o.open(path).map(|f| File::new(Arc::new(NativeFile(f))))
    }
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        let created = {
            let mut dirs = Vec::new();
            let mut current = path;
            while !current.as_os_str().is_empty() && !current.try_exists()? {
                dirs.push(current.to_owned());
                current = current.parent().unwrap_or(Path::new("."));
            }
            dirs
        };
        fs::create_dir_all(path)?;
        // Persist newly created ancestor names, not just the data file's own
        // directory. Existing ancestors remain the caller/filesystem contract.
        #[cfg(unix)]
        for dir in created {
            self.sync_parent(&dir)?;
        }
        Ok(())
    }
    fn exists(&self, path: &Path) -> io::Result<bool> {
        path.try_exists()
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
    fn replace(&self, src: &Path, dst: &Path) -> io::Result<()> {
        #[cfg(not(windows))]
        {
            fs::rename(src, dst)
        }
        #[cfg(windows)]
        {
            move_windows(src, dst, true)
        }
    }
    fn publish(&self, src: &Path, dst: &Path) -> io::Result<()> {
        #[cfg(not(windows))]
        {
            fs::hard_link(src, dst)?;
            fs::remove_file(src)
        }
        #[cfg(windows)]
        {
            move_windows(src, dst, false)
        }
    }
    fn sync_parent(&self, path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            std::fs::File::open(dir)?.sync_all()
        }
        // replace_windows uses MOVEFILE_WRITE_THROUGH. File data has been flushed.
        #[cfg(windows)]
        {
            let _ = path;
            Ok(())
        }
    }
}
#[cfg(windows)]
fn move_windows(src: &Path, dst: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(src: *const u16, dst: *const u16, flags: u32) -> i32;
    }
    let src: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
    let dst: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
    if src[..src.len() - 1].contains(&0) || dst[..dst.len() - 1].contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: both paths are valid, NUL-terminated UTF-16 buffers for this call.
    if unsafe { MoveFileExW(src.as_ptr(), dst.as_ptr(), 8 | if replace { 1 } else { 0 }) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn windows_link_count(path: &Path) -> io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    // BY_HANDLE_FILE_INFORMATION consists of thirteen DWORDs (including its
    // three FILETIMEs). NumberOfLinks is the eleventh DWORD, at byte offset 40.
    #[repr(C)]
    struct Info {
        attributes: u32,
        times: [u32; 6],
        volume: u32,
        size: [u32; 2],
        links: u32,
        index: [u32; 2],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Info) -> i32;
    }
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(7)
        .open(path)?;
    let mut info = Info {
        attributes: 0,
        times: [0; 6],
        volume: 0,
        size: [0; 2],
        links: 0,
        index: [0; 2],
    };
    // SAFETY: valid open handle and writable, correctly laid-out Win32 buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.links)
}
