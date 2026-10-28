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

    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn add(&mut self, run: Run) {
        if run.len == 0 {
            return;
        }
        let (mut start, mut len) = (run.start, run.len);
        if let Some((&before, &before_len)) = self.runs.range(..start).next_back() {
            debug_assert!(u64::from(before) + u64::from(before_len) <= u64::from(start), "blocks freed twice");
            if u64::from(before) + u64::from(before_len) == u64::from(start) {
                self.runs.remove(&before);
                start = before;
                len += before_len;
            }
        }
        if let Ok(after) = u32::try_from(run.end())
            && let Some(after_len) = self.runs.remove(&after)
        {
            len += after_len;
        }
        self.runs.insert(start, len);
        self.total += u64::from(run.len);
    }

    /// Takes `n` blocks, at most [`Free::total`]: from the first run long enough, or else
    /// from the longest runs.
    pub(crate) fn take(&mut self, n: u32) -> Vec<Run> {
        assert!(u64::from(n) <= self.total, "taking more blocks than are free");
        let mut taken = Vec::new();
        if n == 0 {
            return taken;
        }
        let first_fit = self.runs.iter().find(|&(_, &len)| len >= n).map(|(&start, _)| start);
        let starts = match first_fit {
            Some(start) => vec![start],
            None => {
                let mut longest: Vec<(u32, u32)> = self.runs.iter().map(|(&start, &len)| (start, len)).collect();
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
        assert_eq!(free.take(4), vec![run(10, 4)]);
        assert_eq!(free.take(11), vec![run(14, 11)]);
        assert_eq!(free.total(), 0);
    }

    #[test]
    fn taking_more_than_any_run_holds_uses_the_longest() {
        let mut free = Free::default();
        free.add(run(0, 2));
        free.add(run(10, 5));
        free.add(run(20, 3));
        assert_eq!(free.take(7), vec![run(10, 5), run(20, 2)]);
        assert_eq!(free.total(), 3);
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
