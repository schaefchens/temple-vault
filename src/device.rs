//! Where a vault's bytes live: a file, a file of the browser's origin private file system,
//! or memory in tests.

use std::io;

#[cfg(unix)]
pub(crate) use file::FileDevice;
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
}

#[cfg(unix)]
mod file {
    use super::Device;
    use std::fs::{self, File};
    use std::io;
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
