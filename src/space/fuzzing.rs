//! Fuzzing a space's superblock: its encoding, and what opening a space makes of records that
//! pass every check a copy can be sealed with.

use super::Access;
use super::runs;
use super::store::{File, PAYLOAD, Store, associated_data, offset_in, seal_superblock};
use super::superblock::{self, Superblock};
use crate::crypto::{Key, TAG, seal_trailing};
use crate::device::Device;
use crate::fuzzing::{Image, Input, NO_NOISE};
use crate::slots::{INNER_VERSION, KeyBlock};
use crate::vault::Vault;
use crate::{BLOCK, ZONE};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Arbitrary bytes as a superblock's payload: what decodes encodes, if it fits, to a payload
/// that fills its blocks and decodes the same.
pub fn superblock(data: &[u8]) {
    let Some(record) = Superblock::decode(data) else { return };
    let Some(payload) = record.encode() else { return };
    assert_eq!((payload.len() + 40) % BLOCK, 0);
    let again = Superblock::decode(&payload).expect("an encoded record decodes");
    assert!(same(&record, &again));
}

/// Arbitrary bytes as a space: a vault of a few zones, and in the space's first zone two
/// superblock copies sealed from records the fuzzer makes up. On request a record is fixed
/// up to pass the checks before the interesting ones – current version, the first zone the
/// key block names – and gets database pages and a tag table where it says. Opening the
/// space's store, working out its free blocks, reading, writing and syncing never panic;
/// whatever opens adds up.
pub fn space(data: &[u8]) {
    let mut input = Input(data);
    let zones = 3 + u64::from(input.byte() % 6);
    let first_zone = 2 + u64::from(input.byte()) % (zones - 2);
    let master = Key::from_bytes(&[7; 32]);
    let superblock_key = master.derive(b"temple/v1/superblock");
    let database_key = master.derive(b"temple/v1/db");
    let image = Image::new(zones * ZONE);
    for copy in 0..2 {
        // A copy left out is random bytes, which costs the most to rule out: all 16 lengths.
        let flags = input.byte();
        if flags & 1 != 0 {
            continue;
        }
        let len = usize::from(input.u16()) % (superblock::SIZE - 40);
        let mut payload = input.bytes(len);
        if flags & 2 != 0
            && let Some(mut record) = Superblock::decode(&payload)
        {
            record.inner_version = INNER_VERSION;
            if let Some(first) = record.extents.first_mut() {
                first.first_zone = first_zone;
            }
            if flags & 4 != 0 {
                furnish(&mut record, &image, &database_key, zones, &mut input);
            }
            if let Some(encoded) = record.encode() {
                payload = encoded.to_vec();
            }
        }
        let offset = first_zone * ZONE + (copy * superblock::SIZE) as u64;
        image.write_at(&seal_superblock(&superblock_key, offset, &payload), offset).unwrap();
    }
    let vault = Vault::open_on(image, NO_NOISE, false).unwrap();
    let access = if input.byte() % 4 == 0 { Access::Salvage } else { Access::Write };
    let Ok(mut store) = Store::load(vault.file.clone(), &master, first_zone, access) else { return };
    let count = usize::from(input.byte() % 8);
    let objects = runs::decode(&input.bytes(8 * count)).unwrap_or_default();
    if store.init_free(objects).is_err() {
        return;
    }
    store.read_only = false;
    let mut buf = vec![0u8; 1 + usize::from(input.u16()) % 9000];
    let _ = store.read(File::Database, u64::from(input.u32()), &mut buf);
    let _ = store.read(File::Log, u64::from(input.u32()), &mut buf);
    let len = usize::from(input.u16()) % 9000;
    let written = input.bytes(len);
    if store.write(File::Log, u64::from(input.u16()), &written).is_ok() {
        let _ = store.sync();
    }
}

/// The space a key block points at, opened as far as its store.
pub(crate) fn open_store(vault: &Vault, block: &KeyBlock) {
    let _ = Store::load(vault.file.clone(), &block.master_key, block.first_zone, Access::Write);
    let _ = Store::load(vault.file.clone(), &block.master_key, block.first_zone, Access::Salvage);
}

/// Seals a database page at every block the record gives its database, within the file, and
/// writes the tag table for them where the record says; the record then vouches for it.
fn furnish(record: &mut Superblock, image: &Arc<Image>, key: &Key, zones: u64, input: &mut Input<'_>) {
    let fits = |offset: u64| offset + BLOCK as u64 <= zones * ZONE;
    let mut content = Vec::new();
    for page in 0..(record.database.size / BLOCK as u64).min(256) {
        let offset = runs::nth(&record.database.runs, page).and_then(|number| offset_in(&record.extents, number));
        let Some(offset) = offset.filter(|&offset| fits(offset)) else { break };
        let mut block = [0u8; BLOCK];
        block[..64].copy_from_slice(&input.bytes(64));
        seal_trailing(key, &associated_data(offset), &mut block);
        image.write_at(&block, offset).unwrap();
        content.extend_from_slice(&block[BLOCK - TAG..]);
    }
    for (index, chunk) in content.chunks(PAYLOAD).enumerate() {
        let offset = runs::nth(&record.tags.runs, index as u64).and_then(|number| offset_in(&record.extents, number));
        let Some(offset) = offset.filter(|&offset| fits(offset)) else { break };
        let mut block = [0u8; BLOCK];
        block[..chunk.len()].copy_from_slice(chunk);
        seal_trailing(key, &associated_data(offset), &mut block);
        image.write_at(&block, offset).unwrap();
    }
    record.tags.size = content.len() as u64;
    record.tags_hash = Sha256::digest(&content).into();
}

fn same(a: &Superblock, b: &Superblock) -> bool {
    let session = |record: &Superblock| record.session.as_ref().map(|key| *key.as_bytes());
    (a.inner_version, a.generation, &a.extents, &a.database, &a.log, &a.tags, a.tags_hash, &a.checkpoint)
        == (b.inner_version, b.generation, &b.extents, &b.database, &b.log, &b.tags, b.tags_hash, &b.checkpoint)
        && session(a) == session(b)
}
