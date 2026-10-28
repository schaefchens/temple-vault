//! redb on the block layer: the space's file 0 is redb's whole storage.

use crate::blocks::Space;
use crate::workload::{Engine, Op, State};
use redb::{ReadableDatabase, ReadableTable, StorageBackend, TableDefinition};
use std::io;
use std::sync::{Arc, Mutex};

const KV: TableDefinition<u32, &str> = TableDefinition::new("kv");

#[derive(Debug)]
struct Backend(Arc<Mutex<Space>>);

impl StorageBackend for Backend {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().size(0))
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let n = self.0.lock().unwrap().read(0, offset, out)?;
        if n < out.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past the end"));
        }
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.0.lock().unwrap().set_len(0, len)
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.0.lock().unwrap().write(0, offset, data)
    }
}

pub struct RedbEngine {
    db: redb::Database,
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl RedbEngine {
    pub fn open(space: Arc<Mutex<Space>>) -> Result<RedbEngine, String> {
        let db = redb::Database::builder().create_with_backend(Backend(space)).map_err(text)?;
        Ok(RedbEngine { db })
    }

    /// Rewrites the database compactly; the file shrinks and the block layer scrubs the rest.
    pub fn compact(&mut self) -> Result<(), String> {
        while self.db.compact().map_err(text)? {}
        Ok(())
    }
}

impl Engine for RedbEngine {
    fn commit(&mut self, ops: &[Op]) -> Result<(), String> {
        let tx = self.db.begin_write().map_err(text)?;
        {
            let mut table = tx.open_table(KV).map_err(text)?;
            for op in ops {
                match op {
                    Op::Put(key, value) => {
                        table.insert(*key, value.as_str()).map_err(text)?;
                    }
                    Op::Del(key) => {
                        table.remove(*key).map_err(text)?;
                    }
                }
            }
        }
        tx.commit().map_err(text)
    }

    fn read_all(&mut self) -> Result<State, String> {
        let tx = self.db.begin_read().map_err(text)?;
        let table = match tx.open_table(KV) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(State::new()),
            Err(e) => return Err(text(e)),
        };
        let mut state = State::new();
        for entry in table.iter().map_err(text)? {
            let (key, value) = entry.map_err(text)?;
            state.insert(key.value(), value.value().to_string());
        }
        Ok(state)
    }

    fn check(&mut self) -> Result<bool, String> {
        self.db.check_integrity().map_err(text)
    }
}
