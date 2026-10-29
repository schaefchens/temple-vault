//! How long deriving a phrase key takes on this device – Argon2id over the words, 64 MiB,
//! 3 passes, 4 lanes – and how long it would take with 32 MiB: the numbers that decide the
//! format's memory (docs/format.md, "From words to keys"). On an Android phone connected over USB:
//! `tools/argon2_on_android.sh`.

use argon2::{Algorithm, Argon2, Block, Params, Version};
use std::time::{Duration, Instant};
use temple_vault::{NewWords, PhraseKey};

fn main() {
    let words = NewWords::generate();
    let format = times(|| drop(PhraseKey::from_words(&words)));
    let half = times(|| {
        let params = Params::new(32 * 1024, 3, 4, Some(32)).unwrap();
        let mut key = [0u8; 32];
        let mut memory = vec![Block::default(); 32 * 1024];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into_with_memory(&[7; 16], b"temple-vault-v1", &mut key, &mut memory[..])
            .unwrap();
    });
    println!("Argon2id, 3 passes, 4 lanes – median (fastest–slowest) of 7:");
    println!("  64 MiB, the format's: {}", summary(format));
    println!("  32 MiB:               {}", summary(half));
}

fn times(mut derive: impl FnMut()) -> Vec<Duration> {
    let mut times: Vec<Duration> = (0..7)
        .map(|_| {
            let clock = Instant::now();
            derive();
            clock.elapsed()
        })
        .collect();
    times.sort();
    times
}

fn summary(times: Vec<Duration>) -> String {
    let ms = |time: Duration| time.as_millis();
    format!("{} ms ({}–{} ms)", ms(times[3]), ms(times[0]), ms(times[6]))
}
