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

/// An object written piece by piece – for content that arrives in pieces, as through the
/// bindings: from [`Space::begin_object`], each piece through [`Space::write_object`], and
/// stored by [`Space::finish_object`]. Until then nothing points at its blocks: an object
/// never finished leaves them free from the space's next opening. A piece that fails to be
/// written ends the object, its blocks free at once; it is begun again.
pub struct ObjectWriter {
    /// The space it belongs to, by the name of its database.
    space: String,
    key: Key,
    used: Vec<Run>,
    /// What's left of the blocks last allocated.
    spare: Vec<Run>,
    size: u64,
    /// The block being filled: `filled` bytes so far.
    block: Box<Zeroizing<[u8; BLOCK]>>,
    filled: usize,
    /// Writing failed: the object is gone.
    failed: bool,
}

impl Space {
    /// Stores `content` as a new object and returns its id. Its blocks are written and
    /// synced before the row that points at them commits – with a transaction the app has
    /// open, so that the app's own rows for the object commit with it. Rolled back, the
    /// object is gone, and its blocks are free from the next opening.
    pub fn put_object(&self, mut content: impl Read) -> Result<ObjectId, Error> {
        let mut object = self.begin_object()?;
        self.begin_work();
        let mut piece = Zeroizing::new([0u8; PAYLOAD]);
        let written = loop {
            match read_full(&mut content, &mut piece[..]) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    if let Err(e) = self.write_pieces(&mut object, &piece[..n]) {
                        break Err(e);
                    }
                }
                Err(e) => break Err(e.into()),
            }
        };
        match written {
            Ok(()) => self.finish_object(object),
            Err(e) => {
                self.end(&mut object);
                Err(e)
            }
        }
    }

    /// A new object, empty so far.
    pub fn begin_object(&self) -> Result<ObjectWriter, Error> {
        self.writable()?;
        Ok(ObjectWriter {
            space: self.name.clone(),
            key: Key::random(),
            used: Vec::new(),
            spare: Vec::new(),
            size: 0,
            block: Box::new(Zeroizing::new([0; BLOCK])),
            filled: 0,
            failed: false,
        })
    }

    /// Adds `bytes` to an object, writing every block they fill.
    pub fn write_object(&self, object: &mut ObjectWriter, bytes: &[u8]) -> Result<(), Error> {
        self.owns(object)?;
        self.begin_work();
        let written = self.write_pieces(object, bytes);
        if written.is_err() {
            self.end(object);
        }
        written
    }

    fn write_pieces(&self, object: &mut ObjectWriter, mut bytes: &[u8]) -> Result<(), Error> {
        self.writable()?;
        self.owns(object)?;
        while !bytes.is_empty() {
            let n = (PAYLOAD - object.filled).min(bytes.len());
            object.block[object.filled..object.filled + n].copy_from_slice(&bytes[..n]);
            object.filled += n;
            bytes = &bytes[n..];
            if object.filled == PAYLOAD {
                self.go_on()?;
                self.store_block(object)?;
            }
        }
        Ok(())
    }

    /// Writes the block being filled, in blocks allocated in chunks that grow with the object,
    /// so that a large one lies in few runs.
    fn store_block(&self, object: &mut ObjectWriter) -> Result<(), Error> {
        object.block[object.filled..].fill(0);
        let mut store = self.store.lock().unwrap();
        if object.spare.is_empty() {
            let chunk = (runs::count(&object.used) / 2).clamp(256, 65536) as u32;
            object.spare = store.allocate(1, chunk, 1)?;
        }
        let number = object.spare[0].start;
        object.spare[0] = Run { start: number + 1, len: object.spare[0].len - 1 };
        if object.spare[0].len == 0 {
            object.spare.remove(0);
        }
        runs::push(&mut object.used, Run { start: number, len: 1 });
        store.write_block(number, &object.key, &mut object.block)?;
        object.size += object.filled as u64;
        object.filled = 0;
        Ok(())
    }

    /// Stores an object written with [`Space::write_object`]: its last block, synced, then
    /// its row – as [`Space::put_object`] does. Returns its id.
    pub fn finish_object(&self, mut object: ObjectWriter) -> Result<ObjectId, Error> {
        self.writable()?;
        self.owns(&object)?;
        let last = if object.filled > 0 { self.store_block(&mut object) } else { Ok(()) };
        if let Err(e) = last.and_then(|()| self.store.lock().unwrap().sync()) {
            self.end(&mut object);
            return Err(e);
        }
        self.store.lock().unwrap().give_back(&std::mem::take(&mut object.spare));
        // A commit that fails may still come back from the log after a crash, so its blocks
        // stay out of use until the next opening, which works out the free ones anew.
        self.as_temple(|db| {
            db.execute(
                "INSERT INTO temple_objects (key, size, blocks) VALUES (?1, ?2, ?3)",
                params![&object.key.as_bytes()[..], object.size as i64, runs::encode(&object.used)],
            )
        })?;
        Ok(self.db().last_insert_rowid())
    }

    fn owns(&self, object: &ObjectWriter) -> Result<(), Error> {
        if object.space != self.name {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "the object was begun in another space").into());
        }
        if object.failed {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "writing the object failed: begin it again").into());
        }
        Ok(())
    }

    /// Ends an object that failed to be written: nothing points at its blocks yet.
    fn end(&self, object: &mut ObjectWriter) {
        let mut store = self.store.lock().unwrap();
        store.give_back(&std::mem::take(&mut object.spare));
        store.give_back(&std::mem::take(&mut object.used));
        object.failed = true;
    }

    /// Bytes `range` of an object, as far as it reaches.
    pub fn read_object(&self, id: ObjectId, range: Range<u64>) -> Result<Vec<u8>, Error> {
        self.begin_work();
        let object = self.object(id)?;
        let end = range.end.min(object.size);
        // On 32-bit targets such as wasm, a range can exceed what memory holds.
        let len = usize::try_from(end.saturating_sub(range.start))
            .map_err(|_| io::Error::new(io::ErrorKind::OutOfMemory, "the range is larger than memory can hold"))?;
        // Wiped if reading fails halfway.
        let mut out = Zeroizing::new(Vec::with_capacity(len));
        let mut block = Zeroizing::new([0u8; BLOCK]);
        let store = self.store.lock().unwrap();
        let mut at = range.start;
        while at < end {
            self.go_on()?;
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
        self.writable()?;
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

/// The blocks of every object, for working out the free ones. A row Temple didn't write
/// claims none; reading its object fails.
pub(crate) fn all_runs(db: &Connection) -> Result<Vec<Run>, Error> {
    let mut statement = db.prepare("SELECT blocks FROM temple_objects")?;
    let mut rows = statement.query([])?;
    let mut all = Vec::new();
    while let Some(row) = rows.next()? {
        if let Ok(blocks) = row.get::<_, Vec<u8>>(0)
            && let Some(runs) = runs::decode(&blocks)
        {
            all.extend(runs);
        }
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
