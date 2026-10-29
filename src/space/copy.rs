//! Copying a space into another vault: a step of rebuilding a vault.

use super::objects::ObjectId;
use super::runs;
use super::store::{PAYLOAD, Store};
use super::{Access, Open, Space, connect, zones_for};
use crate::crypto::{Key, TAG, wiping_stack};
use crate::keys::PhraseKey;
use crate::noise;
use crate::slots::{INNER_VERSION, KeyBlock};
use crate::vault::{Vault, VaultFile};
use crate::words::NewWords;
use crate::{BLOCK, Error};
use rusqlite::Connection;
#[cfg(unix)]
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

impl Space {
    /// A new vault at `path` holding a copy of this space: the first step of rebuilding a
    /// vault, with the everyday space. Under the same words – or under `new_words` after words
    /// leaked: the copy opens only with those, and its fresh master key is one the old words
    /// never wrapped. The reserve follows it, and the vault is written to a temporary file
    /// and renamed into place.
    #[cfg(unix)]
    pub fn copy_to_new_vault(&self, path: impl AsRef<Path>, new_words: Option<&NewWords>) -> Result<Vault, Error> {
        let key = self.copy_key(new_words);
        Vault::create_at(path.as_ref(), noise::REAL, |file, _claim| self.copy_to(file, key, true))
            .map(|(vault, ())| vault)
    }

    /// On the web: [`Space::copy_to_new_vault`] into an empty file of the origin private
    /// file system, through a sync access handle to it – named as
    /// [`Vault::create_with_handle`] describes.
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    pub fn copy_to_new_vault_with_handle(
        &self,
        handle: web_sys::FileSystemSyncAccessHandle,
        new_words: Option<&NewWords>,
    ) -> Result<Vault, Error> {
        let key = self.copy_key(new_words);
        let device = Arc::new(crate::device::OpfsDevice::new(handle));
        Vault::create_on(device, noise::REAL, |file, _claim| self.copy_to(file, key, true)).map(|(vault, ())| vault)
    }

    /// Copies this space into another vault, under the same words or `new_words`: the next
    /// steps of rebuilding a vault, with hidden spaces. Copied in one go – appended with room
    /// for all it holds, which leaves out deleted content – and its key blocks written last,
    /// so a crash leaves only noise.
    pub fn copy_into(&self, vault: &Vault, new_words: Option<&NewWords>) -> Result<(), Error> {
        vault.file.writable()?;
        let key = self.copy_key(new_words);
        let _claim = vault.file.claim()?;
        self.copy_to(&vault.file, key, false)
    }

    fn copy_key(&self, new_words: Option<&NewWords>) -> PhraseKey {
        new_words.map_or_else(|| self.key.duplicate(), |words| PhraseKey::from_words(words))
    }

    pub(crate) fn copy_to(&self, file: &Arc<VaultFile>, key: PhraseKey, first: bool) -> Result<(), Error> {
        self.begin_work();
        wiping_stack(|| self.copy_with(file, key, first))
    }

    fn copy_with(&self, file: &Arc<VaultFile>, key: PhraseKey, first: bool) -> Result<(), Error> {
        if file.find_key_block(&key.wrapping_key(), key.slots())?.is_some() {
            return Err(Error::WordsInUse);
        }
        file.take_over()?;
        let objects = self.sql(objects_with_sizes(self.db()))?;
        let pages: i64 = self.sql(self.db().query_row("PRAGMA page_count", [], |row| row.get(0)))?;
        // Room in blocks: the database's pages and its tag table, every object's blocks.
        let pages = pages as u64;
        let objects_blocks: u64 = objects.iter().map(|&(_, size)| size.div_ceil(PAYLOAD as u64)).sum();
        let zones = zones_for(pages + (pages * TAG as u64).div_ceil(PAYLOAD as u64) + objects_blocks)?;
        let first_zone = file.append_noise(zones)?;
        if first {
            file.append_noise((file.noise.reserve)())?;
        }
        let block = KeyBlock { inner_version: INNER_VERSION, generation: 1, master_key: Key::random(), first_zone };
        let store = Store::create(file.clone(), &block.master_key, first_zone, zones)?;
        let mut copy = Space::new(file.clone(), None, key, block, store, Access::Write);
        // VACUUM INTO writes a compact copy of the database; SQLite opens it through the
        // VFS under the copy's name. Then the copy turns to WAL mode.
        self.sql(self.db().execute("VACUUM INTO ?1", [&copy.name]))?;
        copy.db = Some(connect(&copy.name, Open::Existing, &copy.store)?);
        // All rows in one transaction, so that the log stays small.
        let rows = copy.sql(copy.db().unchecked_transaction())?;
        for (id, _) in objects {
            copy.copy_object(self, id)?;
        }
        copy.sql(rows.commit())?;
        let db = copy.db.take().expect("opened above");
        copy.lock_database(db)?;
        copy.claim_slots(&copy.key)
    }

    /// Copies one object from `source` under a fresh key and points its row – copied with
    /// the database – at the new blocks.
    fn copy_object(&self, source: &Space, id: ObjectId) -> Result<(), Error> {
        let object = source.object(id)?;
        let key = Key::random();
        let count = u32::try_from(object.size.div_ceil(PAYLOAD as u64)).map_err(|_| Error::Damaged)?;
        let runs = self.store.lock().unwrap().allocate(count, count, 1)?;
        let mut block = Zeroizing::new([0u8; BLOCK]);
        for index in 0..u64::from(count) {
            source.go_on()?;
            let from = runs::nth(&object.runs, index).ok_or(Error::Damaged)?;
            if !source.store.lock().unwrap().read_block(from, &object.key, &mut block)? {
                return Err(Error::Damaged);
            }
            let to = runs::nth(&runs, index).expect("allocated above");
            self.store.lock().unwrap().write_block(to, &key, &mut block)?;
        }
        self.as_temple(|db| {
            db.execute(
                "UPDATE temple_objects SET key = ?2, blocks = ?3 WHERE id = ?1",
                rusqlite::params![id, &key.as_bytes()[..], runs::encode(&runs)],
            )
        })?;
        Ok(())
    }
}

fn objects_with_sizes(db: &Connection) -> rusqlite::Result<Vec<(ObjectId, u64)>> {
    let mut statement = db.prepare("SELECT id, size FROM temple_objects ORDER BY id")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?;
    rows.collect()
}
