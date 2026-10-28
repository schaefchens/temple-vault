//! A space's superblock, inner format v1. Two copies, 64 KiB apart, sit at the start of the
//! space's first zone. A copy takes as many 4 KiB blocks as its record needs – nearly always
//! one, which a copy taken mid-write can't tear – sealed as one piece: payload ‖ nonce ‖ tag
//! under the superblock key, with its position and its number of blocks as associated data.
//!
//! The payload, little-endian:
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 2 | Inner format version |
//! | 8 | Generation |
//! | 1; 32 | Session key: present (1) or not (0); the key, or zeros – always in the first block, which every rewrite of the copy replaces |
//! | 4, then 16 each | The space's zones, in runs: first zone, number of zones |
//! | 8; 4, then 8 each | Database: size in bytes; its blocks, in runs: first block, number (u32) |
//! | 8; 4, then 8 each | Log: the same |
//! | 8; 4, then 8 each; 32 | Tag table – the tag of each database page's block, page by page: the same, and SHA-256 over its content |
//! | 1, … | Pages the last lock's checkpoint wrote: 0, then runs as above; or 1, then n (u32) and a bitmap of n bits, page i at bit i % 8 of byte i / 8 – whichever is shorter |
//! | the rest | Zeros |

use super::runs::Run;
use crate::BLOCK;
use crate::crypto::{Key, NONCE, TAG};
use zeroize::Zeroizing;

/// The room of one copy: 16 blocks.
pub(crate) const SIZE: usize = 64 * 1024;
pub(crate) const MAX_BLOCKS: usize = SIZE / BLOCK;
/// The database holds at most 1 GiB, so that a checkpoint's record fits a copy and the tag
/// table stays small. Large content belongs in objects.
pub(crate) const MAX_PAGES: u32 = 1 << 18;

/// Zones a space owns, one after another in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extent {
    pub(crate) first_zone: u64,
    pub(crate) zones: u64,
}

/// A file inside the space: its size, and its blocks in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FileRecord {
    pub(crate) size: u64,
    pub(crate) runs: Vec<Run>,
}

pub(crate) struct Superblock {
    pub(crate) inner_version: u16,
    pub(crate) generation: u64,
    pub(crate) extents: Vec<Extent>,
    pub(crate) database: FileRecord,
    pub(crate) log: FileRecord,
    /// The tag of each database page's block, page by page: a page that doesn't match isn't
    /// the one this record knows.
    pub(crate) tags: FileRecord,
    pub(crate) tags_hash: [u8; 32],
    pub(crate) session: Option<Key>,
    /// The pages the last lock's checkpoint wrote, ascending: a copy of the vault taken while
    /// the space locked shows here whether it holds all of them.
    pub(crate) checkpoint: Vec<u32>,
}

/// How many blocks a sealed payload of `len` bytes takes.
pub(crate) fn blocks_for(len: usize) -> usize {
    (len + NONCE + TAG).div_ceil(BLOCK)
}

impl Superblock {
    /// The payload, zero-padded to fill its blocks; `None` if it needs more than 16.
    pub(crate) fn encode(&self) -> Option<Zeroizing<Vec<u8>>> {
        let mut payload = Zeroizing::new(vec![0u8; SIZE - NONCE - TAG]);
        let mut w = Writer { buf: &mut payload, at: 0 };
        w.put(&self.inner_version.to_le_bytes())?;
        w.put(&self.generation.to_le_bytes())?;
        match &self.session {
            Some(key) => {
                w.put(&[1])?;
                w.put(key.as_bytes())?;
            }
            None => w.put(&[0; 33])?,
        }
        w.put(&u32::try_from(self.extents.len()).ok()?.to_le_bytes())?;
        for extent in &self.extents {
            w.put(&extent.first_zone.to_le_bytes())?;
            w.put(&extent.zones.to_le_bytes())?;
        }
        for file in [&self.database, &self.log, &self.tags] {
            w.put(&file.size.to_le_bytes())?;
            w.runs(&file.runs)?;
        }
        w.put(&self.tags_hash)?;
        let pages = &self.checkpoint;
        let runs = page_runs(pages);
        let bits = pages.last().map_or(0, |&last| last + 1);
        if 4 + runs.len() * 8 <= 4 + (bits as usize).div_ceil(8) {
            w.put(&[0])?;
            w.runs(&runs)?;
        } else {
            w.put(&[1])?;
            w.put(&bits.to_le_bytes())?;
            let mut bitmap = vec![0u8; (bits as usize).div_ceil(8)];
            for &page in pages {
                bitmap[page as usize / 8] |= 1 << (page % 8);
            }
            w.put(&bitmap)?;
        }
        let len = blocks_for(w.at) * BLOCK - NONCE - TAG;
        payload.truncate(len);
        Some(payload)
    }

    pub(crate) fn decode(payload: &[u8]) -> Option<Superblock> {
        let mut r = Reader { bytes: payload };
        let inner_version = r.u16()?;
        let generation = r.u64()?;
        let session = match r.take(1)?[0] {
            0 => {
                r.take(32)?;
                None
            }
            1 => Some(Key::from_bytes(r.take(32)?.try_into().unwrap())),
            _ => return None,
        };
        let count = r.count(16)?;
        let extents =
            (0..count).map(|_| Some(Extent { first_zone: r.u64()?, zones: r.u64()? })).collect::<Option<_>>()?;
        let mut files = [FileRecord::default(), FileRecord::default(), FileRecord::default()];
        for file in &mut files {
            file.size = r.u64()?;
            file.runs = r.runs()?;
        }
        let tags_hash = r.take(32)?.try_into().unwrap();
        let pages: Vec<u32> = match r.take(1)?[0] {
            0 => {
                // Ascending and apart, before they are counted out: then they hold at most
                // MAX_PAGES pages.
                let runs = r.runs()?;
                let mut next = 0;
                for run in &runs {
                    if run.len == 0 || u64::from(run.start) < next || run.end() > u64::from(MAX_PAGES) {
                        return None;
                    }
                    next = run.end();
                }
                runs.iter().flat_map(|run| run.start..run.end() as u32).collect()
            }
            1 => {
                let bits = r.u32()?;
                if bits > MAX_PAGES {
                    return None;
                }
                let bitmap = r.take((bits as usize).div_ceil(8))?;
                (0..bits).filter(|&page| bitmap[page as usize / 8] >> (page % 8) & 1 == 1).collect()
            }
            _ => return None,
        };
        if !pages.is_sorted_by(|a, b| a < b) {
            return None;
        }
        let [database, log, tags] = files;
        Some(Superblock {
            inner_version,
            generation,
            extents,
            database,
            log,
            tags,
            tags_hash,
            session,
            checkpoint: pages,
        })
    }
}

/// Ascending pages as runs.
fn page_runs(pages: &[u32]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for &page in pages {
        match runs.last_mut() {
            Some(last) if last.end() == u64::from(page) => last.len += 1,
            _ => runs.push(Run { start: page, len: 1 }),
        }
    }
    runs
}

struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.at.checked_add(bytes.len()).filter(|&end| end <= self.buf.len())?;
        self.buf[self.at..end].copy_from_slice(bytes);
        self.at = end;
        Some(())
    }

    fn runs(&mut self, runs: &[Run]) -> Option<()> {
        self.put(&u32::try_from(runs.len()).ok()?.to_le_bytes())?;
        for run in runs {
            self.put(&run.start.to_le_bytes())?;
            self.put(&run.len.to_le_bytes())?;
        }
        Some(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.bytes.len() {
            return None;
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Some(head)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A count of items of `size` bytes each, no more than the bytes left could hold.
    fn count(&mut self, size: usize) -> Option<usize> {
        let count = self.u32()? as usize;
        (count <= self.bytes.len() / size).then_some(count)
    }

    fn runs(&mut self) -> Option<Vec<Run>> {
        let count = self.count(8)?;
        let runs: Vec<Run> =
            (0..count).map(|_| Some(Run { start: self.u32()?, len: self.u32()? })).collect::<Option<_>>()?;
        runs.iter().all(|run| run.end() <= u64::from(u32::MAX)).then_some(runs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(checkpoint: Vec<u32>) -> Superblock {
        Superblock {
            inner_version: 1,
            generation: 9,
            extents: vec![Extent { first_zone: 2, zones: 16 }, Extent { first_zone: 400, zones: 32 }],
            database: FileRecord { size: 8192, runs: vec![Run { start: 32, len: 64 }] },
            log: FileRecord { size: 5000, runs: vec![Run { start: 96, len: 2 }, Run { start: 200, len: 7 }] },
            tags: FileRecord { size: 32, runs: vec![Run { start: 210, len: 1 }] },
            tags_hash: [5; 32],
            session: Some(Key::random()),
            checkpoint,
        }
    }

    fn round_trip(original: Superblock) -> usize {
        let payload = original.encode().unwrap();
        let decoded = Superblock::decode(&payload).unwrap();
        assert_eq!(decoded.inner_version, 1);
        assert_eq!(decoded.generation, 9);
        assert_eq!(decoded.extents, original.extents);
        assert_eq!(decoded.database, original.database);
        assert_eq!(decoded.log, original.log);
        assert_eq!(decoded.tags, original.tags);
        assert_eq!(decoded.tags_hash, original.tags_hash);
        assert_eq!(decoded.session.unwrap().as_bytes(), original.session.unwrap().as_bytes());
        assert_eq!(decoded.checkpoint, original.checkpoint);
        blocks_for(payload.len())
    }

    #[test]
    fn small_records_take_one_block() {
        assert_eq!(round_trip(example(vec![0, 1, 2, 3, 17, 4000])), 1);
        assert_eq!(round_trip(example(vec![])), 1);
    }

    #[test]
    fn scattered_pages_take_a_bitmap() {
        let every_other: Vec<u32> = (0..200_000).step_by(2).collect();
        assert_eq!(round_trip(example(every_other)), 7);
    }

    #[test]
    fn a_record_too_large_does_not_fit() {
        let mut large = example(vec![]);
        large.database.runs = (0..9000).map(|i| Run { start: 32 + 2 * i, len: 1 }).collect();
        assert!(large.encode().is_none());
    }

    /// Found by reading the decoder for the fuzz targets: overlapping runs of checkpoint pages
    /// once asked for billions of entries before they were turned down.
    #[test]
    fn overlapping_checkpoint_runs_do_not_decode() {
        let with_runs = |count: u32| {
            let mut payload = example(vec![]).encode().unwrap().to_vec();
            // The checkpoint follows the tag table's hash, which the example fills with 5s.
            let at = payload.windows(32).position(|window| window == [5; 32]).unwrap() + 32;
            payload.truncate(at);
            payload.push(0);
            payload.extend(count.to_le_bytes());
            for _ in 0..count {
                payload.extend(0u32.to_le_bytes());
                payload.extend(MAX_PAGES.to_le_bytes());
            }
            payload
        };
        assert_eq!(Superblock::decode(&with_runs(1)).unwrap().checkpoint.len(), MAX_PAGES as usize);
        let clock = std::time::Instant::now();
        assert!(Superblock::decode(&with_runs(4000)).is_none());
        assert!(clock.elapsed() < std::time::Duration::from_millis(100));
    }

    #[test]
    fn truncated_payloads_do_not_decode() {
        let payload = example(vec![3, 4, 9]).encode().unwrap();
        // Pages 3, 4 and 9 as a bitmap: 10 bits in 2 bytes.
        let used = 2 + 8 + 33 + 4 + 32 + (8 + 4 + 8) + (8 + 4 + 16) + (8 + 4 + 8) + 32 + 1 + 4 + 2;
        assert!(Superblock::decode(&payload[..used]).is_some());
        for cut in 0..used {
            assert!(Superblock::decode(&payload[..cut]).is_none(), "{cut}");
        }
    }
}
