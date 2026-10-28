# Temple

A vault library. A vault is one file holding any number of spaces, each opened by its own 12 words. Without the words, a vault is random bytes; with one space's words, nothing shows whether other spaces exist.

Status: v1 built and tested, October 2026, its format final; its release waits for an external audit ([below](#before-v1)). First user: the Hosana app; Temple depends on nothing from it.

| Path | Contents |
| --- | --- |
| [src](src) | The library, about 2,400 lines of Rust – its API documented in the code: `cargo doc --open`. Tests: `cargo test --release` |
| [fuzz](fuzz) | Fuzz targets for cargo-fuzz – superblock decoding, the slot table, whole spaces sealed from made-up records: `cargo +nightly fuzz run space` |
| [docs/design.md](docs/design.md) | How a vault behaves and why: growth, noise numbers, keep only this space, vault properties, what a vault reveals, limits |
| [docs/format.md](docs/format.md) | Vault format: file, keys, key blocks, inside a space |
| [docs/storage-spike.md](docs/storage-spike.md) | Storage spike: SQLite against redb – results and decision |
| [tools/noise_sim.py](tools/noise_sim.py) | Noise simulation behind the format's numbers: `python3 tools/noise_sim.py` |
| [tools/test_on_devices.sh](tools/test_on_devices.sh) | The tests on an Android emulator or phone and on the iOS simulator |
| [tools/argon2_on_android.sh](tools/argon2_on_android.sh) | How long the phrase key takes on an Android phone over USB |
| [spikes/storage](spikes/storage) | The storage spike's code: `cargo run --release` |

## Scope

- **In Temple:** the vault file, words to keys, spaces (create, open, lock, change words, keep only this space, copy into another vault), storage inside a space, the phrase key for quick unlock. A Rust API; Hosana's Rust core binds what the app needs for Swift and Kotlin.
- **In the app:** screens, wordlists, lock triggers, sealing the phrase key with the phone's secure hardware, its own data schema.

## What the tests show

- **On phones:** every test passes on the Android emulator (Android 16, arm64) and the iOS simulator (iOS 26.3) as on the Mac. On the emulator slowed to a cheap phone's pace, the phrase key takes 1.1–1.65 s – inside the 2 s that settled Argon2 at 64 MiB ([format.md](docs/format.md#from-words-to-keys)).
- **Crashes:** a crash at any write keeps every commit made durable – while a space is used, created, grows, locks, changes words or keeps only itself. A crash keeps what the last sync made durable and any choice of the 4 KiB pieces written since. A long run (`cargo test --release soak -- --ignored`) tries 8,000 crashes and 2,000 copies over 40 workloads.
- **Copies** taken during a session open to a state the space had. Taken while it locks, they open to the newest state – read front to back always, in random order in 195 of 200 tries; the rest don't open. Taken across commits and a lock, a copy may hold pages from both sides, which then fail when read ([limits](docs/design.md#limits)). Never does a copy open to a state the space didn't have.
- **Deleted values** can't be read once a space locks, not even with its words – search terms in FTS5 included; no superblock copy keeps the log's key, not even decrypted unchecked.
- **Unknown words** cost what known ones do: Argon2, then all four slots read and opened.
- **No zeros, no sparse region:** no 16 zero bytes in a row anywhere in a vault, and a vault file takes its full size on disk.
- **Format numbers:** slot collisions as [format.md](docs/format.md#key-blocks) computes; noise and layout as the simulation assumes; the phrase key and its subkeys match the reference Argon2 tool and OpenSSL.
- **Fuzzing** (2026-10-27), two rounds of 20 minutes per target: 39 million superblocks, 1.5 million slot tables, 1.4 million spaces. It found an overflow turning a space's zones into offsets – only reachable around the checks that come first; writing the targets turned up a checkpoint record that could ask for billions of entries. Both are fixed and kept as tests. Worth running longer before the audit.

## Before v1

- **An external security audit** – Temple stays small so it can be audited whole; about 400 of its lines are the unsafe bridge to SQLite.

## License

Apache License 2.0 – see [LICENSE](LICENSE). The crate name `temple` is taken on crates.io; publishing would need another name.
