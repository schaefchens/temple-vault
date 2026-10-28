//! Large objects – audio, documents: written once, in blocks of their own, each object under
//! its own random key in its database row. Deleting the row makes the object unreadable
//! without rewriting hundreds of megabytes.

use super::Space;
use super::runs::{self, Run};
use super::store::PAYLOAD;
use crate::crypto::Key;
use crate::{BLOCK, Error};
use rusqlite::{Connection, OptionalExtension, params};
use std::io::{self, Read};
use std::ops::Range;
use zeroize::Zeroizing;

/// Temple's own table in every space's database.
pub(crate) const SCHEMA: &str = "CREATE TABLE temple_objects (
    id INTEGER PRIMARY KEY,
    key BLOB NOT NULL,
    size INTEGER NOT NULL,
    blocks BLOB NOT NULL
) STRICT;";

/// An object's number in its space.
pub type ObjectId = i64;

/// An object's row.
pub(super) struct Object {
    pub(super) key: Key,
    pub(super) size: u64,
    pub(super) runs: Vec<Run>,
}

impl Space {
    /// Stores `content` as a new object and returns its id. Its blocks are written and
    /// synced before the row that points at them commits.
    pub fn put_object(&self, mut content: impl Read) -> Result<ObjectId, Error> {
        if !self.db().is_autocommit() {
            return Err(Error::InTransaction);
        }
        let key = Key::random();
        let mut used = Vec::new();
        let mut spare = Vec::new();
        let written = self.write_object(&mut content, &key, &mut used, &mut spare);
        self.store.lock().unwrap().give_back(&spare);
        let stored = written.and_then(|size| {
            self.store.lock().unwrap().sync()?;
            self.sql(self.db().execute(
                "INSERT INTO temple_objects (key, size, blocks) VALUES (?1, ?2, ?3)",
                params![&key.as_bytes()[..], size as i64, runs::encode(&used)],
            ))?;
            Ok(self.db().last_insert_rowid())
        });
        if stored.is_err() {
            self.store.lock().unwrap().give_back(&used);
        }
        stored
    }

    /// Writes `content` in blocks, allocated in chunks that grow with the object so that a
    /// large one lies in few runs. Returns its size; `spare` holds what's left of the chunk.
    fn write_object(
        &self,
        content: &mut impl Read,
        key: &Key,
        used: &mut Vec<Run>,
        spare: &mut Vec<Run>,
    ) -> Result<u64, Error> {
        let mut size = 0u64;
        let mut block = Zeroizing::new([0u8; BLOCK]);
        loop {
            let n = read_full(content, &mut block[..PAYLOAD])?;
            if n == 0 {
                return Ok(size);
            }
            block[n..].fill(0);
            let mut store = self.store.lock().unwrap();
            if spare.is_empty() {
                let chunk = (runs::count(used) / 2).clamp(256, 65536) as u32;
                *spare = store.allocate(1, chunk)?;
            }
            let number = spare[0].start;
            spare[0] = Run { start: number + 1, len: spare[0].len - 1 };
            if spare[0].len == 0 {
                spare.remove(0);
            }
            runs::push(used, Run { start: number, len: 1 });
            store.write_block(number, key, &mut block)?;
            size += n as u64;
            if n < PAYLOAD {
                return Ok(size);
            }
        }
    }

    /// Bytes `range` of an object, as far as it reaches.
    pub fn read_object(&self, id: ObjectId, range: Range<u64>) -> Result<Vec<u8>, Error> {
        let object = self.object(id)?;
        let end = range.end.min(object.size);
        // Wiped if reading fails halfway.
        let mut out = Zeroizing::new(Vec::with_capacity(end.saturating_sub(range.start) as usize));
        let mut block = Zeroizing::new([0u8; BLOCK]);
        let store = self.store.lock().unwrap();
        let mut at = range.start;
        while at < end {
            let (index, within) = (at / PAYLOAD as u64, (at % PAYLOAD as u64) as usize);
            let number = runs::nth(&object.runs, index).ok_or(Error::Damaged)?;
            if !store.read_block(number, &object.key, &mut block)? {
                return Err(Error::Damaged);
            }
            let n = (PAYLOAD - within).min((end - at) as usize);
            out.extend_from_slice(&block[within..within + n]);
            at += n as u64;
        }
        Ok(std::mem::take(&mut *out))
    }

    /// An object's size in bytes.
    pub fn object_len(&self, id: ObjectId) -> Result<u64, Error> {
        Ok(self.object(id)?.size)
    }

    /// Deletes an object: its row goes, and with it the only copy of its key. Its blocks are
    /// free from the space's next opening – not before, so that a copy of the vault taken
    /// during this session never finds them in use twice.
    pub fn delete_object(&self, id: ObjectId) -> Result<(), Error> {
        let deleted = self.sql(self.db().execute("DELETE FROM temple_objects WHERE id = ?1", [id]))?;
        if deleted == 0 {
            return Err(Error::NoSuchObject);
        }
        Ok(())
    }

    pub(super) fn object(&self, id: ObjectId) -> Result<Object, Error> {
        let row = self.sql(
            self.db()
                .query_row("SELECT key, size, blocks FROM temple_objects WHERE id = ?1", [id], |row| {
                    Ok((Zeroizing::new(row.get::<_, Vec<u8>>(0)?), row.get::<_, i64>(1)?, row.get::<_, Vec<u8>>(2)?))
                })
                .optional(),
        )?;
        let (key, size, blocks) = row.ok_or(Error::NoSuchObject)?;
        let key: &[u8; 32] = key.as_slice().try_into().map_err(|_| Error::Damaged)?;
        let size = u64::try_from(size).map_err(|_| Error::Damaged)?;
        let runs = runs::decode(&blocks).ok_or(Error::Damaged)?;
        if size > runs::count(&runs) * PAYLOAD as u64 {
            return Err(Error::Damaged);
        }
        Ok(Object { key: Key::from_bytes(key), size, runs })
    }
}

/// The blocks of every object, for working out the free ones.
pub(crate) fn all_runs(db: &Connection) -> Result<Vec<Run>, Error> {
    let mut statement = db.prepare("SELECT blocks FROM temple_objects")?;
    let mut rows = statement.query([])?;
    let mut all = Vec::new();
    while let Some(row) = rows.next()? {
        all.extend(runs::decode(&row.get::<_, Vec<u8>>(0)?).ok_or(Error::Damaged)?);
    }
    Ok(all)
}

/// Reads until `buf` is full or `content` ends.
fn read_full(content: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match content.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}
