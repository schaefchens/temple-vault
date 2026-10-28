//! Runs of blocks in a space's block space, and the free ones among them.

use std::cmp::Reverse;
use std::collections::BTreeMap;

/// `len` blocks from `start` on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

impl Run {
    pub(crate) fn end(self) -> u64 {
        u64::from(self.start) + u64::from(self.len)
    }
}

/// The `n`-th block of something laid out in `runs`, in order.
pub(crate) fn nth(runs: &[Run], mut n: u64) -> Option<u32> {
    for run in runs {
        if n < u64::from(run.len) {
            return Some(run.start + n as u32);
        }
        n -= u64::from(run.len);
    }
    None
}

pub(crate) fn count(runs: &[Run]) -> u64 {
    runs.iter().map(|run| u64::from(run.len)).sum()
}

/// Appends `run`, merged into the last one where they meet.
pub(crate) fn push(runs: &mut Vec<Run>, run: Run) {
    match runs.last_mut() {
        Some(last) if last.end() == u64::from(run.start) => last.len += run.len,
        _ => runs.push(run),
    }
}

/// Keeps the first `keep` blocks of `runs`; returns the rest.
pub(crate) fn split_off(runs: &mut Vec<Run>, keep: u64) -> Vec<Run> {
    let mut left = keep;
    let mut kept = Vec::new();
    let mut rest = Vec::new();
    for run in runs.drain(..) {
        let len = u64::from(run.len);
        if left >= len {
            kept.push(run);
            left -= len;
        } else if left > 0 {
            let head = left as u32;
            kept.push(Run { start: run.start, len: head });
            rest.push(Run { start: run.start + head, len: run.len - head });
            left = 0;
        } else {
            rest.push(run);
        }
    }
    *runs = kept;
    rest
}

pub(crate) fn encode(runs: &[Run]) -> Vec<u8> {
    runs.iter().flat_map(|run| [run.start.to_le_bytes(), run.len.to_le_bytes()]).flatten().collect()
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Vec<Run>> {
    if !bytes.len().is_multiple_of(8) {
        return None;
    }
    let runs = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| Run {
            start: u32::from_le_bytes(c[..4].try_into().unwrap()),
            len: u32::from_le_bytes(c[4..].try_into().unwrap()),
        })
        .collect::<Vec<_>>();
    runs.iter().all(|run| run.end() <= u64::from(u32::MAX)).then_some(runs)
}

/// The free blocks of a space, as runs keyed by their start; neighbours merge.
#[derive(Debug, Default)]
pub(crate) struct Free {
    runs: BTreeMap<u32, u32>,
    total: u64,
}

impl Free {
    /// The blocks of `first..end` that none of `used` covers; `None` if used runs overlap
    /// or lie outside.
    pub(crate) fn complement(first: u32, end: u32, mut used: Vec<Run>) -> Option<Free> {
        if first > end {
            return None;
        }
        used.retain(|run| run.len > 0);
        used.sort_by_key(|run| run.start);
        let mut free = Free::default();
        let mut at = first;
        for run in used {
            if run.start < at || run.end() > u64::from(end) {
                return None;
            }
            free.add(Run { start: at, len: run.start - at });
            at = run.end() as u32;
        }
        free.add(Run { start: at, len: end - at });
        Some(free)
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn add(&mut self, run: Run) {
        if run.len == 0 {
            return;
        }
        let before = self.runs.range(..run.start).next_back().map(|(&start, &len)| Run { start, len });
        let after = self.runs.range(run.start..).next().map(|(&start, _)| u64::from(start));
        // Blocks freed twice would be handed out twice, and one file would overwrite another.
        assert!(
            before.is_none_or(|before| before.end() <= u64::from(run.start))
                && after.is_none_or(|after| after >= run.end()),
            "blocks freed twice"
        );
        let (mut start, mut len) = (run.start, run.len);
        if let Some(before) = before
            && before.end() == u64::from(start)
        {
            self.runs.remove(&before.start);
            start = before.start;
            len += before.len;
        }
        if after == Some(run.end()) {
            len += self.runs.remove(&(run.end() as u32)).expect("the run after");
        }
        self.runs.insert(start, len);
        self.total += u64::from(run.len);
    }

    /// Takes the free blocks among `run` out of use; the others stay as they are.
    pub(crate) fn remove(&mut self, run: Run) {
        let (start, end) = (u64::from(run.start), run.end());
        let before = self.runs.range(..run.start).next_back().map(|(&start, &len)| Run { start, len });
        let within: Vec<Run> = self
            .runs
            .range(run.start..)
            .take_while(|&(&free, _)| u64::from(free) < end)
            .map(|(&start, &len)| Run { start, len })
            .collect();
        for free in before.into_iter().chain(within).filter(|free| free.end() > start && u64::from(free.start) < end) {
            self.runs.remove(&free.start);
            self.total -= u64::from(free.len);
            if u64::from(free.start) < start {
                self.insert(free.start, start);
            }
            if free.end() > end {
                self.insert(end as u32, free.end());
            }
        }
    }

    fn insert(&mut self, start: u32, end: u64) {
        let len = (end - u64::from(start)) as u32;
        self.runs.insert(start, len);
        self.total += u64::from(len);
    }

    /// Free blocks in runs of at least `piece` blocks.
    pub(crate) fn in_runs_of(&self, piece: u32) -> u64 {
        if piece <= 1 {
            return self.total;
        }
        self.runs.values().filter(|&&len| len >= piece).map(|&len| u64::from(len)).sum()
    }

    /// The most blocks [`Free::take`] can give in pieces of at least `piece`: the longest run,
    /// or all runs that long together.
    pub(crate) fn room(&self, piece: u32) -> u64 {
        let longest = self.runs.values().max().map_or(0, |&len| u64::from(len));
        longest.max(self.in_runs_of(piece))
    }

    /// Takes `n` blocks, at most [`Free::room`]: from the first run long enough, or else from
    /// the runs of at least `piece` blocks, the longest first.
    pub(crate) fn take(&mut self, n: u32, piece: u32) -> Vec<Run> {
        assert!(u64::from(n) <= self.room(piece), "taking more blocks than are free");
        let mut taken = Vec::new();
        if n == 0 {
            return taken;
        }
        let first_fit = self.runs.iter().find(|&(_, &len)| len >= n).map(|(&start, _)| start);
        let starts = match first_fit {
            Some(start) => vec![start],
            None => {
                let mut longest: Vec<(u32, u32)> =
                    self.runs.iter().filter(|&(_, &len)| len >= piece).map(|(&start, &len)| (start, len)).collect();
                longest.sort_by_key(|&(start, len)| (Reverse(len), start));
                longest.into_iter().map(|(start, _)| start).collect()
            }
        };
        let mut left = n;
        for start in starts {
            if left == 0 {
                break;
            }
            let len = self.runs.remove(&start).unwrap();
            let part = len.min(left);
            if len > part {
                self.runs.insert(start + part, len - part);
            }
            taken.push(Run { start, len: part });
            left -= part;
        }
        self.total -= u64::from(n);
        taken.sort_by_key(|run| run.start);
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    fn run(start: u32, len: u32) -> Run {
        Run { start, len }
    }

    #[test]
    fn free_runs_merge_and_split() {
        let mut free = Free::default();
        free.add(run(10, 5));
        free.add(run(20, 5));
        free.add(run(15, 5));
        assert_eq!(free.runs.len(), 1);
        assert_eq!(free.total(), 15);
        assert_eq!(free.take(4, 1), vec![run(10, 4)]);
        assert_eq!(free.take(11, 1), vec![run(14, 11)]);
        assert_eq!(free.total(), 0);
    }

    #[test]
    fn taking_more_than_any_run_holds_uses_the_longest() {
        let mut free = Free::default();
        free.add(run(0, 2));
        free.add(run(10, 5));
        free.add(run(20, 3));
        assert_eq!(free.take(7, 1), vec![run(10, 5), run(20, 2)]);
        assert_eq!(free.total(), 3);
    }

    /// Files the superblock records take room in runs of a minimum length, so that scattered
    /// free blocks can't make their records long.
    #[test]
    fn pieces_shorter_than_asked_are_left() {
        let mut free = Free::default();
        for start in (0..40).step_by(4) {
            free.add(run(start, 2));
        }
        free.add(run(100, 8));
        free.add(run(200, 6));
        assert_eq!((free.total(), free.in_runs_of(6), free.room(6)), (34, 14, 14));
        assert_eq!(free.room(9), 8);
        // One run long enough wins; else only runs of 6 or more.
        assert_eq!(free.take(5, 6), vec![run(100, 5)]);
        assert_eq!(free.room(6), 6);
        assert_eq!(free.take(6, 6), vec![run(200, 6)]);
        assert_eq!(free.room(6), 3);
    }

    #[test]
    fn removed_blocks_leave_the_rest_free() {
        let mut free = Free::complement(0, 100, vec![run(40, 10)]).unwrap();
        // Across a run's end and the next one's start, inside a run, and outside any.
        free.remove(run(35, 20));
        free.remove(run(60, 5));
        free.remove(run(200, 5));
        let runs: Vec<Run> = free.runs.iter().map(|(&s, &l)| run(s, l)).collect();
        assert_eq!(runs, vec![run(0, 35), run(55, 5), run(65, 35)]);
        assert_eq!(free.total(), 75);
    }

    #[test]
    #[should_panic(expected = "blocks freed twice")]
    fn blocks_freed_twice_are_refused() {
        let mut free = Free::default();
        free.add(run(12, 3));
        // Not caught before: a free run starting inside the one added.
        free.add(run(10, 5));
    }

    #[test]
    fn complement_finds_the_gaps_and_rejects_overlaps() {
        let free = Free::complement(32, 100, vec![run(40, 10), run(32, 4), run(90, 10)]).unwrap();
        assert_eq!(free.runs.iter().map(|(&s, &l)| run(s, l)).collect::<Vec<_>>(), vec![run(36, 4), run(50, 40)]);
        assert!(Free::complement(32, 100, vec![run(40, 10), run(45, 10)]).is_none());
        assert!(Free::complement(32, 100, vec![run(95, 10)]).is_none());
        assert!(Free::complement(32, 100, vec![run(20, 4)]).is_none());
    }

    #[test]
    fn split_off_cuts_inside_a_run() {
        let mut runs = vec![run(0, 4), run(10, 4)];
        assert_eq!(split_off(&mut runs, 6), vec![run(12, 2)]);
        assert_eq!(runs, vec![run(0, 4), run(10, 2)]);
        assert_eq!(nth(&runs, 5), Some(11));
        assert_eq!(nth(&runs, 6), None);
    }

    #[test]
    fn runs_round_trip_through_bytes() {
        let runs = vec![run(1, 2), run(300, 70000)];
        assert_eq!(decode(&encode(&runs)), Some(runs));
        assert_eq!(decode(&[1, 2, 3]), None);
        assert_eq!(decode(&encode(&[run(u32::MAX, 1)])), None);
    }
}
