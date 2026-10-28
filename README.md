# Temple

A vault library. A vault is one file holding any number of spaces, each opened by its own 12 words. Without the words, a vault is random bytes; with one space's words, nothing shows whether other spaces exist.

Status: v1 built and tested, October 2026, its format final; its release waits for an external audit ([below](#before-v1)). First user: the Hosana app; Temple depends on nothing from it.

| Path | Contents |
| --- | --- |
| [src](src) | The library, about 2,900 lines of Rust – its API documented in the code: `cargo doc --open`. Tests: `cargo test --release` |
| [fuzz](fuzz) | Fuzz targets for cargo-fuzz – superblock decoding, the slot table, whole spaces sealed from made-up records: `cargo +nightly fuzz run space` |
| [docs/design.md](docs/design.md) | How a vault behaves and why: growth, noise numbers, keep only this space, vault properties, what a vault reveals, limits |
| [docs/format.md](docs/format.md) | Vault format: file, keys, key blocks, inside a space |
| [docs/storage-spike.md](docs/storage-spike.md) | Storage spike: SQLite against redb – results and decision |
| [docs/review.md](docs/review.md) | Review of 2026-10-28: findings by severity, each fixed or still open |
| [tools/noise_sim.py](tools/noise_sim.py) | Noise simulation behind the format's numbers: `python3 tools/noise_sim.py` |
| [tools/test_on_devices.sh](tools/test_on_devices.sh) | The tests on an Android emulator or phone and on the iOS simulator |
| [tools/test_on_wasm.sh](tools/test_on_wasm.sh) | The tests on wasm32, in Node: 32-bit, one thread, the browser's clock and random source |
| [tools/argon2_on_android.sh](tools/argon2_on_android.sh) | How long the phrase key takes on an Android phone over USB |
| [spikes/storage](spikes/storage) | The storage spike's code: `cargo run --release` |
| [bindings/ffi](bindings/ffi) | Temple's API for TypeScript through UniFFI, which uniffi-bindgen-react-native builds for React Native and the web |
| [bindings/js](bindings/js) | `@schaefchens/temple`: the TypeScript API both platforms share, BIP-39's English wordlist, and the self-test |
| [bindings/react-native](bindings/react-native) | `@schaefchens/temple-react-native`, for iOS and Android: `npm run build:ios`, `npm run build:android` |
| [bindings/web](bindings/web) | `@schaefchens/temple-web`: Temple in a worker, vaults in the origin private file system: `npm run build` |
| [bindings/react](bindings/react) | `@schaefchens/temple-react`: hooks for a vault, its open space and queries, on either platform |
| [examples/react-native](examples/react-native) | A notes app on Temple for iOS and Android (Expo), with Maestro flows: [tools/e2e_react_native.sh](tools/e2e_react_native.sh) |
| [examples/web](examples/web) | The same as a PWA (Vite), with Playwright specs for Chromium, Firefox and WebKit |

## Scope

- **In Temple:** the vault file, words to keys, spaces (create, open, lock, change words, keep only this space, copy into another vault), storage inside a space, the phrase key for quick unlock. A Rust API, and TypeScript bindings for React Native (iOS, Android) and the web, with React hooks.
- **In the app:** screens, wordlists (BIP-39's English one comes along), lock triggers, sealing the phrase key with the device's secure hardware, its own data schema.

## What the tests show

- **On phones:** every test passes on the Android emulator (Android 16, arm64) and the iOS simulator (iOS 26.3) as on the Mac. On the emulator slowed to a cheap phone's pace, the phrase key takes 1.1–1.65 s – inside the 2 s that settled Argon2 at 64 MiB ([format.md](docs/format.md#from-words-to-keys)).
- **On wasm32:** every test but the real-file ones passes in Node (2026-10-28), 32-bit and on one thread.
- **In apps:** the bindings' self-test – 19 checks, from SQL values, full-text search and objects to second spaces, changed words, rebuilding and a vault opened twice – passes in the React Native demo on the iOS simulator and the Android emulator, and in the PWA in Chromium, Firefox and WebKit (2026-10-28). So do the demos' own flows, with Maestro and Playwright: two spaces that don't see each other, a file in and out, wrong words and a typo, locking when the app or page is left, a restart or reload – and on the web a second tab refused and working offline.
- **Crashes:** a crash at any write keeps every commit made durable – while a space is used, created, grows, locks, turns its log over, changes words or keeps only itself. A crash keeps what the last sync made durable and any choice of the 4 KiB pieces written since. A long run (`cargo test --release soak -- --ignored`) tries 8,000 crashes and 2,000 copies over 40 workloads, half of them turning the log over every few commits.
- **Copies** taken during a session open to a state the space had. Taken while it locks, they open to the newest state – read front to back always, in random order in 195 of 200 tries; the rest don't open. So do copies taken while the log turns over. Taken across commits and a lock or a turnover, a copy may hold pages from both sides, which then fail when read; across several turnovers most don't open ([limits](docs/design.md#limits)). Never does a copy open to a state the space didn't have.
- **Deleted values** can't be read once a space locks or its log turns over, not even with its words – search terms in FTS5 included, however the table was made; no superblock copy keeps the log's key, not even decrypted unchecked. A crash while a shrinking database locks leaves none readable either. A lock that fails keeps the log for the next opening ([limits](docs/design.md#limits)).
- **Long sessions** keep a vault its size: 5,000 small commits in one session, as an app saving a reading position makes them, add nothing – nor to a hidden space set up in one go. Free room scattered over 10,000 one-block holes keeps a space's records short.
- **Guards:** the app's SQL can't set pragmas that would break a space, write Temple's tables, attach its log, or write a file beside the vault. Opening a file that isn't a vault leaves it as it was; a second process opening a vault is refused.
- **Unknown words** cost what known ones do: Argon2, then all four slots read and opened.
- **No zeros, no sparse region:** no 16 zero bytes in a row anywhere in a vault, and a vault file takes its full size on disk.
- **Format numbers:** slot collisions as [format.md](docs/format.md#key-blocks) computes; noise and layout as the simulation assumes; the phrase key and its subkeys match the reference Argon2 tool and OpenSSL.
- **Fuzzing** (2026-10-27), two rounds of 20 minutes per target: 39 million superblocks, 1.5 million slot tables, 1.4 million spaces. It found an overflow turning a space's zones into offsets – only reachable around the checks that come first; writing the targets turned up a checkpoint record that could ask for billions of entries. Both are fixed and kept as tests. Worth running longer before the audit.

## Before v1

- **An external security audit** – Temple stays small so it can be audited whole; about 600 of its lines are the unsafe bridge to SQLite.

## License

Apache License 2.0 – see [LICENSE](LICENSE). On crates.io the crate is `temple-vault`, imported as `temple_vault` – not published yet.
