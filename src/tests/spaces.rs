//! Spaces in vaults in memory: creating, opening, locking, objects, words.

use super::memory::Memory;
use super::{SMALL_NOISE, contains, create_on, phrase, readable, superblock_bytes};
use crate::slots::{SLOT, SLOTS};
use crate::vault::{Setup, Vault};
use crate::{Error, Space, ZONE};
use std::sync::Arc;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

fn new_vault() -> (Arc<Memory>, Vault, Space) {
    let memory = Memory::new();
    let (vault, space) = create_on(&memory, 0, Setup::default());
    (memory, vault, space)
}

fn reopen(memory: &Arc<Memory>) -> Vault {
    Vault::open_on(memory.clone(), SMALL_NOISE).unwrap()
}

fn unlock(vault: &Vault, n: usize) -> Option<Space> {
    vault.unlock_with_key(&phrase(n).1).unwrap()
}

fn note(space: &Space, body: &str) {
    space.db().execute_batch("CREATE TABLE IF NOT EXISTS note (body TEXT NOT NULL)").unwrap();
    space.db().execute("INSERT INTO note VALUES (?1)", [body]).unwrap();
}

fn notes(space: &Space) -> Vec<String> {
    let mut statement = space.db().prepare("SELECT body FROM note ORDER BY rowid").unwrap();
    statement.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
}

/// Bytes that show where they came from.
fn content(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 12) as u8).collect()
}

#[test]
fn a_new_space_keeps_what_it_was_given() {
    let (memory, vault, space) = new_vault();
    note(&space, "in the beginning");
    space.lock().unwrap();
    drop(vault);
    let space = unlock(&reopen(&memory), 0).expect("the space");
    assert_eq!(notes(&space), ["in the beginning"]);
}

#[test]
fn unknown_words_open_nothing() {
    let (memory, vault, space) = new_vault();
    space.lock().unwrap();
    assert!(unlock(&vault, 1).is_none());
    drop(vault);
    assert!(unlock(&reopen(&memory), 1).is_none());
}

#[test]
fn one_space_at_a_time() {
    let (_memory, vault, space) = new_vault();
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::SpaceOpen)));
    assert!(matches!(vault.create_space(&phrase(1).0, Setup::default()), Err(Error::SpaceOpen)));
    space.lock().unwrap();
    assert!(unlock(&vault, 0).is_some());
}

#[test]
fn spaces_keep_apart() {
    let (memory, vault, space) = new_vault();
    note(&space, "everyday");
    space.lock().unwrap();
    let space = vault.create_space(&phrase(1).0, Setup::default()).unwrap();
    note(&space, "hidden");
    space.lock().unwrap();
    assert!(matches!(vault.create_space(&phrase(0).0, Setup::default()), Err(Error::WordsInUse)));
    drop(vault);

    let vault = reopen(&memory);
    assert_eq!(notes(&unlock(&vault, 0).unwrap()), ["everyday"]);
    assert_eq!(notes(&unlock(&vault, 1).unwrap()), ["hidden"]);
    assert!(unlock(&vault, 2).is_none());
}

#[test]
fn a_new_space_is_appended_without_noise() {
    let (memory, vault, space) = new_vault();
    space.lock().unwrap();
    let before = memory.bytes().len() as u64;
    let space = vault.create_space(&phrase(1).0, Setup { room: 40 << 20 }).unwrap();
    space.lock().unwrap();
    // Room for 40 MB, an eighth more and 2 MiB for the log: 48 zones, nothing else.
    assert_eq!(memory.bytes().len() as u64 - before, 48 * ZONE);
}

#[test]
fn objects_round_trip() {
    let (memory, vault, space) = new_vault();
    let bytes = content(100_000, 1);
    let id = space.put_object(&bytes[..]).unwrap();
    assert_eq!(space.object_len(id).unwrap(), 100_000);
    assert_eq!(space.read_object(id, 0..100_000).unwrap(), bytes);
    assert_eq!(space.read_object(id, 4000..9000).unwrap(), bytes[4000..9000]);
    assert_eq!(space.read_object(id, 99_990..200_000).unwrap(), bytes[99_990..]);
    assert!(space.read_object(id, 200_000..300_000).unwrap().is_empty());
    let empty = space.put_object(&[][..]).unwrap();
    assert_eq!(space.object_len(empty).unwrap(), 0);
    space.lock().unwrap();
    drop(vault);

    let space = unlock(&reopen(&memory), 0).unwrap();
    assert_eq!(space.read_object(id, 0..100_000).unwrap(), bytes);
    space.delete_object(id).unwrap();
    assert!(matches!(space.read_object(id, 0..10), Err(Error::NoSuchObject)));
    assert!(matches!(space.delete_object(id), Err(Error::NoSuchObject)));
    space.lock().unwrap();
    let space = unlock(&reopen(&memory), 0).unwrap();
    assert!(matches!(space.object_len(id), Err(Error::NoSuchObject)));
    assert_eq!(space.object_len(empty).unwrap(), 0);
}

#[test]
fn objects_are_written_outside_transactions() {
    let (_memory, _vault, space) = new_vault();
    space.db().execute_batch("BEGIN").unwrap();
    assert!(matches!(space.put_object(&b"x"[..]), Err(Error::InTransaction)));
    space.db().execute_batch("COMMIT").unwrap();
    assert!(space.put_object(&b"x"[..]).is_ok());
}

#[test]
fn a_full_space_grows_in_big_steps() {
    let (memory, vault, space) = new_vault();
    let before = memory.bytes().len() as u64 / ZONE;
    // 40 MB into a space of 16 zones: two growth steps, each doubling it and adding a zone
    // of noise.
    let bytes = content(40 << 20, 2);
    let id = space.put_object(&bytes[..]).unwrap();
    let after = memory.bytes().len() as u64 / ZONE;
    assert_eq!(after - before, 16 + 1 + 32 + 1);
    space.lock().unwrap();
    drop(vault);
    let space = unlock(&reopen(&memory), 0).unwrap();
    assert!(space.read_object(id, 0..bytes.len() as u64).unwrap() == bytes);
}

#[test]
fn freed_room_is_used_again_once_the_space_locked() {
    let (memory, vault, space) = new_vault();
    let bytes = content(9 << 20, 3);
    let id = space.put_object(&bytes[..]).unwrap();
    let len = memory.bytes().len();
    // Not in the same session: a copy of the vault taken meanwhile could still need them.
    space.delete_object(id).unwrap();
    space.put_object(&bytes[..]).unwrap();
    assert!(memory.bytes().len() > len);
    space.lock().unwrap();

    let space = unlock(&vault, 0).unwrap();
    let len = memory.bytes().len();
    let again = space.put_object(&bytes[..]).unwrap();
    assert_eq!(memory.bytes().len(), len);
    assert!(space.read_object(again, 0..bytes.len() as u64).unwrap() == bytes);
}

#[test]
fn full_text_search_works() {
    let (_memory, _vault, space) = new_vault();
    space
        .db()
        .execute_batch(
            "CREATE VIRTUAL TABLE verse USING fts5(body);
             INSERT INTO verse VALUES ('In the beginning was the Word'), ('and the Word was with God');",
        )
        .unwrap();
    let found: i64 =
        space.db().query_row("SELECT count(*) FROM verse WHERE verse MATCH 'word'", [], |row| row.get(0)).unwrap();
    assert_eq!(found, 2);
}

#[test]
fn pragmas_that_would_break_the_space_are_refused() {
    let (_memory, _vault, space) = new_vault();
    for sql in [
        "PRAGMA journal_mode = DELETE",
        "PRAGMA main.journal_mode = MEMORY",
        "PRAGMA wal_checkpoint",
        "PRAGMA wal_checkpoint(TRUNCATE)",
        "PRAGMA wal_autocheckpoint = 1000",
        "PRAGMA synchronous = OFF",
        "PRAGMA secure_delete = OFF",
        "PRAGMA locking_mode = NORMAL",
        "PRAGMA max_page_count = 2000000000",
        "PRAGMA writable_schema = ON",
    ] {
        let refused = space.db().execute_batch(sql).unwrap_err();
        assert!(refused.to_string().contains("Temple"), "{sql}: {refused}");
    }
    let mode: String = space.db().query_row("PRAGMA journal_mode", [], |row| row.get(0)).unwrap();
    assert_eq!(mode, "wal");
    space.db().execute_batch("PRAGMA user_version = 3; PRAGMA foreign_keys = ON;").unwrap();
    let check: String = space.db().query_row("PRAGMA integrity_check", [], |row| row.get(0)).unwrap();
    assert_eq!(check, "ok");
}

#[test]
fn new_words_move_the_space() {
    let (memory, vault, mut space) = new_vault();
    note(&space, "kept");
    space.change_words(&phrase(1).0).unwrap();
    assert_eq!(space.phrase_key().as_bytes(), phrase(1).1.as_bytes());
    space.lock().unwrap();
    drop(vault);
    let vault = reopen(&memory);
    assert!(unlock(&vault, 0).is_none());
    assert_eq!(notes(&unlock(&vault, 1).unwrap()), ["kept"]);
}

#[test]
fn words_in_use_are_not_taken_over() {
    let (_memory, vault, space) = new_vault();
    space.lock().unwrap();
    vault.create_space(&phrase(1).0, Setup::default()).unwrap().lock().unwrap();
    let mut space = unlock(&vault, 0).unwrap();
    assert!(matches!(space.change_words(&phrase(1).0), Err(Error::WordsInUse)));
}

#[test]
fn keeping_only_this_space_destroys_the_others() {
    let (memory, vault, space) = new_vault();
    space.lock().unwrap();
    for n in 1..=2 {
        let space = vault.create_space(&phrase(n).0, Setup::default()).unwrap();
        note(&space, &format!("space {n}"));
        space.lock().unwrap();
    }
    let before = memory.bytes()[..SLOTS * SLOT].to_vec();
    let mut space = unlock(&vault, 1).unwrap();
    space.keep_only_this().unwrap();
    space.lock().unwrap();

    let after = memory.bytes()[..SLOTS * SLOT].to_vec();
    let ours = phrase(1).1.slots();
    for slot in 0..SLOTS {
        let range = slot * SLOT..(slot + 1) * SLOT;
        assert_ne!(before[range.clone()], after[range], "slot {slot}");
    }
    let vault = reopen(&memory);
    assert!(unlock(&vault, 0).is_none());
    assert!(unlock(&vault, 2).is_none());
    assert_eq!(notes(&unlock(&vault, 1).unwrap()), ["space 1"]);
    assert_eq!(ours.len(), 4);
}

#[test]
fn another_device_opening_the_vault_stops_writes() {
    let (memory, _vault, space) = new_vault();
    note(&space, "before");
    let _elsewhere = reopen(&memory);
    let refused = space.db().execute("INSERT INTO note VALUES ('after')", []);
    assert!(refused.is_err());
    assert!(space.changed_elsewhere());
    assert!(matches!(space.put_object(&b"x"[..]), Err(Error::ChangedElsewhere)));
    assert!(matches!(space.lock(), Err(Error::ChangedElsewhere)));
}

#[test]
fn a_replaced_file_stops_writes() {
    let (memory, _vault, space) = new_vault();
    note(&space, "before");
    memory.replace();
    assert!(space.db().execute("INSERT INTO note VALUES ('after')", []).is_err());
    assert!(space.changed_elsewhere());
}

#[test]
fn a_damaged_space_is_not_repaired() {
    let (memory, vault, space) = new_vault();
    note(&space, "x");
    space.lock().unwrap();
    drop(vault);
    // Both superblock copies sit at the start of the space's first zone, zone 2.
    let mut bytes = memory.bytes();
    for copy in 0..2 {
        let at = 2 * ZONE as usize + copy * 64 * 1024 + 100;
        bytes[at] ^= 1;
    }
    memory.set_bytes(bytes);
    let vault = reopen(&memory);
    let before = memory.bytes();
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::Damaged)));
    let after = memory.bytes();
    // Nothing but the opening marker changed: zone 1's first block.
    assert_eq!(before[..ZONE as usize], after[..ZONE as usize]);
    assert_eq!(before[ZONE as usize + 4096..], after[ZONE as usize + 4096..]);
}

#[test]
fn a_space_whose_objects_overlap_its_database_is_not_repaired() {
    let (memory, vault, space) = new_vault();
    note(&space, "x");
    space.put_object(&content(10_000, 5)[..]).unwrap();
    // Point the object at blocks the database uses: the space no longer adds up.
    space.db().execute("UPDATE temple_objects SET blocks = ?1", [crate::space::encode_runs(32, 2)]).unwrap();
    space.lock().unwrap();
    drop(vault);
    let vault = reopen(&memory);
    let before = memory.bytes();
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::Damaged)));
    assert!(before == memory.bytes());
}

#[test]
fn locking_drops_what_was_left_uncommitted() {
    let (memory, vault, space) = new_vault();
    note(&space, "committed");
    space.db().execute_batch("BEGIN; INSERT INTO note VALUES ('left open');").unwrap();
    space.lock().unwrap();
    drop(vault);
    assert_eq!(notes(&unlock(&reopen(&memory), 0).unwrap()), ["committed"]);
}

#[test]
fn small_files_are_not_vaults() {
    let memory = Memory::with_bytes(vec![7; ZONE as usize]);
    assert!(matches!(Vault::open_on(memory, SMALL_NOISE), Err(Error::NotAVault)));
}

#[test]
fn a_vault_holds_no_zeros() {
    let (memory, vault, space) = new_vault();
    note(&space, "x");
    space.put_object(&content(1 << 20, 4)[..]).unwrap();
    space.lock().unwrap();
    vault.create_space(&phrase(1).0, Setup::default()).unwrap().lock().unwrap();
    let bytes = memory.bytes();
    assert_eq!(bytes.len() as u64 % ZONE, 0);
    // In random bytes, 16 zeros in a row turn up once in 2^128 tries.
    let zeros = bytes.chunks(16).filter(|chunk| chunk.iter().all(|&b| b == 0)).count();
    assert_eq!(zeros, 0);
}

#[test]
fn deleted_values_are_unreadable_once_locked() {
    let (memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT NOT NULL)").unwrap();
    let marker = |k: i64, version: u32| format!("<<{k}:{version}>>");
    for k in 0..200 {
        space.db().execute("INSERT INTO kv VALUES (?1, ?2)", (k, marker(k, 1).repeat(20))).unwrap();
    }
    for k in (0..200).step_by(3) {
        space.db().execute("UPDATE kv SET v = ?2 WHERE k = ?1", (k, marker(k, 2).repeat(20))).unwrap();
    }
    for k in (1..200).step_by(3) {
        space.db().execute("DELETE FROM kv WHERE k = ?1", [k]).unwrap();
    }
    let object = space.put_object(&b"<<object>>".repeat(1000)[..]).unwrap();
    space.delete_object(object).unwrap();
    let (master, first_zone) = (space.master_key(), space.first_zone());
    let database = master.derive(b"temple/v1/db");
    // While the space is open, its log still holds them.
    let session = space.session_key().unwrap();
    assert!(readable(&memory.bytes(), &[database.duplicate(), session.duplicate()]).contains(&marker(1, 1)));
    space.lock().unwrap();
    drop(vault);

    // Locked, no superblock copy holds the log's key any more – not even decrypted unchecked.
    assert!(!contains(&superblock_bytes(&memory.bytes(), &master, first_zone), session.as_bytes()));
    let readable = readable(&memory.bytes(), &[database]);
    for k in 0..200 {
        let (gone, kept) = match k % 3 {
            0 => (vec![marker(k, 1)], marker(k, 2)),
            1 => (vec![marker(k, 1)], String::new()),
            _ => (vec![], marker(k, 1)),
        };
        for value in gone {
            assert!(!readable.contains(&value), "{value} is still readable");
        }
        assert!(readable.contains(&kept), "{kept} is gone");
    }
    assert!(!readable.contains("<<object>>"));
}

/// The app's core keeps a vault and its open space across threads.
#[test]
fn vaults_and_spaces_move_between_threads() {
    fn movable<T: Send>() {}
    fn shared<T: Send + Sync>() {}
    shared::<Vault>();
    movable::<Space>();
    shared::<crate::Words>();
    shared::<crate::PhraseKey>();
}

/// From the review: a session's superblock record can take more blocks than the lock's –
/// here with the log in 900 pieces. The log's key sits in the first block, which every
/// rewrite replaces, so no block left over from the longer record holds it.
#[test]
fn a_longer_record_leaves_no_session_key_behind() {
    let (memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT)").unwrap();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    space.store().fragment();
    for k in 0..400 {
        space.db().execute("INSERT INTO kv VALUES (?1, ?2)", (k, format!("<secret {k}>"))).unwrap();
    }
    space.db().execute("DELETE FROM kv WHERE k = 7", []).unwrap();
    assert!(space.store().log_runs() > 500);
    let session = space.session_key().unwrap();
    let (master, first_zone) = (space.master_key(), space.first_zone());
    space.lock().unwrap();
    drop(vault);
    assert!(!contains(&superblock_bytes(&memory.bytes(), &master, first_zone), session.as_bytes()));
}

/// From the review: FTS5 keeps a deleted document's terms in its index – unless its own
/// secure-delete option is on, which locking turns on.
#[test]
fn deleted_text_leaves_no_search_terms_once_locked() {
    let (memory, vault, space) = new_vault();
    space
        .db()
        .execute_batch(
            "CREATE VIRTUAL TABLE verse USING fts5(body);
             INSERT INTO verse VALUES ('zebrafinchmarker went home'), ('another text entirely');",
        )
        .unwrap();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    space.db().execute("DELETE FROM verse WHERE verse MATCH 'zebrafinchmarker'", []).unwrap();
    space.db().execute("INSERT INTO verse VALUES ('quokkamarker stays')", []).unwrap();
    let master = space.master_key();
    space.lock().unwrap();
    drop(vault);
    let text = readable(&memory.bytes(), &[master.derive(b"temple/v1/db")]);
    assert!(!text.contains("zebrafinchmarker"));
    assert!(text.contains("quokkamarker"));
    let space = unlock(&reopen(&memory), 0).unwrap();
    let found: i64 = space
        .db()
        .query_row("SELECT count(*) FROM verse WHERE verse MATCH 'quokkamarker'", [], |row| row.get(0))
        .unwrap();
    assert_eq!(found, 1);
}

#[test]
fn room_beyond_what_a_space_holds_appends_nothing() {
    let (memory, vault, space) = new_vault();
    space.lock().unwrap();
    let before = memory.bytes().len();
    let refused = vault.create_space(&phrase(1).0, Setup { room: u64::MAX });
    assert!(matches!(refused, Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::StorageFull));
    assert_eq!(memory.bytes().len(), before);
    assert!(vault.create_space(&phrase(1).0, Setup::default()).is_ok());
}

/// The app's SQL can't open a live space's database a second time – that connection would
/// bypass the first one's locks.
#[test]
fn a_space_database_opens_once() {
    let (_memory, _vault, space) = new_vault();
    let name = space.database_name();
    assert!(space.db().execute(&format!("ATTACH '{name}' AS again"), []).is_err());
    assert!(space.db().execute(&format!("VACUUM INTO '{name}'"), []).is_err());
    note(&space, "still fine");
    assert_eq!(notes(&space), ["still fine"]);
}
