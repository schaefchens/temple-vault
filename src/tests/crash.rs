//! Crashes and copies at any write: every space still opens, to a state it really had.
//!
//! A crash keeps what the last sync made durable and, of the writes since, any choice of
//! 4 KiB pieces – a file system may have written some of them, in any order. A copy holds
//! each 4 KiB block as it stood at its own moment: random, as when a sync app uploads pieces
//! in parallel, or front to back, as most copies are read.

use super::memory::{Event, Memory};
use super::{SMALL_NOISE, create_on, phrase};
use crate::keys::PhraseKey;
use crate::vault::{Setup, Vault};
use crate::{BLOCK, Space};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

/// A tiny deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// What happened to a vault while it was recorded.
struct Recording {
    initial: Vec<u8>,
    events: Vec<Event>,
}

impl Recording {
    fn of(memory: &Memory) -> Recording {
        let (initial, events) = memory.recorded();
        Recording { initial, events }
    }

    /// The vault after a crash at event `at`.
    fn crash(&self, at: usize, rng: &mut Rng) -> Vec<u8> {
        let synced = self.events[..at].iter().rposition(|event| matches!(event, Event::Sync)).map_or(0, |i| i + 1);
        let mut image = self.initial.clone();
        for (i, event) in self.events[..at].iter().enumerate() {
            if let Event::Write { offset, data } = event {
                for (offset, piece) in pieces(*offset, data) {
                    if i < synced || rng.below(2) == 0 {
                        apply(&mut image, offset, piece);
                    }
                }
            }
        }
        image
    }

    /// A copy taken while events `from..to` happened: block `n` as it stood at `moment(n)`.
    fn copy(&self, from: usize, to: usize, mut moment: impl FnMut(usize) -> usize) -> Vec<u8> {
        let mut image = self.initial.clone();
        for (i, event) in self.events[..to].iter().enumerate() {
            if let Event::Write { offset, data } = event {
                for (offset, piece) in pieces(*offset, data) {
                    if i < from || i < moment(offset as usize / BLOCK) {
                        apply(&mut image, offset, piece);
                    }
                }
            }
        }
        image
    }

    /// A copy with every block at its own random moment.
    fn random_copy(&self, from: usize, to: usize, rng: &mut Rng) -> Vec<u8> {
        self.random_copy_and_moments(from, to, rng).0
    }

    fn random_copy_and_moments(&self, from: usize, to: usize, rng: &mut Rng) -> (Vec<u8>, HashMap<usize, usize>) {
        let mut moments = HashMap::new();
        let image =
            self.copy(from, to, |block| *moments.entry(block).or_insert_with(|| from + rng.below(to - from + 1)));
        (image, moments)
    }

    /// The last event that wrote to a block.
    fn last_write(&self, block: usize) -> Option<usize> {
        self.events.iter().rposition(|event| match event {
            Event::Write { offset, data } => pieces(*offset, data).any(|(offset, _)| offset as usize / BLOCK == block),
            Event::Sync => false,
        })
    }

    /// A copy read front to back, starting and ending at random moments.
    fn sequential_copy(&self, from: usize, to: usize, rng: &mut Rng) -> Vec<u8> {
        let start = from + rng.below(to - from + 1);
        let end = start + rng.below(to - start + 1);
        let blocks = self.initial.len().max(self.final_len()) / BLOCK;
        self.copy(from, to, |block| start + (end - start) * block / blocks)
    }

    fn final_len(&self) -> usize {
        let mut len = self.initial.len();
        for event in &self.events {
            if let Event::Write { offset, data } = event {
                len = len.max(*offset as usize + data.len());
            }
        }
        len
    }
}

/// A write cut at 4 KiB boundaries.
fn pieces(offset: u64, data: &[u8]) -> impl Iterator<Item = (u64, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        if at == data.len() {
            return None;
        }
        let offset = offset + at as u64;
        let n = (BLOCK - offset as usize % BLOCK).min(data.len() - at);
        let piece = &data[at..at + n];
        at += n;
        Some((offset, piece))
    })
}

/// Writes past the end leave zeros in between, as a file system does after a crash.
fn apply(image: &mut Vec<u8>, offset: u64, piece: &[u8]) {
    let (start, end) = (offset as usize, offset as usize + piece.len());
    if image.len() < end {
        image.resize(end, 0);
    }
    image[start..end].copy_from_slice(piece);
}

type State = BTreeMap<i64, String>;

/// Commits of puts, edits and deletes, each value marked, some spanning pages.
struct Workload {
    rng: Rng,
    next: i64,
    state: State,
}

impl Workload {
    fn new(seed: u64) -> Workload {
        Workload { rng: Rng(seed), next: 0, state: State::new() }
    }

    fn setup(&mut self, space: &Space, commits: usize) {
        space.db().execute_batch("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT NOT NULL)").unwrap();
        for _ in 0..commits {
            self.commit(space);
        }
    }

    fn commit(&mut self, space: &Space) {
        let tx = space.db().unchecked_transaction().unwrap();
        for _ in 0..3 + self.rng.below(6) {
            let roll = self.rng.below(100);
            if roll < 25 && !self.state.is_empty() {
                let k = *self.state.keys().nth(self.rng.below(self.state.len())).unwrap();
                tx.execute("DELETE FROM kv WHERE k = ?1", [k]).unwrap();
                self.state.remove(&k);
            } else {
                let k = if roll < 55 && !self.state.is_empty() {
                    *self.state.keys().nth(self.rng.below(self.state.len())).unwrap()
                } else {
                    self.next += 1;
                    self.next
                };
                let v = format!("<{k}:{}>", self.rng.next()).repeat(1 + self.rng.below(200));
                tx.execute("INSERT INTO kv VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = excluded.v", (k, &v))
                    .unwrap();
                self.state.insert(k, v);
            }
        }
        tx.commit().unwrap();
    }
}

/// What a space holds, after its database passed its integrity check.
fn state(space: &Space) -> Result<State, String> {
    let check: String =
        space.db().query_row("PRAGMA integrity_check", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if check != "ok" {
        return Err(format!("integrity check: {check}"));
    }
    let exists: bool = space
        .db()
        .query_row("SELECT count(*) > 0 FROM sqlite_schema WHERE name = 'kv'", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if !exists {
        return Ok(State::new());
    }
    let mut statement = space.db().prepare("SELECT k, v FROM kv").map_err(|e| e.to_string())?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|e| e.to_string())?;
    rows.collect::<Result<State, _>>().map_err(|e| e.to_string())
}

/// Opens the vault an image holds and the space `key` opens there.
fn open(image: Vec<u8>, key: &PhraseKey) -> Result<Option<Space>, String> {
    let vault = Vault::open_on(Memory::with_bytes(image), SMALL_NOISE).map_err(|e| format!("vault: {e}"))?;
    vault.unlock_with_key(key).map_err(|e| format!("space: {e}"))
}

fn open_state(image: Vec<u8>, key: &PhraseKey) -> Result<State, String> {
    let space = open(image, key)?.ok_or("the space is gone")?;
    state(&space)
}

/// A vault in memory with space 0, set up by a workload and locked.
fn vault_with(work: &mut Workload) -> (Arc<Memory>, Vault) {
    let memory = Memory::new();
    let (vault, space) = create_on(&memory, 0, Setup::default());
    work.setup(&space, 5);
    space.lock().unwrap();
    (memory, vault)
}

/// A session of `commits` commits, recorded: the states after each, and where each commit's
/// events start and end.
struct Session {
    recording: Recording,
    states: Vec<State>,
    starts: Vec<usize>,
    ends: Vec<usize>,
}

fn session(commits: usize, then_lock: bool) -> Session {
    seeded_session(0x5eed, commits, then_lock)
}

fn seeded_session(seed: u64, commits: usize, then_lock: bool) -> Session {
    let mut work = Workload::new(seed);
    let (memory, vault) = vault_with(&mut work);
    let mut states = vec![work.state.clone()];
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    memory.record();
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    for _ in 0..commits {
        starts.push(memory.events());
        work.commit(&space);
        ends.push(memory.events());
        states.push(work.state.clone());
    }
    if then_lock {
        space.lock().unwrap();
    }
    Session { recording: Recording::of(&memory), states, starts, ends }
}

#[test]
fn a_crash_during_a_session_keeps_every_durable_commit() {
    let Session { recording, states, starts, ends } = session(12, false);
    let mut rng = Rng(1);
    for _ in 0..300 {
        let at = rng.below(recording.events.len() + 1);
        let durable = ends.iter().filter(|&&end| end <= at).count();
        let begun = starts.iter().filter(|&&start| start < at).count();
        let found = open_state(recording.crash(at, &mut rng), &phrase(0).1);
        let found = found.unwrap_or_else(|e| panic!("crash at {at}: {e}"));
        assert!(states[durable..=begun].contains(&found), "crash at {at}: a state no commit had");
    }
}

#[test]
fn a_space_goes_on_after_a_crash() {
    let Session { recording, states, ends, .. } = session(6, false);
    let mut rng = Rng(2);
    for _ in 0..20 {
        let at = ends[rng.below(ends.len())];
        let vault = Vault::open_on(Memory::with_bytes(recording.crash(at, &mut rng)), SMALL_NOISE).unwrap();
        let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
        let recovered = state(&space).unwrap();
        let mut work = Workload { rng: Rng(rng.next()), next: 1000, state: recovered };
        assert!(states.contains(&work.state));
        work.commit(&space);
        space.lock().unwrap();
        let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
        assert_eq!(state(&space).unwrap(), work.state);
    }
}

#[test]
fn a_copy_during_a_session_opens_to_a_state_it_had() {
    let Session { recording, states, starts, ends } = session(12, false);
    let mut rng = Rng(3);
    for span in [1, 3, 10] {
        for first in 0..=starts.len() - span {
            for _ in 0..4 {
                let (from, to) = (starts[first], ends[first + span - 1]);
                let window = &states[first..=first + span];
                for image in [recording.random_copy(from, to, &mut rng), recording.sequential_copy(from, to, &mut rng)]
                {
                    let found =
                        open_state(image, &phrase(0).1).unwrap_or_else(|e| panic!("copy over {first}+{span}: {e}"));
                    assert!(window.contains(&found), "copy over {first}+{span}: a state no commit had");
                }
            }
        }
    }
}

#[test]
fn a_crash_while_locking_keeps_the_newest_state() {
    let Session { recording, states, ends, .. } = session(12, true);
    let newest = states.last().unwrap();
    let mut rng = Rng(4);
    for at in *ends.last().unwrap()..=recording.events.len() {
        for _ in 0..3 {
            let found = open_state(recording.crash(at, &mut rng), &phrase(0).1);
            assert!(found.as_ref() == Ok(newest), "crash at {at} of {}: {:?}", recording.events.len(), found.err());
        }
    }
}

/// Read front to back, a copy taken while the space locks always opens to the newest state.
/// Read in random order, it can't open when it holds both superblock copies as the lock
/// rewrote them but misses a page the checkpoint wrote: the log's key is gone from both,
/// and the record shows the page missing. Never does it open to anything but the newest.
#[test]
fn a_copy_while_locking_opens_to_the_newest_state() {
    let Session { recording, states, ends, .. } = session(12, true);
    let newest = states.last().unwrap();
    let (from, to) = (*ends.last().unwrap(), recording.events.len());
    // The space's first zone is zone 2; its superblock copies start 64 KiB apart.
    let superblocks = [2 * 256, 2 * 256 + 16].map(|block| (block, recording.last_write(block).unwrap()));
    let mut rng = Rng(5);
    let mut failed = 0;
    for _ in 0..200 {
        let found = open_state(recording.sequential_copy(from, to, &mut rng), &phrase(0).1);
        assert!(found.as_ref() == Ok(newest), "{:?}", found.err());

        let (image, moments) = recording.random_copy_and_moments(from, to, &mut rng);
        match open_state(image, &phrase(0).1) {
            Ok(found) => assert!(&found == newest),
            Err(e) => {
                assert_eq!(e, "space: the space is damaged");
                assert!(superblocks.iter().all(|&(block, written)| moments[&block] > written));
                failed += 1;
            }
        }
    }
    // 5 of 200 when last run.
    assert!(failed < 20, "{failed} of 200");
}

#[test]
fn a_crash_while_a_space_is_created_leaves_it_whole_or_gone() {
    let mut everyday = Workload::new(10);
    let (memory, vault) = vault_with(&mut everyday);
    memory.record();
    let space = vault.create_space(&phrase(1).0, Setup::default()).unwrap();
    let mut hidden = Workload::new(11);
    let mut states = vec![State::new()];
    hidden.setup(&space, 0);
    for _ in 0..3 {
        hidden.commit(&space);
        states.push(hidden.state.clone());
    }
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(6);
    for at in 0..=recording.events.len() {
        let image = recording.crash(at, &mut rng);
        assert!(open_state(image.clone(), &phrase(0).1) == Ok(everyday.state.clone()), "crash at {at}");
        match open(image, &phrase(1).1) {
            Ok(None) => {}
            Ok(Some(space)) => assert!(states.contains(&state(&space).unwrap()), "crash at {at}"),
            Err(e) => panic!("crash at {at}: {e}"),
        }
    }
}

#[test]
fn a_crash_while_the_space_grows_keeps_what_was_durable() {
    let mut work = Workload::new(12);
    let (memory, vault) = vault_with(&mut work);
    let before = work.state.clone();
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    memory.record();
    // More than a space of 16 zones holds: it grows once.
    let content: Vec<u8> = (0..18u32 << 20).map(|i| (i % 251) as u8).collect();
    let id = space.put_object(&content[..]).unwrap();
    work.commit(&space);
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(7);
    for _ in 0..150 {
        let at = rng.below(recording.events.len() + 1);
        let space = open(recording.crash(at, &mut rng), &phrase(0).1).unwrap().expect("the space");
        let found = state(&space).unwrap();
        assert!(found == before || found == work.state, "crash at {at}");
        match space.read_object(id, 0..content.len() as u64) {
            Ok(read) => assert!(read == content, "crash at {at}: the object differs"),
            Err(crate::Error::NoSuchObject) => assert!(found == before, "crash at {at}: the object is gone"),
            Err(e) => panic!("crash at {at}: {e}"),
        }
    }
}

#[test]
fn a_crash_while_keeping_only_this_space_keeps_it() {
    let mut works: Vec<Workload> = (0..3).map(|n| Workload::new(20 + n)).collect();
    let (memory, vault) = vault_with(&mut works[0]);
    for (n, work) in works.iter_mut().enumerate().skip(1) {
        let space = vault.create_space(&phrase(n).0, Setup::default()).unwrap();
        work.setup(&space, 3);
        space.lock().unwrap();
    }
    let mut space = vault.unlock_with_key(&phrase(1).1).unwrap().unwrap();
    memory.record();
    space.keep_only_this().unwrap();
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(8);
    for at in 0..=recording.events.len() {
        for _ in 0..3 {
            let image = recording.crash(at, &mut rng);
            for (n, work) in works.iter().enumerate() {
                match open(image.clone(), &phrase(n).1) {
                    Ok(Some(space)) => assert!(state(&space) == Ok(work.state.clone()), "crash at {at}, space {n}"),
                    Ok(None) => assert!(n != 1, "crash at {at}: the kept space is gone"),
                    Err(e) => panic!("crash at {at}, space {n}: {e}"),
                }
            }
        }
    }
}

#[test]
fn a_crash_while_changing_words_keeps_the_space_under_old_or_new() {
    let mut work = Workload::new(13);
    let (memory, vault) = vault_with(&mut work);
    let mut space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    memory.record();
    space.change_words(&phrase(1).0).unwrap();
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(9);
    for at in 0..=recording.events.len() {
        for _ in 0..3 {
            let image = recording.crash(at, &mut rng);
            let mut opened = 0;
            for n in 0..2 {
                if let Some(space) = open(image.clone(), &phrase(n).1).unwrap() {
                    assert!(state(&space) == Ok(work.state.clone()), "crash at {at}");
                    opened += 1;
                }
            }
            assert!(opened > 0, "crash at {at}: neither words open the space");
        }
    }
}

/// format.md: the log's sectors are its blocks, so SQLite pads every commit to a block
/// boundary and never rewrites a block an earlier commit made durable – a torn write can't
/// take a durable commit with it. (The first commit of a session rewrites the block holding
/// the log's header, which SQLite syncs first; no commit is durable then.)
#[test]
fn no_commit_rewrites_a_durable_log_block() {
    let Session { recording, ends, .. } = session(30, false);
    // The space's first zone is zone 2; its superblock copies take blocks 512 to 543.
    let superblocks = 512..544;
    let (mut durable, mut synced, mut since_sync) = (HashSet::new(), Vec::new(), Vec::new());
    let mut log_writes = 0;
    for (i, event) in recording.events.iter().enumerate() {
        if ends.contains(&i) {
            durable.extend(synced.drain(..));
        }
        match event {
            Event::Write { offset, data } => {
                for (offset, _) in pieces(*offset, data) {
                    let block = offset as usize / BLOCK;
                    if !superblocks.contains(&block) {
                        assert!(!durable.contains(&block), "block {block} rewritten after its commit");
                        since_sync.push(block);
                        log_writes += 1;
                    }
                }
            }
            Event::Sync => synced.append(&mut since_sync),
        }
    }
    assert!(log_writes > 100);
}

/// Many more crashes and copies than the quick tests try, over many workloads. Run by hand
/// before an audit: `cargo test --release soak -- --ignored`.
#[test]
#[ignore]
fn soak() {
    let mut rng = Rng(0x50a4);
    let (mut crashes, mut copies, mut failed) = (0, 0, 0);
    for seed in 1..=40 {
        let Session { recording, states, starts, ends } = seeded_session(seed, 20, true);
        let last_commit = *ends.last().unwrap();
        for _ in 0..200 {
            let at = rng.below(recording.events.len() + 1);
            let durable = ends.iter().filter(|&&end| end <= at).count();
            let begun = starts.iter().filter(|&&start| start < at).count();
            let found = open_state(recording.crash(at, &mut rng), &phrase(0).1)
                .unwrap_or_else(|e| panic!("seed {seed}, crash at {at}: {e}"));
            assert!(states[durable..=begun].contains(&found), "seed {seed}, crash at {at}");
            crashes += 1;
        }
        for _ in 0..50 {
            let first = rng.below(starts.len());
            let (from, to) = (starts[first], recording.events.len());
            let image = if rng.below(2) == 0 {
                recording.random_copy(from, to, &mut rng)
            } else {
                recording.sequential_copy(from, to, &mut rng)
            };
            // Across a lock, a copy can mix pages from before and after the checkpoint: it
            // must then fail – at opening, or where a page doesn't match its tag – and never
            // open to a state no commit had.
            match open_state(image, &phrase(0).1) {
                Ok(found) => {
                    assert!(states[first..].contains(&found), "seed {seed}: a copy opened to a state no commit had")
                }
                Err(_) if to > last_commit => failed += 1,
                Err(e) => panic!("seed {seed}: {e}"),
            }
            copies += 1;
        }
    }
    eprintln!("{crashes} crashes, {copies} copies ({failed} spanning a lock didn't open)");
}

/// From the review: if the tag table needs room when a full space locks, the space grows –
/// and must not record anything before the lock's own record, or a crash in between leaves
/// a record whose pages fail their tags and whose log is gone.
#[test]
fn a_crash_while_a_full_space_locks_keeps_the_newest_state() {
    let mut work = Workload::new(31);
    let (memory, vault) = vault_with(&mut work);
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    for _ in 0..5 {
        work.commit(&space);
    }
    space.store().fill_up();
    memory.record();
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(41);
    for at in 0..=recording.events.len() {
        for _ in 0..3 {
            let found = open_state(recording.crash(at, &mut rng), &phrase(0).1);
            assert!(found.as_ref() == Ok(&work.state), "crash at {at}: {:?}", found.err());
        }
    }
}

/// The same, reached through the API: an object leaves a few blocks free, commits take them,
/// then the space locks.
#[test]
fn a_crash_while_a_nearly_full_space_locks_keeps_the_newest_state() {
    let mut work = Workload::new(32);
    let (memory, vault) = vault_with(&mut work);
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    work.commit(&space);
    let free = space.store().free_blocks() as usize;
    space.put_object(&vec![9u8; (free - 10) * crate::space::PAYLOAD_BYTES][..]).unwrap();
    while space.store().free_blocks() > 0 {
        work.commit(&space);
    }
    memory.record();
    space.lock().unwrap();
    let recording = Recording::of(&memory);
    let mut rng = Rng(43);
    for at in 0..=recording.events.len() {
        let found = open_state(recording.crash(at, &mut rng), &phrase(0).1);
        assert!(found.as_ref() == Ok(&work.state), "crash at {at}: {:?}", found.err());
    }
}

/// From the review: an object deleted during a session keeps its blocks until the space
/// locks, so that a copy holding the log from before the deletion doesn't find them in use
/// by the log as well.
#[test]
fn a_copy_after_an_object_was_deleted_opens() {
    let mut work = Workload::new(77);
    let (memory, vault) = vault_with(&mut work);
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    let id = space.put_object(&vec![5u8; 1000 * crate::space::PAYLOAD_BYTES][..]).unwrap();
    space.lock().unwrap();
    let space = vault.unlock_with_key(&phrase(0).1).unwrap().unwrap();
    memory.record();
    let mut states = vec![work.state.clone()];
    for _ in 0..20 {
        work.commit(&space);
        states.push(work.state.clone());
    }
    space.delete_object(id).unwrap();
    for _ in 0..60 {
        work.commit(&space);
        states.push(work.state.clone());
    }
    let recording = Recording::of(&memory);
    let mut rng = Rng(11);
    for _ in 0..200 {
        let found = open_state(recording.random_copy(0, recording.events.len(), &mut rng), &phrase(0).1);
        assert!(found.as_ref().is_ok_and(|found| states.contains(found)), "{:?}", found.err());
    }
}
