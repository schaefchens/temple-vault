//! Keys, randomness and XChaCha20-Poly1305.

use chacha20poly1305::aead::{Nonce, Tag};
use chacha20poly1305::{AeadInOut, KeyInit, XChaCha20Poly1305};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

pub(crate) const NONCE: usize = 24;
pub(crate) const TAG: usize = 16;

/// A 256-bit key, wiped from memory when dropped.
pub(crate) struct Key([u8; 32]);

impl Key {
    pub(crate) fn random() -> Key {
        let mut key = Key([0; 32]);
        random(&mut key.0);
        key
    }

    pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Key {
        Key(*bytes)
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn duplicate(&self) -> Key {
        Key(self.0)
    }

    /// HKDF-SHA256 without salt, from this key to `out`.
    pub(crate) fn expand(&self, info: &[u8], out: &mut [u8]) {
        Hkdf::<Sha256>::new(None, &self.0).expand(info, out).expect("HKDF-SHA256 gives up to 8160 bytes");
    }

    /// A subkey: 32 bytes of [`Key::expand`].
    pub(crate) fn derive(&self, info: &[u8]) -> Key {
        let mut key = Key([0; 32]);
        self.expand(info, &mut key.0);
        key
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.0.into())
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Fills `out` from the operating system's random source.
pub(crate) fn random(out: &mut [u8]) {
    getrandom::fill(out).expect("the system's random source failed");
}

/// Encrypts `data` in place under a fresh random nonce; returns the nonce and the tag.
pub(crate) fn seal(key: &Key, ad: &[u8], data: &mut [u8]) -> ([u8; NONCE], [u8; TAG]) {
    let mut nonce = [0u8; NONCE];
    random(&mut nonce);
    let tag = key
        .cipher()
        .encrypt_inout_detached(&Nonce::<XChaCha20Poly1305>::from(nonce), ad, data.into())
        .expect("XChaCha20-Poly1305 seals any length Temple uses");
    (nonce, tag.into())
}

/// Decrypts `data` in place. False if it fails its check – noise, damage, another key or
/// another place – and then `data` stays as it was.
pub(crate) fn open(key: &Key, ad: &[u8], nonce: &[u8; NONCE], tag: &[u8; TAG], data: &mut [u8]) -> bool {
    key.cipher()
        .decrypt_inout_detached(
            &Nonce::<XChaCha20Poly1305>::from(*nonce),
            ad,
            data.into(),
            &Tag::<XChaCha20Poly1305>::from(*tag),
        )
        .is_ok()
}

/// Seals a buffer laid out as payload ‖ nonce ‖ tag, as blocks and superblocks are.
pub(crate) fn seal_trailing(key: &Key, ad: &[u8], buf: &mut [u8]) {
    let (payload, trailer) = buf.split_at_mut(buf.len() - NONCE - TAG);
    let (nonce, tag) = seal(key, ad, payload);
    trailer[..NONCE].copy_from_slice(&nonce);
    trailer[NONCE..].copy_from_slice(&tag);
}

/// Opens a buffer sealed by [`seal_trailing`]; the trailer stays as it was.
pub(crate) fn open_trailing(key: &Key, ad: &[u8], buf: &mut [u8]) -> bool {
    let (payload, trailer) = buf.split_at_mut(buf.len() - NONCE - TAG);
    let nonce = trailer[..NONCE].try_into().unwrap();
    let tag = trailer[NONCE..].try_into().unwrap();
    open(key, ad, &nonce, &tag, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn sealed_data_opens_only_with_its_key_and_place() {
        let key = Key::random();
        let mut buf = [7u8; 100];
        seal_trailing(&key, b"place 1", &mut buf);
        assert_ne!(buf[..60], [7u8; 60]);

        let mut copy = buf;
        assert!(!open_trailing(&key, b"place 2", &mut copy));
        assert!(!open_trailing(&Key::random(), b"place 1", &mut copy));
        // Failing leaves the bytes as they were.
        assert_eq!(copy, buf);
        copy[3] ^= 1;
        assert!(!open_trailing(&key, b"place 1", &mut copy));

        assert!(open_trailing(&key, b"place 1", &mut buf));
        assert_eq!(buf[..60], [7u8; 60]);
    }
}
