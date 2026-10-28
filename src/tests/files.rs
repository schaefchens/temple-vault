//! Vaults in real files.

use super::{SMALL_NOISE, phrase};
use crate::device::FileDevice;
use crate::vault::{Setup, Vault};
use crate::{Error, Space, ZONE};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

fn create(path: &std::path::Path) -> (Vault, Space) {
    Vault::create_at(path, SMALL_NOISE, |file| {
        Space::create(file, file.claim(), phrase(0).1.duplicate(), Setup::default(), true)
    })
    .unwrap()
}

fn open(path: &std::path::Path) -> Vault {
    let file = fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
    Vault::open_on(Arc::new(FileDevice::new(file, path)), SMALL_NOISE).unwrap()
}

#[test]
fn a_vault_file_is_created_in_one_piece() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (_vault, space) = create(&path);
    space.db().execute_batch("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('kept');").unwrap();
    space.lock().unwrap();
    // Only the vault: the temporary file was renamed into place.
    let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(names, ["reading"]);
    assert!(matches!(
        Vault::create(&path, &phrase(1).0, Setup::default()),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists
    ));

    let space = open(&path).unlock_with_key(&phrase(0).1).unwrap().unwrap();
    let body: String = space.db().query_row("SELECT body FROM note", [], |row| row.get(0)).unwrap();
    assert_eq!(body, "kept");
}

#[test]
fn a_creation_a_crash_interrupted_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    // What an interrupted creation left: a part, never a vault.
    fs::write(dir.path().join(".reading.temple-part"), vec![1u8; 4 << 20]).unwrap();
    let (_vault, space) = create(&path);
    space.lock().unwrap();
    let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(names, ["reading"]);
}

#[test]
fn a_vault_file_has_no_sparse_region() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (_vault, space) = create(&path);
    space.put_object(&vec![3u8; 20 << 20][..]).unwrap();
    space.lock().unwrap();
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.len() % ZONE, 0);
    // Blocks of 512 bytes really allocated on disk: at least the file's size.
    assert!(metadata.blocks() * 512 >= metadata.len(), "{} of {}", metadata.blocks() * 512, metadata.len());
}

#[test]
fn a_vault_file_replaced_by_a_sync_app_stops_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (_vault, space) = create(&path);
    space.db().execute_batch("CREATE TABLE note (body TEXT)").unwrap();
    // Sync apps write a new file and rename it over the old one.
    let incoming = dir.path().join("incoming");
    fs::copy(&path, &incoming).unwrap();
    fs::rename(&incoming, &path).unwrap();
    assert!(space.db().execute("INSERT INTO note VALUES ('lost')", []).is_err());
    assert!(space.changed_elsewhere());
}

/// With real noise: the reserve, up to 2 GiB, is written for real. Run by hand:
/// `cargo test --release -- --ignored`.
#[test]
#[ignore]
fn a_vault_with_real_noise() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let clock = std::time::Instant::now();
    let (vault, space) = Vault::create(&path, &phrase(0).0, Setup::default()).unwrap();
    let created = clock.elapsed();
    space.lock().unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let reserve = metadata.len() / ZONE - 2 - 16;
    assert!(reserve <= 2048);
    assert!(metadata.blocks() * 512 >= metadata.len());
    eprintln!("reserve {reserve} zones, created in {created:?}");
    assert!(vault.unlock(&phrase(0).0).unwrap().is_some());
}

/// Throughput for large objects in a real file. Run by hand:
/// `cargo test --release objects_throughput -- --ignored --nocapture`.
#[test]
#[ignore]
fn objects_throughput() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (_vault, space) = create(&path);
    let content = vec![5u8; 200 << 20];
    let clock = std::time::Instant::now();
    let id = space.put_object(&content[..]).unwrap();
    let written = clock.elapsed();
    let clock = std::time::Instant::now();
    let mut at = 0;
    while at < content.len() as u64 {
        at += space.read_object(id, at..at + (1 << 20)).unwrap().len() as u64;
    }
    let read = clock.elapsed();
    eprintln!("200 MB: written in {written:?}, read in 1 MB pieces in {read:?}");
}

/// From the review: SQLite read `file:` URIs, so the app's SQL could attach a database in the
/// clear outside the vault, or VACUUM INTO one. Names are only names now; VACUUM still works.
#[test]
fn sql_writes_no_file_outside_the_vault() {
    let dir = tempfile::tempdir().unwrap();
    let (_vault, space) = create(&dir.path().join("reading"));
    space.db().execute_batch("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('kept');").unwrap();
    let plain = dir.path().join("plain.db");
    let uri = format!("file:{}?vfs=unix", plain.display());
    assert!(space.db().execute_batch(&format!("ATTACH '{uri}' AS plain")).is_err());
    assert!(space.db().execute_batch(&format!("VACUUM INTO '{uri}'")).is_err());
    assert!(space.db().execute_batch(&format!("VACUUM INTO '{}'", plain.display())).is_err());
    assert!(!plain.exists());
    space.db().execute_batch("VACUUM").unwrap();
    let body: String = space.db().query_row("SELECT body FROM note", [], |row| row.get(0)).unwrap();
    assert_eq!(body, "kept");
}

/// From the review: opening wrote the opening marker at once – 4 KiB at 1 MiB of whatever
/// file it was given, as no file tells a vault from other data. Opening writes nothing now,
/// and neither do words that open no space.
#[test]
fn opening_a_file_that_isnt_a_vault_leaves_it_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.jpg");
    let original: Vec<u8> = (0..3u32 << 20).map(|i| (i % 251) as u8).collect();
    fs::write(&path, &original).unwrap();
    let vault = Vault::open(&path).unwrap();
    assert!(vault.unlock_with_key(&phrase(0).1).unwrap().is_none());
    drop(vault);
    assert!(fs::read(&path).unwrap() == original);
}

/// From the review: creating a vault removed `.<name>.part` unconditionally, and nothing kept
/// a second process from writing beside the first. A vault's file is locked while open now:
/// a creation still running elsewhere keeps its part, and a second opener is refused.
#[test]
fn a_vault_open_elsewhere_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let partial = dir.path().join(".reading.temple-part");
    let elsewhere = fs::File::create(&partial).unwrap();
    elsewhere.lock().unwrap();
    let busy = |result: Result<Vault, Error>| matches!(result, Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::ResourceBusy);
    assert!(busy(Vault::create(&path, &phrase(0).0, Setup::default()).map(|(vault, _)| vault)));
    assert!(partial.exists());
    drop(elsewhere);

    let (vault, space) = create(&path);
    space.lock().unwrap();
    assert!(busy(Vault::open(&path)));
    drop(vault);
    assert!(Vault::open(&path).unwrap().unlock_with_key(&phrase(0).1).unwrap().is_some());
}
