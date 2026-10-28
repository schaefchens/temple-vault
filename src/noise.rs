//! Noise: random numbers of whole zones, filled with random bytes. Why these numbers:
//! design.md, "Noise numbers".

use crate::crypto::random;
use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use zeroize::Zeroize;

/// How many zones of noise a vault appends where. Tests make them small.
#[derive(Clone, Copy)]
pub(crate) struct Amounts {
    pub(crate) reserve: fn() -> u64,
    pub(crate) growth: fn() -> u64,
}

pub(crate) const REAL: Amounts = Amounts { reserve, growth };

/// The reserve after the first space's first block: geometric, mean 384 zones, cut off at 2048.
pub(crate) fn reserve() -> u64 {
    loop {
        let zones = geometric(384.0);
        if zones <= 2048 {
            return zones;
        }
    }
}

/// The noise after every growth step: geometric, mean 64 zones.
pub(crate) fn growth() -> u64 {
    geometric(64.0)
}

/// A geometric number with the given mean: P(k) = (1 − q)·q^k with q = mean / (mean + 1).
/// The only shape whose odds don't depend on the amount drawn.
fn geometric(mean: f64) -> u64 {
    let mut bits = [0u8; 8];
    random(&mut bits);
    // Uniform in (0, 1], from 53 random bits.
    let u = ((u64::from_le_bytes(bits) >> 11) + 1) as f64 / (1u64 << 53) as f64;
    // P(k ≥ n) = P(u ≤ q^n) = q^n.
    (-u.ln() / (1.0 / mean).ln_1p()).floor() as u64
}

/// Random bytes fast enough for gigabytes of noise: ChaCha20 under a fresh random key,
/// the construction operating systems use for their own random sources.
pub(crate) struct Noise(ChaCha20);

impl Noise {
    pub(crate) fn new() -> Noise {
        let mut key = [0u8; 32];
        let mut nonce = [0u8; 12];
        random(&mut key);
        random(&mut nonce);
        let noise = Noise(ChaCha20::new(&key.into(), &nonce.into()));
        key.zeroize();
        noise
    }

    pub(crate) fn fill(&mut self, out: &mut [u8]) {
        out.fill(0);
        self.0.apply_keystream(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn mean(draws: &[u64]) -> f64 {
        draws.iter().sum::<u64>() as f64 / draws.len() as f64
    }

    #[test]
    fn growth_noise_is_geometric_with_mean_64() {
        let draws: Vec<u64> = (0..200_000).map(|_| growth()).collect();
        assert!((mean(&draws) - 64.0).abs() < 1.0, "{}", mean(&draws));
        // Geometric: P(k ≥ n) = q^n with q = 64/65.
        for n in [0u64, 16, 64, 128, 256] {
            let share = draws.iter().filter(|&&k| k >= n).count() as f64 / draws.len() as f64;
            let expected = (64.0f64 / 65.0).powi(n as i32);
            assert!((share - expected).abs() < 0.01, "P(k ≥ {n}) = {share}, expected {expected}");
        }
    }

    #[test]
    fn reserve_has_mean_384_and_stops_at_2048() {
        let draws: Vec<u64> = (0..100_000).map(|_| reserve()).collect();
        assert!(draws.iter().all(|&k| k <= 2048));
        // The cut-off pulls the mean down a little, to about 374.
        let m = mean(&draws);
        assert!((360.0..390.0).contains(&m), "{m}");
        // design.md: half of all vaults carry less than 256 MB, 1 in 10 more than 880 MB.
        let below_256 = draws.iter().filter(|&&k| k < 256).count() as f64 / draws.len() as f64;
        let above_880 = draws.iter().filter(|&&k| k > 880).count() as f64 / draws.len() as f64;
        assert!((0.47..0.51).contains(&below_256), "{below_256}");
        assert!((0.08..0.11).contains(&above_880), "{above_880}");
    }

    /// The reserve follows the simulation's `geometric(384, 2048)` (tools/noise_sim.py): a
    /// chi-square test over bins of 64 zones.
    #[test]
    fn reserve_follows_the_simulation() {
        let draws = 200_000;
        let mut counts = [0u32; 33];
        for _ in 0..draws {
            counts[(reserve() / 64) as usize] += 1;
        }
        let q = 384.0f64 / 385.0;
        let kept = 1.0 - q.powi(2049);
        let chi_square: f64 = counts
            .iter()
            .enumerate()
            .map(|(bin, &count)| {
                let (from, to) = (bin as i32 * 64, (bin as i32 * 64 + 64).min(2049));
                let expected = draws as f64 * (q.powi(from) - q.powi(to)) / kept;
                (count as f64 - expected).powi(2) / expected
            })
            .sum();
        // 32 degrees of freedom: above 70 in 1 run of 8000.
        assert!(chi_square < 70.0, "{chi_square}");
    }

    #[test]
    fn noise_is_not_zeros() {
        let mut buf = vec![0u8; 1 << 16];
        Noise::new().fill(&mut buf);
        assert!(buf.chunks(16).all(|c| c != [0u8; 16]));
    }
}
