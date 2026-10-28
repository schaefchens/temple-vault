# Temple review

Status: 2026-10-28, at commit 16bd3e5. All code read – library, bindings, demos, tests, tools. The Rust suite passed (77 tests), the soak run too (8,000 crashes, 2,000 copies), and the noise simulation reproduces design.md. Probe tests on a scratch copy confirmed what is marked **Seen**; **Where** alone means found in the code. Not rerun: wasm, devices, E2E, fuzzing.

A parallel review of the same day was checked point by point; what held up is in here, so this is the list to fix from. Left out: its generation overflow and hard-link fallback race (they need a record sealed with the space's key, or another process writing the folder), and its claim that the first commit after another opening goes through – the check catches it at that commit's first sync (see Low).

Severity: **high** – breaks a promise in normal use; **medium** – through a plausible mistake, crash or app bug; **low** – hardening and docs. Line numbers are those of 16bd3e5.

Fixes, the same day: every item has a **Fixed** or **Open** line, the fixed ones a test that failed before. With them, the Rust suite passes in release and debug builds, on wasm32, on the iOS simulator (iOS 26.3) and on the Android emulator (Android 16, arm64); the soak run passes, clippy and rustfmt are clean, and the demos' flows pass – Maestro on both, Playwright in Chromium, Firefox and WebKit. The emulator caught the file lock missing on Android, where Rust's `File::try_lock` has none; Temple calls `flock` itself now. Not run: fuzzing.

## What holds up

- **Crypto:** XChaCha20-Poly1305 with random nonces and each block's place as associated data; separate keys per space, log session and object; HKDF separation; Argon2id measured on devices. No misuse found.
- **Crash and copy tests:** writes torn at 4 KiB, copies in random and sequential order, the soak run – unusually thorough. Clippy is clean.
- **Small unsafe surface:** the VFS, and a heap that wipes whatever SQLite frees. SQLite stays private in the native libraries (no `sqlite3_*` exported), so other SQLite users in an app don't collide with it.
- **Fuzz targets** seal what the fuzzer makes up, so they get past the AEAD checks.
- **Numbers:** `tools/noise_sim.py` reproduces design.md's LR table, the reserve's quantiles and the 0.5 % cut-off.
- **Independent:** own Rust, wasm and device tests, React Native and web bindings, React hooks, two demos with Maestro and Playwright E2E, CI.

## High

### 1. Scattered free room overflows the superblock record

- **Where:** `allocate`/`reserve` (`src/space/store.rs:469`, `:487`) take the longest free runs when none is long enough, so the log and database collect one-block holes. Their runs live in a record of at most 16 blocks (`superblock.rs:26`); `write_superblock` then fails (`store.rs:553`).
- **Seen**, through the API alone: 20,000 small objects, every other one deleted, the rest of the room filled. In the next session, after 1,805 one-row commits the next fails with SQLite's "disk I/O error" (log in 9,073 runs), and `lock()` fails with `StorageFull` – the log's key stays on disk, and with it the session's deleted values. Reopening works, but the holes remain for the next long session.
- **Fixed:** the files the superblock records take room in runs of a zone at least, or in one run that holds all they need; objects still take any hole. `scattered_free_room_keeps_the_record_short` – 5,000 commits, the log in under 200 runs. **Open:** a record that outgrows its superblock all the same still shows as SQLite's "disk I/O error".

### 2. A long session grows the vault for good – hidden spaces too

- **Where:** the log is checkpointed only at a lock (`src/space/mod.rs:345`); SQLite pads every commit to a log block, so a one-page commit takes about 8 KiB of log. A full space at least doubles (`store.rs:505`) and never shrinks.
- **Seen:** 5,000 one-row commits in one session – an app saving a reading position – grew a vault by 50 MiB (21 to 71 zones); a second such session reused that room. A 16 MiB hidden space, set up in one go with what it holds, doubled after 692 such commits. With real noise, each growth step adds 64 MiB on average.
- **Impact:** storage on phones; and design.md rates hidden spaces by the footprint they're set up with – frequent commits add growth steps nobody planned, each with only growth noise around it.
- **Fixed:** the log turns over after a commit once it reaches 4 MiB, or 256 KiB with less free room than it holds: a checkpoint and record as at a lock, without closing; what a turnover frees waits for the next one, so a copy across one finds the old log whole. Also shortens how long deleted values stay readable. `a_long_session_keeps_the_vault_its_size`, `a_hidden_space_keeps_its_size_through_many_commits`, `deleted_values_are_unreadable_once_the_log_turned_over`, crash and copy tests across turnovers; format.md and design.md say what copies across several turnovers do.

## Medium

### 3. A crash before scrubbing leaves deleted rows readable

- **Where:** when a lock's checkpoint shrinks the database – after VACUUM, or with `auto_vacuum`, which isn't guarded – the freed blocks still hold the pages the previous lock wrote. They're scrubbed only once both superblocks have stopped listing them (`src/space/store.rs:447`), and the list of them lives only in memory (`:365`).
- **Seen:** 100 rows, lock; delete them, VACUUM, lock – and a crash after the superblocks, before the scrub. The space reopens with 0 rows and locks again without error, but the deleted rows still decrypt with the space's key; without the crash they don't.
- **Fixed** without a format change: the checkpoint cuts the database only while the newest superblock still has the log, which ends the database before the cut blocks – so they get random bytes at once, durably, before any superblock drops them. `a_crash_while_a_shrinking_database_locks_leaves_nothing_deleted`, a crash at every write of the lock.

### 4. A lock asked for while a space unlocks is lost

- **Where:** `useVault`'s `lock` does nothing unless a space is already open (`bindings/react/src/index.ts:126`); an `unlock` still running opens it afterwards (`:106`). Both demos lock when the app or page is left.
- **Impact:** words entered, app left within Argon2's second – the space opens in the background and stays open.
- **Fixed** as suggested, `create` included; a failed lock leaves the hook `locked`. **Open:** no test – the repository has no harness for the hooks.

### 5. An interrupted word change leaves the old words working

- **Where:** `change_words` (`src/space/mod.rs:133`) writes the new key blocks, syncs, then overwrites the old ones; a retry finds the new words in use (`:136`).
- **Seen:** a crash once the new key blocks are written: old and new words both open the space. A retry to the same new words gives `WordsInUse` from either side; changing the old words to a third set leaves the new ones working. Only `keep_only_this` – destroying every other space – clears both.
- **Impact:** words get changed because they leaked; an app killed at the wrong moment keeps them valid, and nothing tells it.
- **Fixed:** changing to the same new words again, from the old ones, finishes it; `forget_words` (TS `forgetWords`) overwrites another phrase's key blocks of this space from inside it. `an_interrupted_word_change_can_be_finished`, and the self-test.

### 6. Changing words keeps the master key

- **Where:** `change_words` seals the same master key under the new words (`src/space/mod.rs:133`); every other key of the space derives from it or sits in its database.
- **Impact:** old words together with any older copy of the vault – a backup, a device image – still give the master key, which opens the space as it is now and as it will be. Neither design.md nor the API says so.
- **Fixed** in design.md and the API's docs: after a leak, rebuild the vault. **Open:** a word change that gives the space a fresh master key itself.

### 7. One SQL statement makes a space unopenable

- **Where:** `temple_objects` is an ordinary table (`src/space/objects.rs:16`); opening refuses overlapping or malformed rows (`store.rs:192`, `runs.rs:95`), and Temple never repairs.
- **Seen:** `INSERT INTO temple_objects (key, size, blocks) SELECT key, size, blocks FROM temple_objects` – an app duplicating an object – and after the lock the space no longer opens: `Damaged`, for good. A bad `blocks` value or a dropped table does the same.
- **Fixed:** an authorizer refuses the app's SQL writing, creating, dropping or altering `temple_…` tables and triggers on them; rows Temple didn't write keep their blocks from use, and their objects fail when read, while the space opens. `temple_tables_are_temples_to_write`, `a_space_whose_objects_overlap_its_database_still_opens`.

### 8. ATTACH gets around the VFS's guards

- **Where:** `x_open` tells files apart by their name's suffix and guards only the database against a second open (`src/space/vfs.rs:143`); the pragma guard covers only the main database (`:295`). The database's name is readable through `pragma_database_list`.
- **Seen:** `ATTACH '<name>-wal' AS w; PRAGMA w.journal_mode = OFF` lets SQL write into the space's log; a crash then loses the session's commits (3 rows before that write, 1 after). SQLite is built with `USE_URI`, so `ATTACH 'file:…?vfs=unix'` writes plain text outside the vault – `VACUUM INTO` likewise.
- **Fixed** otherwise – the limit would also stop plain VACUUM, which attaches a temporary database: URI filenames are off, and `x_open` opens each file only in its role, the log once, while its database is open. `a_space_log_attaches_as_nothing`, `sql_writes_no_file_outside_the_vault`.

### 9. Opening a file that isn't a vault damages it

- **Where:** opening writes the opening marker at once (`src/vault.rs:129`, `:237`); nothing tells a vault from other data.
- **Seen:** a 3 MiB file of other data opens without error, and its 4 KiB at 1 MiB are overwritten. A vault on read-only storage can't be opened at all.
- **Fixed** as suggested; `create_space` still writes into whatever file it's given, as its docs say. `opening_a_file_that_isnt_a_vault_leaves_it_as_it_was`. **Open:** a vault on read-only storage.

### 10. FTS5 tables Temple doesn't recognize keep deleted terms

- **Where:** `uses_fts5` parses the `CREATE` text (`src/space/fulltext.rs:30`); `optimize`, which clears terms deleted earlier, runs only when Temple itself turns `secure-delete` on (`:20`).
- **Seen:** with `USING "fts5"(body)`, or a comment containing "using" before the clause, a deleted term stays readable with the space's key after the lock; the plain form is fine.
- **Fixed** as suggested; Temple's table `temple_fulltext` remembers the tables it cleared. `every_fts5_table_forgets_deleted_text`.

### 11. Wiping misses copies

- **Where:** keys are plain arrays moved by value – the phrase key out of `from_words` (`src/keys.rs:33`), the `Store` with its keys into `Arc<Mutex<…>>` (`src/space/mod.rs:105`), a `Space` into the bindings' objects. A move leaves the old bytes on the stack or in a freed block; dropping wipes only the last copy. The bindings hand rows, objects and the phrase key through buffers nobody wipes (`bindings/ffi/src/lib.rs:120`, `:279`).
- **Impact:** design.md's "Locking wipes its keys and everything Temple decrypted from memory" holds only in part.
- **Fixed** for Temple's own copies: keys and words live on the heap, the cipher reads the key where it is, the bindings wipe the words and keys they're given. What leaves Temple – rows, objects, the phrase key – design.md now lists as the app's.

## Low

- **A failed lock keeps the log readable.** On `ChangedElsewhere`, `StorageFull` or an I/O error the session key stays in the superblock – deliberately, for replay – but the TS `lock()` closes the space either way (`bindings/js/src/wrap.ts:168`), and `useVault` then stays `open`, showing what the space held (`bindings/react/src/index.ts:130`). **Fixed:** design.md says so; `useVault` shows `locked`.
- **The double-free check never runs.** `Free::add`'s `debug_assert!` (`src/space/runs.rs:124`) is compiled out in `cargo test --release`, and misses a free run starting inside the added one. **Fixed:** a real check on both sides, `blocks_freed_twice_are_refused`; CI runs the suite as a debug build too.
- **`put_object` gives written blocks back after a failed commit** (`objects.rs:54`): a crash could bring the row back while another object reuses its blocks. **Fixed:** they wait for the next opening.
- **Panics in SQLite callbacks abort the app,** and a panic in `lock_database` kept the store and its keys registered. **Fixed:** the write path returns errors instead of `expect`s, the registry survives a panic elsewhere, and a space unregisters even when locking panics. The free map's own checks still abort – a double allocation would corrupt.
- **The opening marker isn't checked until the first sync** (`src/vault.rs:242`), and nothing locks the file natively. **Fixed:** checked before the first write, too; on iOS and Android a vault's file is locked while open, and a second process gets `InUse`.
- **Creating a vault deletes `.<name>.part` unconditionally**, before checking the vault's own name; the web does the same. **Fixed:** the name first; the part is `.<name>.temple-part`, removed only when no creation still holds it. `a_vault_open_elsewhere_is_left_alone`.
- **Objects can't commit with the app's rows** (`objects.rs:37`), so a crash in between left an orphan. **Fixed:** `put_object` works inside the app's transaction; both demos put an object and its row in one. `objects_commit_with_the_apps_rows`.
- **Objects cross the bindings whole.** `putObject(Uint8Array)` takes the object in one buffer – two to three copies on a phone, one more as the web worker's message; the Rust API streams. **Open:** a chunked writer, or a native write from a file path.
- **Word indices aren't checked before the bindings narrow them:** 65536 or 0.5 arrive as 0. **Fixed** in `wrap`, as other numbers are: `InvalidWords`, `InvalidArgument`.
- **Web worker:** forgotten spaces stay open; any non-Temple error – a fractional range – closes the worker; `copyInto` takes a stale vault's number; two `lock()` calls collide; a space locked during `copyToNewVault` leaves a handle open. **Fixed:** a `FinalizationRegistry`, only a wasm trap is fatal, `copyInto` checks the generation, locks share one call (natively too), the handle closes.
- **`useQuery` keys its parameters by joining them with NUL.** **Fixed:** typed and unambiguous.
- **`fits` doesn't enforce the 1 GiB database limit** (`src/space/store.rs:666`). **Fixed:** `a_database_past_its_limit_does_not_fit`.
- **`toIndices`** compares typed words exactly. **Fixed:** both sides NFKD, as BIP-39.
- **The React Native demo** keeps its vault where backups reach it, offers quick unlock in any space, shows the open space in iOS's app switcher, and any `temple-demo://self-test` link writes two vaults. **Fixed:** Android backup off, a warning with quick unlock, a cover while inactive, the self-test on a tap – the web demo's too. **Open:** iOS backup – the binding has no way to exclude a file; the demo's README says so.
- **Supply chain and CI.** **Fixed:** CI sets `permissions`, checks formatting, clippy and RustSec's advisories – the core has none; the FFI lockfile warns of `basic-toml`, unmaintained, which UniFFI's templates use while building. **Open:** actions pinned by tag; pre-releases under the bindings (`uniffi-runtime-wasm =0.31.0-6`, `@ubjs/*` and `uniffi-bindgen-react-native` 0.31.0-6, `wasm-unstable-single-threaded`); two generations of RustCrypto traits; no fuzz run in CI.

## Docs

All **fixed**: the README's claims on deleted values and its line counts, design.md on wiping, quick unlock, temporary data, whole old copies, new words and what leaves Temple; format.md on the salt and typed words; the crate docs on SQLite's first use; `examples/argon2_time.rs`'s link.
