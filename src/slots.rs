//! The slot table, zone 0 of every vault: 4096 slots of 256 bytes. A space keeps four
//! copies of its key block there, at slots its words decide; every other slot holds
//! another space's copy or random bytes.

use crate::crypto::{Key, NONCE, TAG, open, seal};
use zeroize::Zeroizing;

pub(crate) const SLOTS: usize = 4096;
pub(crate) const SLOT: usize = 256;
const PLAINTEXT: usize = SLOT - NONCE - TAG;

/// The inner format this version of Temple writes and reads.
pub(crate) const INNER_VERSION: u16 = 1;

/// What a space's words unlock: its master key, and where the space starts.
pub(crate) struct KeyBlock {
    pub(crate) inner_version: u16,
    /// Of copies that disagree, the one with the highest generation counts.
    pub(crate) generation: u64,
    pub(crate) master_key: Key,
    pub(crate) first_zone: u64,
}

impl KeyBlock {
    /// The block for one slot: nonce ‖ ciphertext ‖ tag, under the wrapping key, with
    /// `temple/v1/slot` and the slot number as associated data.
    pub(crate) fn seal(&self, wrapping: &Key, slot: u16) -> [u8; SLOT] {
        let mut sealed = [0u8; SLOT];
        let plain = &mut sealed[NONCE..NONCE + PLAINTEXT];
        plain[0..2].copy_from_slice(&self.inner_version.to_le_bytes());
        plain[2..10].copy_from_slice(&self.generation.to_le_bytes());
        plain[10..42].copy_from_slice(self.master_key.as_bytes());
        plain[42..50].copy_from_slice(&self.first_zone.to_le_bytes());
        // The remaining 166 bytes are reserved: zeros.
        let (nonce, tag) = seal(wrapping, &associated_data(slot), plain);
        sealed[..NONCE].copy_from_slice(&nonce);
        sealed[NONCE + PLAINTEXT..].copy_from_slice(&tag);
        sealed
    }

    /// The key block in a slot, if the slot holds one under this wrapping key.
    pub(crate) fn open(wrapping: &Key, slot: u16, sealed: &[u8; SLOT]) -> Option<KeyBlock> {
        let mut plain = Zeroizing::new([0u8; PLAINTEXT]);
        plain.copy_from_slice(&sealed[NONCE..NONCE + PLAINTEXT]);
        let nonce = sealed[..NONCE].try_into().unwrap();
        let tag = sealed[NONCE + PLAINTEXT..].try_into().unwrap();
        if !open(wrapping, &associated_data(slot), &nonce, &tag, &mut plain[..]) {
            return None;
        }
        Some(KeyBlock {
            inner_version: u16::from_le_bytes(plain[0..2].try_into().unwrap()),
            generation: u64::from_le_bytes(plain[2..10].try_into().unwrap()),
            master_key: Key::from_bytes(plain[10..42].try_into().unwrap()),
            first_zone: u64::from_le_bytes(plain[42..50].try_into().unwrap()),
        })
    }
}

fn associated_data(slot: u16) -> [u8; 16] {
    let mut ad = [0u8; 16];
    ad[..14].copy_from_slice(b"temple/v1/slot");
    ad[14..].copy_from_slice(&slot.to_le_bytes());
    ad
}

pub(crate) fn offset(slot: u16) -> u64 {
    u64::from(slot) * SLOT as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn block() -> KeyBlock {
        KeyBlock { inner_version: 1, generation: 7, master_key: Key::random(), first_zone: 42 }
    }

    #[test]
    fn opens_with_its_key_in_its_slot_only() {
        let wrapping = Key::random();
        let original = block();
        let sealed = original.seal(&wrapping, 9);
        let opened = KeyBlock::open(&wrapping, 9, &sealed).unwrap();
        assert_eq!(
            (opened.inner_version, opened.generation, opened.first_zone, opened.master_key.as_bytes()),
            (1, 7, 42, original.master_key.as_bytes())
        );
        assert!(KeyBlock::open(&wrapping, 10, &sealed).is_none());
        assert!(KeyBlock::open(&Key::random(), 9, &sealed).is_none());
    }

    #[test]
    fn sealing_twice_gives_different_bytes() {
        let wrapping = Key::random();
        let block = block();
        assert_ne!(block.seal(&wrapping, 1), block.seal(&wrapping, 1));
    }
}
