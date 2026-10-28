//! Where a vault's bytes live: a file, or memory in tests.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub(crate) trait Device: Send + Sync {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()>;
    /// Makes every write so far durable.
    fn sync(&self) -> io::Result<()>;
    fn len(&self) -> io::Result<u64>;
    /// Whether the vault's path now leads elsewhere, as when a sync app replaced the file.
    fn replaced(&self) -> bool;
}

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
