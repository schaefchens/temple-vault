//! Temple's block layer in miniature: an in-memory vault image of 4 KiB blocks,
//! each sealed with XChaCha20-Poly1305 and a fresh nonce on every write, plus a
//! write log for crash and copy experiments.
//!
//! Blocks 0 and 1 are the two superblock copies, written alternately. A file can
//! be a shreddable log: its blocks use a session key kept only in the superblock,
//! so dropping the key makes the whole log unreadable without overwriting it.

use chacha20poly1305::aead::{Nonce, Tag};
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use std::io;

pub const BLOCK: usize = 4096;
const NONCE: usize = 24;
const TAG: usize = 16;
/// Usable bytes per block; nonce and tag take the last 40 bytes.
pub const PAYLOAD: usize = BLOCK - NONCE - TAG;
const SLOTS: usize = 2;

pub type Block = [u8; BLOCK];

/// How a file maps onto blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Any byte range, 4056 bytes per block.
    Bytes,
    /// Whole 4096-byte pages whose last 40 bytes are left to us (SQLite's reserved bytes).
    Pages,
}

#[derive(Clone, Debug)]
pub struct FileSpec {
    pub start: usize,
    pub blocks: usize,
    pub mode: Mode,
}

/// Every block write since logging started, in order.
#[derive(Clone, Default)]
pub struct Log {
    pub initial: Vec<Block>,
    pub writes: Vec<(usize, Box<Block>)>,
}

pub struct Space {
    pub blocks: Vec<Block>,
    cipher: XChaCha20Poly1305,
    files: Vec<FileSpec>,
    sizes: Vec<u64>,
    generation: u64,
    /// Overwrite blocks a shrinking file gives up with random bytes.
    pub scrub: bool,
    /// Per file: read blocks that fail their check as zeros instead of an error,
    /// leaving the judgement to the engine's own checksums.
    pub lenient: Vec<bool>,
    /// The file whose blocks use the session key.
    pub shred: Option<usize>,
    session: Option<([u8; 32], XChaCha20Poly1305)>,
    /// While locking: superblock writes wait, database writes are remembered.
    deferring: bool,
    checkpointed: Vec<usize>,
    pub log: Option<Log>,
}

impl std::fmt::Debug for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Space").field("files", &self.files).field("sizes", &self.sizes).finish()
    }
}

pub fn random_block() -> Block {
    let mut b = [0u8; BLOCK];
    getrandom::fill(&mut b).expect("system randomness");
    b
}

/// Lays files out one after another behind the two superblock copies.
pub fn layout(files: &[(usize, Mode)]) -> Vec<FileSpec> {
    let mut start = SLOTS;
    files
        .iter()
        .map(|&(blocks, mode)| {
            let spec = FileSpec { start, blocks, mode };
            start += blocks;
            spec
        })
        .collect()
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new_from_slice(key).expect("32-byte key")
}

/// One superblock copy, decoded.
struct Superblock {
    generation: u64,
    sizes: Vec<u64>,
    session: Option<[u8; 32]>,
    /// Blocks the last checkpoint wrote, with their tags.
    verify: Vec<(usize, [u8; TAG])>,
}

impl Space {
    /// A fresh space: every block is real random noise, every file empty.
    pub fn new(key: [u8; 32], files: Vec<FileSpec>) -> Space {
        let total = SLOTS + files.iter().map(|f| f.blocks).sum::<usize>();
        let blocks = (0..total).map(|_| random_block()).collect();
        let sizes = vec![0; files.len()];
        let mut space = Space::assemble(key, files, blocks, sizes);
        space.write_header(&[]);
        space
    }

    /// Opens an existing image, such as a snapshot: the newest superblock copy whose
    /// checkpoint is fully present, otherwise the other one.
    pub fn from_image(key: [u8; 32], files: Vec<FileSpec>, blocks: Vec<Block>) -> io::Result<Space> {
        let mut space = Space::assemble(key, files, blocks, Vec::new());
        let mut copies: Vec<Superblock> = (0..SLOTS).filter_map(|slot| space.decode(slot)).collect();
        copies.sort_by_key(|s| std::cmp::Reverse(s.generation));
        let chosen = copies
            .into_iter()
            .find(|s| s.verify.iter().all(|(index, tag)| space.blocks[*index][PAYLOAD + NONCE..] == tag[..]))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no usable superblock"))?;
        space.generation = chosen.generation;
        space.sizes = chosen.sizes;
        space.session = chosen.session.map(|k| (k, cipher(&k)));
        Ok(space)
    }

    fn assemble(key: [u8; 32], files: Vec<FileSpec>, blocks: Vec<Block>, sizes: Vec<u64>) -> Space {
        let lenient = vec![false; files.len()];
        Space {
            blocks,
            cipher: cipher(&key),
            files,
            sizes,
            generation: 0,
            scrub: true,
            lenient,
            shred: None,
            session: None,
            deferring: false,
            checkpointed: Vec::new(),
            log: None,
        }
    }

    pub fn files(&self) -> Vec<FileSpec> {
        self.files.clone()
    }

    pub fn start_log(&mut self) {
        self.log = Some(Log { initial: self.blocks.clone(), writes: Vec::new() });
    }

    pub fn size(&self, file: usize) -> u64 {
        self.sizes[file]
    }

    fn file_of(&self, index: usize) -> Option<usize> {
        self.files.iter().position(|f| (f.start..f.start + f.blocks).contains(&index))
    }

    /// The cipher for a block: the session key inside the shreddable log, the space key elsewhere.
    fn cipher_for(&self, index: usize) -> Option<&XChaCha20Poly1305> {
        if self.shred.is_some() && self.file_of(index) == self.shred {
            self.session.as_ref().map(|(_, c)| c)
        } else {
            Some(&self.cipher)
        }
    }

    fn seal(&self, index: usize, payload: &[u8]) -> Block {
        let mut block = [0u8; BLOCK];
        block[..PAYLOAD].copy_from_slice(payload);
        let mut nonce = [0u8; NONCE];
        getrandom::fill(&mut nonce).expect("system randomness");
        let nonce = Nonce::<XChaCha20Poly1305>::try_from(&nonce[..]).unwrap();
        let tag = self
            .cipher_for(index)
            .expect("writing the log needs a session key")
            .encrypt_inout_detached(&nonce, &(index as u64).to_le_bytes(), (&mut block[..PAYLOAD]).into())
            .expect("encryption");
        block[PAYLOAD..PAYLOAD + NONCE].copy_from_slice(&nonce);
        block[PAYLOAD + NONCE..].copy_from_slice(&tag);
        block
    }

    /// Decrypts one block; fails if it is noise, torn, not at its own position, or its key is gone.
    pub fn open_block(&self, index: usize) -> io::Result<[u8; PAYLOAD]> {
        let failed = || io::Error::new(io::ErrorKind::InvalidData, format!("block {index} fails its check"));
        let cipher = self.cipher_for(index).ok_or_else(failed)?;
        let block = &self.blocks[index];
        let mut payload = [0u8; PAYLOAD];
        payload.copy_from_slice(&block[..PAYLOAD]);
        let nonce = Nonce::<XChaCha20Poly1305>::try_from(&block[PAYLOAD..PAYLOAD + NONCE]).unwrap();
        let tag = Tag::<XChaCha20Poly1305>::try_from(&block[PAYLOAD + NONCE..]).unwrap();
        cipher
            .decrypt_inout_detached(&nonce, &(index as u64).to_le_bytes(), (&mut payload[..]).into(), &tag)
            .map_err(|_| failed())?;
        Ok(payload)
    }

    fn store(&mut self, index: usize, block: Block) {
        if self.deferring && self.file_of(index) == Some(0) {
            self.checkpointed.push(index);
        }
        if let Some(log) = &mut self.log {
            log.writes.push((index, Box::new(block)));
        }
        self.blocks[index] = block;
    }

    fn put(&mut self, index: usize, payload: &[u8]) {
        let block = self.seal(index, payload);
        self.store(index, block);
    }

    fn decode(&self, slot: usize) -> Option<Superblock> {
        let p = self.open_block(slot).ok()?;
        let u64_at = |at: usize| u64::from_le_bytes(p[at..at + 8].try_into().unwrap());
        let u32_at = |at: usize| u32::from_le_bytes(p[at..at + 4].try_into().unwrap()) as usize;
        let generation = u64_at(0);
        let n = u32_at(8);
        let sizes: Vec<u64> = (0..n).map(|i| u64_at(12 + i * 8)).collect();
        let mut at = 12 + n * 8;
        let session = (p[at] == 1).then(|| p[at + 1..at + 33].try_into().unwrap());
        at += 33;
        let count = u32_at(at);
        at += 4;
        let verify = (0..count)
            .map(|i| {
                let e = at + i * (4 + TAG);
                (u32_at(e), p[e + 4..e + 4 + TAG].try_into().unwrap())
            })
            .collect();
        Some(Superblock { generation, sizes, session, verify })
    }

    /// Writes the next generation into the older copy.
    fn write_header(&mut self, verify: &[(usize, [u8; TAG])]) {
        self.generation += 1;
        let mut p = [0u8; PAYLOAD];
        p[0..8].copy_from_slice(&self.generation.to_le_bytes());
        p[8..12].copy_from_slice(&(self.sizes.len() as u32).to_le_bytes());
        let mut at = 12;
        for size in &self.sizes {
            p[at..at + 8].copy_from_slice(&size.to_le_bytes());
            at += 8;
        }
        if let Some((key, _)) = &self.session {
            p[at] = 1;
            p[at + 1..at + 33].copy_from_slice(key);
        }
        at += 33;
        assert!(at + 4 + verify.len() * (4 + TAG) <= PAYLOAD, "checkpoint list too long for one block");
        p[at..at + 4].copy_from_slice(&(verify.len() as u32).to_le_bytes());
        at += 4;
        for (index, tag) in verify {
            p[at..at + 4].copy_from_slice(&(*index as u32).to_le_bytes());
            p[at + 4..at + 4 + TAG].copy_from_slice(tag);
            at += 4 + TAG;
        }
        let slot = (self.generation % SLOTS as u64) as usize;
        self.put(slot, &p);
    }

    fn set_size(&mut self, file: usize, size: u64) {
        if self.sizes[file] != size {
            self.sizes[file] = size;
            if !self.deferring {
                self.write_header(&[]);
            }
        }
    }

    /// Starts a session: an empty log gets a fresh session key; a log left by a crash keeps its key.
    pub fn begin_session(&mut self) {
        if let Some(file) = self.shred {
            if self.sizes[file] == 0 || self.session.is_none() {
                let mut key = [0u8; 32];
                getrandom::fill(&mut key).expect("system randomness");
                self.session = Some((key, cipher(&key)));
                self.sizes[file] = 0;
                self.write_header(&[]);
            }
        }
    }

    /// From here until `finish_lock`, the superblock stays as it was.
    pub fn begin_lock(&mut self) {
        self.deferring = true;
        self.checkpointed.clear();
    }

    /// Both superblock copies get the new generation: log empty, no session key,
    /// and the list of blocks the checkpoint wrote.
    pub fn finish_lock(&mut self) {
        if let Some(file) = self.shred {
            self.sizes[file] = 0;
            self.session = None;
        }
        let mut written = std::mem::take(&mut self.checkpointed);
        written.sort_unstable();
        written.dedup();
        let verify: Vec<(usize, [u8; TAG])> =
            written.iter().map(|&i| (i, self.blocks[i][PAYLOAD + NONCE..].try_into().unwrap())).collect();
        self.deferring = false;
        self.write_header(&verify);
        self.write_header(&verify);
    }

    fn unit(&self, file: usize) -> usize {
        match self.files[file].mode {
            Mode::Bytes => PAYLOAD,
            Mode::Pages => BLOCK,
        }
    }

    fn physical(&self, file: usize, unit: usize) -> io::Result<usize> {
        let spec = &self.files[file];
        if unit >= spec.blocks {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "file region full"));
        }
        Ok(spec.start + unit)
    }

    fn open_or_zeros(&self, file: usize, physical: usize) -> io::Result<[u8; PAYLOAD]> {
        match self.open_block(physical) {
            Ok(payload) => Ok(payload),
            Err(_) if self.lenient[file] => Ok([0u8; PAYLOAD]),
            Err(e) => Err(e),
        }
    }

    /// Reads what exists at `offset`; returns how many bytes that was.
    pub fn read(&self, file: usize, offset: u64, out: &mut [u8]) -> io::Result<usize> {
        let size = self.sizes[file];
        let available = size.saturating_sub(offset).min(out.len() as u64) as usize;
        let unit = self.unit(file);
        let mut done = 0;
        while done < available {
            let position = offset as usize + done;
            let (index, within) = (position / unit, position % unit);
            let payload = self.open_or_zeros(file, self.physical(file, index)?)?;
            let mut page = [0u8; BLOCK];
            page[..PAYLOAD].copy_from_slice(&payload);
            let n = (unit - within).min(available - done);
            out[done..done + n].copy_from_slice(&page[within..within + n]);
            done += n;
        }
        Ok(available)
    }

    pub fn write(&mut self, file: usize, offset: u64, data: &[u8]) -> io::Result<()> {
        if offset > self.sizes[file] {
            self.set_len(file, offset)?;
        }
        let unit = self.unit(file);
        if self.files[file].mode == Mode::Pages {
            assert!(offset as usize % BLOCK == 0 && data.len() % BLOCK == 0, "page-mode writes are whole pages");
        }
        let size = self.sizes[file];
        let mut done = 0;
        while done < data.len() {
            let position = offset as usize + done;
            let (index, within) = (position / unit, position % unit);
            let physical = self.physical(file, index)?;
            let n = (unit - within).min(data.len() - done);
            let mut payload = [0u8; PAYLOAD];
            if (within > 0 || n < unit.min(PAYLOAD)) && ((index * unit) as u64) < size {
                payload = self.open_or_zeros(file, physical)?;
            }
            let end = (within + n).min(PAYLOAD);
            if within < PAYLOAD {
                payload[within..end].copy_from_slice(&data[done..done + (end - within)]);
            }
            self.put(physical, &payload);
            done += n;
        }
        let end = offset + data.len() as u64;
        if end > size {
            self.set_size(file, end);
        }
        Ok(())
    }

    /// Grows with zeros or shrinks; shrinking scrubs freed blocks when `scrub` is on,
    /// except in the shreddable log, which its lost key makes unreadable instead.
    pub fn set_len(&mut self, file: usize, len: u64) -> io::Result<()> {
        let unit = self.unit(file) as u64;
        let old = self.sizes[file];
        let used = |bytes: u64| bytes.div_ceil(unit) as usize;
        if len > old {
            let tail = old % unit;
            if tail != 0 && (tail as usize) < PAYLOAD {
                // Zero the rest of the last block so it reads as zeros once the file grows.
                let index = (old / unit) as usize;
                let physical = self.physical(file, index)?;
                let mut payload = self.open_or_zeros(file, physical)?;
                payload[tail as usize..].fill(0);
                self.put(physical, &payload);
            }
            for index in used(old)..used(len) {
                let physical = self.physical(file, index)?;
                self.put(physical, &[0u8; PAYLOAD]);
            }
        } else if len < old {
            // Shorten first: a crash must never leave a size that covers scrubbed blocks.
            self.set_size(file, len);
            if Some(file) == self.shred {
                return Ok(());
            }
            let tail = len % unit;
            if tail != 0 && (tail as usize) < PAYLOAD {
                let index = (len / unit) as usize;
                let physical = self.physical(file, index)?;
                let mut payload = self.open_or_zeros(file, physical)?;
                payload[tail as usize..].fill(0);
                self.put(physical, &payload);
            }
            if self.scrub {
                for index in used(len)..used(old) {
                    let physical = self.physical(file, index)?;
                    self.store(physical, random_block());
                }
            }
            return Ok(());
        }
        self.set_size(file, len);
        Ok(())
    }

    /// Every block that decrypts, as one byte string: what someone holding the words could read.
    pub fn readable(&self) -> Vec<u8> {
        (0..self.blocks.len()).filter_map(|i| self.open_block(i).ok()).flatten().collect()
    }
}

/// A tiny deterministic generator for the experiments.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// The image after the first `k` logged writes – a crash at that point.
pub fn crash_image(log: &Log, k: usize) -> Vec<Block> {
    let mut blocks = log.initial.clone();
    for (index, block) in &log.writes[..k] {
        blocks[*index] = **block;
    }
    blocks
}

/// Each block as it stood at `moment(block)`, for moments within writes `from..=to`.
fn image_at(log: &Log, from: usize, to: usize, mut moment: impl FnMut(usize) -> usize) -> Vec<Block> {
    let mut blocks = crash_image(log, from);
    let mut versions: std::collections::HashMap<usize, Vec<(usize, &Block)>> = Default::default();
    for (position, (index, block)) in log.writes[from..to].iter().enumerate() {
        versions.entry(*index).or_default().push((from + position, block));
    }
    let mut touched: Vec<_> = versions.into_iter().collect();
    touched.sort_by_key(|(index, _)| *index);
    for (index, list) in touched {
        let at = moment(index);
        if let Some((_, block)) = list.iter().rev().find(|(position, _)| *position < at) {
            blocks[index] = **block;
        }
    }
    blocks
}

/// A copy taken while writes `from..to` happened: each block as it stood at its own random moment –
/// the worst case, as when a sync app uploads pieces in parallel.
pub fn torn_image(log: &Log, from: usize, to: usize, rng: &mut Rng) -> Vec<Block> {
    image_at(log, from, to, |_| from + rng.below((to - from + 1) as u64) as usize)
}

/// A copy read front to back while writes `from..to` happened, starting and ending at random moments.
pub fn sequential_image(log: &Log, from: usize, to: usize, rng: &mut Rng) -> Vec<Block> {
    let total = log.initial.len();
    let start = from + rng.below((to - from + 1) as u64) as usize;
    let end = start + rng.below((to - start + 1) as u64) as usize;
    image_at(log, from, to, |index| start + (end - start) * index / total)
}
