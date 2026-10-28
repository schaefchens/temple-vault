//! From words to keys: Argon2id to the phrase key, HKDF-SHA256 from there.

use crate::crypto::Key;
use crate::slots::SLOTS;
use crate::words::Words;
use argon2::{Algorithm, Argon2, Block, Params, Version};
use std::fmt;
use zeroize::Zeroize;

/// Argon2id with 64 MiB, 3 passes and 4 lanes – RFC 9106's choice for limited memory.
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 4;
/// No salt per vault: 128 random bits already rule out precomputation.
const SALT: &[u8] = b"temple-vault-v1";

/// The key 12 words stand for. The app may seal it with the phone's secure hardware for
/// quick unlock and later open the space with it instead of the words.
pub struct PhraseKey(Key);

impl PhraseKey {
    /// Argon2id over the words' 128 random bits: deliberately slow, a tenth of a second
    /// or more.
    pub fn from_words(words: &Words) -> PhraseKey {
        let params = Params::new(MEMORY_KIB, PASSES, LANES, Some(32)).expect("valid Argon2 parameters");
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut memory = vec![Block::default(); MEMORY_KIB as usize];
        let mut out = [0u8; 32];
        argon2
            .hash_password_into_with_memory(words.bits(), SALT, &mut out, &mut memory[..])
            .expect("Argon2 takes 16 bytes of password and this salt");
        memory.zeroize();
        let key = PhraseKey(Key::from_bytes(&out));
        out.zeroize();
        key
    }

    /// A phrase key the app kept, as [`PhraseKey::as_bytes`] gave it.
    pub fn from_bytes(mut bytes: [u8; 32]) -> PhraseKey {
        let key = PhraseKey(Key::from_bytes(&bytes));
        bytes.zeroize();
        key
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    pub(crate) fn duplicate(&self) -> PhraseKey {
        PhraseKey(self.0.duplicate())
    }

    /// The key that wraps the space's key blocks.
    pub(crate) fn wrapping_key(&self) -> Key {
        self.0.derive(b"temple/v1/wrap")
    }

    /// The four slots of the space's key blocks: 32 numbers below 4096, of which the first
    /// four distinct ones count.
    pub(crate) fn slots(&self) -> [u16; 4] {
        let mut out = [0u8; 64];
        self.0.expand(b"temple/v1/slots", &mut out);
        let mut slots = [0u16; 4];
        let mut found = 0;
        for pair in out.as_chunks::<2>().0 {
            let slot = u16::from_le_bytes([pair[0], pair[1]]) % SLOTS as u16;
            if found < 4 && !slots[..found].contains(&slot) {
                slots[found] = slot;
                found += 1;
            }
        }
        // 32 draws from 4096 hold fewer than four distinct numbers with odds below 1 in 10^90.
        assert_eq!(found, 4, "fewer than four distinct slots");
        slots
    }
}

impl fmt::Debug for PhraseKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PhraseKey(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_words_same_key() {
        let words = Words::generate();
        let again = Words::from_indices(words.indices()).unwrap();
        assert_eq!(PhraseKey::from_words(&words).as_bytes(), PhraseKey::from_words(&again).as_bytes());
        assert_ne!(PhraseKey::from_words(&words).as_bytes(), PhraseKey::from_words(&Words::generate()).as_bytes());
    }

    /// The outer format can't change after v1, so its keys are pinned here. Computed apart
    /// from this code: the phrase key with the reference `argon2` tool and OpenSSL's
    /// ARGON2ID, the rest with OpenSSL's HKDF.
    #[test]
    fn known_answer() {
        let words = Words::from_indices([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]).unwrap();
        let key = PhraseKey::from_words(&words);
        assert_eq!(hex(key.as_bytes()), "11b486923e30316b0d40f214920e43b45d44ba869889eb4ea529f6e533a51d13");
        assert_eq!(
            hex(key.wrapping_key().as_bytes()),
            "cec44d13dca60f355e2dc3f6fba70cde3c6fcb2d31765d8f20b8a344f32a3815"
        );
        assert_eq!(key.slots(), [1400, 3512, 1125, 1005]);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn slots_are_distinct_and_in_range() {
        for _ in 0..1000 {
            let key = PhraseKey(Key::random());
            let slots = key.slots();
            for (i, slot) in slots.iter().enumerate() {
                assert!((*slot as usize) < SLOTS);
                assert!(!slots[..i].contains(slot));
            }
        }
    }
}
