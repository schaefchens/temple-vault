# Temple vault format

Status: v1, built and tested 2026-10-27. With Argon2's cost settled on 2026-10-28, the outer format is final. Byte layout of a Temple vault, built in Rust with the RustCrypto crates (argon2, chacha20poly1305, hkdf, sha2).

The **outer format** – everything needed before a space opens – holds from v1 on: Temple can't migrate spaces it doesn't know, and the file carries no version marker. A later change would mean trying every old variant at each unlock. The **inner format** – what a space keeps in its own zones – is versioned and migrates when its space is opened.

Conventions: 1 zone = 1 MiB, 1 block = 4 KiB, integers little-endian. Every byte of the file is random or ciphertext.

## File

| Zones | Contents |
| --- | --- |
| 0 | Slot table: 4096 slots × 256 bytes |
| 1 | Opening marker in the first block; the rest reserved, random |
| 2 … | Spaces and noise, appended in whole zones |

- **One file per vault.** Where it lives is the app's choice. A default vault belongs in app-private storage – not in a cache the system may purge, and out of cloud backups.
- **Opening marker:** whenever a space opens, is created or copied in, and whenever one locks, Temple writes fresh random bytes into the first block of zone 1 and keeps them in memory; opening a vault alone writes nothing, as no file tells a vault from other data – nor does a vault opened read-only, or a salvaged space. Before the first write after that, and after each commit, it checks they are still there; if not, another device has opened a space of the vault, and Temple stops writing. On iOS and Android the file is also locked while a space is open – shared for reading – so a second process opens none meanwhile. The file's time then shows when the app last used a space, not which.
- **Created with the first space:** zones 0–1, the space's first block and the reserve are written to a temporary file, `.<name>.temple-part`, and renamed into place, so a crash leaves no half-made vault.
- **New space:** a first block of at least 16 zones, more if it is set up with more content.
- **Growth step:** at least doubles the space, followed by growth noise. A space grows only once it is full.
- **A file ending inside a zone** – a crash while appending – is filled up to the zone's end before anything new is appended.
- **Noise:** geometric numbers of zones, filled with real random bytes. The reserve (mean 384, cut off at 2048) follows the first space's first block; growth noise (mean 64) follows every growth step; creating a space adds none. Why these numbers: [design.md](design.md#noise-numbers).

## From words to keys

1. **Words:** 12 words = 132 bits: 128 random bits and a 4-bit checksum (the first 4 bits of SHA-256 over them), 11 bits per word index, most significant first – as in BIP39. Only the word indices count; wordlists, in any language, come from the app.
2. **Phrase key:** Argon2id over the 128 bits, salt `temple-vault-v1`, 64 MiB, 3 passes, 4 lanes, 32 bytes out. No per-file salt: 128 random bits already rule out precomputation – so words come from Temple, never from people: a phrase someone picks would be open to one precomputation against every vault. A new space and a word change take only `NewWords`, which only Temple's random source makes.
3. **Subkeys:** HKDF-SHA256 from the phrase key, without salt. `temple/v1/wrap` gives the wrapping key; `temple/v1/slots` gives 64 bytes, read as 32 numbers (u16 mod 4096) – the first four distinct ones are the space's slots.

Argon2: 64 MiB, settled on 2026-10-28 – the bar was at most 2 s on a cheap phone, with 32 MiB to fall back on. On the Android emulator, 64 MiB takes 143 ms at full speed; held to a Mac's efficiency cores – about 7.5× slower – 1.1 s, held to an eighth of the time 1.65 s. A [Cortex-A53 board](https://github.com/bothy-sync/bothy/issues/56) takes 1.09 s for the same work. `tools/argon2_on_android.sh` times it on a real phone. 3 passes and 4 lanes follow RFC 9106's choice for limited memory. The words carry 128 random bits on their own, so Argon2 mainly protects a phrase someone partly saw. The argon2 crate (0.5) computes the 4 lanes one after another, so they cost no more than 1 – running them in parallel would gain time on a slow phone without changing the format.

## Key blocks

A slot is nonce (24 bytes) ‖ ciphertext (216) ‖ tag (16): XChaCha20-Poly1305 with the wrapping key, associated data `temple/v1/slot` and the slot number (u16). The generation starts at 1 and rises with every rewrite.

| Plaintext | Bytes |
| --- | --- |
| Inner format version | 2 |
| Generation | 8 |
| Master key (random, encrypts the space) | 32 |
| The space's first zone | 8 |
| Reserved, zeros | 166 |

- **Opening:** try all four slots – always all four, so every phrase costs the same. The valid copy with the highest generation wins; none valid means unknown words.
- **Creating:** zones and superblock first, sync, then the four key blocks, sync. A crash in between leaves only noise.
- **Writing:** a space overwrites only slots that still hold its own valid block – except when it is created (it claims four slots blindly) and with "keep only this space" (zone 0 rewritten: the open space's blocks at its slots, random bytes everywhere else).
- **New words:** write the blocks at the new slots, then overwrite the old ones that still hold the space's block.
- **Collisions:** a new space may overwrite copies of others. A space loses all four copies with odds of about 1 in 7 million after 20 later spaces, 1 in 13,000 after 100.
- **Quick unlock:** Temple hands out the phrase key for the app to seal with the phone's secure hardware, and later opens the space with it instead of the words. After "keep only this space", the app drops that copy unless the open space was opened with it.

## Inside a space (inner format v1)

The engine is SQLite in WAL mode, chosen by the [storage spike](storage-spike.md). The inner format is versioned, so a later engine never touches the outer format.

The space's zones, in order, form its block space. Its keys come from the master key via HKDF-SHA256 (`temple/v1/superblock`, `…/db`); the log has a session key of its own. Every block is XChaCha20-Poly1305 – payload (4056 bytes) ‖ nonce ‖ tag – with a fresh random nonce on each write and its number in the file (u64) as associated data.

- **Superblock:** two copies, 64 KiB apart at the start of the first zone: inner version, generation, the space's zones, the blocks of the database, its log and its tag table, a hash over the tag table, the log's session key while a log exists – always in the copy's first block, which every rewrite replaces – and the last checkpoint's record. A copy takes as many blocks as its record needs – nearly always one, which a copy of the vault taken mid-write can't tear – sealed as one piece, its number of blocks in the associated data. Opening takes the valid copy with the higher generation whose tag table is intact and whose checkpoint is fully present (below). A sync first records newly used blocks in both copies, one after the other, so that either is current when a copy of the vault falls back on it.
- **Database:** SQLite in WAL mode with exclusive locking (no shared-memory file) and automatic checkpoints off; FTS5 for search; page size 4096 with 40 reserved bytes per page for nonce and tag; at most 1 GiB, so that a checkpoint's record fits a superblock copy. It holds the app's data and Temple's tables: the index of large objects (`temple_objects`) and the FTS5 tables Temple has cleared (`temple_fulltext`). `secure_delete` on, so deleted rows are overwritten. Pragmas that would undo any of this are refused, and so are the app's writes to `temple_…` tables but deleting rows; filenames are never URIs, and a space's files open only as what they are, so SQL can't attach a file beside them.
- **Tag table:** the tag of each database page's block, page by page, written to fresh blocks at every lock. A page is read only if its tag is the one in the table – a page from another moment, in a copy of the vault taken mid-write or put back by someone, fails instead of mixing into the state the superblock describes. SQLite's first read of page 1, for page size and reserved bytes, is exempt: it reads them again through the log. Salvaging a damaged space reads pages without the table, from the newest superblock copy that opens.
- **Log:** SQLite's write-ahead log as a second file in the block space, encrypted with a session key – random, fresh for each log, kept only in the superblock. Blocks of it that fail their check read as zeros, so SQLite's own frame checksums cut a torn log. Its record covers all its blocks rather than its length, so it changes only when the log gets more blocks. Its sectors are its blocks: SQLite pads every commit to a block boundary and never rewrites a block an earlier commit made durable.
- **Locking a space:** what the app left uncommitted is dropped, and every FTS5 table – known by its own tables, not by the SQL that made it – gets its own `secure-delete` option, and an `optimize` the first time Temple sees it, so the search index forgets deleted text too; a full checkpoint copies the log into the database and leaves the log as it is. A tag table needing room makes the space grow without a record until the lock's own. Then the new tag table, and both superblock copies get the next generation, one after the other: log empty, no session key, and the checkpoint's record – which database pages it wrote, as runs or a bitmap, whichever is shorter. Without its key the log can't be read any more, so it needs no overwriting; its blocks become free.
- **Turning the log over:** once a commit leaves the log at 4 MiB – or past 256 KiB with less free room than the log holds – Temple does with the database what a lock does, without closing it: a checkpoint, the new tag table, both superblock copies without the session key. The next commit starts a log under a fresh key. So a long session neither grows the space nor keeps deleted values readable for long. The old log and tag table stay unused until the next turnover, so that a copy of the vault taken across one finds them whole.
- **A copy taken while locking** – or while the log turns over – holds either an older superblock copy, whose session key reads the untouched log for SQLite to replay over whatever database blocks the copy has, or the newer one, whose record and tag table show whether every block the checkpoint wrote is present. If one is missing, the older copy is used. A copy taken across a session's commits and the lock that follows can hold pages from both sides of the checkpoint: those that don't match the tag table fail when read. A crash between the two superblock writes leaves the session key in one copy until the space is next opened, which rewrites it.
- **Large objects** (audio, documents): write-once objects in 4 KiB blocks, each with its own random key in its database row. Deleting the row makes the object unreadable without rewriting hundreds of MB; its blocks are free from the space's next opening, so a copy of the vault taken during the session never finds them in use twice.
- **Free space** is worked out at opening from the superblock and the object index; blocks never written keep their random bytes. The files the superblock records – database, log, tag table – take free room in runs of a zone at least, or in one run that holds all they need, so their records stay short however scattered the free room is; objects take any free blocks. Object rows that overlap or point outside the space keep their blocks from use and fail when read.
- **Crash safety:** SQLite's log for the database, superblock generations for everything else. Database blocks a checkpoint cuts off get random bytes at once, durably – the log, still in the newest superblock, ends the database before them – so no crash leaves them readable; other freed blocks are free once a superblock that doesn't list them is durable. A growth step appends zones and noise, syncs, then records them. Syncs are durable (F_FULLFSYNC on iOS).
