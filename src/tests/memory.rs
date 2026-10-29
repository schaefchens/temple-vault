//! A vault in memory that records every write and sync.

use crate::device::Device;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Write { offset: u64, data: Vec<u8> },
    Sync,
}

#[derive(Default)]
pub(crate) struct Memory {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    /// Every event since recording started, and the bytes as they were then.
    recording: Option<(Vec<u8>, Vec<Event>)>,
    /// Every read since watching started: offset and length.
    reads: Option<Vec<(u64, usize)>>,
    replaced: bool,
    /// Writes fail, as on a full disk.
    full: bool,
}

impl Memory {
    pub(crate) fn new() -> Arc<Memory> {
        Arc::new(Memory::default())
    }

    pub(crate) fn with_bytes(bytes: Vec<u8>) -> Arc<Memory> {
        Arc::new(Memory { state: Mutex::new(State { bytes, ..State::default() }) })
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.state.lock().unwrap().bytes.clone()
    }

    pub(crate) fn set_bytes(&self, bytes: Vec<u8>) {
        self.state.lock().unwrap().bytes = bytes;
    }

    pub(crate) fn record(&self) {
        let mut state = self.state.lock().unwrap();
        state.recording = Some((state.bytes.clone(), Vec::new()));
    }

    /// The bytes when recording started, and every event since; recording stops.
    pub(crate) fn recorded(&self) -> (Vec<u8>, Vec<Event>) {
        self.state.lock().unwrap().recording.take().expect("recording")
    }

    /// How many events are recorded so far.
    pub(crate) fn events(&self) -> usize {
        self.state.lock().unwrap().recording.as_ref().map_or(0, |(_, events)| events.len())
    }

    pub(crate) fn watch_reads(&self) {
        self.state.lock().unwrap().reads = Some(Vec::new());
    }

    /// Every read since watching started; watching stops.
    pub(crate) fn reads(&self) -> Vec<(u64, usize)> {
        self.state.lock().unwrap().reads.take().expect("watching reads")
    }

    pub(crate) fn replace(&self) {
        self.state.lock().unwrap().replaced = true;
    }

    /// From now on writes fail, as on a full disk.
    pub(crate) fn fill_up(&self) {
        self.state.lock().unwrap().full = true;
    }
}

impl Device for Memory {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(reads) = &mut state.reads {
            reads.push((offset, buf.len()));
        }
        let start = offset as usize;
        let end = start + buf.len();
        if end > state.bytes.len() {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past the end"));
        }
        buf.copy_from_slice(&state.bytes[start..end]);
        Ok(())
    }

    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.full {
            return Err(io::ErrorKind::StorageFull.into());
        }
        let start = offset as usize;
        let end = start + data.len();
        assert!(start <= state.bytes.len(), "a write leaves a hole: the file would be sparse");
        if end > state.bytes.len() {
            state.bytes.resize(end, 0);
        }
        state.bytes[start..end].copy_from_slice(data);
        if let Some((_, events)) = &mut state.recording {
            events.push(Event::Write { offset, data: data.to_vec() });
        }
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        if let Some((_, events)) = &mut self.state.lock().unwrap().recording {
            events.push(Event::Sync);
        }
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.state.lock().unwrap().bytes.len() as u64)
    }

    fn replaced(&self) -> bool {
        self.state.lock().unwrap().replaced
    }
}
