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

pub use objects::{ObjectId, ObjectWriter};

use crate::crypto::{Key, wiping_stack};
use crate::keys::PhraseKey;
use crate::noise::Noise;
use crate::slots::{self, INNER_VERSION, KeyBlock, SLOT, SLOTS};
use crate::vault::{Claim, Setup, VaultFile};
use crate::words::{NewWords, Words};
use crate::{BLOCK, Error};
use rusqlite::{Connection, InterruptHandle, OpenFlags, ffi};
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use store::{BLOCKS_PER_ZONE, FIRST_FREE, PAYLOAD, Store, too_large};
use superblock::MAX_PAGES;

/// A new space's first block holds at least 16 zones.
const MIN_ZONES: u64 = 16;

/// How a space opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Write,
    /// Nothing written: in a vault opened read-only.
    Read,
    /// Read-only, its blocks checked but not against the tag table: what's left of a damaged
    /// space.
    Salvage,
}

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
    access: Access,
    /// The vault's one open space is this one – except for a copy being made.
    claim: Option<Claim>,
    /// Set by an [`Interrupt`]: the work running stops.
    stop: Arc<AtomicBool>,
    /// Opened or set up completely; until then, closing writes nothing.
    ready: bool,
    closed: bool,
}

/// Stops the work running on a space, from another thread – SQL, an object written or read,
/// a copy into another vault – which then fails: SQL with SQLite's "interrupted", the rest
/// with `Interrupted`. For locking a space without waiting for that work; work that starts
/// afterwards runs as usual.
#[derive(Clone)]
pub struct Interrupt {
    sql: Arc<InterruptHandle>,
    stop: Arc<AtomicBool>,
}

impl Interrupt {
    pub fn interrupt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.sql.interrupt();
    }
}

impl Space {
    pub(crate) fn open(claim: Claim, key: PhraseKey, block: KeyBlock, access: Access) -> Result<Space, Error> {
        match block.inner_version {
            INNER_VERSION => {}
            newer if newer > INNER_VERSION => return Err(Error::NewerFormat),
            _ => return Err(Error::Damaged),
        }
        vfs::install()?;
        let file = claim.file().clone();
        let store = Store::load(file.clone(), &block.master_key, block.first_zone, access)?;
        let mut space = Space::new(file, Some(claim), key, block, store, access);
        let how = if access == Access::Write { Open::Existing } else { Open::ReadOnly };
        space.db = Some(connect(&space.name, how, &space.store)?);
        if access == Access::Write {
            let objects = objects::all_runs(space.db()).map_err(|e| match e {
                Error::Database(e) => space.explain(e),
                other => other,
            })?;
            let mut store = space.store.lock().unwrap();
            store.init_free(objects)?;
            store.read_only = false;
            store.finish_open()?;
            drop(store);
            // A space from 0.1.0 got this table only once an FTS5 table locked.
            space.sql(space.db().execute_batch(fulltext::SCHEMA))?;
        }
        space.store.lock().unwrap().admin = false;
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
        let mut space = Space::new(file.clone(), claim, key, block, store, Access::Write);
        space.db = Some(connect(&space.name, Open::New, &space.store)?);
        {
            let mut store = space.store.lock().unwrap();
            store.finish_create()?;
            store.admin = false;
        }
        space.claim_slots(&space.key)?;
        space.ready = true;
        Ok(space)
    }

    fn new(
        file: Arc<VaultFile>,
        claim: Option<Claim>,
        key: PhraseKey,
        block: KeyBlock,
        store: Store,
        access: Access,
    ) -> Space {
        let store = Arc::new(Mutex::new(store));
        let name = vfs::add(store.clone());
        let stop = Arc::new(AtomicBool::new(false));
        Space { db: None, store, file, name, key, block, access, claim, stop, ready: false, closed: false }
    }

    /// The space's database: SQLite with full-text search. Tables and views named `temple_…`
    /// are Temple's, in the temporary database too. Pragmas that would break the space's
    /// guarantees – journal mode, checkpoints, `synchronous`, `secure_delete` and the like –
    /// are refused. For search, use FTS5: locking turns on its `secure-delete` option, where
    /// FTS3 and FTS4 would keep deleted terms in their index. A page that isn't the one the
    /// space recorded – in a copy of the vault taken mid-write, or put back by someone – fails
    /// to read with SQLite's I/O error, which [`Space::explain`] tells apart.
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

    /// Whether the space is read-only: in a vault opened read-only, or salvaged. Its
    /// database then refuses writes as a read-only SQLite database does.
    pub fn is_read_only(&self) -> bool {
        self.access != Access::Write
    }

    /// A handle that stops the work running on this space from another thread.
    pub fn interrupt_handle(&self) -> Interrupt {
        Interrupt { sql: Arc::new(self.db().get_interrupt_handle()), stop: self.stop.clone() }
    }

    /// Work that an [`Interrupt`] stops begins: an interrupt from before doesn't count.
    fn begin_work(&self) {
        self.stop.store(false, Ordering::SeqCst);
    }

    /// `Interrupted` once an [`Interrupt`] has stopped the work running.
    fn go_on(&self) -> Result<(), Error> {
        if self.stop.load(Ordering::SeqCst) { Err(Error::Interrupted) } else { Ok(()) }
    }

    fn writable(&self) -> Result<(), Error> {
        if self.is_read_only() { Err(Error::ReadOnly) } else { Ok(()) }
    }

    /// New words for this space: its key blocks at the new words' slots, then the old ones
    /// overwritten. A phrase key the app kept for quick unlock stops working. Interrupted,
    /// it leaves both words opening the space: changing to the same new words again, from the
    /// old ones, finishes it – or [`Space::forget_words`] from the new ones. The master key
    /// stays: whoever has the old words and a copy of the vault from before can still read
    /// the space. After words leaked, rebuild the vault under new words instead
    /// ([`Space::copy_to_new_vault`]): its copy gets a fresh master key.
    pub fn change_words(&mut self, words: &NewWords) -> Result<(), Error> {
        self.writable()?;
        let key = PhraseKey::from_words(words);
        if key.as_bytes() == self.key.as_bytes()
            || self.file.find_key_block(&key.wrapping_key(), key.slots())?.is_some_and(|block| !self.is_ours(&block))
        {
            return Err(Error::WordsInUse);
        }
        self.block.generation += 1;
        self.claim_slots(&key)?;
        let old = std::mem::replace(&mut self.key, key);
        self.overwrite_slots_of(&old)?;
        self.file.sync()
    }

    /// Words that open this space no more – after a word change a crash interrupted, say:
    /// their key blocks overwritten. Whether they opened it; the words this space was opened
    /// with are refused as `WordsInUse`.
    pub fn forget_words(&mut self, words: &Words) -> Result<bool, Error> {
        self.writable()?;
        let key = PhraseKey::from_words(words);
        if key.as_bytes() == self.key.as_bytes() {
            return Err(Error::WordsInUse);
        }
        let forgotten = self.overwrite_slots_of(&key)?;
        if forgotten {
            self.file.sync()?;
        }
        Ok(forgotten)
    }

    /// Overwrites the slots of `key` that hold a key block of this space; whether any did.
    fn overwrite_slots_of(&self, key: &PhraseKey) -> Result<bool, Error> {
        let wrapping = key.wrapping_key();
        let mut any = false;
        for slot in key.slots() {
            if KeyBlock::open(&wrapping, slot, &self.file.read_slot(slot)?).is_some_and(|block| self.is_ours(&block)) {
                self.file.write_slot(slot, &random_slot())?;
                any = true;
            }
        }
        Ok(any)
    }

    /// Whether a key block opens this space.
    fn is_ours(&self, block: &KeyBlock) -> bool {
        block.master_key.as_bytes() == self.block.master_key.as_bytes() && block.first_zone == self.block.first_zone
    }

    /// Keeps only this space: the slot table is rewritten with this space's key blocks at its
    /// slots and random bytes everywhere else, so every other space is gone at once – without
    /// Temple ever knowing them. The room they took stays.
    pub fn keep_only_this(&mut self) -> Result<(), Error> {
        self.writable()?;
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
    /// opening marker renewed. A read-only space only closes.
    pub fn lock(mut self) -> Result<(), Error> {
        self.close()
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        // Even when locking panics: the registry would keep the store, and its keys, for good.
        struct Unregister(String);
        impl Drop for Unregister {
            fn drop(&mut self) {
                vfs::remove(&self.0);
            }
        }
        let _unregister = Unregister(self.name.clone());
        let locked = wiping_stack(|| match self.db.take() {
            Some(db) if self.ready && self.access == Access::Write => self.lock_database(db),
            Some(db) => {
                self.store.lock().unwrap().read_only = true;
                drop(db);
                Ok(())
            }
            None => Ok(()),
        });
        self.claim = None;
        locked
    }

    fn lock_database(&self, db: Connection) -> Result<(), Error> {
        if self.file.stopped() {
            self.store.lock().unwrap().read_only = true;
            return Err(Error::ChangedElsewhere);
        }
        // From here on, only Temple's own SQL runs.
        self.store.lock().unwrap().admin = true;
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

    /// Temple's own SQL on its tables, which the guard refuses the app's.
    pub(crate) fn as_temple<T>(&self, sql: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, Error> {
        self.store.lock().unwrap().admin = true;
        let result = sql(self.db());
        self.store.lock().unwrap().admin = false;
        self.sql(result)
    }

    /// SQLite's result, with its I/O errors told apart when another device opened the vault.
    pub(crate) fn sql<T>(&self, result: rusqlite::Result<T>) -> Result<T, Error> {
        result.map_err(|e| self.sql_error(e))
    }

    fn sql_error(&self, e: rusqlite::Error) -> Error {
        self.explain(e)
    }

    /// Temple's error behind one of SQLite's: `ChangedElsewhere` once another device opened the
    /// vault; for an I/O error, what failed – `Damaged` for a page that fails its check,
    /// `StorageFull` for a full disk or a record that outgrew its superblock; otherwise
    /// `Database`.
    pub fn explain(&self, error: rusqlite::Error) -> Error {
        if self.file.stopped() {
            return Error::ChangedElsewhere;
        }
        let failure = self.store.lock().unwrap().failure.take();
        match failure {
            Some(failure) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::SystemIoFailure) => failure,
            _ => Error::Database(error),
        }
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    New,
    Existing,
    /// SQLite reads, the log as well, and writes nothing – not even a checkpoint as it closes.
    ReadOnly,
}

/// Opens the space's database through Temple's VFS. A new one is set up without a journal –
/// a crash then leaves only noise anyway – and switched to WAL mode, which writes its first
/// page: SQLite drops a log whose database is empty. Its log turns over as it grows.
fn connect(name: &str, how: Open, store: &Arc<Mutex<Store>>) -> Result<Connection, Error> {
    let flags = match how {
        Open::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        _ => OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    };
    let db = Connection::open_with_flags_and_vfs(name, flags, vfs::NAME)?;
    if how == Open::New {
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
        db.execute_batch(fulltext::SCHEMA)?;
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
    // After wal_autocheckpoint, which replaces the hook.
    vfs::turn_over_log(&db, store);
    vfs::guard_tables(&db, store);
    Ok(db)
}
