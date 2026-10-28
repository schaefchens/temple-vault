//! Runs the storage spike and prints a report: `cargo run --release`.

use std::sync::{Arc, Mutex};
use std::time::Instant;
use storage_spike::blocks::{self, Block, FileSpec, Log, Mode, Rng, Space};
use storage_spike::normalize::normalize;
use storage_spike::redb_engine::RedbEngine;
use storage_spike::sqlite_engine::SqliteEngine;
use storage_spike::workload::{self, Engine, State, Workload};

const KEY: [u8; 32] = [7; 32];
const WARMUP: usize = 5;

#[derive(Clone, Copy)]
enum Kind {
    Journal,
    /// `None`: SQLite's default checkpoint every 1000 pages; `Some(0)`: only when the space locks.
    Wal { autocheckpoint: Option<u32> },
    /// WAL, checkpoint only on lock, the log under a session key that the lock drops.
    Shred,
    Redb,
}

impl Kind {
    fn name(self) -> String {
        match self {
            Kind::Journal => "SQLite, rollback journal".into(),
            Kind::Wal { autocheckpoint: None } => "SQLite, WAL".into(),
            Kind::Wal { autocheckpoint: Some(0) } => "SQLite, WAL, checkpoint only on lock".into(),
            Kind::Wal { autocheckpoint: Some(n) } => format!("SQLite, WAL, checkpoint every {n} pages"),
            Kind::Shred => "SQLite, WAL, log key dropped on lock".into(),
            Kind::Redb => "redb".into(),
        }
    }

    fn files(self) -> Vec<FileSpec> {
        match self {
            Kind::Redb => blocks::layout(&[(8192, Mode::Bytes)]),
            _ => blocks::layout(&[(4096, Mode::Pages), (1024, Mode::Bytes), (4096, Mode::Bytes)]),
        }
    }

    /// The write-ahead log may be torn at its end; SQLite's own frame checksums cut it there.
    /// Everything else must authenticate.
    fn lenient(self) -> Vec<bool> {
        match self {
            Kind::Wal { .. } | Kind::Shred => vec![false, false, true],
            Kind::Journal => vec![false; 3],
            Kind::Redb => vec![false],
        }
    }

    fn sqlite(self, space: Arc<Mutex<Space>>) -> Result<SqliteEngine, String> {
        match self {
            Kind::Journal => SqliteEngine::open(space, true, false, None),
            Kind::Wal { autocheckpoint } => SqliteEngine::open(space, true, true, autocheckpoint),
            Kind::Shred => SqliteEngine::open(space, true, true, Some(0)),
            Kind::Redb => unreachable!(),
        }
    }

    /// Read policy, and for the shredded log its session key.
    fn prepare(self, space: &mut Space) {
        space.lenient = self.lenient();
        if let Kind::Shred = self {
            space.shred = Some(2);
            space.begin_session();
        }
    }

    fn new_space(self) -> Arc<Mutex<Space>> {
        let mut space = Space::new(KEY, self.files());
        self.prepare(&mut space);
        Arc::new(Mutex::new(space))
    }

    fn open(self, space: Arc<Mutex<Space>>) -> Result<Box<dyn Engine>, String> {
        Ok(match self {
            Kind::Redb => Box::new(RedbEngine::open(space)?),
            _ => Box::new(self.sqlite(space)?),
        })
    }

    /// What the app does when a space locks: SQLite checkpoints and drops its log on close,
    /// redb is compacted first so deleted values go.
    fn lock(self, space: Arc<Mutex<Space>>, engine_commits: &[Vec<workload::Op>]) {
        match self {
            Kind::Redb => {
                let mut engine = RedbEngine::open(space.clone()).expect("open");
                for ops in engine_commits {
                    engine.commit(ops).expect("commit");
                }
                space.lock().unwrap().start_log();
                engine.compact().expect("compact");
            }
            Kind::Shred => {
                let mut engine = self.sqlite(space.clone()).expect("open");
                for ops in engine_commits {
                    engine.commit(ops).expect("commit");
                }
                space.lock().unwrap().start_log();
                space.lock().unwrap().begin_lock();
                engine.checkpoint_full().expect("checkpoint");
                drop(engine);
                space.lock().unwrap().finish_lock();
            }
            _ => {
                let mut engine = self.sqlite(space.clone()).expect("open");
                for ops in engine_commits {
                    engine.commit(ops).expect("commit");
                }
                space.lock().unwrap().start_log();
            }
        }
    }
}

/// A recorded run: the write log and where each recorded commit started and ended in it.
struct Run {
    log: Log,
    starts: Vec<usize>,
    ends: Vec<usize>,
}

fn record(kind: Kind, work: &Workload) -> Run {
    let space = kind.new_space();
    let mut engine = kind.open(space.clone()).expect("open");
    for ops in &work.commits[..WARMUP] {
        engine.commit(ops).expect("warm-up commit");
    }
    space.lock().unwrap().start_log();
    let position = |s: &Arc<Mutex<Space>>| s.lock().unwrap().log.as_ref().unwrap().writes.len();
    let (mut starts, mut ends) = (Vec::new(), Vec::new());
    for ops in &work.commits[WARMUP..] {
        starts.push(position(&space));
        engine.commit(ops).expect("commit");
        ends.push(position(&space));
    }
    drop(engine);
    let log = space.lock().unwrap().log.take().unwrap();
    Run { log, starts, ends }
}

#[derive(Default)]
struct Tally {
    latest: usize,
    older: usize,
    fails: usize,
    corrupt: usize,
    mixed: usize,
    panics: usize,
}

impl Tally {
    fn total(&self) -> usize {
        self.latest + self.older + self.fails + self.corrupt + self.mixed + self.panics
    }
}

/// Opens an image and sorts the outcome: newest acceptable state, an older committed
/// state, failure to open, a failed integrity check, or content no commit ever had.
fn judge(kind: Kind, image: Vec<Block>, newest: &[&State], all: &[State], tally: &mut Tally) {
    let mut inner = Tally::default();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| judge_inner(kind, image, newest, all, &mut inner)));
    match caught {
        Ok(()) => {
            tally.latest += inner.latest;
            tally.older += inner.older;
            tally.fails += inner.fails;
            tally.corrupt += inner.corrupt;
            tally.mixed += inner.mixed;
        }
        Err(_) => tally.panics += 1,
    }
}

fn judge_inner(kind: Kind, image: Vec<Block>, newest: &[&State], all: &[State], tally: &mut Tally) {
    let Ok(mut space) = Space::from_image(KEY, kind.files(), image) else {
        tally.fails += 1;
        return;
    };
    kind.prepare(&mut space);
    let mut engine = match kind.open(Arc::new(Mutex::new(space))) {
        Ok(engine) => engine,
        Err(e) => {
            if std::env::var_os("SPIKE_DEBUG").is_some() {
                eprintln!("{} won't open: {e}", kind.name());
            }
            tally.fails += 1;
            return;
        }
    };
    if engine.check() != Ok(true) {
        tally.corrupt += 1;
        return;
    }
    match engine.read_all() {
        Ok(state) if newest.contains(&&state) => tally.latest += 1,
        Ok(state) if all.contains(&state) => tally.older += 1,
        Ok(_) => tally.mixed += 1,
        Err(_) => tally.corrupt += 1,
    }
}

fn crash_test(kind: Kind, work: &Workload, run: &Run, trials: usize) -> Tally {
    let mut rng = Rng(0x5eed_0001);
    let mut tally = Tally::default();
    let total = run.log.writes.len();
    for _ in 0..trials {
        let k = rng.below(total as u64 + 1) as usize;
        // Commits finished by k must survive; the one under way may or may not.
        let done = run.ends.iter().filter(|&&end| end <= k).count();
        let mut newest = vec![&work.states[WARMUP + done - 1]];
        if done < run.starts.len() && run.starts[done] < k {
            newest.push(&work.states[WARMUP + done]);
        }
        judge(kind, blocks::crash_image(&run.log, k), &newest, &work.states, &mut tally);
    }
    tally
}

/// A copy taken while `span` commits ran; `intact_sizes` keeps the size header from the
/// end of the window, so only the engine's own blocks tear.
fn copy_test(kind: Kind, work: &Workload, run: &Run, span: usize, intact_sizes: bool) -> Tally {
    let mut tally = Tally::default();
    for round in 0..4 {
        let mut rng = Rng(0x5eed_1000 + round);
        for i in 0..run.starts.len().saturating_sub(span - 1) {
            let (from, to) = (run.starts[i], run.ends[i + span - 1]);
            let newest: Vec<&State> = (WARMUP + i - 1..WARMUP + i + span).map(|j| &work.states[j]).collect();
            let mut image = blocks::torn_image(&run.log, from, to, &mut rng);
            if intact_sizes {
                image[0] = blocks::crash_image(&run.log, to)[0];
            }
            judge(kind, image, &newest, &work.states, &mut tally);
        }
    }
    tally
}

/// Copies – blocks in random order, or read front to back – and crashes while the space locks.
fn lock_test(kind: Kind, work: &Workload, trials: u64) -> (Tally, Tally, Tally) {
    let space = kind.new_space();
    kind.lock(space.clone(), &work.commits);
    let log = space.lock().unwrap().log.take().unwrap();
    let last = work.states.last().unwrap();
    let total = log.writes.len();
    let (mut random, mut ordered, mut crashes) = (Tally::default(), Tally::default(), Tally::default());
    for trial in 0..trials {
        let mut rng = Rng(0x5eed_2000 + trial);
        judge(kind, blocks::torn_image(&log, 0, total, &mut rng), &[last], &work.states, &mut random);
        judge(kind, blocks::sequential_image(&log, 0, total, &mut rng), &[last], &work.states, &mut ordered);
        let k = rng.below(total as u64 + 1) as usize;
        judge(kind, blocks::crash_image(&log, k), &[last], &work.states, &mut crashes);
    }
    (random, ordered, crashes)
}

#[derive(Clone, Copy)]
enum Finish {
    StillOpen,
    Locked,
}

/// Runs the whole workload and counts gone values someone holding the key could still read.
fn leftovers(kind: Kind, work: &Workload, finish: Finish) -> (usize, usize) {
    let space = kind.new_space();
    match finish {
        Finish::StillOpen => {
            let mut engine = kind.open(space.clone()).expect("open");
            for ops in &work.commits {
                engine.commit(ops).expect("commit");
            }
            count(&space, work)
        }
        Finish::Locked => {
            kind.lock(space.clone(), &work.commits);
            count(&space, work)
        }
    }
}

fn count(space: &Arc<Mutex<Space>>, work: &Workload) -> (usize, usize) {
    let readable = space.lock().unwrap().readable();
    let text = String::from_utf8_lossy(&readable);
    (work.gone.iter().filter(|m| text.contains(m.as_str())).count(), work.gone.len())
}

fn row(label: &str, t: &Tally) {
    println!(
        "| {label} | {} | {} | {} | {} | {} | {} | {} |",
        t.total(),
        t.latest,
        t.older,
        t.fails,
        t.corrupt,
        t.mixed,
        t.panics
    );
}

fn search() {
    let docs = ["بِسْمِ ٱللَّهِ ٱلرَّحْمَٰنِ ٱلرَّحِيمِ", "در ابتدا کلمه بود و کلمه نزد خدا بود"];
    let queries = [("الرحمن", 0), ("بسم", 0), ("كلمه", 1), ("خدا", 1)];
    for normalized in [false, true] {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body, tokenize = 'unicode61 remove_diacritics 2')")
            .unwrap();
        for body in docs {
            let body = if normalized { normalize(body) } else { body.to_string() };
            conn.execute("INSERT INTO t(body) VALUES (?1)", [body]).unwrap();
        }
        let mut hits = Vec::new();
        for (query, expected) in queries {
            let query = if normalized { normalize(query) } else { query.to_string() };
            let found: Vec<i64> = conn
                .prepare("SELECT rowid FROM t WHERE t MATCH ?1")
                .unwrap()
                .query_map([format!("\"{query}\"")], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            hits.push(format!("{query} {}", if found == vec![expected as i64 + 1] { "✓" } else { "✗" }));
        }
        println!("| FTS5 {} | {} |", if normalized { "with normalization" } else { "as is" }, hits.join(", "));
    }
}

fn streaming() {
    const MIB: usize = 1 << 20;
    let size = 64 * MIB;
    let mut space = Space::new(KEY, blocks::layout(&[(size.div_ceil(blocks::PAYLOAD), Mode::Bytes)]));
    let chunk = vec![0x5au8; 64 * 1024];
    let clock = Instant::now();
    for i in 0..size / chunk.len() {
        space.write(0, (i * chunk.len()) as u64, &chunk).unwrap();
    }
    let write = clock.elapsed().as_secs_f64();
    let mut out = vec![0u8; chunk.len()];
    let clock = Instant::now();
    for i in 0..size / chunk.len() {
        space.read(0, (i * chunk.len()) as u64, &mut out).unwrap();
    }
    let read = clock.elapsed().as_secs_f64();
    println!("| 64 MiB through the block layer | write {:.0} MiB/s | read {:.0} MiB/s |", 64.0 / write, 64.0 / read);
}

fn main() {
    // Panics inside an engine are counted, not printed.
    std::panic::set_hook(Box::new(|info| {
        if std::env::var_os("SPIKE_DEBUG").is_some() {
            eprintln!("panic: {info}");
        }
    }));
    let work = workload::build(60, 0xa11ce);
    println!("Workload: {} commits, {} values gone by the end\n", work.commits.len(), work.gone.len());

    println!("| While a space is open | Trials | Newest state | Older state | Won't open | Integrity fails | Mixed state | Engine panics |");
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
    let kinds = [
        Kind::Journal,
        Kind::Wal { autocheckpoint: None },
        Kind::Wal { autocheckpoint: Some(20) },
        Kind::Wal { autocheckpoint: Some(0) },
        Kind::Shred,
        Kind::Redb,
    ];
    for kind in kinds {
        let run = record(kind, &work);
        let database = &kind.files()[0];
        let into_database = run.log.writes.iter().filter(|(i, _)| (database.start..database.start + database.blocks).contains(i));
        eprintln!("{}: {} block writes, {} into the database file", kind.name(), run.log.writes.len(), into_database.count());
        let name = kind.name();
        row(&format!("{name}: crash at a random write"), &crash_test(kind, &work, &run, 200));
        row(&format!("{name}: copy during 1 commit"), &copy_test(kind, &work, &run, 1, false));
        if let Kind::Journal = kind {
            row(&format!("{name}: copy during 1 commit, sizes intact"), &copy_test(kind, &work, &run, 1, true));
        }
        row(&format!("{name}: copy during 3 commits"), &copy_test(kind, &work, &run, 3, false));
        row(&format!("{name}: copy during 10 commits"), &copy_test(kind, &work, &run, 10, false));
    }

    println!("\n| While a space locks and cleans up | Trials | Newest state | Older state | Won't open | Integrity fails | Mixed state | Engine panics |");
    println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
    for kind in [Kind::Wal { autocheckpoint: Some(0) }, Kind::Shred, Kind::Redb] {
        let (random, ordered, crashes) = lock_test(kind, &work, 200);
        let name = kind.name();
        row(&format!("{name}: copy, blocks in random order"), &random);
        row(&format!("{name}: copy read front to back"), &ordered);
        row(&format!("{name}: crash"), &crashes);
    }

    println!("\n| Deleted or overwritten values still readable with the key | Space still open | Space locked |");
    println!("| --- | --- | --- |");
    for kind in [Kind::Journal, Kind::Wal { autocheckpoint: Some(0) }, Kind::Shred, Kind::Redb] {
        let (open, total) = leftovers(kind, &work, Finish::StillOpen);
        let (locked, _) = leftovers(kind, &work, Finish::Locked);
        println!("| {} | {open} of {total} | {locked} of {total} |", kind.name());
    }

    println!("\n| Search | Queries |");
    println!("| --- | --- |");
    search();

    println!("\n| Streaming | | |");
    println!("| --- | --- | --- |");
    streaming();
}
