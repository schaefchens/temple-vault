//! The 12 words that open a space.

use crate::Error;
use crate::crypto::random;
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroize;

/// The 12 words that open a space, as indices into a list of 2048 words: 128 random bits
/// and a 4-bit checksum (the first 4 bits of SHA-256 over them), laid out as in BIP39.
/// Only the indices count, so every phrase works with every wordlist; the lists come
/// from the app.
pub struct Words {
    bits: [u8; 16],
}

impl Words {
    /// 12 fresh words.
    pub fn generate() -> Words {
        let mut words = Words { bits: [0; 16] };
        random(&mut words.bits);
        words
    }

    /// Words from their indices. `InvalidWords` if an index isn't below 2048 or the
    /// checksum fails – most likely a typo.
    pub fn from_indices(indices: [u16; 12]) -> Result<Words, Error> {
        if indices.iter().any(|&index| index >= 2048) {
            return Err(Error::InvalidWords);
        }
        // 132 bits, most significant first: the random bits, then the checksum.
        let mut packed = [0u8; 17];
        for (i, &index) in indices.iter().enumerate() {
            for b in 0..11 {
                if index >> (10 - b) & 1 == 1 {
                    let n = i * 11 + b;
                    packed[n / 8] |= 0x80 >> (n % 8);
                }
            }
        }
        let words = Words { bits: packed[..16].try_into().unwrap() };
        let valid = packed[16] >> 4 == checksum(&words.bits);
        packed.zeroize();
        if valid { Ok(words) } else { Err(Error::InvalidWords) }
    }

    /// The 12 indices, each below 2048.
    pub fn indices(&self) -> [u16; 12] {
        let mut packed = [0u8; 17];
        packed[..16].copy_from_slice(&self.bits);
        packed[16] = checksum(&self.bits) << 4;
        let mut indices = [0u16; 12];
        for (i, index) in indices.iter_mut().enumerate() {
            for b in 0..11 {
                let n = i * 11 + b;
                *index = *index << 1 | u16::from(packed[n / 8] >> (7 - n % 8) & 1);
            }
        }
        packed.zeroize();
        indices
    }

    pub(crate) fn bits(&self) -> &[u8; 16] {
        &self.bits
    }
}

fn checksum(bits: &[u8; 16]) -> u8 {
    Sha256::digest(bits)[0] >> 4
}

impl Drop for Words {
    fn drop(&mut self) {
        self.bits.zeroize();
    }
}

impl fmt::Debug for Words {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Words(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn from_bits(bits: [u8; 16]) -> Words {
        Words { bits }
    }

    #[test]
    fn indices_match_bip39() {
        // BIP39's test vectors: "abandon" ×11 "about"; "legal winner thank year wave sausage
        // worth useful legal winner thank yellow"; "letter advice cage absurd amount doctor
        // acoustic avoid letter advice cage above".
        assert_eq!(from_bits([0; 16]).indices(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3]);
        assert_eq!(
            from_bits([0x7f; 16]).indices(),
            [1019, 2015, 1790, 2039, 1983, 1533, 2031, 1919, 1019, 2015, 1790, 2040]
        );
        assert_eq!(from_bits([0x80; 16]).indices(), [1028, 32, 257, 8, 64, 514, 16, 128, 1028, 32, 257, 4]);
    }

    #[test]
    fn indices_round_trip() {
        for _ in 0..100 {
            let words = Words::generate();
            let again = Words::from_indices(words.indices()).unwrap();
            assert_eq!(again.bits, words.bits);
        }
    }

    #[test]
    fn checksum_catches_most_typos() {
        let words = Words::generate();
        let mut caught = 0;
        let mut tried = 0;
        for position in 0..12 {
            for typo in 0..2048 {
                let mut indices = words.indices();
                if indices[position] == typo {
                    continue;
                }
                indices[position] = typo;
                tried += 1;
                if Words::from_indices(indices).is_err() {
                    caught += 1;
                }
            }
        }
        // A 4-bit checksum lets 1 typo in 16 through.
        let missed = (tried - caught) as f64 / tried as f64;
        assert!((0.04..0.09).contains(&missed), "{missed}");
    }

    #[test]
    fn out_of_range_index_is_invalid() {
        let mut indices = Words::generate().indices();
        indices[5] = 2048;
        assert!(matches!(Words::from_indices(indices), Err(Error::InvalidWords)));
    }

    #[test]
    fn debug_hides_the_words() {
        assert_eq!(format!("{:?}", Words::generate()), "Words(…)");
    }
}
