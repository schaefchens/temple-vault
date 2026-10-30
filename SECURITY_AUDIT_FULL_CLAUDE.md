# Temple — full technical and security audit

Independent audit, 2026-10-29. Read-only: no implementation file, dependency, binding, build
artifact or Git state was changed. The one file created is this report.

## 1. Baseline, scope, threat model, methods

### Baseline

- **Commit audited:** `e49de12` ("Fix what the recheck of 2026-10-29 found"), branch `main`, even
  with `origin/main`, working tree clean. 11 commits total.
- **Release tag:** `v0.1.0` points at `8a353dd` (the previous commit). `e49de12` is the only commit
  after the tag and is **not** released (no 0.1.1). This matters — see F1.
- **Size:** ~3,160 non-blank, non-comment, non-test lines of Rust (README says "about 3,200"),
  plus TypeScript bindings and two demo apps.
- **Crates:** argon2 0.6.0, chacha20 0.10.2, chacha20poly1305 0.11.0, hkdf 0.13.0, hmac 0.13.0,
  sha2 0.11.1, blake2 0.11.0, zeroize 1.9.1, getrandom 0.4.3, rusqlite 0.41.0 (libsqlite3-sys
  0.39.1, **SQLite 3.54.1**), and on wasm32 sqlite-wasm-rs 0.6.2 (**SQLite 3.53.4**), uniffi 0.31.2
  with uniffi-runtime-wasm 0.31.0-6. 93 crates in the lockfile, all from crates.io, no git/path
  deps, one generation of the RustCrypto trait crates.
- **Architecture / platforms:** a Rust core (`temple-vault`) exposing SQLite through its own
  encrypting VFS; a UniFFI bridge (`bindings/ffi`) built by uniffi-bindgen-react-native into a
  React Native module (iOS/Android, prebuilt `.so`/`xcframework`) and a web worker wasm module;
  a shared TypeScript API (`bindings/js`) and React hooks (`bindings/react`).
- **Artifacts vs. source:** the shipped `temple_ffi.wasm` and the Android `libtemple_ffi.so` both
  contain strings introduced by `e49de12`, so the local artifacts match current source. The prebuilt
  binaries were inspected read-only; nothing was regenerated. (The *registry* artifacts are a
  different matter — F1.)

### Security boundaries and sensitive data

- **Sensitive data:** the 12 words, the Argon2 phrase key, per-space master keys and the subkeys
  derived from them (wrapping, superblock, database keys), the per-session log key, per-object keys,
  and all plaintext (SQL rows, objects, FTS terms).
- **Persistent format:** one file per vault — zone 0 the 4096-slot key-block table, zone 1 the
  opening marker, zones 2+ spaces and random noise. Every on-disk byte is ciphertext
  (XChaCha20-Poly1305) or noise (ChaCha20 keystream). Outer format is final for v1 (no version
  marker); the inner per-space format is versioned.
- **Public entry points:** the Rust API (`Vault`, `Space`, `NewWords`, `PhraseKey`); the TS API
  (`temple.*`, `Vault`, `Space`, `ObjectWriter`); the React hooks; and, inside a space, the app's
  arbitrary SQL executed against SQLite through Temple's VFS, authorizer and pragma guard.

### Threat model (as the design states and this audit assumes)

1. **Adversary with the file, no words** — must see only random bytes, learn nothing of how many
   spaces exist. (Primary guarantee.)
2. **Adversary who compels the everyday words** — must not be able to prove a hidden space exists
   (plausible deniability), beyond the quantified likelihood ratios in `design.md`.
3. **Adversary with multiple copies over time / a forensic image** — explicitly *out of scope*:
   changed zones are visible. Documented.
4. **Active tamperer who can write the file but has no keys** — content tampering is possible for
   object blocks and whole-copy rollback (documented); database pages, key blocks and superblocks
   are tamper-evident (AEAD + tag table + superblock generations).
5. **The app itself is trusted.** On the web, any script in the page (XSS, a malicious extension) has
   the app's powers once a space is open — documented as the page's responsibility (strict CSP).
6. Crashes/power loss at any write must not lose a committed state or leave deleted data readable
   after a lock.

### Methods

- Full read of the Rust core, the FFI/TS/web/React-Native bindings, both demo apps, all tests, the
  three design docs and the prior reviews; the bundled SQLite build flags and relevant WAL/checkpoint
  behaviour; rusqlite and sqlite-wasm-rs registry sources.
- **Five parallel reviewers** over independent areas (Rust core/crypto, SQLite/VFS, bindings/FFI,
  React/demos, production/dependencies). Four returned; the dedicated SQLite/VFS reviewer hit a
  rate limit mid-run, so that area was covered directly by me and corroborated by the storage,
  bindings and production reviewers (see §7 coverage limits).
- **Independent verification:** I rebuilt and ran the core test suite in a scratch target dir
  (**105 passed, 0 failed, 3 ignored**); reproduced the two binding defects in Node; confirmed the
  release-tag timeline, the SQLite version/flag split, dependency generations, and the artifact/
  source string match. Every finding below I re-checked against the current code rather than trusting
  a reviewer or a prior report.

## 2. Executive assessment

**The cryptographic and storage core is sound and unusually carefully built. No vulnerability
exploitable by an adversary without the words was found.** The crash-, copy-, turnover-, word-change-
and deletion-safety properties are real and backed by tests that I ran and that pass. Domain
separation, nonce handling, key zeroization, the tag table, superblock generations, the
temp/journal/WAL-all-encrypted VFS, and the authorizer/pragma guards are all correct for the stated
threat model.

**It is not yet production-ready**, for reasons that are mostly around the core rather than in it:

- **The published 0.1.0 packages do not contain the fixes the docs attribute to them** (F1, High for
  anyone installing the preview).
- **One Medium data-integrity defect in the TypeScript binding** silently stores the wrong bytes when
  a React Native app passes a `Buffer` (the normal binary type) instead of a plain `Uint8Array` (F2).
- **Web-only hardening gap:** the web runs a different, older SQLite with a writable `sqlite_dbpage`
  and no defensive mode (F3).
- **Release/CI/provenance gaps:** no CI compiles the Apple/Android-only Rust, and the shipped native/
  wasm binaries have no provenance or published checksums (F4, F5).
- The project's **own stated prerequisites** remain open: an external security audit (this is an
  internal one, not a substitute), real-device test runs, and the long soak/real-noise runs.

This matches Temple's own self-description: a preview, format-final, not yet audited. The defects
are few and most are one-liners; none is a crypto break.

## 3. Findings by severity

Severities: **High** breaks a promise users rely on in normal use; **Medium** reachable through a
plausible app mistake, a crash, or the web XSS model; **Low** hardening/robustness/docs. "Confirmed"
= reproduced or proven from code I read this session; "Verified-by-reading" = traced in code but not
executed.

### F1 — Published 0.1.0 packages predate the fixes `review.md` calls "Fixed" — High (release)

- **Severity:** High for the published preview. **Status:** Confirmed. **Confidence:** High.
- **Evidence:** the `v0.1.0` tag = `8a353dd`; the fix commit `e49de12` is the only commit after it
  and edits `src/space/vfs.rs`, `src/space/fulltext.rs`, `src/space/mod.rs`, `src/vault.rs`,
  `bindings/js/src/wrap.ts`, `bindings/ffi/src/lib.rs`. The production reviewer downloaded the
  registry artifacts: `@schaefchens/*@0.1.0` carry `gitHead 8a353dd`, published ~10:18Z; the crate
  `temple-vault 0.1.0` likewise; `e49de12` is dated 11:45Z — after publication; no 0.1.1 exists.
  The temp-schema authorizer fix (`schema == b"temp"`) and the `Array.from(words)` holes fix are
  absent from the downloaded tarballs.
- **Impact:** anyone installing 0.1.0 from npm/crates.io today gets code **without** these fixes:
  a `CREATE TEMP TABLE temple_objects …` shadowing Temple's table (objects lost at lock / a lock
  that fails leaving the log key on disk), object-writer keys surviving a lock in memory, and word
  arrays with holes accepted as a valid phrase — while `README.md` and `docs/review.md` tell users
  these are fixed at 0.1.0.
- **Remediation:** publish 0.1.1 (crate + 4 npm packages, binaries rebuilt from the tagged commit);
  state in `review.md`/`README.md` which released version contains which fix; adopt a tag → CI-green →
  build-from-tag → publish checklist.

### F2 — `buffer()` stores the wrong bytes for `Buffer` views (React Native) — Medium (data integrity)

- **Severity:** Medium. **Status:** Confirmed (JS semantics reproduced). **Confidence:** High on the
  mechanism, Medium on how often it bites.
- **Where:** `bindings/js/src/wrap.ts:326` — `return bytes.slice().buffer as ArrayBuffer;`, used by
  `putObject` (`:158`), `ObjectWriter.write` (`:244`), `unlockWithKey` (`:111`) and `Value.Blob`
  params (`:98`).
- **Root cause:** `Uint8Array.prototype.slice()` copies exactly the view; `Buffer.prototype.slice()`
  (Node and the `buffer` npm polyfill common in React Native) is an alias of `subarray` and returns
  a view sharing memory, so `.buffer` is the whole backing `ArrayBuffer` from offset 0. `Buffer`
  extends `Uint8Array`, so it passes the `instanceof Uint8Array` check at `:98`.
- **Reproduction (Node, run this session):** `Buffer.from("hello").slice().buffer.byteLength` →
  **65600** (the whole internal pool, byteOffset 136), not 5; `Buffer.alloc(100).subarray(10,20)
  .slice().buffer.byteLength` → **100**, not 10. A plain `Uint8Array` sub-view returns 10 correctly.
- **Impact (React Native only):** passing a `Buffer` — e.g. `Buffer.from(base64,'base64')`, or the
  README/self-test chunking pattern `file.subarray(at, at+chunk)` where `file` is a Buffer — stores
  the wrong, larger byte range: object/blob corruption, adjacent heap memory persisted into the
  vault, and chunked writes that each store the whole file (a 50 MB file in 1 MB pieces writes
  gigabytes → `StorageFull`/vault growth). `unlockWithKey` with a 32-byte Buffer sub-view fails safe
  (`InvalidKey`). **The web is unaffected** — the page→worker `postMessage` structured-clone
  normalises a `Buffer` to a correct plain `Uint8Array` before `wrap()` runs in the worker.
- **Remediation:** `function buffer(b: Uint8Array): ArrayBuffer { return b.buffer.slice(b.byteOffset,
  b.byteOffset + b.byteLength) as ArrayBuffer; }` (verified correct for both types this session). Add
  a test that passes a `Buffer` sub-view.

### F3 — Web SQLite has a writable `sqlite_dbpage` and no defensive mode — Medium (web hardening)

- **Severity:** Medium (web only). **Status:** Confirmed by source; not probed at runtime.
  **Confidence:** Medium-high.
- **Evidence:** native `libsqlite3-sys-0.39.1` bundles SQLite **3.54.1** without DBPAGE; wasm
  `sqlite-wasm-rs-0.6.2` bundles SQLite **3.53.4** with `SQLITE_ENABLE_DBPAGE_VTAB` (also
  `BYTECODE_VTAB`, `DBSTAT_VTAB`, and `SQLITE_OMIT_LOAD_EXTENSION`). Temple sets no
  `SQLITE_DBCONFIG_DEFENSIVE` (grep of `src`, `bindings/ffi/src` finds none; rusqlite 0.41 only
  exposes it as an option). The authorizer (`src/space/vfs.rs:505-535`) keys on the `temple_` table
  name, so `UPDATE sqlite_dbpage SET data=? WHERE pgno=?` is an UPDATE on `sqlite_dbpage` → allowed;
  the pragma guard does not cover it. Default DEFENSIVE is off, so `sqlite_dbpage` is writable.
- **Impact:** on the web only, the app's SQL (or any injected script — `design.md` already treats
  page XSS as fatal for confidentiality) can overwrite any page, including page 1 (schema) or
  `temple_objects`'s pages, bypassing the table-name authorizer → the space becomes `Damaged`/
  unopenable. Writes still go through the encrypting VFS, so there is **no plaintext leak** and no
  new confidentiality loss beyond what XSS already has; the harm is integrity/availability.
- **Remediation:** `sqlite3_db_config(db, SQLITE_DBCONFIG_DEFENSIVE, 1)` after connect (also blocks
  `writable_schema` and direct shadow-table writes; compatible with Temple's FTS5/VACUUM use); add a
  test that `sqlite_dbpage` writes fail on wasm; document in `design.md`/`format.md` that web and
  native run different SQLite versions and feature sets ("the same on every platform" is API-level).

### F4 — No CI compiles or tests the Apple- or Android-specific Rust — Medium (process)

- **Severity:** Medium. **Status:** Confirmed. **Confidence:** High.
- **Evidence:** `.github/workflows/ci.yml` runs `rust`/`rust-debug`/`audit`/`wasm`/`fuzz`/`react` on
  Ubuntu; the only macOS job builds the wasm target. Never built in CI: `src/device.rs:126-163`
  (`#[cfg(target_vendor = "apple")]` CoreFoundation backup exclusion) and
  `bindings/ffi/src/background.rs:92-134` (`#[cfg(target_os = "ios")]` `NSProcessInfo` expiring
  activity). The backup test only asserts `Ok(false)` on Linux. iOS/Android suites exist only as
  local scripts.
- **Impact:** a regression in backup exclusion (a backup is another vault copy — security-relevant)
  or in the iOS lock-time path would pass CI unnoticed.
- **Remediation:** add `cargo test --release` on `macos-latest`, `cargo check --target
  aarch64-apple-ios` and `cargo ndk -t arm64-v8a check` for `bindings/ffi`.

### F5 — Shipped native/wasm artifacts have no provenance or published checksums — Medium (supply chain)

- **Severity:** Medium for a security library. **Status:** Confirmed. **Confidence:** High.
- **Evidence:** npm shows no `dist.attestations` for `@schaefchens/*` (the upstream `@ubjs/*` do carry
  provenance). The shipped `temple_ffi.wasm`, `libtemple_ffi.so` (×2) and `temple_ffi.xcframework`
  are gitignored, built locally, not reproducible, and CI never builds or publishes them; nothing
  ties a binary to a commit. Good news confirmed: **no `sqlite3_*` symbols are exported** from the
  native binaries, the `.so` files are **16 KB-page aligned** (Android 15 ready) with full RELRO, and
  ELF hardening is correct.
- **Remediation:** build in CI from the tag and publish with `npm publish --provenance` and
  `cargo publish` from CI; publish SHA-256 of the wasm/.so/xcframework in the release notes.

### F6 — `lock()` lock-ahead race: one in-flight call can run to completion before the lock — Low

- **Status:** Verified-by-reading (race window). **Confidence:** High the window exists, Low it
  matters.
- **Where:** `bindings/ffi/src/lib.rs:422-424` calls `interrupt()` then `run_first`; a job popped in
  the window around it, or just before `Space::begin_work` resets the stop flag
  (`src/space/mod.rs:191`), clears the interrupt and runs fully.
- **Impact:** that one job (e.g. a large `putObject` queued just before backgrounding) finishes and
  the lock waits behind it — the pre-fix behaviour of review.md 2026-10-29 #2, with its documented
  consequence (on iOS the app may suspend before the lock finishes; the log key stays on disk until
  the next opening). No data loss, no wrong call interrupted.
- **Remediation:** a sticky `locking` flag that `begin_work` does not reset, checked in `go_on`/
  `Space::with`.

### F7 — React Native demo regenerates the recovery words on any interruption — Medium (demo)

- **Severity:** Medium within the demo (not the library). **Status:** Confirmed. **Confidence:** High.
- **Where:** `examples/react-native/App.tsx:64` renders the inactive cover by *replacing* the whole
  tree (`if (!active) return <View style={styles.cover} />`), and `FreshWords` regenerates words on
  every mount (`:97`, `useEffect(..., [])`). `active` is set false on `inactive` **and** `background`
  (`:50`).
- **Impact:** a user on "write these 12 words down", interrupted (notification shade, Control Centre,
  a call, the app switcher), returns to the *same screen showing different words*; tapping Create
  makes a first space under words they never recorded (a space they can never open), and for Change
  words, confirming without re-reading loses the space's data — the exact "no recovery" failure the
  README tells apps to prevent. Not a confidentiality issue.
- **Remediation:** render the cover as an absolute overlay (keep children mounted), or hold
  `NewWords` outside the covered subtree, or only swap on `'background'`.

### F8 — Web demo ships no Content-Security-Policy; nothing hides content synchronously on hide — Low (demo)

- **Status:** Confirmed. **Confidence:** High.
- **Evidence:** `examples/web/index.html` and the built `dist/index.html` have no CSP meta and
  `vite.config.ts` sets no headers, although `design.md` names a strict CSP as the page's part and
  the folder is presented as "a complete PWA". Separately, on `visibilitychange→hidden` the demo
  calls `void lock()` (async) with no synchronous cover (`App.tsx:23-29`), so the PWA app-switcher
  snapshot can capture notes/words until the worker finishes the lock; `FreshWords` is never hidden.
- **Remediation:** add a meta CSP (e.g. `default-src 'self'; script-src 'self' 'wasm-unsafe-eval';
  worker-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'`) and a README note;
  set a synchronous cover on hide before awaiting the lock.

### F9 — Smaller binding/demo/robustness items — Low / Info

- **Out-of-range `bigint` SQL params wrap mod 2⁶⁴ silently** (`wrap.ts:93`; `setBigInt64` wraps —
  reproduced: `2⁶³ → -2⁶³`, `2⁶⁴+1 → 1`). Reject with `InvalidArgument` in `lower()`. (Low, confirmed.)
- **GC-triggered lock runs on the JS thread** outside the queue/interrupt/iOS-activity
  (`lib.rs:265-266`, FinalizationRegistry → `Drop for Space` → checkpoint): a UI stall and, on iOS,
  no background time. Apps should `lock()` explicitly; or hand the drop to the temple thread. (Low.)
- **Closed-vault reuse and sparse params leak non-Temple errors** through the RN wrapper
  (`wrap.ts:125-127, 145`): `WrappedVault.close()` keeps `inner`, and `params.map` preserves holes.
  Null `inner`; use `Array.from(params, lower)`. (Low.)
- **A wasm out-of-memory (huge `readObject`/`putObject`) aborts the worker** and every open space
  (`objects.rs:191-195` guards only > usize; worker traps → `fatal` → all spaces end without a lock).
  Cap `end-start` on wasm or document chunked reads. (Low, web availability.)
- **`objects_blocks` sum in `copy_with` is an unchecked `u64` fold** over DB-supplied sizes
  (`copy.rs:76`): a crafted `temple_objects` row with near-`u64::MAX` size overflows (panic in debug,
  wrap in release) — but `copy_object`/`object()` re-validate and fail `Damaged` before any short
  write, and crafting the row needs the master key. Use `checked_add`. (Low, robustness.)
- **`checkpoint_present` can issue up to 262,144 single-tag reads at open** after a whole-database
  checkpoint (`store.rs:195, 835`). Bounded by the AEAD-sealed superblock, so not attacker-amplified;
  a few hundred ms on slow flash. (Low, perf.)
- **A leftover `.<name>.temple-part` that is a FIFO can hang creation** (`vault.rs:77` opens it
  O_WRONLY). Gate the probe to regular files or use `O_NONBLOCK`. (Very low; needs local dir write.)
- **Word-entry fields** lack `autoComplete="off"`/`importantForAutofill` (web/RN) and the web has no
  "Clear"; **Android** has no `FLAG_SECURE` (recents thumbnail) and `allowBackup=false` does not by
  itself govern device-to-device transfer on API 31+ (the vault is excluded only incidentally via
  expo-secure-store's include-only rules). (Low, demo/platform; some platform-dependent.)
- **Native SQLite is built with `SQLITE_ENABLE_LOAD_EXTENSION`** (wasm omits it). rusqlite leaves it
  disabled and Temple never enables it, so apps cannot load extensions via SQL — but defence-in-depth
  would be an explicit `SQLITE_DBCONFIG_ENABLE_LOAD_EXTENSION=0`. Worth one doc line for Rust-API
  users (who must also not install their own authorizer/wal-hook). (Info.)
- **CI/release hygiene:** `cargo audit` skips `fuzz/Cargo.lock`; no `cargo deny`/SBOM/Dependabot;
  floating `dtolnay/rust-toolchain@master` with `toolchain: stable`; `tools/test_on_wasm.sh` lacks
  `--locked`; fuzz crash artifacts not uploaded; only `examples/web` type-checks in CI (the RN
  binding and demo never do); release profile has no `overflow-checks` (CI compensates with a debug
  test run); no declared MSRV; Windows is unsupported but the README says "as a Rust library". (Low.)

## 4. Confirmed vs. unverified vs. accepted vs. operational

### Confirmed defects in current code (`e49de12`)

F2 (Buffer bytes), F9 bigint-wrap, F9 `objects_blocks` overflow, F9 FIFO hang, F9 wasm-OOM worker
abort, F6 lock-ahead window, F7 (RN demo words), F8 (web CSP + sync hide), F9 demo/platform items.
F3 is confirmed by source (web `sqlite_dbpage` writable, no DEFENSIVE) but not runtime-probed.

### Unverified concerns (need execution/devices to close)

- F3 runtime probe of a `sqlite_dbpage` write on wasm.
- `finish_turnover`'s partial-checkpoint branch (`store.rs:505-521`, `log.size != 0`) is reached only
  on an I/O error mid-checkpoint; argued crash-safe but not fault-injected by the soak test.
- Real-device behaviour: iOS/Android backup flags end-to-end, flock on Android external storage,
  Android recents timing, keyboard learning of typed words, per-browser OPFS `flush()` durability,
  bfcache restore showing notes before the lock settles.
- The exact SQLite 3.54.0→3.54.1 32-bit fix (the wasm build is 3.53.4; likely unaffected, not proven).

### Accepted design limitations (documented in `design.md`/`format.md`, reasonable for v1)

Multi-copy / forensic-image adversary sees changed zones; large hidden spaces cost storage noise;
object-block and whole-copy rollback are not detected (no object hash tree); changing words keeps the
master key (rebuild under new words for a fresh one); a failed lock keeps the log + session key until
the next open; deleted values stay readable in the log until turnover (≤4 MiB of commits) or lock;
FTS3/FTS4 keep deleted terms (only FTS5 secure-delete; FTS3/4 aren't even compiled on wasm);
destroyed spaces keep their footprint; file times reveal last use; 1 GiB per database, temp data in
RAM; WebKit's OPFS quirk caps vaults ~450 MB; the free-map asserts abort on internally-inconsistent
(AEAD-valid) state; keys/rows/objects that leave Temple into the app's JS heap are not wiped; wiping
is best-effort (registers, compiler copies). On the web, any page script can use an open space.

### Operational requirements the security design relies on

One process/tab per vault (flock-capable FS or marker-only fallback); app-private storage, not caches
or external/FUSE storage; no cloud sync/backups for vaults with hidden spaces, plus `excludeFromBackup`
after every open on iOS and manifest backup rules on Android; web served over HTTPS with a strict CSP
and no third-party script; `navigator.storage.persist()`; the phrase key sealed by biometrics only for
the everyday space; a lock trigger on background/hide; SQLite must not be initialised by anything else
in the process before Temple; device full-disk encryption and screenshot/switcher covers are the app's
part. Several of these are documented only in `design.md`/`format.md`, not the README (e.g.
app-private storage, the web CSP).

## 5. Previous findings — independently verified status

All findings from the three prior reviews that fall in the audited code were re-checked against
current source; each is genuinely fixed and carries a test that I ran and that passes.

- **2026-10-28 review (11 items + lows):** scattered-free-room record overflow (#1) fixed
  (`allocate_for_file` zone-runs; `scattered_free_room_keeps_the_record_short`); long-session growth /
  log turnover (#2) fixed (`log_due`/turnover; `a_long_session_keeps_the_vault_its_size`); crash
  before scrubbing (#3) fixed (durable scrub before the superblock drops the pages;
  `a_crash_while_a_shrinking_database_locks_leaves_nothing_deleted`); lock-while-unlocking (#4),
  interrupted word change (#5), master key on word change (#6, design + rebuild),
  `temple_objects` corruption (#7), ATTACH/URI escape (#8), opening a non-vault (#9), FTS5 deleted
  terms (#10), wiping copies (#11) — all fixed with the cited tests. Lows (double-free check now a
  real assert on both sides, panics in callbacks, opening-marker check, `.part` deletion, object
  commit-with-rows, index validation, web worker finalizer, `useQuery` keying, 1 GiB `fits`,
  `toIndices` NFKD, WebKit quota, supply chain) all fixed or documented-open.
- **2026-10-29 production/crypto review:** the fixed salt now depends on `NewWords` only (random
  words); stack-wiping below derivations; file-lock scoped to an open space; a lock jumps the queue
  and interrupts running work; one damaged block is salvageable; read-only open; error translation;
  chunked object writer; iOS backup exclusion — all present and tested.
- **2026-10-29 recheck (the `e49de12` fixes):** object writers end at lock
  (`a_lock_ends_the_objects_being_written`, which I ran); Temple's SQL names `main` and `temple_…`
  names are refused in the temp database (`temples_sql_names_the_main_database`,
  `temple_tables_are_temples_to_write`); `temple_fulltext` exists from the start so a rename can't
  take it (`temples_tables_are_there_from_the_start`); `.temple-part` names refused
  (`no_vault_takes_the_name_of_one_being_created`); word holes refused; demo transaction/backup fixes.
  **Caveat (F1):** these recheck fixes are in the repo but **not in the published 0.1.0**.

Carried-forward design limitations (master key on word change, object rollback, failed-lock log
retention, multi-copy adversary) remain as documented, not as defects.

## 6. Crypto and VFS assessment (my own trace)

- **Keys:** Argon2id 64 MiB / 3 passes / 4 lanes / v0x13, 32-byte output, salt `temple-vault-v1`;
  HKDF-SHA256 with one label per subkey; master/log/object keys separate; a known-answer test pins
  the phrase key, wrapping key and slots. Fixed salt is safe because words are 128 random bits from
  the system RNG and the API refuses non-`NewWords` for creation.
- **AEAD:** XChaCha20-Poly1305, fresh random 192-bit nonce per seal (no deterministic nonces);
  associated data is the absolute block number (blocks can't be replayed to another position or
  space), the slot number for key blocks, and offset+block-count for superblocks. Decryption is
  verify-then-use and leaves the buffer unchanged on failure.
- **Integrity/rollback:** database pages are bound by the tag table, whose SHA-256 is inside the
  sealed superblock; superblocks carry monotonic generations and are chosen newest-valid; a
  rolled-back page, a torn copy or a mid-lock copy is detected. Object blocks and whole-file rollback
  are the documented exceptions.
- **VFS:** URI filenames disabled at runtime (so ATTACH can't name another VFS or an outside file);
  `x_open` enforces role flags and one-open-per-role, so the log can't be attached as a database and
  the database can't be reopened; anonymous temp/journal files go to an in-memory, zeroized handle,
  so no temp data ever reaches disk regardless of `temp_store`; the wiping heap zeroizes every SQLite
  free; the authorizer denies app writes/DDL/triggers on `temple_…` in `main` and `temp`; the pragma
  guard blocks the mode/durability/scrub/limit pragmas; `auto_vacuum`/`incremental_vacuum` truncation
  is safe because every DB truncate happens during a checkpoint and scrubs durably first; the log's
  sector size = Temple's block payload makes SQLite pad each commit to a Temple block boundary (so no
  commit rewrites a durable log block — `no_commit_rewrites_a_durable_log_block`). The rusqlite
  `close()` concern (an open cursor at lock) is not reachable: the bindings drain every query, and the
  Rust API's borrow checker forbids holding a `Statement` across `lock(self)`.

## 7. Checks run, results, skipped, coverage limits

**Run this session (read-only; scratch target dir; repo untouched):**

- `cargo test --release --locked --lib` (core suite) → **105 passed, 0 failed, 3 ignored** (10.9 s).
- Node reproductions of F2 (`Buffer.slice().buffer` returns the whole pool / parent) and F9 bigint
  wrap; confirmed the proposed F2 fix returns the exact bytes.
- Git: `v0.1.0` → `8a353dd`; `e49de12` the only later commit; it edits security-relevant files (F1).
- Bundled SQLite: native 3.54.1 (no DBPAGE, `LOAD_EXTENSION` on, `USE_URI`), wasm 3.53.4 (DBPAGE on,
  `OMIT_LOAD_EXTENSION`); Temple sets no DEFENSIVE/trusted_schema/set_db_config (F3).
- Dependency generations (single RustCrypto trait generation; no git/path deps; 93 crates);
  artifact→source string match for the wasm and Android `.so`.

**From the four parallel reviewers (each finding re-verified by me against the code):** storage core —
no confirmed vulnerabilities, all prior fixes present; bindings — F2/F6/F9 and a sound handle model,
16 KB-aligned `.so`, API surface matches; production — F1/F3/F4/F5, clean advisories (only build-time
`basic-toml` unmaintained; dev-only `happy-dom`/`zod`; `cargo audit` clean on the two main lockfiles);
React/demos — F7/F8/F9.

**Skipped / not run (constraints or environment):** the binding/release build pipeline (ubrn,
wasm-bindgen, npm build — excluded by the task); real iOS/Android device and emulator runs; the
`#[ignore]` soak (8,000 crashes / 2,000 copies) and real-noise runs; Playwright/Maestro E2E; Miri/
ASan over the unsafe VFS/heap; a runtime probe of `sqlite_dbpage` on wasm.

**Coverage limitations:** the dedicated SQLite/VFS reviewer terminated on a rate limit mid-run; I
covered that area directly (§6) and it was corroborated by the storage, bindings and production
reviewers, so I judge it well-covered, but it did not receive its own independent write-up. Fault
injection inside the turnover/checkpoint path and device-specific behaviour (durability, flock,
backups, recents) are reasoned, not executed. This audit is internal; it is not the external audit
the project itself names as a release prerequisite.

## 8. Prioritized remediation

1. **Publish 0.1.1 from `e49de12`** (crate + four npm packages, binaries rebuilt from the tag) and
   correct the "Fixed at 0.1.0" wording (F1).
2. **Fix `buffer()`** to slice the backing buffer by byteOffset/length, with a `Buffer`-input test
   (F2).
3. **Set `SQLITE_DBCONFIG_DEFENSIVE=1`** after connect and test that `sqlite_dbpage` writes fail;
   document the native/web SQLite divergence (F3).
4. **Add Apple/Android CI** (`cargo test` on macOS, `cargo check`/`cargo ndk check` for the FFI) and
   **publish provenance + checksums** for the native/wasm artifacts (F4, F5).
5. **Harden the demos/hooks:** overlay cover without remounting + stable words (F7); web CSP and
   synchronous hide (F8); the F9 binding items (bigint range check, `Array.from` params, closed-vault
   error, wasm read cap, `checked_add` in copy), word-field attributes, Android backup rules /
   `FLAG_SECURE`, the lock-ahead sticky flag (F6), the FIFO probe guard.
6. **Finish the stated prerequisites:** run the soak + real-noise + device suites, widen CI
   (`cargo deny`, fuzz-lock audit, `--locked`, RN type-check, `overflow-checks` in release, declared
   MSRV), and obtain the external audit.

## Summary

Temple's cryptography and storage core are sound and well-tested; I found **no vulnerability
exploitable by an adversary who lacks the words**, and the core test suite passes (105/0). It is
**not production-ready yet**. Blockers, in order: the **published 0.1.0 preview lacks the fixes the
docs claim** (F1); a **Medium data-integrity bug in the TypeScript binding** for `Buffer` inputs on
React Native (F2); a **web-only `sqlite_dbpage`/DEFENSIVE hardening gap** (F3); and **release/CI/
provenance gaps** (F4, F5), alongside the project's own open prerequisites (external audit, device and
soak runs). The demo and hooks findings (F6–F9) are fixable with small, local changes. None of this
contradicts Temple's self-description as an unaudited preview. I do not claim complete security or
production readiness beyond the evidence above.

Report path: `temple/SECURITY_AUDIT_FULL_CLAUDE.md`.
