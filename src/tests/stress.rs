//! Lots of content in a real file: several spaces with gigabytes of objects and searchable text,
//! sessions alternating between them, and a rebuild. Run by hand: `stress` below.

use crate::{Error, NewWords, ObjectId, PhraseKey, Setup, Space, Vault};
use rusqlite::params;
use std::fs;
use std::io::ErrorKind;
use std::ops::Range;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

const MIB: u64 = 1 << 20;
/// A chapter's audio, about: one object each.
const CHAPTER: u64 = MIB;
/// A reading of one's own, added now and then during the sessions.
const RECORDING: u64 = 64 * 1024;
const PIECE: u64 = 64 * 1024;
const WORDS: usize = 30_000;

/// Several spaces in one real file, each with a gigabyte of chapter-sized objects and tens of
/// MB of searchable text, measured as they are filled, locked and opened again; then sessions
/// that alternate between the spaces – a reading position saved, a note added, now and then a
/// small recording in and an old one out – whose opening time and the file's size should stay
/// flat; then a rebuild into a fresh vault, which must hold everything. Run by hand – it writes
/// and reads many gigabytes, and needs twice the vault's size free on disk:
/// `cargo test --release stress -- --ignored --nocapture`. Scaled with `TEMPLE_STRESS_SPACES`,
/// `TEMPLE_STRESS_OBJECT_MB` and `TEMPLE_STRESS_TEXT_MB` (both per space), `TEMPLE_STRESS_SESSIONS`
/// and `TEMPLE_STRESS_REBUILD=0`.
#[test]
#[ignore]
fn stress() {
    let spaces = setting("TEMPLE_STRESS_SPACES", 5) as usize;
    let chapters = setting("TEMPLE_STRESS_OBJECT_MB", 1024) as usize;
    let text = setting("TEMPLE_STRESS_TEXT_MB", 64) * MIB;
    let sessions = setting("TEMPLE_STRESS_SESSIONS", 300) as usize;
    let rebuild = setting("TEMPLE_STRESS_REBUILD", 1) != 0;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library");
    eprintln!(
        "stress: {spaces} spaces, each {chapters} chapters of 1 MiB and {} of text; {sessions} sessions; {} free at {}",
        size(text),
        size(free_bytes(dir.path())),
        dir.path().display()
    );
    let words = vocabulary();
    let phrases: Vec<(NewWords, PhraseKey)> = (0..spaces)
        .map(|_| {
            let words = NewWords::generate();
            let key = PhraseKey::from_words(&words);
            (words, key)
        })
        .collect();
    let mut dice = Dice(0xd1ce);
    let mut verses = vec![0u64; spaces];

    // Filling: the first space grows as it fills, as an everyday space does; the others are set
    // up in one go with room for all they will hold, as hidden spaces should be. A full disk –
    // a large random reserve on a small disk, say – stops the filling, and the test measures
    // what fit rather than failing.
    let clock = Instant::now();
    let (vault, space) = match Vault::create(&path, &phrases[0].0, Setup::default()) {
        Ok(pair) => pair,
        Err(e) if is_full(&e) => {
            eprintln!("disk full creating the first space; {} free, nothing to measure", size(free_bytes(dir.path())));
            return;
        }
        Err(e) => panic!("creating the first space: {e}"),
    };
    let setup = clock.elapsed();
    let mut made = match fill(space, 0, chapters, text, &words) {
        Ok(filled) => {
            verses[0] = filled.verses;
            report_fill(0, "created, grown", setup, &filled, chapters, &path);
            1
        }
        Err(e) if is_full(&e) => {
            eprintln!("disk full filling the first space; {} free, nothing to measure", size(free_bytes(dir.path())));
            return;
        }
        Err(e) => panic!("filling the first space: {e}"),
    };
    for n in 1..spaces {
        let clock = Instant::now();
        let room = chapters as u64 * CHAPTER + 3 * text;
        let space = match vault.create_space(&phrases[n].0, Setup { room }) {
            Ok(space) => space,
            Err(e) if is_full(&e) => break,
            Err(e) => panic!("creating space {n}: {e}"),
        };
        let setup = clock.elapsed();
        match fill(space, n, chapters, text, &words) {
            Ok(filled) => {
                verses[n] = filled.verses;
                made += 1;
                report_fill(n, "set up in one go", setup, &filled, chapters, &path);
            }
            Err(e) if is_full(&e) => break,
            Err(e) => panic!("filling space {n}: {e}"),
        }
    }
    let full_fill = made == spaces;
    let spaces = made;
    if !full_fill {
        eprintln!(
            "disk full during setup: {spaces} space(s) filled, {} free. The later phases need room, so stopping here – give it more disk, or smaller sizes (TEMPLE_STRESS_*).",
            size(free_bytes(dir.path()))
        );
        eprintln!("peak memory {}", size(peak_memory()));
        return;
    }

    // Opening again: by words and with the key – the first opening after a lock that wrote
    // the whole database checks every page it wrote – then searches, a lookup and reads.
    drop(vault);
    let vault = Vault::open(&path).unwrap();
    for (n, (phrase, key)) in phrases.iter().take(spaces).enumerate() {
        let clock = Instant::now();
        let space = vault.unlock(phrase).unwrap().expect("the space");
        let by_words = clock.elapsed();
        space.lock().unwrap();
        let clock = Instant::now();
        let space = vault.unlock_with_key(key).unwrap().expect("the space");
        let with_key = clock.elapsed();
        let searched = queries(&space, &words);
        let reads = read_ranges(&space, n, chapters, &mut dice, 100);
        let clock = Instant::now();
        let whole = space.read_object(chapter_id(&space, 0), 0..CHAPTER).unwrap();
        let a_chapter = clock.elapsed();
        assert_eq!(whole, chapter_bytes(n, 0, 0..CHAPTER));
        let clock = Instant::now();
        space.lock().unwrap();
        eprintln!(
            "space {n} again: by words {by_words:.1?}, with the key {with_key:.1?}; {searched}; 64 KiB reads median {:.1?} max {:.1?}, a chapter {a_chapter:.1?}; lock {:.1?}",
            median(&reads),
            max(&reads),
            clock.elapsed()
        );
    }

    // Sessions alternating between the spaces, as an app's days go: a reading position saved,
    // a note added, every fifth time a recording in and the previous one out, a range read.
    let before = fs::metadata(&path).unwrap().len();
    let mut recordings: Vec<Option<ObjectId>> = vec![None; spaces];
    let (mut opens, mut locks) = (Vec::new(), Vec::new());
    for s in 0..sessions {
        let n = s % spaces;
        let clock = Instant::now();
        let space = vault.unlock_with_key(&phrases[n].1).unwrap().expect("the space");
        opens.push(clock.elapsed());
        space.db().execute("UPDATE settings SET value = ?1 WHERE key = 'position'", [s.to_string()]).unwrap();
        space.db().execute("INSERT INTO note VALUES (?1)", [format!("session {s}")]).unwrap();
        if s % 5 == 0 {
            let id = space.put_object(&chapter_bytes(n, 1 << 20 | s, 0..RECORDING)[..]).unwrap();
            if let Some(old) = recordings[n].replace(id) {
                space.delete_object(old).unwrap();
            }
        }
        read_ranges(&space, n, chapters, &mut dice, 1);
        let clock = Instant::now();
        space.lock().unwrap();
        locks.push(clock.elapsed());
    }
    let after = fs::metadata(&path).unwrap().len();
    if sessions > 0 {
        let few = sessions.min(20);
        eprintln!(
            "{sessions} sessions over {spaces} spaces: opening median {:.1?} (the first {few}: {:.1?}, the last {few}: {:.1?}), max {:.1?}; locking median {:.1?}, max {:.1?}; file {} before, {} after",
            median(&opens),
            median(&opens[..few]),
            median(&opens[sessions - few..]),
            max(&opens),
            median(&locks),
            max(&locks),
            size(before),
            size(after)
        );
    }
    let notes = |n: usize| (0..sessions).filter(|s| s % spaces == n).count();
    for n in 0..spaces {
        let space = vault.unlock_with_key(&phrases[n].1).unwrap().expect("the space");
        check(&space, n, chapters, verses[n], notes(n), &mut dice);
        space.lock().unwrap();
    }
    eprintln!("every space holds its chapters, verses and notes");

    // A rebuild: the first space into a fresh vault under its words, the others after it under
    // new ones. The copies must hold everything, and open.
    if rebuild {
        let rebuilt = dir.path().join("rebuilt");
        let mut keys: Vec<PhraseKey> = phrases.iter().map(|(_, key)| key.duplicate()).collect();
        let clock = Instant::now();
        let space = vault.unlock_with_key(&phrases[0].1).unwrap().expect("the space");
        let copied = space.copy_to_new_vault(&rebuilt, None);
        space.lock().unwrap();
        let mut done = 0;
        match copied {
            Ok(_fresh) => {
                eprintln!("rebuild: space 0 copied in {:.1?}", clock.elapsed());
                done = 1;
            }
            Err(Error::Io(e)) if e.kind() == ErrorKind::StorageFull => {
                eprintln!("rebuild: no room for a copy of space 0, {} free", size(free_bytes(dir.path())));
            }
            Err(e) => panic!("rebuild: {e}"),
        }
        if done == 1 {
            let fresh = Vault::open(&rebuilt).unwrap();
            for n in 1..spaces {
                let new_words = NewWords::generate();
                let clock = Instant::now();
                let space = vault.unlock_with_key(&phrases[n].1).unwrap().expect("the space");
                let copied = space.copy_into(&fresh, Some(&new_words));
                space.lock().unwrap();
                match copied {
                    Ok(()) => {
                        eprintln!("rebuild: space {n} copied under new words in {:.1?}", clock.elapsed());
                        keys[n] = PhraseKey::from_words(&new_words);
                        done += 1;
                    }
                    Err(Error::Io(e)) if e.kind() == ErrorKind::StorageFull => {
                        eprintln!("rebuild: no room for a copy of space {n}, {} free", size(free_bytes(dir.path())));
                        break;
                    }
                    Err(e) => panic!("rebuild: {e}"),
                }
            }
            let old = fs::metadata(&path).unwrap().len();
            let new = fs::metadata(&rebuilt).unwrap().len();
            eprintln!(
                "rebuild: {done} of {spaces} spaces copied; the vault {}, the rebuilt one {}",
                size(old),
                size(new)
            );
            for (n, key) in keys.iter().enumerate().take(done) {
                let clock = Instant::now();
                let space = fresh.unlock_with_key(key).unwrap().expect("the copied space");
                let opened = clock.elapsed();
                check(&space, n, chapters, verses[n], notes(n), &mut dice);
                let clock = Instant::now();
                space.lock().unwrap();
                eprintln!(
                    "rebuilt space {n}: opens in {opened:.1?}, holds everything, locks in {:.1?}",
                    clock.elapsed()
                );
            }
        }
    }
    eprintln!("peak memory {}", size(peak_memory()));
}

/// A setting from the environment, or its default.
fn setting(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// xorshift: the same choices on every run.
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Bytes `range` of chapter `chapter` of space `n`: a pattern that tells chapters apart.
fn chapter_bytes(n: usize, chapter: usize, range: Range<u64>) -> Vec<u8> {
    let seed = ((n as u64) << 32 | chapter as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    range.map(|i| (i.wrapping_add(seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8).collect()
}

/// Words a text is made of: pronounceable nonsense, 30,000 of them.
fn vocabulary() -> Vec<String> {
    let mut dice = Dice(0x7e57);
    (0..WORDS)
        .map(|_| {
            let len = 3 + dice.below(7) as usize;
            (0..len).map(|_| (b'a' + dice.below(26) as u8) as char).collect()
        })
        .collect()
}

/// A verse: 15 to 35 words, the common ones common, as in a text.
fn verse(dice: &mut Dice, words: &[String]) -> String {
    let n = 15 + dice.below(21) as usize;
    let mut text = String::with_capacity(200);
    for i in 0..n {
        if i > 0 {
            text.push(' ');
        }
        let u = dice.below(1 << 20) as f64 / (1u64 << 20) as f64;
        text.push_str(&words[(u * u * words.len() as f64) as usize]);
    }
    text
}

/// What an app keeps in a space: settings, notes, chapters as objects, verses with an index
/// and full-text search.
fn schema(space: &Space) {
    space
        .db()
        .execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO settings VALUES ('position', '0');
             CREATE TABLE note (body TEXT NOT NULL);
             CREATE TABLE chapter (book INTEGER NOT NULL, number INTEGER NOT NULL, object INTEGER NOT NULL,
                                   PRIMARY KEY (book, number));
             CREATE TABLE verse (id INTEGER PRIMARY KEY, book INTEGER NOT NULL, chapter INTEGER NOT NULL,
                                 number INTEGER NOT NULL, text TEXT NOT NULL);
             CREATE INDEX verse_place ON verse (book, chapter, number);
             CREATE VIRTUAL TABLE search USING fts5(text, content='verse', content_rowid='id');",
        )
        .unwrap();
}

struct Filled {
    objects: Duration,
    verses: u64,
    text: Duration,
    lock: Duration,
}

/// Fills a space with its chapters – each with its row, in one transaction – and its verses,
/// in batches of 5,000, then locks it. A full disk comes back as an error, not a panic, so the
/// test can stop and report rather than fail: the space is dropped on the way out.
fn fill(space: Space, n: usize, chapters: usize, text: u64, words: &[String]) -> Result<Filled, Error> {
    schema(&space);
    let clock = Instant::now();
    for chapter in 0..chapters {
        let transaction = space.db().unchecked_transaction()?;
        let id = space.put_object(&chapter_bytes(n, chapter, 0..CHAPTER)[..])?;
        transaction.execute(
            "INSERT INTO chapter VALUES (?1, ?2, ?3)",
            params![(chapter / 60) as i64, (chapter % 60) as i64, id],
        )?;
        transaction.commit()?;
    }
    let objects = clock.elapsed();

    let clock = Instant::now();
    let mut dice = Dice(0x1234 + n as u64);
    let (mut verses, mut written) = (0u64, 0u64);
    while written < text {
        let transaction = space.db().unchecked_transaction()?;
        {
            let mut row = transaction
                .prepare_cached("INSERT INTO verse (book, chapter, number, text) VALUES (?1, ?2, ?3, ?4)")?;
            let mut index = transaction.prepare_cached("INSERT INTO search (rowid, text) VALUES (?1, ?2)")?;
            for _ in 0..5000 {
                let text = verse(&mut dice, words);
                let (book, chapter, number) = ((verses / 1500) as i64, (verses / 30 % 50) as i64, (verses % 30) as i64);
                row.execute(params![book, chapter, number, &text])?;
                index.execute(params![transaction.last_insert_rowid(), &text])?;
                written += text.len() as u64;
                verses += 1;
            }
        }
        transaction.commit()?;
    }
    let text = clock.elapsed();

    let clock = Instant::now();
    space.lock()?;
    Ok(Filled { objects, verses, text, lock: clock.elapsed() })
}

/// Whether an error is the disk running out of room – however it surfaced: a storage error from
/// Temple, or SQLite's I/O error for a write that failed underneath it.
fn is_full(e: &Error) -> bool {
    match e {
        Error::Io(io) => io.kind() == ErrorKind::StorageFull,
        Error::Database(db) => db.sqlite_error_code() == Some(rusqlite::ErrorCode::SystemIoFailure),
        _ => false,
    }
}

fn report_fill(n: usize, how: &str, setup: Duration, filled: &Filled, chapters: usize, path: &Path) {
    eprintln!(
        "space {n}, {how}: setup {setup:.1?}; {chapters} chapters in {:.1?} ({}); {} verses in {:.1?} ({:.0}/s); first lock {:.1?}; file {}",
        filled.objects,
        rate(chapters as u64 * CHAPTER, filled.objects),
        filled.verses,
        filled.text,
        filled.verses as f64 / filled.text.as_secs_f64(),
        filled.lock,
        size(fs::metadata(path).unwrap().len())
    );
}

/// A rare word and a common one searched, a verse found by its place: how long each takes.
fn queries(space: &Space, words: &[String]) -> String {
    let time = |sql: &str, parameters: &[&dyn rusqlite::ToSql]| {
        let clock = Instant::now();
        let n: i64 = space.db().query_row(sql, parameters, |row| row.get(0)).unwrap();
        (n, clock.elapsed())
    };
    let rare = format!("\"{}\"", words[words.len() - 1]);
    let common = format!("\"{}\"", words[0]);
    let (rare_hits, rare_took) = time("SELECT count(*) FROM search WHERE search MATCH ?1", &[&rare]);
    let (common_hits, common_took) = time("SELECT count(*) FROM search WHERE search MATCH ?1", &[&common]);
    let (len, place_took) = time("SELECT length(text) FROM verse WHERE book = 3 AND chapter = 7 AND number = 5", &[]);
    assert!(len > 0);
    format!(
        "search: a rare word ({rare_hits} hits) {rare_took:.1?}, a common one ({common_hits}) {common_took:.1?}; a verse by its place {place_took:.1?}"
    )
}

fn chapter_id(space: &Space, chapter: usize) -> ObjectId {
    space
        .db()
        .query_row(
            "SELECT object FROM chapter WHERE book = ?1 AND number = ?2",
            params![(chapter / 60) as i64, (chapter % 60) as i64],
            |row| row.get(0),
        )
        .unwrap()
}

/// `reads` pieces of 64 KiB at random places of random chapters, each checked against what
/// was written: how long each read takes.
fn read_ranges(space: &Space, n: usize, chapters: usize, dice: &mut Dice, reads: usize) -> Vec<Duration> {
    (0..reads)
        .map(|_| {
            let chapter = dice.below(chapters as u64) as usize;
            let id = chapter_id(space, chapter);
            let start = dice.below(CHAPTER - PIECE);
            let clock = Instant::now();
            let bytes = space.read_object(id, start..start + PIECE).unwrap();
            let took = clock.elapsed();
            assert_eq!(
                bytes,
                chapter_bytes(n, chapter, start..start + PIECE),
                "space {n}, chapter {chapter} at {start}"
            );
            took
        })
        .collect()
}

/// The space holds its chapters, verses and notes, and 20 pieces read as written.
fn check(space: &Space, n: usize, chapters: usize, verses: u64, notes: usize, dice: &mut Dice) {
    let count = |sql: &str| -> i64 { space.db().query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM chapter"), chapters as i64, "space {n}");
    assert_eq!(count("SELECT count(*) FROM verse"), verses as i64, "space {n}");
    assert_eq!(count("SELECT count(*) FROM note"), notes as i64, "space {n}");
    read_ranges(space, n, chapters, dice, 20);
}

fn median(times: &[Duration]) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

fn max(times: &[Duration]) -> Duration {
    times.iter().max().copied().unwrap_or_default()
}

fn size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GiB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    }
}

fn rate(bytes: u64, took: Duration) -> String {
    format!("{:.0} MiB/s", bytes as f64 / MIB as f64 / took.as_secs_f64())
}

/// Bytes free on the file system holding `path`.
fn free_bytes(path: &Path) -> u64 {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a zeroed statvfs is a valid value for the call to fill; the path outlives it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return 0;
    }
    // The fields' widths differ between systems.
    fn wide(n: impl Into<u64>) -> u64 {
        n.into()
    }
    wide(stat.f_bavail) * wide(stat.f_frsize)
}

/// The most memory this process has held so far.
fn peak_memory() -> u64 {
    // SAFETY: a zeroed rusage is a valid value for the call to fill.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0;
    }
    let peak = u64::try_from(usage.ru_maxrss).unwrap_or(0);
    // Bytes on Apple's systems, kilobytes elsewhere.
    if cfg!(any(target_os = "macos", target_os = "ios")) { peak } else { peak * 1024 }
}
