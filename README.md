# Temple

Encrypted storage for apps whose users need their data to stay private – even from someone who takes their phone and makes them unlock it.

Your app keeps its data – notes, settings, files, audio – in a **vault**: one file on the device. Twelve words open it, much like a crypto wallet's recovery phrase. Without them the file is noise: nobody can read it, or tell what or how much is in it. A vault can hold several **spaces**, each opened by its own twelve words, and opening one shows nothing of the others. Someone forced to hand over their words can open an everyday space while a second one stays hidden.

Temple runs inside your app on iOS and Android (React Native) and in the browser (a PWA), with one TypeScript API and React hooks for all three – and as a Rust library. It is open source (Apache 2.0) and young: v1 was built and tested in October 2026, but hasn't had an external security audit yet, and it isn't published on npm or crates.io – you build it from this repository ([install](#install)).

## What it gives your app

- **A database in every space:** SQLite – tables, SQL, full-text search – plus room for large files such as audio or documents. Everything is encrypted, piece by piece.
- **Twelve words instead of passwords:** Temple makes them; the user writes them down. A typo is caught 15 times in 16. For everyday use, the app can keep a key behind Face ID or the fingerprint sensor ("quick unlock").
- **Hidden spaces:** any number per vault, and nothing in the file counts them. Random padding around the spaces keeps the file's size from telling much.
- **Deleted means gone:** once a space locks, deleted data can't be read again – not even with its words.
- **Crash-safe:** a crash or a dead battery keeps everything the app committed.
- **On the device, offline:** no server, no account. A vault is a file: it can be moved, copied, kept or left out of backups.
- **The same on every platform:** one API on iOS, Android and the web, tested on all three.

## Is it for your project?

Temple fits apps whose users are at risk if their data is seen: journals, religious or political texts where they are persecuted, notes of journalists and activists, health records, people leaving an abusive home. What it costs:

- **Storage:** every vault carries random padding – about 400 MB on average, at most 2 GB – so that its size doesn't give hidden spaces away.
- **No recovery:** lost words mean lost data. Your app must get users to write them down.
- **One device at a time:** a vault can be copied to another device, but Temple doesn't sync or merge.
- **Limits:** each space's database holds up to 1 GiB (large files go beside it); opening a space takes a tenth of a second to under two seconds, depending on the phone – deliberately, so that words can't be guessed quickly.
- **Maturity:** not audited yet, and not on npm or crates.io yet.

If your app only needs its data encrypted on the device, the phone's own encryption or SQLCipher are simpler:

| Alternative | What it does well | What it leaves out, compared with Temple |
| --- | --- | --- |
| **The phone's own encryption** – iOS Data Protection, Android's file-based encryption | Free and automatic; keys in the phone's secure hardware | Protects data mainly while the phone is locked: once it's unlocked, or the passcode given, the app's data is readable. One secret for everything, nothing hidden |
| **SQLCipher** – encrypted SQLite, e.g. through expo-sqlite or op-sqlite | Mature (since 2008), fast, widely used | One password per database: whoever gets it gets everything. No hidden areas, large files only as blobs inside the database; choosing passwords and handling keys is up to you |
| **VeraCrypt** hidden volumes | The established tool for hidden data, audited | Made for desktop systems, mounted through system drivers – nothing a mobile or web app can embed. Fixed size, one hidden volume |
| **Your own**, with libsodium or WebCrypto | Full control, small | Easy to get subtly wrong: nonces used twice, deleted data surviving in journals and logs, file sizes and times giving hidden data away, crashes halfway through a write |

## Install

Not published yet: build the packages from this repository. You need Node 22 or later and Rust through [rustup](https://rustup.rs).

### React Native – iOS and Android

For apps on React Native's New Architecture (tested with React Native 0.88 and Expo SDK 58, not in Expo Go), with Xcode for iOS and the Android NDK for Android. Builds for 64-bit phones and the simulator and emulator; Android from API level 24.

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk

git clone https://github.com/schaefchens/temple-vault.git
cd temple-vault
npm install
npm run build:ios -w @schaefchens/temple-react-native      # the iOS framework
npm run build:android -w @schaefchens/temple-react-native  # the Android libraries
mkdir -p ../temple-packages
npm pack -w @schaefchens/temple -w @schaefchens/temple-react-native -w @schaefchens/temple-react --pack-destination ../temple-packages
```

Then in your app – `@ubjs/react-native` loads Temple's native library and must be a direct dependency:

```sh
npm install ../temple-packages/schaefchens-temple-0.0.0.tgz \
  ../temple-packages/schaefchens-temple-react-native-0.0.0.tgz \
  ../temple-packages/schaefchens-temple-react-0.0.0.tgz \
  @ubjs/react-native@0.31.0-6
cd ios && pod install          # with Expo: npx expo prebuild, then npx expo run:ios
```

[examples/react-native](examples/react-native) is a complete Expo app.

### Web – a PWA

Temple runs in a web worker and keeps vaults in the browser's private file system (the origin private file system): the page must be served over HTTPS, or from localhost – and WebKit's private sessions have no such file system. Building needs the wasm32 target, a clang that compiles to wasm32 – on macOS Homebrew's `llvm`, which the build finds; on Linux `clang` and `llvm`, named in `CC_wasm32_unknown_unknown=clang AR_wasm32_unknown_unknown=llvm-ar` – and wasm-bindgen-cli at Temple's version. In the clone, after `npm install`:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked --version "$(cargo metadata --manifest-path bindings/ffi/Cargo.toml --format-version 1 | jq -r '.packages[] | select(.name == "wasm-bindgen") | .version')"
npm run build -w @schaefchens/temple-web
mkdir -p ../temple-packages
npm pack -w @schaefchens/temple -w @schaefchens/temple-web -w @schaefchens/temple-react --pack-destination ../temple-packages
```

Then install the three packages in your app, as for React Native but with `schaefchens-temple-web-0.0.0.tgz` and without `@ubjs/react-native`; Vite takes them as they are, worker and wasm module included. Ask the browser to keep the vault (`navigator.storage.persist()`); [examples/web](examples/web) is a complete PWA.

### Rust

```toml
[dependencies]
temple-vault = { git = "https://github.com/schaefchens/temple-vault" }
```

It builds SQLite, so it needs a C compiler. The API is documented in the code: `cargo doc --open`.

## Use

The same on React Native and the web:

```ts
import { temple } from '@schaefchens/temple-react-native'; // on the web: '@schaefchens/temple-web'
import { english, toIndices, toWords } from '@schaefchens/temple/words';

// A new vault: Temple makes the words, the user writes them down.
const words = await temple.generateWords();
show(toWords(words.indices, english).join(' '));
const { vault, space } = await temple.createVault(path, words);
await temple.excludeFromBackup(path); // on iOS: a backup would be another copy

await space.executeBatch('CREATE TABLE IF NOT EXISTS note (body TEXT)');
await space.execute('INSERT INTO note (body) VALUES (?)', ['first note']);
const notes = await space.query<{ body: string }>('SELECT body FROM note');
await space.lock();

// Later, with the words the user types: null if a word isn't on the list, or if they open no space.
const typed = toIndices(input, english);
const opened = typed && (await vault.unlock(typed));
```

- **Where:** on iOS and Android `path` is a file path in the app's own storage, such as expo-file-system's document directory; on the web, a file name.
- **Lock** when the app goes to the background or the page is hidden: it wipes the keys from memory and makes deleted data unreadable.
- **Words that open nothing** may be a typo – or a wish for a new space. Ask, then `vault.createSpace(await temple.generateWords())`: a new space always gets fresh words, never typed ones.
- **Large files:** `space.writeObject()` takes them in pieces, `space.readObject(id, start, end)` reads any range.
- **Quick unlock:** `space.phraseKey()` gives a key for the app to keep behind biometrics (expo-secure-store, say); `vault.unlockWithKey(key)` opens the space with it. Keep it for the everyday space only.
- **React:** `TempleProvider`, `useVault(path)` with its states – opening, missing, locked, open, failed – plus `useQuery` and `useExecute` from `@schaefchens/temple-react`.
- **Check a device:** `selfTest` from `@schaefchens/temple/self-test` runs Temple's checks end to end.

In Rust:

```rust
use temple_vault::{NewWords, Setup, Vault};

let words = NewWords::generate(); // words.indices() to show the user
let (vault, space) = Vault::create("notes.vault", &words, Setup::default())?;
space.db().execute("CREATE TABLE note (body TEXT)", [])?;
space.lock()?;
let space = vault.unlock(&words)?.expect("these words open a space");
```

## How it works

- **Words to keys:** 12 words are 128 random bits and a checksum, laid out as in BIP-39. Argon2id with 64 MiB turns them into the phrase key, HKDF-SHA256 derives every other key from there.
- **Encryption:** XChaCha20-Poly1305 with random nonces; every 4 KiB block is bound to its place in the file, and a tag table catches database blocks put back from an older copy.
- **Spaces:** four encrypted copies of each space's key block hide among 4,096 random slots. Spaces grow in whole MiB, surrounded by random noise ([noise numbers](docs/design.md#noise-numbers)).
- **Database:** SQLite in WAL mode through Temple's own file layer. The log has a key of its own, gone at every lock – and with it every deleted value the log still held.
- **More:** [design.md](docs/design.md) – behavior, threat model and limits; [format.md](docs/format.md) – the byte layout; [review.md](docs/review.md) – the reviews' findings and fixes.

## Scope

- **In Temple:** the vault file, words to keys, spaces (create, open, lock, change words, keep only this space, copy into another vault – under new words, too), storage inside a space, the phrase key for quick unlock, opening read-only, salvaging a damaged space. A Rust API, and TypeScript bindings for React Native (iOS, Android) and the web, with React hooks.
- **In the app:** screens, wordlists (BIP-39's English one comes along), lock triggers, sealing the phrase key with the device's secure hardware, its own data schema, where vaults live and whether backups take them.

## Status

v1 built and tested, October 2026, its format final; its release waits for an external security audit. Temple stays small so it can be audited whole: about 3,200 lines of Rust, of which about 650 are the unsafe bridge to SQLite and a few dozen call the system for file locks and the backup mark. First user: the Hosana app; Temple depends on nothing from it.

## Repository

| Path | Contents |
| --- | --- |
| [src](src) | The library, about 3,200 lines of Rust – its API documented in the code: `cargo doc --open`. Tests: `cargo test --release` |
| [fuzz](fuzz) | Fuzz targets for cargo-fuzz – superblock decoding, the slot table, whole spaces sealed from made-up records: `cargo +nightly fuzz run space` |
| [docs/design.md](docs/design.md) | How a vault behaves and why: growth, noise numbers, keep only this space, vault properties, what a vault reveals, limits |
| [docs/format.md](docs/format.md) | Vault format: file, keys, key blocks, inside a space |
| [docs/storage-spike.md](docs/storage-spike.md) | Storage spike: SQLite against redb – results and decision |
| [docs/review.md](docs/review.md) | Reviews of 2026-10-28 and 2026-10-29 – production readiness and crypto: findings by severity, each fixed or still open |
| [tools/noise_sim.py](tools/noise_sim.py) | Noise simulation behind the format's numbers: `python3 tools/noise_sim.py` |
| [tools/test_on_devices.sh](tools/test_on_devices.sh) | The tests on an Android emulator or phone and on the iOS simulator |
| [tools/test_on_wasm.sh](tools/test_on_wasm.sh) | The tests on wasm32, in Node: 32-bit, one thread, the browser's clock and random source |
| [tools/argon2_on_android.sh](tools/argon2_on_android.sh) | How long the phrase key takes on an Android phone over USB |
| [spikes/storage](spikes/storage) | The storage spike's code: `cargo run --release` |
| [bindings/ffi](bindings/ffi) | Temple's API for TypeScript through UniFFI, which uniffi-bindgen-react-native builds for React Native and the web |
| [bindings/js](bindings/js) | `@schaefchens/temple`: the TypeScript API both platforms share, BIP-39's English wordlist, and the self-test |
| [bindings/react-native](bindings/react-native) | `@schaefchens/temple-react-native`, for iOS and Android: `npm run build:ios`, `npm run build:android` |
| [bindings/web](bindings/web) | `@schaefchens/temple-web`: Temple in a worker, vaults in the origin private file system: `npm run build` |
| [bindings/react](bindings/react) | `@schaefchens/temple-react`: hooks for a vault, its open space and queries, on either platform; tests: `npm test -w @schaefchens/temple-react` |
| [examples/react-native](examples/react-native) | A notes app on Temple for iOS and Android (Expo), with Maestro flows: [tools/e2e_react_native.sh](tools/e2e_react_native.sh) |
| [examples/web](examples/web) | The same as a PWA (Vite), with Playwright specs for Chromium, Firefox and WebKit |

## What the tests show

- **On phones:** every test passes on the Android emulator (Android 16, arm64) and the iOS simulator (iOS 26.3) as on the Mac. On the emulator slowed to a cheap phone's pace, the phrase key takes 1.1–1.65 s – inside the 2 s that settled Argon2 at 64 MiB ([format.md](docs/format.md#from-words-to-keys)).
- **On wasm32:** every test but the real-file ones passes in Node (2026-10-29), 32-bit and on one thread.
- **In apps:** the bindings' self-test – 22 checks, from SQL values, full-text search and objects in pieces to second spaces, typed words refused for new ones, changed words, read-only and salvaged spaces, rebuilding under new words and a vault opened twice – passes in the React Native demo on the iOS simulator and the Android emulator, and in the PWA in Chromium, Firefox and WebKit (2026-10-29). So do the demos' own flows, with Maestro and Playwright: two spaces that don't see each other, a file in and out, wrong words and a typo, locking when the app or page is left, a restart or reload – and on the web a second tab refused and working offline.
- **React hooks:** opening, unlocking, locking – a lock asked for while a space unlocks, too – and queries that run again after writes, against a Temple in memory.
- **Crashes:** a crash at any write keeps every commit made durable – while a space is used, created, grows, locks, turns its log over, changes words or keeps only itself. A crash keeps what the last sync made durable and any choice of the 4 KiB pieces written since. A long run (`cargo test --release soak -- --ignored`) tries 8,000 crashes and 2,000 copies over 40 workloads, half of them turning the log over every few commits.
- **Copies** taken during a session open to a state the space had. Taken while it locks, they open to the newest state – read front to back always, in random order in 195 of 200 tries; the rest don't open. So do copies taken while the log turns over. Taken across commits and a lock or a turnover, a copy may hold pages from both sides, which then fail when read; across several turnovers most don't open ([limits](docs/design.md#limits)). Never does a copy open to a state the space didn't have.
- **Deleted values** can't be read once a space locks or its log turns over, not even with its words – search terms in FTS5 included, however the table was made; no superblock copy keeps the log's key, not even decrypted unchecked. A crash while a shrinking database locks leaves none readable either. A lock that fails keeps the log for the next opening ([limits](docs/design.md#limits)).
- **Long sessions** keep a vault its size: 5,000 small commits in one session, as an app saving a reading position makes them, add nothing – nor to a hidden space set up in one go. Free room scattered over 10,000 one-block holes keeps a space's records short.
- **Guards:** the app's SQL can't set pragmas that would break a space, write Temple's tables, attach its log, or write a file beside the vault. Opening a file that isn't a vault leaves it as it was; a second process opens no space while one is open, and without an open space the file holds no lock.
- **Read-only and damage:** a vault opened read-only reads – a log that never locked too – and changes not a byte of its file, nor its time. A damaged space salvages read-only and writes nothing; damage to its tag table leaves nothing lost once it's copied into a new vault. A lock stops what runs on the space; an error beneath SQLite comes back as what it was.
- **Unknown words** cost what known ones do: Argon2, then all four slots read and opened.
- **No zeros, no sparse region:** no 16 zero bytes in a row anywhere in a vault, and a vault file takes its full size on disk.
- **Format numbers:** slot collisions as [format.md](docs/format.md#key-blocks) computes; noise and layout as the simulation assumes; the phrase key and its subkeys match the reference Argon2 tool and OpenSSL.
- **Fuzzing** (2026-10-27), two rounds of 20 minutes per target: 39 million superblocks, 1.5 million slot tables, 1.4 million spaces. It found an overflow turning a space's zones into offsets – only reachable around the checks that come first; writing the targets turned up a checkpoint record that could ask for billions of entries. Both are fixed and kept as tests. Since 2026-10-29 CI fuzzes every target for three minutes on every push and an hour each week – salvaging included. Worth running longer before the audit.

## License

Apache License 2.0 – see [LICENSE](LICENSE). The crate is `temple-vault`, imported as `temple_vault`.
