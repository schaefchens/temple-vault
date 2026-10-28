//! A notes-like workload: puts, edits and deletes, with a marker in every value
//! so leftovers of deleted or overwritten values can be found later.

use crate::blocks::Rng;
use std::collections::BTreeMap;

pub type State = BTreeMap<u32, String>;

#[derive(Clone, Debug)]
pub enum Op {
    Put(u32, String),
    Del(u32),
}

pub trait Engine {
    /// One transaction.
    fn commit(&mut self, ops: &[Op]) -> Result<(), String>;
    fn read_all(&mut self) -> Result<State, String>;
    /// The engine's own integrity check.
    fn check(&mut self) -> Result<bool, String>;
}

pub fn marker(key: u32, version: u32) -> String {
    format!("<<k{key}v{version}>>")
}

pub struct Workload {
    pub commits: Vec<Vec<Op>>,
    /// The expected state after each commit.
    pub states: Vec<State>,
    /// Markers of values that are deleted or overwritten by the end.
    pub gone: Vec<String>,
}

pub fn build(commits: usize, seed: u64) -> Workload {
    let mut rng = Rng(seed);
    let mut state = State::new();
    let mut versions: BTreeMap<u32, u32> = BTreeMap::new();
    let mut all = Vec::new();
    let mut out = Workload { commits: Vec::new(), states: Vec::new(), gone: Vec::new() };
    let mut next_key = 0u32;
    for _ in 0..commits {
        let mut ops = Vec::new();
        for _ in 0..5 + rng.below(10) {
            let roll = rng.below(100);
            if roll < 25 && !state.is_empty() {
                let key = *state.keys().nth(rng.below(state.len() as u64) as usize).unwrap();
                state.remove(&key);
                ops.push(Op::Del(key));
            } else {
                let key = if roll < 55 && !state.is_empty() {
                    *state.keys().nth(rng.below(state.len() as u64) as usize).unwrap()
                } else {
                    next_key += 1;
                    next_key
                };
                let version = versions.entry(key).and_modify(|v| *v += 1).or_insert(1);
                let mark = marker(key, *version);
                let filler = "lorem ipsum dolor sit amet ".repeat(4 + rng.below(110) as usize);
                let value = format!("{mark} {filler}{mark}");
                all.push(mark);
                state.insert(key, value.clone());
                ops.push(Op::Put(key, value));
            }
        }
        out.commits.push(ops);
        out.states.push(state.clone());
    }
    // Only values that were committed at some point can leave traces.
    let first_word = |v: &String| v.split(' ').next().unwrap().to_string();
    let committed: std::collections::HashSet<String> =
        out.states.iter().flat_map(|s| s.values().map(first_word)).collect();
    let alive: std::collections::HashSet<String> = state.values().map(first_word).collect();
    out.gone = all.into_iter().filter(|m| committed.contains(m) && !alive.contains(m)).collect();
    out
}
