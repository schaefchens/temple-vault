# Storage spike

2026-10-27. Which engine a space's database runs on: SQLite (rollback journal or WAL) or redb, each on the same encrypted block layer, checked against the must-haves in [format.md](format.md): a vault copied mid-write still opens, and deleted data stays unreadable. Code: [spikes/storage](../spikes/storage) – `cargo run --release`.

## Setup

- **A space in memory:** 4 KiB blocks, XChaCha20-Poly1305 with a fresh nonce per write, two superblock copies at the start. SQLite pages fill one block each, with 40 reserved bytes for nonce and tag; SQLite's journal or log and redb's file run across blocks.
- **Workload:** 60 commits of adding, editing and deleting values, each value carrying a marker.
- **Crash:** the vault as it stood after a random write.
- **Copy:** while 1, 3 or 10 commits ran, or while the space locked – each block as it stood at its own random moment (the worst case, as when a sync app uploads pieces in parallel), or read front to back (as most copies are).
- **Outcomes:** the newest state, an older committed state, won't open, integrity check fails, or a state no commit ever had ("mixed") – the dangerous one.

## Results

While a space is open – crash: 200 trials; copies in random order: 220, 212 and 184 trials:

| Engine | Crash | Copy, 1 commit | Copy, 3 commits | Copy, 10 commits |
| --- | --- | --- | --- | --- |
| SQLite, rollback journal | all newest | 26 newest, 24 mixed | none newest | none newest |
| SQLite, WAL, checkpoint every 20 pages | all newest | 146 newest, 21 mixed | none newest | none newest |
| SQLite, WAL, checkpoint only on lock | all newest | all newest | all newest | all newest |
| redb | all newest | 219 newest | 2 newest | none newest |

Everything else failed to open or failed its integrity check. redb never opened to a wrong state; it refuses instead.

While a space locks – 200 trials each:

| Lock | Copy in random order | Copy front to back | Crash |
| --- | --- | --- | --- |
| SQLite WAL: truncating checkpoint, log overwritten | none newest: 7 won't open, 192 fail integrity, 1 mixed | 169 newest, 31 won't open | all newest |
| SQLite WAL: full checkpoint, log key dropped | all newest | all newest | all newest |
| redb: compaction | 60 newest, 130 won't open, 10 panics inside redb | all newest | all newest |

Deleted or overwritten values still readable with the key, of 267:

| Engine | Space open | Space locked |
| --- | --- | --- |
| SQLite, rollback journal (`secure_delete`, journal scrubbed) | 0 | 0 |
| SQLite, WAL, log overwritten on lock | 267 | 0 |
| SQLite, WAL, log key dropped on lock | 267 | 0 |
| redb, compacted on lock | 5 | 0 |

- **Search:** SQLite's FTS5 found 1 of 4 Arabic and Persian queries as is, all 4 after folding marks and letter forms ([normalize.rs](../spikes/storage/src/normalize.rs)). redb has no full-text search.
- **Streaming:** about 300 MiB/s writing and reading through the block layer on a Mac, one thread – the same for any engine.
- **Our own code:** redb's backend 98 lines; SQLite's VFS 400 lines, mostly unsafe FFI. Underneath: redb 41,000 lines of Rust, SQLite 271,000 lines of C – the most tested database there is.
- **Caught in our own layer:** a file must shrink in its size record before its freed blocks are scrubbed, or a crash leaves a size that covers random bytes.

## Decision

SQLite in WAL mode is the inner format v1 ([format.md](format.md#inside-a-space-inner-format-v1)):
- Exclusive locking, so there's no shared-memory file; automatic checkpoints off; `secure_delete` on.
- The log is encrypted with a session key kept only in the superblock. Blocks of the log that fail their check read as zeros, so SQLite's own frame checksums cut a torn log; the database itself must always authenticate.
- Locking runs a full checkpoint, then drops the session key from both superblock copies along with a record of the blocks the checkpoint wrote. The log needs no overwriting: without its key, nobody can read it.

Why: copies taken during a session or while a space locks always opened to the newest state, crashes always recovered, and nothing ever opened to wrong data. Full-text search comes with it.

The price: deleted values stay readable in the log while the space is open, until it locks.

Rejected:
- **Rollback journal:** copies taken during a commit open to wrong data.
- **Overwriting the log on lock:** no copy taken during the lock opened to the newest state.
- **redb:** copies spanning more than one commit don't open, damaged copies make it panic, and it has no full-text search.

Limits of the spike: in memory, one workload, a Mac; copies tear at 4 KiB block boundaries; the checkpoint record lists blocks one by one, where the format uses a bitmap.
