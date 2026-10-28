//! A space: what one set of 12 words opens.

mod copy;
mod fulltext;
#[cfg(any(test, feature = "fuzzing"))]
pub(crate) mod fuzzing;
mod heap;
mod objects;
mod runs;
mod store;
mod superblock;
mod vfs;

pub use objects::ObjectId;

use crate::crypto::Key;
use crate::keys::PhraseKey;
use crate::noise::Noise;
use crate::slots::{self, INNER_VERSION, KeyBlock, SLOT, SLOTS};
use crate::vault::{Claim, Setup, VaultFile};
use crate::words::Words;
use crate::{BLOCK, Error};
use rusqlite::{Connection, OpenFlags, ffi};
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex};
use store::{BLOCKS_PER_ZONE, FIRST_FREE, PAYLOAD, Store, too_large};
use superblock::MAX_PAGES;

/// A new space's first block holds at least 16 zones.
const MIN_ZONES: u64 = 16;

/// An open space. Locking it – [`Space::lock`], or dropping it – checkpoints the database
/// and drops its log's key, wipes keys and everything decrypted from memory, and renews the
/// vault's opening marker.
pub struct Space {
    db: Option<Connection>,
    store: Arc<Mutex<Store>>,
    file: Arc<VaultFile>,
    /// The name SQLite opened the database under.
    name: String,
    key: PhraseKey,
    block: KeyBlock,
    /// The vault's one open space is this one – except for a copy being made.
    claim: Option<Claim>,
    /// Opened or set up completely; until then, closing writes nothing.
    ready: bool,
    closed: bool,
}

impl Space {
    pub(crate) fn open(claim: Claim, key: PhraseKey, block: KeyBlock) -> Result<Space, Error> {
        match block.inner_version {
            INNER_VERSION => {}
            newer if newer > INNER_VERSION => return Err(Error::NewerFormat),
            _ => return Err(Error::Damaged),
        }
        vfs::install()?;
        let file = claim.file().clone();
        let store = Store::load(file.clone(), &block.master_key, block.first_zone)?;
        let mut space = Space::new(file, Some(claim), key, block, store);
        space.db = Some(connect(&space.name, false)?);
        let objects = objects::all_runs(space.db())?;
        {
            let mut store = space.store.lock().unwrap();
            store.init_free(objects)?;
            store.read_only = false;
            store.finish_open()?;
            store.admin = false;
        }
        space.ready = true;
        Ok(space)
    }

    /// A new space at the end of the vault: its zones and superblock first, then its four key
    /// blocks, so that a crash in between leaves only noise. The reserve follows the first
    /// space's first block.
    pub(crate) fn create(
        file: &Arc<VaultFile>,
        claim: Option<Claim>,
        key: PhraseKey,
        setup: Setup,
        first: bool,
    ) -> Result<Space, Error> {
        vfs::install()?;
        let zones = zones_for(setup.room.div_ceil(PAYLOAD as u64))?;
        let first_zone = file.append_noise(zones)?;
        if first {
            file.append_noise((file.noise.reserve)())?;
        }
        let block = KeyBlock { inner_version: INNER_VERSION, generation: 1, master_key: Key::random(), first_zone };
        let store = Store::create(file.clone(), &block.master_key, first_zone, zones)?;
        let mut space = Space::new(file.clone(), claim, key, block, store);
        space.db = Some(connect(&space.name, true)?);
        {
            let mut store = space.store.lock().unwrap();
            store.finish_create()?;
            store.admin = false;
        }
        space.claim_slots(&space.key)?;
        space.ready = true;
        Ok(space)
    }

    fn new(file: Arc<VaultFile>, claim: Option<Claim>, key: PhraseKey, block: KeyBlock, store: Store) -> Space {
        let store = Arc::new(Mutex::new(store));
        let name = vfs::add(store.clone());
        Space { db: None, store, file, name, key, block, claim, ready: false, closed: false }
    }

    /// The space's database: SQLite with full-text search. Tables named `temple_…` are
    /// Temple's. Pragmas that would break the space's guarantees – journal mode, checkpoints,
    /// `synchronous`, `secure_delete` and the like – are refused. For search, use FTS5:
    /// locking turns on its `secure-delete` option, where FTS3 and FTS4 would keep deleted
    /// terms in their index. A page that isn't the one the space recorded – in a copy of the
    /// vault taken mid-write, or put back by someone – fails to read with SQLite's I/O error.
    pub fn db(&self) -> &Connection {
        self.db.as_ref().expect("a space's database is open until the space locks")
    }

    /// The phrase key, for the app to seal with the phone's secure hardware (quick unlock).
    pub fn phrase_key(&self) -> PhraseKey {
        self.key.duplicate()
    }

    /// Whether another device has opened the vault since this one did. Temple then writes
    /// nothing more, and writes through [`Space::db`] fail with SQLite's I/O error.
    pub fn changed_elsewhere(&self) -> bool {
        self.file.stopped()
    }

    /// New words for this space: its key blocks at the new words' slots, then the old ones
    /// overwritten. A phrase key the app kept for quick unlock stops working.
    pub fn change_words(&mut self, words: &Words) -> Result<(), Error> {
        let key = PhraseKey::from_words(words);
        if self.file.find_key_block(&key.wrapping_key(), key.slots())?.is_some() {
            return Err(Error::WordsInUse);
        }
        self.block.generation += 1;
        self.claim_slots(&key)?;
        let old = std::mem::replace(&mut self.key, key);
        let old_wrapping = old.wrapping_key();
        for slot in old.slots() {
            if KeyBlock::open(&old_wrapping, slot, &self.file.read_slot(slot)?).is_some() {
                self.file.write_slot(slot, &random_slot())?;
            }
        }
        self.file.sync()
    }

    /// Keeps only this space: the slot table is rewritten with this space's key blocks at its
    /// slots and random bytes everywhere else, so every other space is gone at once – without
    /// Temple ever knowing them. The room they took stays.
    pub fn keep_only_this(&mut self) -> Result<(), Error> {
        self.block.generation += 1;
        self.claim_slots(&self.key)?;
        let mut ours = self.key.slots();
        ours.sort_unstable();
        let mut noise = Noise::new();
        let mut start = 0usize;
        for end in ours.iter().map(|&slot| slot as usize).chain([SLOTS]) {
            if end > start {
                let mut others = vec![0u8; (end - start) * SLOT];
                noise.fill(&mut others);
                self.file.write_at(&others, slots::offset(start as u16))?;
            }
            start = end + 1;
        }
        self.file.sync()
    }

    /// Writes this space's key block into the four slots of `key`, whatever they held, and
    /// syncs.
    fn claim_slots(&self, key: &PhraseKey) -> Result<(), Error> {
        let wrapping = key.wrapping_key();
        for slot in key.slots() {
            self.file.write_slot(slot, &self.block.seal(&wrapping, slot))?;
        }
        self.file.sync()
    }

    /// Locks the space: a full checkpoint copies the log into the database; both superblock
    /// copies then record an empty log without a key, so what the log held – deleted values
    /// too – can't be read any more. Keys and everything decrypted are wiped, and the vault's
    /// opening marker renewed.
    pub fn lock(mut self) -> Result<(), Error> {
        self.close()
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let locked = match self.db.take() {
            Some(db) if self.ready => self.lock_database(db),
            Some(db) => {
                self.store.lock().unwrap().read_only = true;
                drop(db);
                Ok(())
            }
            None => Ok(()),
        };
        vfs::remove(&self.name);
        self.claim = None;
        locked
    }

    fn lock_database(&self, db: Connection) -> Result<(), Error> {
        if self.file.stopped() {
            self.store.lock().unwrap().read_only = true;
            return Err(Error::ChangedElsewhere);
        }
        // What the app left uncommitted is dropped: a transaction would keep the checkpoint
        // from finishing, and the log readable.
        let rolled_back = if db.is_autocommit() { Ok(()) } else { db.execute_batch("ROLLBACK") };
        let begun = rolled_back
            .and_then(|()| fulltext::forget_deleted(&db))
            .map_err(|e| self.sql_error(e))
            .and_then(|()| self.store.lock().unwrap().begin_lock());
        if let Err(e) = begun {
            self.store.lock().unwrap().read_only = true;
            return Err(e);
        }
        let checkpoint = db.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        });
        match checkpoint {
            Ok((0, frames, copied)) if frames == copied => {}
            outcome => {
                // The log stays, with its key, for the next opening to replay.
                self.store.lock().unwrap().read_only = true;
                drop(db);
                return Err(match outcome {
                    Err(e) => self.sql_error(e),
                    Ok(_) => Error::Damaged,
                });
            }
        }
        // Closing, SQLite deletes the log, which only empties its record.
        db.close().map_err(|(_, e)| self.sql_error(e))?;
        self.store.lock().unwrap().finish_lock()?;
        self.file.renew_marker()
    }

    /// SQLite's result, with its I/O errors told apart when another device opened the vault.
    pub(crate) fn sql<T>(&self, result: rusqlite::Result<T>) -> Result<T, Error> {
        result.map_err(|e| self.sql_error(e))
    }

    fn sql_error(&self, e: rusqlite::Error) -> Error {
        if self.file.stopped() { Error::ChangedElsewhere } else { Error::Database(e) }
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
pub(crate) fn encode_runs(start: u32, len: u32) -> Vec<u8> {
    runs::encode(&[runs::Run { start, len }])
}

#[cfg(test)]
impl Space {
    pub(crate) fn master_key(&self) -> Key {
        self.block.master_key.duplicate()
    }

    pub(crate) fn first_zone(&self) -> u64 {
        self.block.first_zone
    }

    pub(crate) fn session_key(&self) -> Option<Key> {
        self.store.lock().unwrap().session_key()
    }

    /// The space's zones: first zone and number of zones, run by run.
    pub(crate) fn zones(&self) -> Vec<(u64, u64)> {
        self.store.lock().unwrap().zones()
    }

    pub(crate) fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    pub(crate) fn database_name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
pub(crate) const PAYLOAD_BYTES: usize = PAYLOAD;

/// Zones for a new space's first block: room for `blocks` of content, an eighth more and
/// 2 MiB for the database's log, never fewer than 16 – and no more than a space can hold,
/// checked before anything is appended.
fn zones_for(blocks: u64) -> Result<u64, Error> {
    let blocks = blocks.saturating_add(blocks / 8).saturating_add(2 * BLOCKS_PER_ZONE + u64::from(FIRST_FREE));
    let zones = blocks.div_ceil(BLOCKS_PER_ZONE).max(MIN_ZONES);
    if zones.saturating_mul(BLOCKS_PER_ZONE) > u64::from(u32::MAX) {
        return Err(too_large());
    }
    Ok(zones)
}

fn random_slot() -> [u8; SLOT] {
    let mut bytes = [0u8; SLOT];
    crate::crypto::random(&mut bytes);
    bytes
}

/// Opens the space's database through Temple's VFS. A new one is set up without a journal –
/// a crash then leaves only noise anyway – and switched to WAL mode, which writes its first
/// page: SQLite drops a log whose database is empty.
fn connect(name: &str, new: bool) -> Result<Connection, Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags_and_vfs(name, flags, vfs::NAME)?;
    if new {
        // The last 40 bytes of every page hold its block's nonce and tag.
        let mut reserve = (BLOCK - PAYLOAD) as c_int;
        let rc = unsafe {
            ffi::sqlite3_file_control(
                db.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_RESERVE_BYTES,
                &mut reserve as *mut c_int as *mut c_void,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None).into());
        }
        db.execute_batch("PRAGMA page_size = 4096; PRAGMA locking_mode = EXCLUSIVE; PRAGMA journal_mode = OFF;")?;
        db.execute_batch(objects::SCHEMA)?;
    } else {
        db.execute_batch("PRAGMA locking_mode = EXCLUSIVE")?;
    }
    let mode: String = db.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if mode != "wal" {
        return Err(Error::Damaged);
    }
    db.execute_batch(&format!(
        "PRAGMA synchronous = FULL; PRAGMA secure_delete = ON; PRAGMA wal_autocheckpoint = 0;
         PRAGMA temp_store = MEMORY; PRAGMA max_page_count = {MAX_PAGES};"
    ))?;
    Ok(db)
}
