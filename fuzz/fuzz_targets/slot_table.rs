#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| temple_vault::fuzzing::slot_table(data));
