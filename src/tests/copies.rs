//! Rebuilding a vault: spaces copied one at a time into a fresh one.

use super::memory::Memory;
use super::{SMALL_NOISE, create_on, phrase};
use crate::vault::{Setup, Vault};
use crate::{Error, Space, ZONE};
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

fn notes(space: &Space) -> Vec<String> {
    let mut statement = space.db().prepare("SELECT body FROM note ORDER BY rowid").unwrap();
    statement.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
}

fn unlock(vault: &Vault, n: usize) -> Option<Space> {
    vault.unlock_with_key(&phrase(n).1).unwrap()
}

#[test]
fn a_rebuilt_vault_holds_the_spaces_opened_for_it() {
    let old = Memory::new();
    let (vault, everyday) = create_on(&old, 0, Setup::default());
    everyday.db().execute_batch("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('everyday');").unwrap();
    let audio: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    let kept = everyday.put_object(&audio[..]).unwrap();
    let dropped = everyday.put_object(&vec![9u8; 6 << 20][..]).unwrap();
    everyday.delete_object(dropped).unwrap();
    everyday.lock().unwrap();
    for n in [1, 2] {
        let hidden = vault.create_space(&phrase(n).0, Setup::default()).unwrap();
        hidden
            .db()
            .execute_batch(&format!("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('hidden {n}');"))
            .unwrap();
        hidden.lock().unwrap();
    }

    // The everyday space first, into a fresh vault; then one hidden space. The other stays.
    let new = Memory::new();
    let everyday = unlock(&vault, 0).unwrap();
    let (rebuilt, ()) = Vault::create_on(new.clone(), SMALL_NOISE, |file, _claim| {
        everyday.copy_to(file, phrase(0).1.duplicate(), true)
    })
    .unwrap();
    everyday.lock().unwrap();
    let hidden = unlock(&vault, 1).unwrap();
    hidden.copy_into(&rebuilt, None).unwrap();
    assert!(matches!(hidden.copy_into(&rebuilt, None), Err(Error::WordsInUse)));
    hidden.lock().unwrap();
    drop(rebuilt);

    let rebuilt = Vault::open_on(new.clone(), SMALL_NOISE, false).unwrap();
    let everyday = unlock(&rebuilt, 0).unwrap();
    assert_eq!(notes(&everyday), ["everyday"]);
    assert!(everyday.read_object(kept, 0..audio.len() as u64).unwrap() == audio);
    assert!(matches!(everyday.object_len(dropped), Err(Error::NoSuchObject)));
    everyday.lock().unwrap();
    assert_eq!(notes(&unlock(&rebuilt, 1).unwrap()), ["hidden 1"]);
    assert!(unlock(&rebuilt, 2).is_none());

    // Zones 0 and 1, the everyday space with room for what it holds – deleted content left
    // out – the reserve, then the hidden space right after.
    assert_eq!(new.bytes().len() as u64, (2 + 16 + 3 + 16) * ZONE);
}

#[test]
fn a_copy_is_one_go_with_room_for_everything() {
    let old = Memory::new();
    let (_vault, space) = create_on(&old, 0, Setup::default());
    let content = vec![7u8; 30 << 20];
    let id = space.put_object(&content[..]).unwrap();
    let new = Memory::new();
    let (rebuilt, ()) =
        Vault::create_on(new.clone(), SMALL_NOISE, |file, _claim| space.copy_to(file, phrase(0).1.duplicate(), true))
            .unwrap();
    // 30 MB and an eighth more, 2 MiB for the log: 37 zones, without a growth step.
    assert_eq!(new.bytes().len() as u64, (2 + 37 + 3) * ZONE);
    space.lock().unwrap();
    let copy = unlock(&rebuilt, 0).unwrap();
    assert!(copy.read_object(id, 0..content.len() as u64).unwrap() == content);
}

/// From the review: room for a copy counts every object's blocks – objects just over one
/// block take two – and its rows commit at once: 2,000 small objects come along without a
/// growth step.
#[test]
fn a_copy_of_many_small_objects_does_not_grow() {
    let old = Memory::new();
    let (_vault, space) = create_on(&old, 0, Setup::default());
    for i in 0..2000u32 {
        space.put_object(&vec![(i % 251) as u8; 4100][..]).unwrap();
    }
    let new = Memory::new();
    let (rebuilt, ()) =
        Vault::create_on(new.clone(), SMALL_NOISE, |file, _claim| space.copy_to(file, phrase(0).1.duplicate(), true))
            .unwrap();
    space.lock().unwrap();
    let copy = unlock(&rebuilt, 0).unwrap();
    assert_eq!(copy.zones().len(), 1, "{:?}", copy.zones());
    assert_eq!(copy.read_object(500, 0..4100).unwrap(), vec![(499 % 251) as u8; 4100]);
}

/// From this review: rebuilding after words leaked kept them – a copy opened under the same
/// words, so that the leaked ones read its fresh master key. Rebuilt under new words, the old
/// ones open nothing in it.
#[test]
fn a_vault_rebuilt_under_new_words_opens_only_with_them() {
    let old = Memory::new();
    let (_vault, space) = create_on(&old, 0, Setup::default());
    space.db().execute_batch("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('kept');").unwrap();
    let new = Memory::new();
    let (rebuilt, ()) = Vault::create_on(new, SMALL_NOISE, |_file, _claim| Ok(())).unwrap();
    space.copy_into(&rebuilt, Some(&phrase(1).0)).unwrap();
    let master = space.master_key();
    space.lock().unwrap();
    assert!(unlock(&rebuilt, 0).is_none());
    let copy = unlock(&rebuilt, 1).unwrap();
    assert_eq!(notes(&copy), ["kept"]);
    assert_ne!(copy.master_key().as_bytes(), master.as_bytes());
}
