//! Spaces in vaults in memory: creating, opening, locking, objects, words.

use super::memory::Memory;
use super::{SMALL_NOISE, contains, create_on, phrase, readable, superblock_bytes};
use crate::slots::{SLOT, SLOTS};
use crate::space::PAYLOAD_BYTES;
use crate::vault::{Setup, Vault};
use crate::{Error, Interrupt, Space, ZONE};
use std::io::{self, Read};
use std::sync::Arc;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

fn new_vault() -> (Arc<Memory>, Vault, Space) {
    let memory = Memory::new();
    let (vault, space) = create_on(&memory, 0, Setup::default());
    (memory, vault, space)
}

fn reopen(memory: &Arc<Memory>) -> Vault {
    Vault::open_on(memory.clone(), SMALL_NOISE, false).unwrap()
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
fn objects_commit_with_the_apps_rows() {
    let (_memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE file (object INTEGER NOT NULL, name TEXT NOT NULL)").unwrap();
    let file = |space: &Space, name: &str, then: &str| {
        space.db().execute_batch("BEGIN").unwrap();
        let id = space.put_object(&content(10_000, 7)[..]).unwrap();
        space.db().execute("INSERT INTO file VALUES (?1, ?2)", (id, name)).unwrap();
        space.db().execute_batch(then).unwrap();
        id
    };
    let dropped = file(&space, "dropped", "ROLLBACK");
    assert!(matches!(space.object_len(dropped), Err(Error::NoSuchObject)));
    let kept = file(&space, "kept", "COMMIT");
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let names: Vec<String> = space
        .db()
        .prepare("SELECT name FROM file")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(names, ["kept"]);
    assert_eq!(space.read_object(kept, 0..10_000).unwrap(), content(10_000, 7));
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
fn another_device_opening_a_space_stops_writes() {
    let (memory, vault, space) = new_vault();
    note(&space, "before");
    // Opening the vault alone writes nothing, and stops nothing.
    let elsewhere = reopen(&memory);
    note(&space, "still here");
    let _there = elsewhere.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    let refused = space.db().execute("INSERT INTO note VALUES ('after')", []);
    assert!(refused.is_err());
    assert!(space.changed_elsewhere());
    assert!(matches!(space.put_object(&b"x"[..]), Err(Error::ChangedElsewhere)));
    assert!(matches!(space.lock(), Err(Error::ChangedElsewhere)));
    // This vault has stopped for good.
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::ChangedElsewhere)));
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

/// From the review: one statement on Temple's table – an app duplicating an object's row –
/// made a space refuse to open, for good. The app's SQL may only delete rows there now.
#[test]
fn temple_tables_are_temples_to_write() {
    let (_memory, vault, space) = new_vault();
    let id = space.put_object(&content(10_000, 5)[..]).unwrap();
    for sql in [
        "INSERT INTO temple_objects (key, size, blocks) SELECT key, size, blocks FROM temple_objects",
        "UPDATE temple_objects SET size = 1",
        "DROP TABLE temple_objects",
        "ALTER TABLE temple_objects ADD COLUMN extra",
        "CREATE TABLE temple_mine (x)",
        "CREATE TEMP TRIGGER copying AFTER INSERT ON main.temple_objects BEGIN SELECT 1; END",
        "CREATE TABLE note (body); CREATE TRIGGER sneaking AFTER INSERT ON note BEGIN
             INSERT INTO temple_objects (key, size, blocks) SELECT key, size, blocks FROM temple_objects; END;
         INSERT INTO note VALUES ('x');",
        // From the recheck: tables and views of Temple's names – a temporary one hid its own.
        "CREATE TEMP TABLE temple_objects (id INTEGER PRIMARY KEY, key BLOB, size INTEGER, blocks BLOB)",
        "CREATE TABLE temp.temple_objects (x)",
        "CREATE TEMP VIEW temple_objects AS SELECT 1 AS id",
        "CREATE VIEW temple_fulltext AS SELECT 'verse' AS name, 1 AS data",
        "CREATE VIRTUAL TABLE temp.temple_text USING fts5(body)",
    ] {
        assert!(space.db().execute_batch(sql).is_err(), "{sql}");
    }
    let again = space.put_object(&content(100, 6)[..]).unwrap();
    space.db().execute("DELETE FROM temple_objects WHERE id = ?1", [again]).unwrap();
    space.db().execute_batch("VACUUM").unwrap();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    assert_eq!(space.read_object(id, 0..10_000).unwrap(), content(10_000, 5));
}

/// A row that doesn't add up – pointing at blocks the database uses, here written past the
/// guard – no longer keeps the space from opening: it keeps those blocks from use, and its
/// object fails when read. Nothing is written but the opening marker.
#[test]
fn a_space_whose_objects_overlap_its_database_still_opens() {
    let (memory, vault, space) = new_vault();
    note(&space, "x");
    let id = space.put_object(&content(10_000, 5)[..]).unwrap();
    space.store().admin = true;
    space.db().execute("UPDATE temple_objects SET blocks = ?1", [crate::space::encode_runs(32, 2)]).unwrap();
    space.store().admin = false;
    space.lock().unwrap();
    drop(vault);
    let vault = reopen(&memory);
    let before = memory.bytes();
    let space = unlock(&vault, 0).unwrap();
    assert_eq!(notes(&space), ["x"]);
    assert!(matches!(space.read_object(id, 0..10_000), Err(Error::Damaged)));
    let after = memory.bytes();
    assert_eq!(before[..ZONE as usize], after[..ZONE as usize]);
    assert_eq!(before[ZONE as usize + 4096..], after[ZONE as usize + 4096..]);
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
    assert!(matches!(Vault::open_on(memory, SMALL_NOISE, false), Err(Error::NotAVault)));
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

/// From the review: free room in one-block holes – small objects, every other one deleted,
/// the rest of the room taken – once let the log collect thousands of runs, until its record
/// outgrew the superblock and commits and the lock failed.
#[test]
fn scattered_free_room_keeps_the_record_short() {
    let (_memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE kv (k INTEGER PRIMARY KEY, v BLOB)").unwrap();
    let ids: Vec<_> = (0..20_000u32).map(|i| space.put_object(&i.to_le_bytes()[..]).unwrap()).collect();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let free = space.store().free_blocks() as usize;
    space.put_object(&vec![0u8; (free - 2000) * crate::space::PAYLOAD_BYTES][..]).unwrap();
    let deletes = space.db().unchecked_transaction().unwrap();
    for id in ids.iter().step_by(2) {
        deletes.execute("DELETE FROM temple_objects WHERE id = ?1", [id]).unwrap();
    }
    deletes.commit().unwrap();
    space.lock().unwrap();

    let space = unlock(&vault, 0).unwrap();
    for k in 0..5000i64 {
        space.db().execute("INSERT INTO kv VALUES (?1, ?2)", rusqlite::params![k, vec![7u8; 3000]]).unwrap();
    }
    // 9,073 runs before the fix; a superblock copy holds about 8,000.
    let log_runs = space.store().log_runs();
    assert!(log_runs < 200, "{log_runs} log runs");
    space.lock().unwrap();
}

/// From the review: the log emptied only at a lock, so 5,000 small commits in one session –
/// an app saving a reading position – grew a vault by 50 MiB for good. It turns over now.
#[test]
fn a_long_session_keeps_the_vault_its_size() {
    let (memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE progress (id INTEGER PRIMARY KEY, at TEXT)").unwrap();
    space.lock().unwrap();
    let before = memory.bytes().len();
    let space = unlock(&vault, 0).unwrap();
    for i in 0..5000 {
        space.db().execute("INSERT OR REPLACE INTO progress VALUES (1, ?1)", [format!("John 3:{i}")]).unwrap();
    }
    space.lock().unwrap();
    assert_eq!(memory.bytes().len(), before);
}

/// The same for a hidden space set up in one go with what it holds: it doubled after 692
/// such commits before.
#[test]
fn a_hidden_space_keeps_its_size_through_many_commits() {
    let (_memory, vault, space) = new_vault();
    space.lock().unwrap();
    let hidden = vault.create_space(&phrase(1).0, Setup { room: 16 << 20 }).unwrap();
    hidden.put_object(&vec![3u8; 15 << 20][..]).unwrap();
    hidden.db().execute_batch("CREATE TABLE progress (id INTEGER PRIMARY KEY, at TEXT)").unwrap();
    let zones = hidden.zones();
    for i in 0..3000 {
        hidden.db().execute("INSERT OR REPLACE INTO progress VALUES (1, ?1)", [format!("John 3:{i}")]).unwrap();
    }
    assert_eq!(hidden.zones(), zones);
}

/// A turnover drops the log's key as a lock does: what the old log held can't be read any
/// more, while the space stays open.
#[test]
fn deleted_values_are_unreadable_once_the_log_turned_over() {
    let (memory, _vault, space) = new_vault();
    space.store().set_log_limit(Some(64 << 10));
    space.db().execute_batch("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT NOT NULL)").unwrap();
    space.db().execute("INSERT INTO kv VALUES (1, ?1)", ["<turned over>".repeat(20)]).unwrap();
    space.db().execute("DELETE FROM kv WHERE k = 1", []).unwrap();
    let session = space.session_key().unwrap();
    for k in 2..100 {
        space.db().execute("INSERT INTO kv VALUES (?1, 'more')", [k]).unwrap();
    }
    let current = space.session_key();
    assert!(current.is_none_or(|key| key.as_bytes() != session.as_bytes()), "the log never turned over");
    let (master, first_zone) = (space.master_key(), space.first_zone());
    assert!(!contains(&superblock_bytes(&memory.bytes(), &master, first_zone), session.as_bytes()));
    assert!(!readable(&memory.bytes(), &[master.derive(b"temple/v1/db")]).contains("<turned over>"));
}

/// From the review: SQL could attach the space's log as a database and write into it – a
/// crash then lost the session's commits. The log opens only as the database's log now.
#[test]
fn a_space_log_attaches_as_nothing() {
    let (_memory, vault, space) = new_vault();
    note(&space, "one");
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let name = space.database_name().to_owned();
    // With the log empty, before the session's first commit, and after it.
    for _ in 0..2 {
        let attached = space.db().execute_batch(&format!("ATTACH '{name}-wal' AS w; PRAGMA w.journal_mode = OFF;"));
        assert!(attached.is_err());
        note(&space, "more");
    }
    assert_eq!(notes(&space), ["one", "more", "more"]);
}

/// From the review: Temple told FTS5 tables by the text that made them, so a quoted module
/// name or a comment slipped past, and it merged away old deletions only when it turned
/// `secure-delete` on itself. Neither keeps a deleted term readable now.
#[test]
fn every_fts5_table_forgets_deleted_text() {
    for create in [
        "CREATE VIRTUAL TABLE verse USING \"fts5\"(body)",
        "CREATE VIRTUAL TABLE verse /* using fts4 */ USING fts5(body)",
        // The app turns the option on itself, after a deletion.
        "CREATE VIRTUAL TABLE verse USING fts5(body);
         INSERT INTO verse VALUES ('zebrafinchmarker early'); DELETE FROM verse;
         INSERT INTO verse (verse, rank) VALUES ('secure-delete', 1);",
    ] {
        let (memory, vault, space) = new_vault();
        space.db().execute_batch(create).unwrap();
        space.db().execute_batch("INSERT INTO verse VALUES ('zebrafinchmarker went home'), ('another text');").unwrap();
        space.lock().unwrap();
        let space = unlock(&vault, 0).unwrap();
        space.db().execute("DELETE FROM verse WHERE verse MATCH 'zebrafinchmarker'", []).unwrap();
        let master = space.master_key();
        space.lock().unwrap();
        let text = readable(&memory.bytes(), &[master.derive(b"temple/v1/db")]);
        assert!(!text.contains("zebrafinchmarker"), "{create}");
        assert!(text.contains("another text"), "{create}");
    }
}

/// From the recheck: Temple's SQL named tables without their database, and a temporary table
/// of the same name comes first. An object's row went there, gone after the lock; a temporary
/// table named as one of the app's FTS5 tables made the lock fail, its log's key left on disk.
#[test]
fn temples_sql_names_the_main_database() {
    let (memory, vault, space) = new_vault();
    space
        .db()
        .execute_batch(
            "CREATE VIRTUAL TABLE verse USING fts5(body);
             INSERT INTO verse VALUES ('zebrafinchmarker went home'), ('another text');",
        )
        .unwrap();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    // As if the guard had let it through.
    space.store().admin = true;
    space
        .db()
        .execute_batch("CREATE TEMP TABLE temple_objects (id INTEGER PRIMARY KEY, key BLOB, size INTEGER, blocks BLOB)")
        .unwrap();
    space.store().admin = false;
    let id = space.put_object(&content(10_000, 5)[..]).unwrap();
    space
        .db()
        .execute_batch("DELETE FROM verse WHERE verse MATCH 'zebrafinchmarker'; CREATE TEMP TABLE verse (body);")
        .unwrap();
    let master = space.master_key();
    space.lock().unwrap();
    let text = readable(&memory.bytes(), &[master.derive(b"temple/v1/db")]);
    assert!(!text.contains("zebrafinchmarker"));
    assert!(text.contains("another text"));
    drop(vault);
    let space = unlock(&reopen(&memory), 0).unwrap();
    assert_eq!(space.read_object(id, 0..10_000).unwrap(), content(10_000, 5));
}

/// From the recheck, its second round: the guard sees only the name a table had, so a rename
/// took `temple_fulltext` before Temple made it – and failed the lock, its log's key left on
/// disk. Temple's tables are there from the start now; a space from before gets them as it
/// opens.
#[test]
fn temples_tables_are_there_from_the_start() {
    let (_memory, vault, space) = new_vault();
    let renamed = |space: &Space| {
        space.db().execute_batch("CREATE TABLE IF NOT EXISTS ordinary (x)").unwrap();
        space.db().execute_batch("ALTER TABLE ordinary RENAME TO temple_fulltext").is_ok()
    };
    assert!(!renamed(&space));
    // As a space from 0.1.0 was until an FTS5 table locked.
    space.store().admin = true;
    space.db().execute_batch("DROP TABLE IF EXISTS temple_fulltext").unwrap();
    space.store().admin = false;
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    assert!(!renamed(&space));
    space.db().execute_batch("CREATE VIRTUAL TABLE verse USING fts5(body); INSERT INTO verse VALUES ('x');").unwrap();
    space.lock().unwrap();
}

/// From this review: objects crossed the bindings in one buffer. Written in pieces of any
/// size, they come out as one; begun in one space, they go into no other; never finished,
/// they leave their blocks free from the next opening.
#[test]
fn objects_written_in_pieces_round_trip() {
    let (_memory, vault, space) = new_vault();
    let bytes = content(3 * PAYLOAD_BYTES + 100, 9);
    for size in [1, 999, PAYLOAD_BYTES, PAYLOAD_BYTES + 1, bytes.len()] {
        let mut object = space.begin_object().unwrap();
        for piece in bytes.chunks(size) {
            space.write_object(&mut object, piece).unwrap();
        }
        space.write_object(&mut object, &[]).unwrap();
        let id = space.finish_object(object).unwrap();
        assert_eq!(space.read_object(id, 0..bytes.len() as u64).unwrap(), bytes, "pieces of {size}");
    }
    let empty = space.finish_object(space.begin_object().unwrap()).unwrap();
    assert_eq!(space.object_len(empty).unwrap(), 0);

    let other_memory = Memory::new();
    let (_other_vault, other) = create_on(&other_memory, 1, Setup::default());
    let mut object = space.begin_object().unwrap();
    assert!(matches!(other.write_object(&mut object, b"x"), Err(Error::Io(_))));
    assert!(matches!(other.finish_object(object), Err(Error::Io(_))));
    // A piece that fails ends the object and frees its blocks; another try is refused, as
    // the piece may have been half written.
    let free = other.store().free_blocks();
    let mut object = other.begin_object().unwrap();
    other.write_object(&mut object, &bytes).unwrap();
    other_memory.fill_up();
    assert!(
        matches!(other.write_object(&mut object, &bytes), Err(Error::Io(e)) if e.kind() == io::ErrorKind::StorageFull)
    );
    assert_eq!(other.store().free_blocks(), free);
    assert!(
        matches!(other.write_object(&mut object, &bytes), Err(Error::Io(e)) if e.kind() == io::ErrorKind::InvalidInput)
    );

    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let free = space.store().free_blocks();
    let mut left = space.begin_object().unwrap();
    space.write_object(&mut left, &content(1 << 20, 4)).unwrap();
    let taken = space.store().free_blocks();
    assert!(taken < free);
    drop(left);
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let free_again = space.store().free_blocks();
    assert_eq!(free_again, free);
}

/// Content that asks for an interrupt halfway, as a lock from another thread would.
struct Halfway {
    left: usize,
    interrupt: Option<Interrupt>,
}

impl Read for Halfway {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left <= 1 << 20
            && let Some(interrupt) = self.interrupt.take()
        {
            interrupt.interrupt();
        }
        let n = buf.len().min(self.left);
        buf[..n].fill(7);
        self.left -= n;
        Ok(n)
    }
}

/// From this review: a lock waited for the work running before it – an object of hundreds of
/// MB, a long query – while iOS suspends an app within seconds. An interrupt stops it; work
/// that starts afterwards runs as usual.
#[test]
fn an_interrupt_stops_the_work_running() {
    let (_memory, vault, space) = new_vault();
    let free = space.store().free_blocks();
    let halfway = Halfway { left: 2 << 20, interrupt: Some(space.interrupt_handle()) };
    assert!(matches!(space.put_object(halfway), Err(Error::Interrupted)));
    assert_eq!(space.store().free_blocks(), free);
    let id = space.put_object(&content(100_000, 2)[..]).unwrap();
    assert_eq!(space.read_object(id, 0..100_000).unwrap(), content(100_000, 2));

    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    {
        let interrupt = space.interrupt_handle();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            interrupt.interrupt();
        });
        let counted = space.db().query_row(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 10000000000) SELECT count(*) FROM n",
            [],
            |row| row.get::<_, i64>(0),
        );
        stopper.join().unwrap();
        assert!(
            matches!(&counted, Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::OperationInterrupted),
            "{counted:?}"
        );
    }
    note(&space, "after");
    space.lock().unwrap();
    assert_eq!(notes(&unlock(&vault, 0).unwrap()), ["after"]);
}

/// From this review: one damaged block – the tag table's, or a page every opening reads – made
/// a space unopenable, and nothing read what was left. Salvaged, it opens read-only and writes
/// nothing: whole after damage to its tag table, so that a copy into a new vault keeps
/// everything; without the damaged page's table otherwise. Page 1 – the schema – it can't spare.
#[test]
fn a_damaged_space_can_be_salvaged() {
    let (memory, vault, space) = new_vault();
    note(&space, "kept");
    let id = space.put_object(&content(10_000, 3)[..]).unwrap();
    space.lock().unwrap();
    // Opening and locking with nothing changed leaves the tag table where it is.
    let space = unlock(&vault, 0).unwrap();
    let objects_page: i64 = space
        .db()
        .query_row("SELECT rootpage FROM sqlite_schema WHERE name = 'temple_objects'", [], |row| row.get(0))
        .unwrap();
    let (tags, objects, schema) = {
        let store = space.store();
        (store.tag_table_offset(), store.page_offset(objects_page as u64 - 1), store.page_offset(0))
    };
    space.lock().unwrap();
    drop(vault);
    let intact = memory.bytes();
    let damaged = |offset: u64| {
        let mut bytes = intact.clone();
        bytes[offset as usize + 100] ^= 1;
        let memory = Memory::with_bytes(bytes);
        (memory.clone(), Vault::open_on(memory, SMALL_NOISE, false).unwrap())
    };

    let (memory, vault) = damaged(tags);
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::Damaged)));
    let before = memory.bytes();
    let salvaged = vault.salvage(&phrase(0).0).unwrap().unwrap();
    assert!(salvaged.is_read_only());
    assert_eq!(notes(&salvaged), ["kept"]);
    assert_eq!(salvaged.read_object(id, 0..10_000).unwrap(), content(10_000, 3));
    assert!(matches!(salvaged.put_object(&b"x"[..]), Err(Error::ReadOnly)));
    let new = Memory::new();
    let (rebuilt, ()) =
        Vault::create_on(new, SMALL_NOISE, |file, _claim| salvaged.copy_to(file, phrase(0).1.duplicate(), true))
            .unwrap();
    salvaged.lock().unwrap();
    assert!(memory.bytes() == before, "salvaging wrote to the vault");
    let copy = unlock(&rebuilt, 0).unwrap();
    assert_eq!(notes(&copy), ["kept"]);
    assert_eq!(copy.read_object(id, 0..10_000).unwrap(), content(10_000, 3));

    let (_memory, vault) = damaged(objects);
    assert!(matches!(vault.unlock_with_key(&phrase(0).1), Err(Error::Damaged)));
    let salvaged = vault.salvage(&phrase(0).0).unwrap().unwrap();
    assert_eq!(notes(&salvaged), ["kept"]);
    assert!(salvaged.read_object(id, 0..10).is_err());
    salvaged.lock().unwrap();

    let (_memory, vault) = damaged(schema);
    assert!(vault.salvage(&phrase(0).0).is_err());
    assert!(vault.salvage(&phrase(1).0).unwrap().is_none());
}

/// From the review of 2026-10-28: whatever failed beneath SQLite – a page from another moment,
/// a full disk, a record outgrowing its superblock – reached the app as "disk I/O error".
/// `explain` tells it apart.
#[test]
fn an_io_error_is_explained() {
    let (memory, vault, space) = new_vault();
    space.db().execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap();
    let page: i64 =
        space.db().query_row("SELECT rootpage FROM sqlite_schema WHERE name = 't'", [], |row| row.get(0)).unwrap();
    space.lock().unwrap();
    let space = unlock(&vault, 0).unwrap();
    let offset = space.store().page_offset(page as u64 - 1);
    space.lock().unwrap();
    drop(vault);
    let mut bytes = memory.bytes();
    bytes[offset as usize + 100] ^= 1;
    memory.set_bytes(bytes);

    let space = unlock(&reopen(&memory), 0).unwrap();
    let failed = space.db().query_row("SELECT x FROM t", [], |row| row.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(space.explain(failed), Error::Damaged));
    let failed = space.db().query_row("SELECT nothing FROM t", [], |row| row.get::<_, i64>(0)).unwrap_err();
    assert!(matches!(space.explain(failed), Error::Database(_)));
}
