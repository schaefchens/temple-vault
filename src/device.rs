//! Where a vault's bytes live: a file, a file of the browser's origin private file system,
//! or memory in tests.

use std::io;

#[cfg(unix)]
pub(crate) use file::{FileDevice, exclude_from_backup};
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub(crate) use web::OpfsDevice;

pub(crate) trait Device: Send + Sync {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()>;
    /// Makes every write so far durable.
    fn sync(&self) -> io::Result<()>;
    fn len(&self) -> io::Result<u64>;
    /// Whether the vault's path now leads elsewhere, as when a sync app replaced the file.
    fn replaced(&self) -> bool;
    /// Locks the file against other processes while a space is open – shared for reading,
    /// exclusive for writing – so that none writes beside this one between the opening
    /// marker's checks. `ResourceBusy` if another process holds it.
    fn hold(&self, _shared: bool) -> io::Result<()> {
        Ok(())
    }
    /// Lets go of the lock [`Device::hold`] took.
    fn release(&self) {}
}

#[cfg(unix)]
mod file {
    use super::Device;
    use std::fs::{self, File};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    pub(crate) struct FileDevice {
        file: File,
        path: Mutex<PathBuf>,
    }

    impl FileDevice {
        pub(crate) fn new(file: File, path: &Path) -> FileDevice {
            FileDevice { file, path: Mutex::new(path.to_owned()) }
        }

        /// Gives the file the name `to` – failing if something is there – and makes that durable.
        pub(crate) fn publish(&self, to: &Path) -> io::Result<()> {
            let mut path = self.path.lock().unwrap();
            match fs::hard_link(&*path, to) {
                Ok(()) => {
                    let old = std::mem::replace(&mut *path, to.to_owned());
                    let _ = fs::remove_file(old);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(e),
                // File systems without hard links, such as FAT on memory cards: checked, then renamed.
                Err(_) => {
                    if fs::symlink_metadata(to).is_ok() {
                        return Err(crate::vault::already_exists());
                    }
                    fs::rename(&*path, to)?;
                    *path = to.to_owned();
                }
            }
            let parent = match to.parent() {
                Some(parent) if parent != Path::new("") => parent,
                _ => Path::new("."),
            };
            File::open(parent)?.sync_all()
        }

        pub(crate) fn path(&self) -> PathBuf {
            self.path.lock().unwrap().clone()
        }
    }

    impl Device for FileDevice {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            self.file.read_exact_at(buf, offset)
        }

        fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
            self.file.write_all_at(data, offset)
        }

        /// F_FULLFSYNC on Apple's systems, where a plain fsync stops at the drive's cache.
        fn sync(&self) -> io::Result<()> {
            self.file.sync_all()
        }

        fn len(&self) -> io::Result<u64> {
            Ok(self.file.metadata()?.len())
        }

        fn replaced(&self) -> bool {
            let Ok(ours) = self.file.metadata() else { return true };
            match fs::metadata(&*self.path.lock().unwrap()) {
                Ok(theirs) => (theirs.dev(), theirs.ino()) != (ours.dev(), ours.ino()),
                Err(_) => true,
            }
        }

        /// `flock` itself: Rust's `File::try_lock` has none on Android. Where the file system
        /// has no locks, the opening marker alone tells.
        fn hold(&self, shared: bool) -> io::Result<()> {
            let how = if shared { libc::LOCK_SH } else { libc::LOCK_EX };
            if unsafe { libc::flock(self.file.as_raw_fd(), how | libc::LOCK_NB) } == 0 {
                return Ok(());
            }
            match io::Error::last_os_error() {
                e if e.kind() == io::ErrorKind::WouldBlock => {
                    Err(io::Error::new(io::ErrorKind::ResourceBusy, "another process has a space of the vault open"))
                }
                _ => Ok(()),
            }
        }

        fn release(&self) {
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    /// Backups leave the file out: the mark iOS and macOS keep for it.
    #[cfg(target_vendor = "apple")]
    pub(crate) fn exclude_from_backup(path: &Path) -> io::Result<bool> {
        use std::ffi::c_void;
        use std::os::unix::ffi::OsStrExt;
        use std::ptr;

        type Ref = *const c_void;
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            static kCFURLIsExcludedFromBackupKey: Ref;
            static kCFBooleanTrue: Ref;
            fn CFURLCreateFromFileSystemRepresentation(allocator: Ref, path: *const u8, len: isize, dir: u8) -> Ref;
            fn CFURLSetResourcePropertyForKey(url: Ref, key: Ref, value: Ref, error: *mut Ref) -> u8;
            fn CFRelease(object: Ref);
        }

        // Missing files fail as anywhere else, not with Core Foundation's error.
        fs::metadata(path)?;
        let bytes = path.as_os_str().as_bytes();
        let url =
            unsafe { CFURLCreateFromFileSystemRepresentation(ptr::null(), bytes.as_ptr(), bytes.len() as isize, 0) };
        if url.is_null() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file path"));
        }
        let mut error: Ref = ptr::null();
        let marked =
            unsafe { CFURLSetResourcePropertyForKey(url, kCFURLIsExcludedFromBackupKey, kCFBooleanTrue, &mut error) };
        unsafe {
            if !error.is_null() {
                CFRelease(error);
            }
            CFRelease(url);
        }
        if marked == 0 {
            return Err(io::Error::other("the system kept the file in its backups"));
        }
        Ok(true)
    }

    /// No mark elsewhere: on Android, the app's manifest leaves files out of backups.
    #[cfg(not(target_vendor = "apple"))]
    pub(crate) fn exclude_from_backup(path: &Path) -> io::Result<bool> {
        fs::metadata(path)?;
        Ok(false)
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod web {
    use super::Device;
    use std::io;
    use web_sys::wasm_bindgen::JsValue;
    use web_sys::{FileSystemReadWriteOptions, FileSystemSyncAccessHandle};

    /// A file of the origin private file system, read and written in place through a sync
    /// access handle – which only a worker can open. The handle closes when the device is
    /// dropped, with the vault and its spaces.
    pub(crate) struct OpfsDevice(FileSystemSyncAccessHandle);

    // Without the atomics feature, wasm has one thread: the handle never reaches another.
    #[cfg(not(target_feature = "atomics"))]
    unsafe impl Send for OpfsDevice {}
    #[cfg(not(target_feature = "atomics"))]
    unsafe impl Sync for OpfsDevice {}

    impl OpfsDevice {
        pub(crate) fn new(handle: FileSystemSyncAccessHandle) -> OpfsDevice {
            OpfsDevice(handle)
        }
    }

    impl Device for OpfsDevice {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            let read = self.0.read_with_u8_array_and_options(buf, &at(offset)).map_err(error)?;
            if read as usize == buf.len() { Ok(()) } else { Err(io::ErrorKind::UnexpectedEof.into()) }
        }

        fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
            let written = self.0.write_with_u8_array_and_options(data, &at(offset)).map_err(error)?;
            if written as usize == data.len() { Ok(()) } else { Err(io::ErrorKind::WriteZero.into()) }
        }

        fn sync(&self) -> io::Result<()> {
            self.0.flush().map_err(error)
        }

        fn len(&self) -> io::Result<u64> {
            Ok(self.0.get_size().map_err(error)? as u64)
        }

        /// While the handle is open, nothing else can write the file or replace it.
        fn replaced(&self) -> bool {
            false
        }
    }

    impl Drop for OpfsDevice {
        fn drop(&mut self) {
            self.0.close();
        }
    }

    fn at(offset: u64) -> FileSystemReadWriteOptions {
        let options = FileSystemReadWriteOptions::new();
        options.set_at(offset as f64);
        options
    }

    /// A DOMException as an I/O error; a full quota is `StorageFull`.
    fn error(e: JsValue) -> io::Error {
        let name = js_sys::Reflect::get(&e, &"name".into()).ok().and_then(|name| name.as_string());
        let kind = match name.as_deref() {
            Some("QuotaExceededError") => io::ErrorKind::StorageFull,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, format!("{e:?}"))
    }
}
