//! The vault file: the slot table in zone 0, the opening marker in zone 1, spaces and noise
//! from zone 2 on, appended in whole zones.

use crate::crypto::{Key, random};
use crate::device::Device;
#[cfg(unix)]
use crate::device::FileDevice;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use crate::device::OpfsDevice;
use crate::keys::PhraseKey;
use crate::noise::{self, Noise};
use crate::slots::{self, KeyBlock, SLOT};
use crate::space::Space;
use crate::words::Words;
use crate::{BLOCK, Error, ZONE};
#[cfg(unix)]
use std::fs::{self, OpenOptions};
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Zone 1's first block: random bytes, renewed whenever an app opens the vault and whenever
/// a space locks. Different bytes there mean another device has opened the vault since.
const MARKER: u64 = ZONE;

/// A vault: one file holding any number of spaces, each opened by its own 12 words.
pub struct Vault {
    pub(crate) file: Arc<VaultFile>,
}

/// What a new space should hold from the start, so that it needn't grow soon. A hidden
/// space is best set up in one go with everything it will hold (design.md).
#[derive(Clone, Copy, Debug, Default)]
pub struct Setup {
    /// Bytes of content: the database and objects together.
    pub room: u64,
}

impl Vault {
    /// A new vault at `path` with its first space, which comes back open. Fails if something
    /// exists at `path`. The vault is written next to it, as `.<name>.temple-part`, and given
    /// its name once complete, so a crash leaves no half-made vault at `path`; the next try
    /// removes what it left.
    #[cfg(unix)]
    pub fn create(path: impl AsRef<Path>, words: &Words, setup: Setup) -> Result<(Vault, Space), Error> {
        let key = PhraseKey::from_words(words);
        Vault::create_at(path.as_ref(), noise::REAL, |file| Space::create(file, file.claim(), key, setup, true))
    }

    /// A new vault at `path`, filled by `fill` in `.<name>.temple-part` next to it and given its
    /// name once complete.
    #[cfg(unix)]
    pub(crate) fn create_at<T>(
        path: &Path,
        noise: noise::Amounts,
        fill: impl FnOnce(&Arc<VaultFile>) -> Result<T, Error>,
    ) -> Result<(Vault, T), Error> {
        let name =
            path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a vault needs a file name"))?;
        if fs::symlink_metadata(path).is_ok() {
            return Err(already_exists().into());
        }
        let partial = path.with_file_name(format!(".{}.temple-part", name.to_string_lossy()));
        // Left by a creation a crash interrupted – never a vault, as it never got its name –
        // unless another process is still writing it.
        match OpenOptions::new().write(true).open(&partial) {
            Ok(left) => {
                hold(&left)?;
                fs::remove_file(&partial)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&partial)?;
        hold(&file)?;
        let device = Arc::new(FileDevice::new(file, &partial));
        let created = Vault::create_on(device.clone(), noise, fill).and_then(|(vault, filled)| {
            device.publish(path)?;
            Ok((vault, filled))
        });
        if created.is_err() {
            let _ = fs::remove_file(device.path());
        }
        created
    }

    pub(crate) fn create_on<T>(
        device: Arc<dyn Device>,
        noise: noise::Amounts,
        fill: impl FnOnce(&Arc<VaultFile>) -> Result<T, Error>,
    ) -> Result<(Vault, T), Error> {
        if device.len()? != 0 {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "a new vault needs an empty file").into());
        }
        let file = Arc::new(VaultFile::new(device, noise));
        // The slot table and zone 1: random bytes until spaces claim slots.
        file.append_noise(2)?;
        file.take_over()?;
        let filled = fill(&file)?;
        Ok((Vault { file }, filled))
    }

    /// Opens a vault, writing nothing – no file tells a vault from other data, so a file that
    /// isn't one stays as it was. The opening marker is written once a space opens or is
    /// created there, or a space is copied in. Creating a space writes into any file: only for
    /// vaults.
    /// Another process with the vault open – an app's extension, say – makes it fail with
    /// `ResourceBusy`.
    #[cfg(unix)]
    pub fn open(path: impl AsRef<Path>) -> Result<Vault, Error> {
        let path = path.as_ref();
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        hold(&file)?;
        Vault::open_on(Arc::new(FileDevice::new(file, path)), noise::REAL)
    }

    /// On the web: a new vault with its first space, which comes back open, in an empty file
    /// of the origin private file system, through a sync access handle to it. Name the file
    /// as [`Vault::create`] does – a temporary name, given the vault's own once this returns
    /// and the handle has closed – so a tab closed halfway leaves no half-made vault.
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    pub fn create_with_handle(
        handle: web_sys::FileSystemSyncAccessHandle,
        words: &Words,
        setup: Setup,
    ) -> Result<(Vault, Space), Error> {
        let key = PhraseKey::from_words(words);
        let device = Arc::new(OpfsDevice::new(handle));
        Vault::create_on(device, noise::REAL, |file| Space::create(file, file.claim(), key, setup, true))
    }

    /// On the web: opens a vault through a sync access handle to its file, as [`Vault::open`]
    /// does. The handle closes once the vault and its spaces are dropped.
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    pub fn open_with_handle(handle: web_sys::FileSystemSyncAccessHandle) -> Result<Vault, Error> {
        Vault::open_on(Arc::new(OpfsDevice::new(handle)), noise::REAL)
    }

    pub(crate) fn open_on(device: Arc<dyn Device>, noise: noise::Amounts) -> Result<Vault, Error> {
        if device.len()? < 2 * ZONE {
            return Err(Error::NotAVault);
        }
        Ok(Vault { file: Arc::new(VaultFile::new(device, noise)) })
    }

    /// The space these words open, or `None` if they open none – the same work either way,
    /// and nothing written for words that open none. Unknown words with a valid checksum can
    /// become a new space: [`Vault::create_space`].
    pub fn unlock(&self, words: &Words) -> Result<Option<Space>, Error> {
        self.unlock_with_key(&PhraseKey::from_words(words))
    }

    /// Quick unlock: like [`Vault::unlock`], with a phrase key the app kept.
    pub fn unlock_with_key(&self, key: &PhraseKey) -> Result<Option<Space>, Error> {
        let claim = self.file.claim().ok_or(Error::SpaceOpen)?;
        match self.file.find_key_block(&key.wrapping_key(), key.slots())? {
            Some(block) => {
                self.file.take_over()?;
                Space::open(claim, key.duplicate(), block).map(Some)
            }
            None => Ok(None),
        }
    }

    /// A new space for words that open none, which comes back open. Only after the app
    /// confirmed with the user, so that a typo leaves no trace. Appended to the vault with
    /// room for `setup`, and no noise: a hidden space's own noise would double its footprint.
    pub fn create_space(&self, words: &Words, setup: Setup) -> Result<Space, Error> {
        let claim = self.file.claim().ok_or(Error::SpaceOpen)?;
        let key = PhraseKey::from_words(words);
        if self.file.find_key_block(&key.wrapping_key(), key.slots())?.is_some() {
            return Err(Error::WordsInUse);
        }
        self.file.take_over()?;
        Space::create(&self.file, Some(claim), key, setup, false)
    }
}

#[cfg(unix)]
pub(crate) fn already_exists() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "something exists at the vault's path")
}

/// An exclusive lock on the vault's file while it is open, so that a second process can't
/// write beside this one between the opening marker's checks. Where the file system has no
/// locks, the marker alone tells.
#[cfg(unix)]
fn hold(file: &fs::File) -> Result<(), Error> {
    match file.try_lock() {
        Err(fs::TryLockError::WouldBlock) => {
            Err(io::Error::new(io::ErrorKind::ResourceBusy, "another process has the vault open").into())
        }
        Ok(()) | Err(fs::TryLockError::Error(_)) => Ok(()),
    }
}

/// The vault file as the spaces see it. Before the first write after the opening marker and
/// after each sync, it checks the marker; if another device has opened a space of the vault
/// since, it stops writing for good.
pub(crate) struct VaultFile {
    device: Arc<dyn Device>,
    pub(crate) noise: noise::Amounts,
    marker: Mutex<Marker>,
    stopped: AtomicBool,
    space_open: AtomicBool,
}

struct Marker {
    bytes: [u8; BLOCK],
    check_due: bool,
}

impl VaultFile {
    fn new(device: Arc<dyn Device>, noise: noise::Amounts) -> VaultFile {
        VaultFile {
            device,
            noise,
            marker: Mutex::new(Marker { bytes: [0; BLOCK], check_due: false }),
            stopped: AtomicBool::new(false),
            space_open: AtomicBool::new(false),
        }
    }

    pub(crate) fn len(&self) -> Result<u64, Error> {
        Ok(self.device.len()?)
    }

    pub(crate) fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<(), Error> {
        Ok(self.device.read_at(buf, offset)?)
    }

    pub(crate) fn write_at(&self, data: &[u8], offset: u64) -> Result<(), Error> {
        self.check_marker()?;
        Ok(self.device.write_at(data, offset)?)
    }

    pub(crate) fn sync(&self) -> Result<(), Error> {
        self.check_marker()?;
        self.device.sync()?;
        self.marker.lock().unwrap().check_due = true;
        Ok(())
    }

    /// Appends `zones` zones of random bytes; returns the number of the first. A crash while
    /// appending can leave a part of a zone at the end: it is filled up first.
    pub(crate) fn append_noise(&self, zones: u64) -> Result<u64, Error> {
        let len = self.len()?;
        let first = len.div_ceil(ZONE);
        let mut noise = Noise::new();
        let mut zone = vec![0u8; ZONE as usize];
        if first * ZONE > len {
            let part = &mut zone[..(first * ZONE - len) as usize];
            noise.fill(part);
            self.write_at(part, len)?;
        }
        for i in 0..zones {
            noise.fill(&mut zone);
            self.write_at(&zone, (first + i) * ZONE)?;
        }
        Ok(first)
    }

    /// A space opens, is created or copied in, or locks: a fresh opening marker, synced – and
    /// read back before the next write, in case another device takes over in between.
    pub(crate) fn take_over(&self) -> Result<(), Error> {
        if self.stopped() {
            return Err(Error::ChangedElsewhere);
        }
        let mut marker = self.marker.lock().unwrap();
        random(&mut marker.bytes);
        self.device.write_at(&marker.bytes, MARKER)?;
        self.device.sync()?;
        marker.check_due = true;
        Ok(())
    }

    /// Locking a space: a fresh opening marker, unless another device has opened the vault.
    pub(crate) fn renew_marker(&self) -> Result<(), Error> {
        self.marker.lock().unwrap().check_due = true;
        self.check_marker()?;
        self.take_over()
    }

    fn check_marker(&self) -> Result<(), Error> {
        if self.stopped() {
            return Err(Error::ChangedElsewhere);
        }
        let mut marker = self.marker.lock().unwrap();
        if !marker.check_due {
            return Ok(());
        }
        let mut on_disk = [0u8; BLOCK];
        self.device.read_at(&mut on_disk, MARKER)?;
        if on_disk != marker.bytes || self.device.replaced() {
            self.stopped.store(true, Ordering::SeqCst);
            return Err(Error::ChangedElsewhere);
        }
        marker.check_due = false;
        Ok(())
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    pub(crate) fn read_slot(&self, slot: u16) -> Result<[u8; SLOT], Error> {
        let mut sealed = [0u8; SLOT];
        self.read_at(&mut sealed, slots::offset(slot))?;
        Ok(sealed)
    }

    pub(crate) fn write_slot(&self, slot: u16, sealed: &[u8; SLOT]) -> Result<(), Error> {
        self.write_at(sealed, slots::offset(slot))
    }

    /// Opens all four slots – always all four, so every phrase costs the same – and returns
    /// the valid copy with the highest generation.
    pub(crate) fn find_key_block(&self, wrapping: &Key, slots: [u16; 4]) -> Result<Option<KeyBlock>, Error> {
        let mut found: Option<KeyBlock> = None;
        for slot in slots {
            let opened = KeyBlock::open(wrapping, slot, &self.read_slot(slot)?);
            if let Some(block) = opened
                && found.as_ref().is_none_or(|best| block.generation > best.generation)
            {
                found = Some(block);
            }
        }
        Ok(found)
    }

    /// Claims the vault for a space; `None` if one is open already.
    pub(crate) fn claim(self: &Arc<Self>) -> Option<Claim> {
        (!self.space_open.swap(true, Ordering::SeqCst)).then(|| Claim(self.clone()))
    }
}

/// A vault's one open space: given up when dropped.
pub(crate) struct Claim(Arc<VaultFile>);

impl Claim {
    pub(crate) fn file(&self) -> &Arc<VaultFile> {
        &self.0
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.space_open.store(false, Ordering::SeqCst);
    }
}
