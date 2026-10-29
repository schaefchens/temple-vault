//! Vaults in real files.

use super::{SMALL_NOISE, phrase};
use crate::device::{Device, FileDevice};
use crate::vault::{Setup, Vault};
use crate::{Error, Space, ZONE};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

fn create(path: &std::path::Path) -> (Vault, Space) {
    Vault::create_at(path, SMALL_NOISE, |file, claim| {
        Space::create(file, Some(claim), phrase(0).1.duplicate(), Setup::default(), true)
    })
    .unwrap()
}

fn open(path: &std::path::Path) -> Vault {
    let file = fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
    Vault::open_on(Arc::new(FileDevice::new(file, path)), SMALL_NOISE, false).unwrap()
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

/// From the recheck: creating `reading` removed a vault named `.reading.temple-part`, as if a
/// crash had left it there. Names ending so are Temple's: no vault is created or copied under
/// one.
#[test]
fn no_vault_takes_the_name_of_one_being_created() {
    let dir = tempfile::tempdir().unwrap();
    let refused =
        |result: Result<(), Error>| matches!(result, Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput);
    for name in [".reading.temple-part", "Reading.TEMPLE-PART"] {
        let created = Vault::create(dir.path().join(name), &phrase(0).0, Setup::default());
        assert!(refused(created.map(drop)), "{name}");
    }
    let (_vault, space) = create(&dir.path().join("reading"));
    assert!(refused(space.copy_to_new_vault(dir.path().join(".copy.temple-part"), None).map(drop)));
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

fn busy<T>(result: Result<T, Error>) -> bool {
    matches!(result, Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::ResourceBusy)
}

/// From the review: creating a vault removed `.<name>.part` unconditionally, and nothing kept
/// a second process from writing beside the first. A vault's file is locked while a space is
/// open now: a creation still running elsewhere keeps its part, and a second process opens
/// no space meanwhile.
#[test]
fn a_vault_open_elsewhere_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let partial = dir.path().join(".reading.temple-part");
    let elsewhere = FileDevice::new(fs::File::create(&partial).unwrap(), &partial);
    elsewhere.hold(false).unwrap();
    assert!(busy(Vault::create(&path, &phrase(0).0, Setup::default())));
    assert!(partial.exists());
    drop(elsewhere);

    let (vault, space) = create(&path);
    // Another process – a second file handle here – opens the vault, but no space while one
    // is open; not even read-only.
    let other = Vault::open(&path).unwrap();
    assert!(busy(other.unlock_with_key(&phrase(0).1)));
    assert!(busy(Vault::open_read_only(&path).unwrap().unlock_with_key(&phrase(0).1)));
    space.lock().unwrap();
    let there = other.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    assert!(busy(vault.unlock_with_key(&phrase(0).1)));
    there.lock().unwrap();
    assert!(vault.unlock_with_key(&phrase(0).1).unwrap().is_some());
}

/// From this review: the file was locked for as long as the vault was open – on iOS, a vault
/// in an app group's container would end an app suspended with it, while no space was open.
/// The lock comes and goes with the open space.
#[test]
fn a_vault_without_an_open_space_holds_no_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (vault, space) = create(&path);
    let probe = || FileDevice::new(fs::File::open(&path).unwrap(), &path).hold(false);
    assert!(probe().is_err());
    space.lock().unwrap();
    probe().unwrap();
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    assert!(probe().is_err());
    drop(space);
    probe().unwrap();
}

/// From this review: a vault on read-only storage couldn't be opened at all. Opened read-only,
/// its spaces read – the log of one that didn't lock too – and nothing is written: not the
/// opening marker, not a checkpoint as SQLite closes.
#[test]
fn a_read_only_vault_opens_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (vault, space) = create(&path);
    space.db().execute_batch("CREATE TABLE note (body TEXT); INSERT INTO note VALUES ('locked');").unwrap();
    space.put_object(&b"an object"[..]).unwrap();
    space.lock().unwrap();
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    space.db().execute("INSERT INTO note VALUES ('in the log')", []).unwrap();
    // The vault as the app's end would leave it: the last commit only in the log.
    let crashed = dir.path().join("crashed");
    fs::copy(&path, &crashed).unwrap();
    space.lock().unwrap();
    drop(vault);
    let path = crashed;

    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    assert!(Vault::open(&path).is_err());
    let before = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();

    let vault = Vault::open_read_only(&path).unwrap();
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    assert!(space.is_read_only());
    let mut statement = space.db().prepare("SELECT body FROM note ORDER BY rowid").unwrap();
    let notes: Vec<String> = statement.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    drop(statement);
    assert_eq!(notes, ["locked", "in the log"]);
    assert_eq!(space.read_object(1, 0..100).unwrap(), b"an object");
    assert!(space.db().execute("INSERT INTO note VALUES ('refused')", []).is_err());
    assert!(matches!(space.put_object(&b"x"[..]), Err(Error::ReadOnly)));
    assert!(matches!(space.delete_object(1), Err(Error::ReadOnly)));
    assert!(matches!(vault.create_space(&phrase(1).0, Setup::default()), Err(Error::ReadOnly)));
    space.lock().unwrap();
    assert!(vault.unlock(&phrase(1).0).unwrap().is_none());
    drop(vault);
    assert!(fs::read(&path).unwrap() == before);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
}

/// From this review: the iOS binding had no way to keep a vault out of backups.
#[test]
fn a_vault_can_be_left_out_of_backups() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reading");
    let (_vault, space) = create(&path);
    space.lock().unwrap();
    let marked = Vault::exclude_from_backup(&path).unwrap();
    assert_eq!(marked, cfg!(target_vendor = "apple"));
    #[cfg(target_vendor = "apple")]
    {
        // The extended attribute behind NSURLIsExcludedFromBackupKey.
        let name = c"com.apple.metadata:com_apple_backup_excludeItem";
        let file = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let size = unsafe { libc::getxattr(file.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) };
        assert!(size > 0, "no backup mark");
    }
    assert!(Vault::exclude_from_backup(dir.path().join("missing")).is_err());
}
