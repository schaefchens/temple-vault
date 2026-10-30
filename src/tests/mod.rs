//! Tests that look inside: vaults in memory that record every write, for crashes and copies
//! at any point.

mod copies;
mod costs;
mod crash;
#[cfg(unix)]
mod files;
mod layout;
mod memory;
mod spaces;
#[cfg(unix)]
mod stress;

use crate::crypto::{Key, NONCE, open_trailing};
use crate::keys::PhraseKey;
use crate::noise::Amounts;
use crate::words::NewWords;
use crate::{BLOCK, ZONE};
use chacha20::XChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use std::sync::OnceLock;

/// Noise in a few zones, so that vaults in memory stay small.
pub(crate) const SMALL_NOISE: Amounts = Amounts { reserve: || 3, growth: || 1 };

/// A vault in memory with a first space under phrase `n`, small noise.
pub(crate) fn create_on(
    memory: &std::sync::Arc<memory::Memory>,
    n: usize,
    setup: crate::Setup,
) -> (crate::Vault, crate::Space) {
    crate::Vault::create_on(memory.clone(), SMALL_NOISE, |file, claim| {
        crate::Space::create(file, Some(claim), phrase(n).1.duplicate(), setup, true)
    })
    .unwrap()
}

/// Argon2 is slow on purpose; tests derive a few keys once and reuse them.
pub(crate) fn phrase(n: usize) -> &'static (NewWords, PhraseKey) {
    static PHRASES: OnceLock<Vec<(NewWords, PhraseKey)>> = OnceLock::new();
    &PHRASES.get_or_init(|| {
        (0..4)
            .map(|_| {
                let words = NewWords::generate();
                let key = PhraseKey::from_words(&words);
                (words, key)
            })
            .collect()
    })[n]
}

/// What blocks that open under any of `keys` hold, as text: what someone holding those keys
/// can read in a vault.
pub(crate) fn readable(vault: &[u8], keys: &[Key]) -> String {
    let mut text = Vec::new();
    for (number, block) in vault.as_chunks::<BLOCK>().0.iter().enumerate() {
        for key in keys {
            let mut block = block.to_vec();
            if open_trailing(key, &(number as u64).to_le_bytes(), &mut block) {
                text.extend_from_slice(&block[..BLOCK - 40]);
            }
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// Both superblock copies of the space starting at `first_zone`, decrypted without checking
/// them – as someone holding the master key reads even a torn copy – for every length a
/// copy may have.
pub(crate) fn superblock_bytes(vault: &[u8], master: &Key, first_zone: u64) -> Vec<u8> {
    let key = master.derive(b"temple/v1/superblock");
    let mut plain = Vec::new();
    for copy in 0..2 {
        let start = (first_zone * ZONE) as usize + copy * 64 * 1024;
        for blocks in 1..=16 {
            let sealed = &vault[start..start + blocks * BLOCK];
            let payload = blocks * BLOCK - 40;
            let nonce: [u8; NONCE] = sealed[payload..payload + NONCE].try_into().unwrap();
            let mut bytes = sealed[..payload].to_vec();
            let mut cipher = XChaCha20::new(&(*key.as_bytes()).into(), &nonce.into());
            // XChaCha20-Poly1305 encrypts from the keystream's second 64-byte block on.
            cipher.seek(64u64);
            cipher.apply_keystream(&mut bytes);
            plain.extend(bytes);
        }
    }
    plain
}

/// Seconds on a clock that only moves forward.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub(crate) fn now() -> f64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64()
}

/// wasm has no `Instant`; the browser's clock in milliseconds is fine enough for tests.
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub(crate) fn now() -> f64 {
    js_sys::Date::now() / 1000.0
}

pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}
