//! What words reveal through what unlocking costs, and how often spaces collide in the slot
//! table.

use super::memory::Memory;
use super::{create_on, phrase};
use crate::crypto::random;
use crate::keys::PhraseKey;
use crate::slots::SLOTS;
use crate::{Setup, ZONE};
use std::time::{Duration, Instant};

/// Unlocking reads and opens all four slots whatever they hold, after Argon2 – which takes
/// a hundred milliseconds and more, where opening a slot takes a microsecond.
#[test]
fn unknown_words_cost_the_same_as_known_ones() {
    let memory = Memory::new();
    let (vault, space) = create_on(&memory, 0, Setup::default());
    space.lock().unwrap();
    let unlock = |n: usize| {
        let key = PhraseKey::from_words(&phrase(n).0);
        vault.file.find_key_block(&key.wrapping_key(), key.slots()).unwrap().is_some()
    };
    // Known or not: the four slots read, nothing else.
    for n in [0, 1] {
        memory.watch_reads();
        assert_eq!(unlock(n), n == 0);
        let reads = memory.reads();
        assert_eq!(reads.len(), 4);
        assert!(reads.iter().all(|&(offset, len)| offset < ZONE && len == 256));
    }
    // Measured in turns, so that other tests running alongside slow both down alike.
    let mut times = [Vec::new(), Vec::new()];
    for _ in 0..7 {
        for (n, times) in times.iter_mut().enumerate() {
            let clock = Instant::now();
            unlock(n);
            times.push(clock.elapsed());
        }
    }
    let median = |times: &mut Vec<Duration>| {
        times.sort();
        times[times.len() / 2].as_secs_f64()
    };
    let ratio = median(&mut times[0]) / median(&mut times[1]);
    assert!((0.5..2.0).contains(&ratio), "known / unknown = {ratio}");
}

/// format.md: a space loses all four copies of its key block with odds of about 1 in 7
/// million after 20 later spaces, 1 in 13,000 after 100.
#[test]
fn slot_collisions_as_computed() {
    // Slots are spread evenly: a chi-square test over all 4096.
    let mut counts = vec![0u32; SLOTS];
    let keys = 100_000;
    for _ in 0..keys {
        let mut bytes = [0u8; 32];
        random(&mut bytes);
        for slot in PhraseKey::from_bytes(bytes).slots() {
            counts[slot as usize] += 1;
        }
    }
    let expected = (keys * 4) as f64 / SLOTS as f64;
    let chi_square: f64 = counts.iter().map(|&c| (c as f64 - expected).powi(2) / expected).sum();
    // 4095 degrees of freedom: mean 4095, standard deviation 90.5; six of them either way.
    assert!((3550.0..4640.0).contains(&chi_square), "{chi_square}");

    // Each later space claims 4 of 4096 slots; a space loses all four copies once each of
    // its slots was claimed.
    let lost_after = |spaces: i32| (1.0 - (1.0 - 4.0 / SLOTS as f64).powi(spaces)).powi(4);
    let one_in = |spaces| (1.0 / lost_after(spaces)).round();
    assert!((6.9e6..7.3e6).contains(&one_in(20)), "{}", one_in(20));
    assert!((13_000.0..13_600.0).contains(&one_in(100)), "{}", one_in(100));
}
