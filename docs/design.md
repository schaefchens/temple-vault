# Temple design

Status: settled for v1 on 2026-10-27. How a vault behaves and why; the byte layout is in [format.md](format.md).

A **vault** is one file. A **space** is what one set of 12 words opens; other words open another space with entirely different content. Temple keeps no list or count of spaces.

## Opening and locking

- **Words count by index,** not by spelling, so every phrase works with every wordlist. A checksum catches most typos without revealing anything about existing spaces.
- **Unknown words** with a valid checksum mean a new space. Nothing is written before the app confirms with the user, so a typo leaves no trace – and the new space gets fresh words, never the ones typed: 1 typo in 16 passes the checksum.
- **One space is open at a time.** Locking wipes its keys and what Temple decrypted from memory – Temple's own copies; what it hands the app, and the copies bindings make on the way, are the app's ([limits](#limits)).
- **Quick unlock:** Temple hands out the phrase key for the app to seal with secure hardware, and opens the space with it instead of the words ([format.md](format.md#key-blocks)). It opens that space for whoever passes the device's check, so it belongs to the everyday space only.
- **New words** move the space to other slots and overwrite the old ones; interrupted, both open it until the change is repeated or the old words are forgotten from inside. The master key stays: whoever has the old words and a copy of the vault from before can still read the space. After words leaked, [rebuild the vault](#rebuilding-a-vault) – a copy gets a fresh master key.

## Growing without giving away behavior

Temple can't tell other spaces' zones from noise, so it never reuses existing zones; new room is only appended.

- **Own zones only.** A space writes only into zones it owns; everyday use – even a bookmark added in front of an inspector – touches nothing else. Deleting frees room only inside that space.
- **Big steps.** A space that runs full appends a large share for itself, not just what one download needs, so the size reveals no single download.
- **Noise.** A reserve right after the first space's first block, and random noise with every growth step – none when a space is created. Spaces record only their own zones, never the noise, so in every vault a varying amount of room belongs to no space. Other spaces hide in it.
- **Never shrinks.** The end of the file may belong to another space. Room only comes back by [rebuilding the vault](#rebuilding-a-vault).
- **Real noise.** Random data actually written to disk, never sparse or zero-filled, so the room used on disk matches the file size.
- **Key blocks** sit in a fixed slot table at the start, in four copies at positions derived from the words.

What the size still shows: roughly how much is stored in total. Not what, not when, and not how it splits across spaces – as long as hidden spaces fit in the noise.

## Noise numbers

From a simulation ([noise_sim.py](../tools/noise_sim.py)). The adversary opens the everyday space, knows the noise rules from the open code, and checks every stretch of the file the everyday space doesn't explain. Measure: how much the file raises the odds that a hidden space exists (likelihood ratio, LR; 1 means not at all).

- **Exponential amounts:** the only shape whose LR doesn't depend on the amount drawn. An even spread proves hidden spaces at its upper edge (12 % of them at 64 MB, 48 % at 256 MB); many small pieces add up to a peaked sum that gives them away faster (LR 2.5–4.6 at 256 MB instead of 1.9).
- **Reserve:** mean 384 MB, cut off at 2 GB, appended once right after the first space's first block. Half of all vaults carry less than 256 MB, 1 in 10 more than 880 MB.
- **Growth noise:** mean 64 MB with every growth step, none when a space is created – a hidden space's own creation noise would double its footprint.

A hidden space shares the reserve's stretch if it is set up after the everyday space and before that space first grows, in one step with everything it will hold:

| Hidden space | LR |
| --- | --- |
| 16 MB (texts, notes) | 1.04 |
| 64 MB | 1.18 |
| 160 MB | 1.5 |
| 270 MB (audio New Testament at ~24 kbps) | 2 |

- **Cut-off:** makes 0.5 % of 256 MB hidden spaces provable.
- **After the everyday space has grown,** a new hidden space – or a hidden space's own growth – has only growth noise around it: fine for texts (LR 1.25), not for audio (LR 9; 36 if the adversary knows when it was set up).
- **Set up before the everyday space,** a hidden space pushes the reserve behind its own block, which shows that another space came first.

## Keep only this space

Destroying helps only before someone copies the vault – afterwards the copy keeps everything. Spaces can't be linked, so nothing may point at one particular space: a stored pointer would show anyone with an earlier copy that it exists. Instead, from inside an open space, Temple rewrites the whole slot table: the open space's key blocks at their slots, fresh random data in every other slot. All other spaces are gone at once, without Temple ever knowing them; someone comparing two copies sees that it happened.

## Vault properties

- **One file,** like a KeePassXC database. Where it lives is the app's choice.
- **New device:** copy the vault over, then enter the words.
- **Sync** suits everyday spaces and gives backups for free. Cloud services keep old versions, and comparing them shows where a vault changed, so hidden spaces belong in a vault that doesn't sync. Many phone cloud apps upload the whole vault after a change – fine on Wi-Fi, heavy on mobile data.
- **One device at a time:** the opening marker in zone 1 tells Temple when another device has opened a space of the vault; it then stops writing ([format.md](format.md#file)). On iOS and Android the vault's file is locked while open, so a second process – an app's extension – is refused.
- **Copies taken mid-write** open to a state the space had during a session, and to the newest state while a space locks or its log turns over ([storage spike](storage-spike.md), [Temple's tests](../README.md#what-the-tests-show)) – with one exception under [limits](#limits).
- **Outside the app,** other apps and cloud providers see a vault as random data; its name and place are the user's choice.

## What a vault reveals

| Someone has… | What they learn about hidden spaces |
| --- | --- |
| The vault, no words | Nothing: random bytes, no count of spaces |
| One copy and the everyday words, rules kept | Odds raised 1.04× for 16 MB of texts, 2× for 270 MB; never proof, except 0.5 % at the reserve's cut-off |
| The same, a hidden space added after the everyday space grew | Texts 1.25×; audio 9× to 36× |
| The same, the hidden space set up first | That another space came first |
| Copies from different times | That zones no opened space explains have changed |
| Power and no need for proof | Deniability can't help here – no format can |

## Rebuilding a vault

A fresh vault, filled one opened space at a time: the everyday space first, then hidden ones, each copied in one go. The old vault stays until it is deleted.

- **Room comes back:** deleted content and old growth steps stay behind.
- **Hidden spaces land well again:** right after the everyday space, in the fresh reserve, filled in one go – even if they were set up too late in the old vault.
- **Moving:** the same steps move a vault elsewhere.
- **Spaces not opened stay behind** in the old vault.

## Rejected

- **Fixed size, and entering all other spaces' words before creating one:** an extra step that names multiple spaces, a forgotten space gets overwritten, and no room for large content such as audio.
- **VeraCrypt-compatible vaults** (e.g. VaultPony's Rust core): one hidden space per vault and a fixed size; daily use of the everyday space lets its file system overwrite the hidden one; FAT and exFAT overwrite in place and have no journal. The core isn't audited, so it adds no trust over our own code.
- **Hidden spaces inside a fixed reserve** (Shufflecake's chain: each hidden space holds the key of the one below): no size or layout evidence and no timing rules, but every vault carries the full reserve and hidden spaces can't outgrow it. Considered 2026-10-27; appended hidden spaces with noise stay.

## Limits

- **Repeated copies:** whoever copies a vault more than once – a device at every border crossing, or a cloud service keeping old versions – sees zones change that the opened space can't explain.
- **Old versions:** file systems and flash memory keep overwritten data in free space for a while, so a thorough forensic image can act like a second copy.
- **Large hidden spaces:** hiding costs storage. Keeping the LR at 2 for a hidden space of h MB takes noise averaging 1.4 h in every vault, and 1 vault in 10 then carries over 3 h. A full audio Bible (about 1 GB) can't be hidden at a bearable cost.
- **Destroyed spaces** keep their footprint: "keep only this space" removes the content, not the room it took.
- **File times** show when the app last used a vault – the opening marker is rewritten at every opening and every lock – but not which space wrote. The phone's own app-usage logs are the app's problem.
- **Deleted values while a space is open** stay readable in its log until the log turns over – after 4 MiB of commits at most – or the space locks. A lock that fails, as when another device took over, keeps the log and its key for the next opening to replay.
- **Full-text search** forgets deleted text only with FTS5, whose `secure-delete` option Temple turns on as a space locks; FTS3 and FTS4 keep deleted terms in their index.
- **Copies in random order while a space locks:** a copy that holds both superblock copies as the lock rewrote them, but a database page from before the checkpoint wrote it, doesn't open – the log's key is gone from both copies, and the checkpoint's record shows the page missing. 5 of 200 such copies in Temple's test; read front to back, every copy opens. Copies taken while the log turns over behave alike.
- **Copies across commits and a lock** – or a turnover of the log: read in random order, a copy can hold an early superblock and log with pages the checkpoint wrote later. It opens, but the pages that don't match the tag table fail when read – 5 of 1,000 such copies in Temple's longest test. Across more than one turnover, the old log's blocks may hold a newer log, and most such copies don't open – 500 of 1,000 there, with the log turning over every few commits rather than every 4 MiB. Never does a copy open to a state the space never had.
- **A space's database** holds at most 1 GiB; large content – audio, documents – belongs in objects. Its temporary data – sorting, VACUUM – stays in memory: as much RAM as the database takes.
- **On the web**, a vault is a file of the origin private file system and Temple runs in a worker. Keys and everything decrypted sit in wasm memory, which any script of the page can read – a cross-site script or an extension reads an open space. A vault opens in one tab at a time. Browsers may evict an origin's files unless it asked to persist them, the reserve of up to 2 GB counts against its quota, and how durable a flush is depends on the browser.
- **Rolled-back blocks:** whoever can write to a vault can put back an older version of a block. Database pages are caught by the tag table; a block of an object passes its check. That tampers with content but reveals nothing; catching it for objects too would take a hash tree, which v1 leaves out. A whole older copy put back – blocks and superblocks alike – can't be told from the current one.
- **What leaves Temple:** query results, objects read, the phrase key and words cross into the app – on React Native and the web through the bindings' buffers and the JavaScript heap, which nothing wipes.
