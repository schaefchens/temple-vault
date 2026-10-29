//! Entry points for fuzzing (`fuzz/`), not part of the API: arbitrary bytes for the parsers of
//! a vault's slot table and of a space's superblock. The harnesses seal what the fuzzer makes
//! up – random bytes would never pass their checks – and say what must hold besides not
//! panicking.

use crate::crypto::Key;
use crate::device::Device;
use crate::keys::PhraseKey;
use crate::noise::Amounts;
use crate::slots::{self, KeyBlock, SLOT};
use crate::vault::Vault;
use crate::{BLOCK, ZONE};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

pub use crate::space::fuzzing::{space, superblock};

/// No noise: a growth step then costs only the zones it adds.
pub(crate) const NO_NOISE: Amounts = Amounts { reserve: || 0, growth: || 0 };

/// Arbitrary bytes as the four slots a phrase key opens: each holds random bytes, a key
/// block for it, one sealed under another key, or one sealed for another slot. Unlocking
/// finds the valid copy with the highest generation, and the space it points at – whatever
/// version and first zone it names – opens or is refused without panicking.
pub fn slot_table(data: &[u8]) {
    let mut input = Input(data);
    let key = PhraseKey::from_bytes([3; 32]);
    let (wrapping, other) = (key.wrapping_key(), Key::from_bytes(&[4; 32]));
    let zones = 3 + u64::from(input.byte() % 6);
    let image = Image::new(zones * ZONE);
    let mut highest = None;
    for slot in key.slots() {
        let mode = input.byte() % 4;
        let block = KeyBlock {
            inner_version: input.u16() % 3,
            generation: input.u64(),
            master_key: input.key(),
            first_zone: input.u64() % (zones + 2),
        };
        let sealed: [u8; SLOT] = match mode {
            0 => input.bytes(SLOT).try_into().unwrap(),
            1 => {
                highest = highest.max(Some(block.generation));
                block.seal(&wrapping, slot)
            }
            2 => block.seal(&other, slot),
            _ => block.seal(&wrapping, slot ^ 1),
        };
        image.write_at(&sealed, slots::offset(slot)).unwrap();
    }
    let vault = Vault::open_on(image, NO_NOISE, false).unwrap();
    let found = vault.file.find_key_block(&wrapping, key.slots()).unwrap();
    assert_eq!(found.as_ref().map(|block| block.generation), highest);
    if let Some(block) = found {
        crate::space::fuzzing::open_store(&vault, &block);
    }
}

/// What the fuzzer gave, read front to back; zeros once it runs out.
pub(crate) struct Input<'a>(pub(crate) &'a [u8]);

impl Input<'_> {
    pub(crate) fn bytes(&mut self, n: usize) -> Vec<u8> {
        let take = n.min(self.0.len());
        let mut out = self.0[..take].to_vec();
        self.0 = &self.0[take..];
        out.resize(n, 0);
        out
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.bytes(1)[0]
    }

    pub(crate) fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.bytes(2).try_into().unwrap())
    }

    pub(crate) fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.bytes(4).try_into().unwrap())
    }

    pub(crate) fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.bytes(8).try_into().unwrap())
    }

    pub(crate) fn key(&mut self) -> Key {
        Key::from_bytes(&self.bytes(32).try_into().unwrap())
    }
}

/// A vault in memory that keeps only the blocks written to it, so that a file of many zones
/// costs nothing until used. What was never written reads as zeros.
pub(crate) struct Image {
    state: Mutex<Written>,
}

struct Written {
    len: u64,
    blocks: HashMap<u64, Box<[u8; BLOCK]>>,
}

impl Image {
    pub(crate) fn new(len: u64) -> Arc<Image> {
        Arc::new(Image { state: Mutex::new(Written { len, blocks: HashMap::new() }) })
    }
}

/// The pieces of `offset..offset + len` within each block: block, offset in it, length.
fn pieces(offset: u64, len: usize) -> impl Iterator<Item = (u64, usize, usize)> {
    let mut done = 0;
    std::iter::from_fn(move || {
        if done == len {
            return None;
        }
        let at = offset + done as u64;
        let within = (at % BLOCK as u64) as usize;
        let n = (BLOCK - within).min(len - done);
        done += n;
        Some((at / BLOCK as u64, within, n))
    })
}

impl Device for Image {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let state = self.state.lock().unwrap();
        if offset + buf.len() as u64 > state.len {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "read past the end"));
        }
        let mut done = 0;
        for (block, within, n) in pieces(offset, buf.len()) {
            match state.blocks.get(&block) {
                Some(bytes) => buf[done..done + n].copy_from_slice(&bytes[within..within + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
        Ok(())
    }

    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut done = 0;
        for (block, within, n) in pieces(offset, data.len()) {
            let bytes = state.blocks.entry(block).or_insert_with(|| Box::new([0; BLOCK]));
            bytes[within..within + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        state.len = state.len.max(offset + data.len() as u64);
        Ok(())
    }

    fn sync(&self) -> io::Result<()> {
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.state.lock().unwrap().len)
    }

    fn replaced(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    /// Inputs a fuzz target once crashed on, replayed.
    #[test]
    fn found_by_fuzzing() {
        // space, 2026-10-27: a record's zones overflowed when turned into an offset.
        super::space(&[
            0x00, 0x38, 0xff, 0x5e, 0x70, 0x8a, 0x71, 0x71, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x71,
            0x71, 0x79, 0x71, 0x00, 0x00, 0x23, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xff, 0xfe, 0x67, 0xbe, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x58, 0xb6, 0x00, 0x00, 0x31, 0x31, 0x31, 0x31,
            0x31, 0x31, 0x31, 0x31, 0x31, 0x31, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x71, 0x71, 0x98, 0xb4, 0x01,
        ]);
    }
}
