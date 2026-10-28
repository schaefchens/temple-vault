//! The layout the noise simulation (tools/noise_sim.py) assumes, in real vaults.

use super::memory::Memory;
use super::phrase;
use crate::noise::Amounts;
use crate::vault::{Setup, Vault};
use crate::{Space, ZONE};
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

/// Noise that can be told apart: 7 zones of reserve, 3 after each growth step.
const COUNTED: Amounts = Amounts { reserve: || 7, growth: || 3 };

fn zones(memory: &Memory) -> u64 {
    memory.bytes().len() as u64 / ZONE
}

fn fill(space: &Space, megabytes: usize) {
    space.put_object(&vec![1u8; megabytes << 20][..]).unwrap();
}

/// Zones 0 and 1; the first space's first block of 16 zones, the reserve right after it; a
/// growth step at least doubles a space and is followed by growth noise; a new space comes
/// without noise. Spaces record only their own zones.
#[test]
fn a_vault_is_laid_out_as_the_noise_simulation_assumes() {
    let memory = Memory::new();
    let (vault, everyday) = Vault::create_on(memory.clone(), COUNTED, |file| {
        Space::create(file, file.claim(), phrase(0).1.duplicate(), Setup::default(), true)
    })
    .unwrap();
    assert_eq!(zones(&memory), 2 + 16 + 7);
    fill(&everyday, 20);
    assert_eq!(zones(&memory), 25 + 16 + 3);
    everyday.lock().unwrap();

    let hidden = vault.create_space(&phrase(1).0, Setup::default()).unwrap();
    assert_eq!(zones(&memory), 44 + 16);
    fill(&hidden, 20);
    assert_eq!(zones(&memory), 60 + 16 + 3);
    hidden.lock().unwrap();

    let everyday = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    // 40 MB more into 32 zones: the step is as large as the space, or larger if it must be.
    fill(&everyday, 40);
    assert_eq!(zones(&memory), 79 + 32 + 3);
    assert_eq!(everyday.zones(), [(2, 16), (25, 16), (79, 32)]);
    everyday.lock().unwrap();
    assert_eq!(vault.unlock_with_key(&phrase(1).1).unwrap().unwrap().zones(), [(44, 16), (60, 16)]);
}
