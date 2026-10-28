//! A space's block space: its zones in order, cut into 4 KiB blocks. The first 32 blocks
//! hold the two superblock copies, written alternately; every other block belongs to the
//! database, its log or an object, or is free – and then keeps whatever random bytes or
//! old ciphertext it holds.
//!
//! Every block is XChaCha20-Poly1305 – payload ‖ nonce ‖ tag – with a fresh random nonce
//! on each write and its number in the file as associated data. A database page is read only
//! if its block's tag is the one the space's tag table holds for it: a page from another
//! moment – in a copy of the vault taken mid-write, or put back by someone – fails.

use super::runs::{self, Free, Run};
use super::superblock::{self, Extent, FileRecord, Superblock};
use crate::crypto::{Key, NONCE, TAG, open_trailing, seal_trailing};
use crate::noise::Noise;
use crate::slots::INNER_VERSION;
use crate::vault::VaultFile;
use crate::{BLOCK, Error, ZONE};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;
use zeroize::Zeroizing;

/// What a block holds besides its nonce and tag.
pub(crate) const PAYLOAD: usize = BLOCK - NONCE - TAG;
pub(crate) const BLOCKS_PER_ZONE: u64 = ZONE / BLOCK as u64;
const COPY_BLOCKS: u32 = (superblock::SIZE / BLOCK) as u32;
/// The first block after the two superblock copies.
pub(crate) const FIRST_FREE: u32 = 2 * COPY_BLOCKS;

/// The files SQLite keeps in a space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum File {
    /// Whole 4 KiB pages, one per block; SQLite leaves each page's last 40 bytes to the
    /// block's nonce and tag.
    Database,
    /// SQLite's write-ahead log, 4056 bytes per block, under the session key. Its record
    /// covers all its blocks; SQLite's frame checksums find where it ends.
    Log,
}

pub(crate) struct Store {
    vault: Arc<VaultFile>,
    superblock_key: Key,
    database_key: Key,
    /// The log's key: random, fresh for each session, kept only in the superblock.
    session: Option<Key>,
    extents: Vec<Extent>,
    blocks: u32,
    database: FileRecord,
    log: FileRecord,
    /// The tag of each database page's block, page by page, as the superblock vouches for
    /// it; on disk at `tag_table` once a lock or a setup has written it.
    tags: Vec<[u8; TAG]>,
    tag_table: FileRecord,
    tags_hash: [u8; 32],
    /// Database pages were written since the tag table was.
    tags_changed: bool,
    free: Free,
    /// Freed blocks the newest superblock on disk may still list. They become free once a
    /// superblock that doesn't is durable – database blocks scrubbed first, so deleted
    /// pages can't be read with the space's key.
    pending: Vec<Run>,
    scrub: Vec<Run>,
    /// Of the copies on disk, the highest.
    generation: u64,
    next_copy: usize,
    /// The record in memory differs from the newest copy on disk.
    changed: bool,
    /// The pages the last lock's checkpoint wrote.
    checkpoint: Vec<u32>,
    /// The other superblock copy still holds a session key the last lock dropped.
    stale_copy: bool,
    /// While locking: the database pages the checkpoint writes.
    locking: Option<BTreeSet<u32>>,
    /// Temple's own pragmas pass the VFS's guard.
    pub(crate) admin: bool,
    /// Until a space has opened, and once opening or locking failed: nothing may be written.
    pub(crate) read_only: bool,
    /// SQLite has the database open.
    pub(crate) database_open: bool,
}

impl Store {
    /// The store of a new space over zones that hold random bytes.
    pub(crate) fn create(vault: Arc<VaultFile>, master: &Key, first_zone: u64, zones: u64) -> Result<Store, Error> {
        let extents = vec![Extent { first_zone, zones }];
        let blocks = blocks_in(&extents).ok_or_else(too_large)?;
        let mut free = Free::default();
        free.add(Run { start: FIRST_FREE, len: blocks - FIRST_FREE });
        Ok(Store::new(vault, master, extents, blocks, free))
    }

    fn new(vault: Arc<VaultFile>, master: &Key, extents: Vec<Extent>, blocks: u32, free: Free) -> Store {
        Store {
            vault,
            superblock_key: master.derive(b"temple/v1/superblock"),
            database_key: master.derive(b"temple/v1/db"),
            session: None,
            extents,
            blocks,
            database: FileRecord::default(),
            log: FileRecord::default(),
            tags: Vec::new(),
            tag_table: FileRecord::default(),
            tags_hash: Sha256::digest([]).into(),
            tags_changed: false,
            free,
            pending: Vec::new(),
            scrub: Vec::new(),
            generation: 0,
            next_copy: 0,
            changed: true,
            checkpoint: Vec::new(),
            stale_copy: false,
            locking: None,
            admin: true,
            read_only: false,
            database_open: false,
        }
    }

    /// The store of an existing space: the newest superblock copy whose tag table is intact
    /// and whose last checkpoint is fully present, otherwise the other one. Free blocks come
    /// with [`Store::init_free`].
    pub(crate) fn load(vault: Arc<VaultFile>, master: &Key, first_zone: u64) -> Result<Store, Error> {
        let file_zones = vault.len()? / ZONE;
        if first_zone < 2 || first_zone >= file_zones {
            return Err(Error::Damaged);
        }
        let superblock_key = master.derive(b"temple/v1/superblock");
        let database_key = master.derive(b"temple/v1/db");
        let mut copies = Vec::new();
        for copy in 0..2 {
            let offset = first_zone * ZONE + (copy * superblock::SIZE) as u64;
            let mut room = Zeroizing::new(vec![0u8; superblock::SIZE]);
            vault.read_at(&mut room, offset)?;
            // Opening fails without touching the bytes, so each length is tried in place.
            for blocks in 1..=superblock::MAX_BLOCKS {
                let sealed = &mut room[..blocks * BLOCK];
                if open_trailing(&superblock_key, &superblock_data(offset, blocks), sealed) {
                    if let Some(record) = Superblock::decode(&sealed[..blocks * BLOCK - NONCE - TAG]) {
                        copies.push((copy, record));
                    }
                    break;
                }
            }
        }
        let generation = copies.iter().map(|(_, record)| record.generation).max().ok_or(Error::Damaged)?;
        copies.sort_by_key(|(_, record)| Reverse(record.generation));
        let mut chosen = None;
        for (i, (_, record)) in copies.iter().enumerate() {
            if record.inner_version > INNER_VERSION {
                return Err(Error::NewerFormat);
            }
            if record.inner_version != INNER_VERSION || !fits(record, first_zone, file_zones) {
                continue;
            }
            let Some(tags) = load_tags(&vault, &database_key, record)? else { continue };
            if record.session.is_some() || checkpoint_present(&vault, record, &tags)? {
                chosen = Some((i, tags));
                break;
            }
        }
        let (i, tags) = chosen.ok_or(Error::Damaged)?;
        let (copy, record) = copies.remove(i);
        // After a lock, both copies must be free of the session key – even a torn one, which
        // fails its check but whose bytes the space's key still decrypts.
        let stale_copy = record.session.is_none() && !copies.iter().any(|(_, other)| other.session.is_none());
        let blocks = blocks_in(&record.extents).ok_or(Error::Damaged)?;
        let mut store = Store::new(vault, master, record.extents, blocks, Free::default());
        store.session = record.session;
        store.database = record.database;
        store.log = record.log;
        store.tags = tags;
        store.tag_table = record.tags;
        store.tags_hash = record.tags_hash;
        store.generation = generation;
        store.next_copy = 1 - copy;
        store.changed = false;
        store.checkpoint = record.checkpoint;
        store.stale_copy = stale_copy;
        // Nothing is written until the space has opened: a failed opening leaves the vault
        // as it was.
        store.read_only = true;
        Ok(store)
    }

    /// Works out the free blocks: all that neither the superblock nor the object index
    /// claims.
    pub(crate) fn init_free(&mut self, objects: Vec<Run>) -> Result<(), Error> {
        let mut used = objects;
        used.extend(&self.database.runs);
        used.extend(&self.log.runs);
        used.extend(&self.tag_table.runs);
        self.free = Free::complement(FIRST_FREE, self.blocks, used).ok_or(Error::Damaged)?;
        Ok(())
    }

    /// A new space's database is set up: its tag table, then both superblock copies record it.
    pub(crate) fn finish_create(&mut self) -> Result<(), Error> {
        self.write_tags()?;
        self.write_both(&[])?;
        self.release()
    }

    /// Rewrites the other superblock copy if it may still hold a session key: a crash during
    /// the two superblock writes of a lock left it there.
    pub(crate) fn finish_open(&mut self) -> Result<(), Error> {
        if self.stale_copy {
            let checkpoint = self.checkpoint.clone();
            self.write_superblock(&checkpoint)?;
            self.vault.sync()?;
            self.stale_copy = false;
        }
        Ok(())
    }

    pub(crate) fn size(&self, file: File) -> u64 {
        self.record(file).size
    }

    fn record(&self, file: File) -> &FileRecord {
        match file {
            File::Database => &self.database,
            File::Log => &self.log,
        }
    }

    fn record_mut(&mut self, file: File) -> &mut FileRecord {
        match file {
            File::Database => &mut self.database,
            File::Log => &mut self.log,
        }
    }

    /// Reads from `offset`; returns how many bytes there were. Log blocks that fail their
    /// check read as zeros, so that SQLite's frame checksums end a torn log there; database
    /// pages must pass, tag included.
    pub(crate) fn read(&self, file: File, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let available = self.size(file).saturating_sub(offset).min(out.len() as u64) as usize;
        let unit = unit(file);
        // Opening a database, SQLite reads part of page 1 straight from the file, before it
        // looks at the log: page size, reserved bytes, vacuum flags. It reads them again
        // through the log, so the page needn't be the one the tag table holds.
        let header = file == File::Database && offset + (out.len() as u64) < BLOCK as u64;
        let mut block = Zeroizing::new([0u8; BLOCK]);
        let mut done = 0;
        while done < available {
            let at = offset + done as u64;
            let (index, within) = (at / unit as u64, (at % unit as u64) as usize);
            self.read_unit(file, index, &mut block, !header)?;
            let n = (unit - within).min(available - done);
            out[done..done + n].copy_from_slice(&block[within..within + n]);
            done += n;
        }
        Ok(available)
    }

    /// One block of a file, decrypted; a database page's last 40 bytes come back as zeros.
    fn read_unit(&self, file: File, index: u64, block: &mut [u8; BLOCK], tagged: bool) -> Result<(), Error> {
        let number = runs::nth(&self.record(file).runs, index).ok_or(Error::Damaged)?;
        let valid = match (file, &self.session) {
            (File::Database, _) => {
                self.read_block(number, &self.database_key, block)?
                    && (!tagged || self.tags.get(index as usize).is_some_and(|tag| block[BLOCK - TAG..] == tag[..]))
            }
            (File::Log, Some(session)) => self.read_block(number, session, block)?,
            (File::Log, None) => false,
        };
        match (valid, file) {
            (true, _) => block[PAYLOAD..].fill(0),
            (false, File::Log) => block.fill(0),
            (false, File::Database) => return Err(Error::Damaged),
        }
        Ok(())
    }

    /// Writes at `offset`: whole pages to the database, any bytes to the log.
    pub(crate) fn write(&mut self, file: File, offset: u64, data: &[u8]) -> Result<(), Error> {
        self.writable()?;
        if file == File::Database && (!offset.is_multiple_of(BLOCK as u64) || !data.len().is_multiple_of(BLOCK)) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the database is written in whole pages").into());
        }
        if file == File::Log && self.session.is_none() {
            self.session = Some(Key::random());
            self.changed = true;
        }
        let unit = unit(file);
        let end = offset + data.len() as u64;
        self.reserve(file, end.div_ceil(unit as u64))?;
        let size = self.size(file);
        let mut block = Zeroizing::new([0u8; BLOCK]);
        let mut done = 0;
        while done < data.len() {
            let at = offset + done as u64;
            let (index, within) = (at / unit as u64, (at % unit as u64) as usize);
            let n = (unit - within).min(data.len() - done);
            if n < PAYLOAD && index * (unit as u64) < size {
                self.read_unit(file, index, &mut block, true)?;
            } else {
                block.fill(0);
            }
            let take = n.min(PAYLOAD.saturating_sub(within));
            block[within..within + take].copy_from_slice(&data[done..done + take]);
            self.write_unit(file, index, &mut block)?;
            done += n;
        }
        if end > size {
            self.record_mut(file).size = end;
            // The superblock gives the log all its blocks: it needs no new record until
            // the log gets more.
            self.changed |= file == File::Database;
        }
        Ok(())
    }

    fn write_unit(&mut self, file: File, index: u64, block: &mut [u8; BLOCK]) -> Result<(), Error> {
        let number = runs::nth(&self.record(file).runs, index).expect("reserved before writing");
        let key = match file {
            File::Database => &self.database_key,
            File::Log => self.session.as_ref().expect("the log has a key while written"),
        };
        let tag = self.write_block(number, key, block)?;
        if file == File::Database {
            let index = index as usize;
            if self.tags.len() <= index {
                // Pages SQLite skipped hold nothing a check could pass.
                self.tags.resize(index + 1, [0; TAG]);
            }
            self.tags[index] = tag;
            self.tags_changed = true;
            if let Some(written) = &mut self.locking {
                written.insert(index as u32);
            }
        }
        Ok(())
    }

    /// Sets a file's size. Freed blocks wait for the next superblock; database blocks that
    /// held pages are scrubbed then.
    pub(crate) fn truncate(&mut self, file: File, size: u64) -> Result<(), Error> {
        self.writable()?;
        let old = self.size(file);
        if size == old {
            return Ok(());
        }
        if size > old {
            if file == File::Database {
                return self.write(file, old, &vec![0u8; (size - old) as usize]);
            }
            self.reserve(file, size.div_ceil(PAYLOAD as u64))?;
        }
        let unit = unit(file) as u64;
        let (keep, held) = (size.div_ceil(unit), old.div_ceil(unit));
        let record = self.record_mut(file);
        record.size = size;
        let mut freed = runs::split_off(&mut record.runs, keep);
        let spare = runs::split_off(&mut freed, held.saturating_sub(keep));
        match file {
            File::Database => {
                self.tags.truncate(keep as usize);
                self.tags_changed = true;
                self.scrub.extend(freed);
            }
            File::Log => self.pending.extend(freed),
        }
        self.pending.extend(spare);
        self.changed = true;
        Ok(())
    }

    /// Makes everything so far durable – SQLite calls it at every commit. Newly used blocks
    /// are recorded first, in both superblock copies, so that either is current when a copy
    /// of the vault falls back on it; while locking, the record waits for the end.
    pub(crate) fn sync(&mut self) -> Result<(), Error> {
        self.writable()?;
        if self.locking.is_some() || !self.changed {
            return self.vault.sync();
        }
        self.write_both(&[])?;
        self.release()
    }

    /// Locking starts: from here the superblock stays as it is until [`Store::finish_lock`],
    /// and the database pages the checkpoint writes are noted.
    pub(crate) fn begin_lock(&mut self) -> Result<(), Error> {
        self.admin = true;
        self.sync()?;
        self.locking = Some(BTreeSet::new());
        Ok(())
    }

    /// After the checkpoint: the new tag table, then both superblock copies get the next
    /// generation, one after the other – log empty, no session key, and the pages the
    /// checkpoint wrote. Without its key the log can't be read any more, so it needs no
    /// overwriting.
    pub(crate) fn finish_lock(&mut self) -> Result<(), Error> {
        if self.session.is_none() && !self.changed && !self.tags_changed {
            self.locking = None;
            return Ok(());
        }
        // Still locking: if the tag table needs room, the space grows without a record yet.
        self.write_tags()?;
        let written = self.locking.take().unwrap_or_default();
        let pages = self.tags.len() as u32;
        self.checkpoint = written.into_iter().filter(|&page| page < pages).collect();
        let log = std::mem::take(&mut self.log);
        self.pending.extend(log.runs);
        self.session = None;
        let checkpoint = self.checkpoint.clone();
        self.write_both(&checkpoint)?;
        self.release()
    }

    /// Writes the tag table to fresh blocks; the old ones wait for the next superblock.
    fn write_tags(&mut self) -> Result<(), Error> {
        let content: Vec<u8> = self.tags.iter().flatten().copied().collect();
        let count = content.len().div_ceil(PAYLOAD) as u32;
        let table = self.allocate(count, count)?;
        let mut block = [0u8; BLOCK];
        for (index, chunk) in content.chunks(PAYLOAD).enumerate() {
            block.fill(0);
            block[..chunk.len()].copy_from_slice(chunk);
            let number = runs::nth(&table, index as u64).expect("allocated above");
            self.write_block(number, &self.database_key, &mut block)?;
        }
        let old = std::mem::replace(&mut self.tag_table, FileRecord { size: content.len() as u64, runs: table });
        self.pending.extend(old.runs);
        self.tags_hash = Sha256::digest(&content).into();
        self.tags_changed = false;
        self.changed = true;
        Ok(())
    }

    /// Both superblock copies, one after the other, each synced.
    fn write_both(&mut self, checkpoint: &[u32]) -> Result<(), Error> {
        for _ in 0..2 {
            self.write_superblock(checkpoint)?;
            self.vault.sync()?;
        }
        Ok(())
    }

    /// Frees blocks no durable superblock lists any more, scrubbing database blocks first.
    fn release(&mut self) -> Result<(), Error> {
        if !self.scrub.is_empty() {
            let scrub = std::mem::take(&mut self.scrub);
            let mut noise = Noise::new();
            let mut bytes = [0u8; BLOCK];
            for run in &scrub {
                for number in run.start..run.end() as u32 {
                    noise.fill(&mut bytes);
                    self.vault.write_at(&bytes, self.offset(number)?)?;
                }
            }
            self.vault.sync()?;
            self.pending.extend(scrub);
        }
        for run in self.pending.drain(..) {
            self.free.add(run);
        }
        Ok(())
    }

    /// Between `needed` and `wanted` free blocks: as many as are free, up to `wanted`. Only if
    /// fewer than `needed` are left does the space grow.
    pub(crate) fn allocate(&mut self, needed: u32, wanted: u32) -> Result<Vec<Run>, Error> {
        self.writable()?;
        let free = self.free.total();
        if free < u64::from(needed) {
            self.grow(u64::from(needed) - free)?;
        }
        let n = u64::from(wanted.max(needed)).min(self.free.total());
        Ok(self.free.take(n as u32))
    }

    /// Gives back blocks that nothing durable points at.
    pub(crate) fn give_back(&mut self, runs: &[Run]) {
        for &run in runs {
            self.free.add(run);
        }
    }

    /// Makes sure a file has `blocks` blocks: grows it by half its size at least.
    fn reserve(&mut self, file: File, blocks: u64) -> Result<(), Error> {
        let have = runs::count(&self.record(file).runs);
        if have >= blocks {
            return Ok(());
        }
        let needed = u32::try_from(blocks - have).map_err(|_| too_large())?;
        let wanted = u32::try_from((have / 2).max(64)).unwrap_or(u32::MAX);
        let added = self.allocate(needed, wanted)?;
        let record = self.record_mut(file);
        for run in added {
            runs::push(&mut record.runs, run);
        }
        self.changed = true;
        Ok(())
    }

    /// A growth step: at least doubles the space, followed by growth noise. Appended and
    /// synced first, recorded after – so a crash leaves only unexplained noise.
    fn grow(&mut self, missing: u64) -> Result<(), Error> {
        let zones: u64 = self.extents.iter().map(|extent| extent.zones).sum();
        let added = zones.max(missing.div_ceil(BLOCKS_PER_ZONE));
        let blocks = u64::from(self.blocks) + added * BLOCKS_PER_ZONE;
        let blocks = u32::try_from(blocks).map_err(|_| too_large())?;
        let first_zone = self.vault.append_noise(added)?;
        self.vault.append_noise((self.vault.noise.growth)())?;
        self.vault.sync()?;
        self.extents.push(Extent { first_zone, zones: added });
        self.free.add(Run { start: self.blocks, len: blocks - self.blocks });
        self.blocks = blocks;
        self.changed = true;
        if self.locking.is_none() {
            self.write_both(&[])?;
        }
        Ok(())
    }

    /// Seals a block with `key`; returns its tag.
    pub(crate) fn write_block(&self, number: u32, key: &Key, block: &mut [u8; BLOCK]) -> Result<[u8; TAG], Error> {
        self.writable()?;
        let offset = self.offset(number)?;
        seal_trailing(key, &associated_data(offset), block);
        self.vault.write_at(block, offset)?;
        Ok(block[BLOCK - TAG..].try_into().unwrap())
    }

    /// Reads and opens a block; false if it fails its check.
    pub(crate) fn read_block(&self, number: u32, key: &Key, block: &mut [u8; BLOCK]) -> Result<bool, Error> {
        let offset = self.offset(number)?;
        self.vault.read_at(block, offset)?;
        Ok(open_trailing(key, &associated_data(offset), block))
    }

    fn write_superblock(&mut self, checkpoint: &[u32]) -> Result<(), Error> {
        let record = Superblock {
            inner_version: INNER_VERSION,
            generation: self.generation + 1,
            extents: self.extents.clone(),
            database: self.database.clone(),
            log: FileRecord { size: runs::count(&self.log.runs) * PAYLOAD as u64, runs: self.log.runs.clone() },
            tags: self.tag_table.clone(),
            tags_hash: self.tags_hash,
            session: self.session.as_ref().map(Key::duplicate),
            checkpoint: checkpoint.to_vec(),
        };
        let payload = record
            .encode()
            .ok_or_else(|| io::Error::new(io::ErrorKind::StorageFull, "the space's record outgrew its superblock"))?;
        let offset = self.extents[0].first_zone * ZONE + (self.next_copy * superblock::SIZE) as u64;
        self.vault.write_at(&seal_superblock(&self.superblock_key, offset, &payload), offset)?;
        self.generation += 1;
        self.next_copy = 1 - self.next_copy;
        self.changed = false;
        Ok(())
    }

    fn offset(&self, number: u32) -> Result<u64, Error> {
        offset_in(&self.extents, number).ok_or(Error::Damaged)
    }

    fn writable(&self) -> Result<(), Error> {
        if self.read_only { Err(Error::Damaged) } else { Ok(()) }
    }

    #[cfg(test)]
    pub(crate) fn session_key(&self) -> Option<Key> {
        self.session.as_ref().map(Key::duplicate)
    }

    #[cfg(test)]
    pub(crate) fn zones(&self) -> Vec<(u64, u64)> {
        self.extents.iter().map(|extent| (extent.first_zone, extent.zones)).collect()
    }

    #[cfg(test)]
    pub(crate) fn free_blocks(&self) -> u64 {
        self.free.total()
    }

    /// A space exactly full: every free block taken, never to come back.
    #[cfg(test)]
    pub(crate) fn fill_up(&mut self) {
        let n = self.free.total();
        self.free.take(n as u32);
    }

    /// Free room in pieces of one block: every other free block taken, never to come back.
    #[cfg(test)]
    pub(crate) fn fragment(&mut self) {
        let n = self.free.total();
        for run in self.free.take(n as u32) {
            for number in (run.start..run.end() as u32).step_by(2) {
                self.free.add(Run { start: number, len: 1 });
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn log_runs(&self) -> usize {
        self.log.runs.len()
    }
}

/// Bytes of a file per block.
fn unit(file: File) -> usize {
    match file {
        File::Database => BLOCK,
        File::Log => PAYLOAD,
    }
}

/// A superblock copy for its place: payload ‖ nonce ‖ tag, over as many blocks as the payload
/// needs.
pub(crate) fn seal_superblock(key: &Key, offset: u64, payload: &[u8]) -> Zeroizing<Vec<u8>> {
    let blocks = superblock::blocks_for(payload.len());
    let mut sealed = Zeroizing::new(vec![0u8; blocks * BLOCK]);
    sealed[..payload.len()].copy_from_slice(payload);
    seal_trailing(key, &superblock_data(offset, blocks), &mut sealed);
    sealed
}

/// Associated data of a block: its number in the file.
pub(crate) fn associated_data(offset: u64) -> [u8; 8] {
    (offset / BLOCK as u64).to_le_bytes()
}

/// Associated data of a superblock copy: its first block's number in the file, and how many
/// blocks it takes.
fn superblock_data(offset: u64, blocks: usize) -> [u8; 9] {
    let mut data = [0u8; 9];
    data[..8].copy_from_slice(&associated_data(offset));
    data[8] = blocks as u8;
    data
}

/// Where block `number` of a space lies in the file; `None` past its zones – or for zones no
/// file could hold.
pub(crate) fn offset_in(extents: &[Extent], number: u32) -> Option<u64> {
    let mut left = u64::from(number);
    for extent in extents {
        let blocks = extent.zones.checked_mul(BLOCKS_PER_ZONE)?;
        if left < blocks {
            return extent.first_zone.checked_mul(ZONE)?.checked_add(left * BLOCK as u64);
        }
        left -= blocks;
    }
    None
}

fn blocks_in(extents: &[Extent]) -> Option<u32> {
    let zones = extents.iter().try_fold(0u64, |sum, extent| sum.checked_add(extent.zones))?;
    u32::try_from(zones.checked_mul(BLOCKS_PER_ZONE)?).ok()
}

pub(crate) fn too_large() -> Error {
    io::Error::new(io::ErrorKind::StorageFull, "a space holds at most 16 TiB").into()
}

/// Whether a superblock fits the file it was found in: its zones inside the file, one run
/// after another, the first where the key block says; its files inside its blocks.
fn fits(record: &Superblock, first_zone: u64, file_zones: u64) -> bool {
    let Some(blocks) = blocks_in(&record.extents) else { return false };
    let Some(first) = record.extents.first() else { return false };
    if first.first_zone != first_zone || first.zones.saturating_mul(BLOCKS_PER_ZONE) < u64::from(FIRST_FREE) {
        return false;
    }
    let mut next = 2;
    for extent in &record.extents {
        match extent.first_zone.checked_add(extent.zones) {
            Some(end) if extent.zones > 0 && extent.first_zone >= next && end <= file_zones => next = end,
            _ => return false,
        }
    }
    let inside =
        |file: &FileRecord| file.runs.iter().all(|run| run.start >= FIRST_FREE && run.end() <= u64::from(blocks));
    let holds = |file: &FileRecord, unit: usize| file.size <= runs::count(&file.runs) * unit as u64;
    inside(&record.database)
        && inside(&record.log)
        && inside(&record.tags)
        && record.database.size.is_multiple_of(BLOCK as u64)
        && record.tags.size == record.database.size / BLOCK as u64 * TAG as u64
        && holds(&record.database, BLOCK)
        && holds(&record.log, PAYLOAD)
        && holds(&record.tags, PAYLOAD)
}

/// A superblock's tag table, if its blocks open and its content is what the superblock
/// vouches for.
fn load_tags(vault: &VaultFile, key: &Key, record: &Superblock) -> Result<Option<Vec<[u8; TAG]>>, Error> {
    let mut content = Vec::with_capacity(record.tags.size as usize);
    let mut block = [0u8; BLOCK];
    for index in 0..record.tags.size.div_ceil(PAYLOAD as u64) {
        let number = runs::nth(&record.tags.runs, index).expect("checked by fits");
        let offset = offset_in(&record.extents, number).expect("checked by fits");
        vault.read_at(&mut block, offset)?;
        if !open_trailing(key, &associated_data(offset), &mut block) {
            return Ok(None);
        }
        let n = (record.tags.size as usize - content.len()).min(PAYLOAD);
        content.extend_from_slice(&block[..n]);
    }
    if Sha256::digest(&content)[..] != record.tags_hash[..] {
        return Ok(None);
    }
    Ok(Some(content.as_chunks::<TAG>().0.to_vec()))
}

/// Whether every block the lock's checkpoint wrote holds what it wrote.
fn checkpoint_present(vault: &VaultFile, record: &Superblock, tags: &[[u8; TAG]]) -> Result<bool, Error> {
    for &page in &record.checkpoint {
        let number = runs::nth(&record.database.runs, u64::from(page));
        let offset = number.and_then(|number| offset_in(&record.extents, number));
        let (Some(offset), Some(expected)) = (offset, tags.get(page as usize)) else { return Ok(false) };
        let mut tag = [0u8; TAG];
        vault.read_at(&mut tag, offset + (BLOCK - TAG) as u64)?;
        if tag != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}
