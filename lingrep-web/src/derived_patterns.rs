use std::collections::{HashMap, HashSet};

use lingrep_core::test_scale::ScaleKnob;

use crate::snapshot::PatternKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DerivedMatch {
    pub(crate) components: Vec<PatternKey>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn compound_name(names: &[String], exists: impl Fn(&str) -> bool) -> String {
    let base = format!("複合項目〔{}〕", names.join(" ＋ "));
    if !exists(&base) {
        return base;
    }
    let mut suffix = 2u64;
    loop {
        let candidate = format!("{base}〔{suffix}〕");
        if !exists(&candidate) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

const MAX_SENTENCE_CHAIN_BYTES: usize = 128 * 1024 * 1024;

pub(crate) const MAX_DERIVED_PATTERNS: usize = 4096;

pub(crate) fn max_derived_patterns() -> usize {
    ScaleKnob::CompoundPatterns.or(MAX_DERIVED_PATTERNS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Too many compound matches or patterns. Turn off \"Derive compounds\" (複合項目を生成) or search fewer definitions, then search again."
)]
pub(crate) struct CompoundLimitExceeded;

fn chain_bytes(components: usize) -> usize {
    std::mem::size_of::<DerivedMatch>()
        + std::mem::size_of::<(Vec<PatternKey>, usize, usize)>()
        + 2 * components * std::mem::size_of::<PatternKey>()
}

pub(crate) struct ContiguousChains {
    pub(crate) chains: Vec<DerivedMatch>,
    /// Per input match, whether it is a component of any chain.
    pub(crate) in_chain: Vec<bool>,
}

/// Enumerates every contiguous path through the base matches in one sentence.
/// A derived item is identified by its ordered component keys and full span.
///
/// Fails once the enumeration would hold more than its byte budget.
pub(crate) fn contiguous_chains(
    matches: &[(PatternKey, usize, usize)],
) -> Result<ContiguousChains, CompoundLimitExceeded> {
    let mut seen = HashSet::new();
    let chains = enumerate_chains(matches, &mut seen);
    #[cfg(any(test, feature = "testing"))]
    held_chains::record(seen.len());
    chains
}

fn enumerate_chains(
    matches: &[(PatternKey, usize, usize)],
    seen: &mut HashSet<(Vec<PatternKey>, usize, usize)>,
) -> Result<ContiguousChains, CompoundLimitExceeded> {
    let mut output = ContiguousChains {
        chains: Vec::new(),
        in_chain: vec![false; matches.len()],
    };
    let mut budget = ScaleKnob::CompoundChainBytes.or(MAX_SENTENCE_CHAIN_BYTES);
    let mut starts = HashMap::<usize, Vec<usize>>::new();
    for (index, &(_, start, _)) in matches.iter().enumerate() {
        starts.entry(start).or_default().push(index);
    }
    for (index, &(key, start, end)) in matches.iter().enumerate() {
        let mut path = vec![(index, key)];
        extend(
            matches,
            &starts,
            start,
            end,
            &mut path,
            seen,
            &mut budget,
            &mut output,
        )?;
    }
    Ok(output)
}

/// The most distinct chains `contiguous_chains` has held for one sentence.
#[cfg(any(test, feature = "testing"))]
pub mod held_chains {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static PEAK: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn record(chains: usize) {
        PEAK.fetch_max(chains, Ordering::Relaxed);
    }

    /// Returns the peak since the last call and resets it.
    pub fn take_peak() -> usize {
        PEAK.swap(0, Ordering::Relaxed)
    }
}

fn extend(
    matches: &[(PatternKey, usize, usize)],
    starts: &HashMap<usize, Vec<usize>>,
    start: usize,
    end: usize,
    path: &mut Vec<(usize, PatternKey)>,
    seen: &mut HashSet<(Vec<PatternKey>, usize, usize)>,
    budget: &mut usize,
    output: &mut ContiguousChains,
) -> Result<(), CompoundLimitExceeded> {
    if path.len() >= 2 {
        *budget = budget
            .checked_sub(chain_bytes(path.len()))
            .ok_or(CompoundLimitExceeded)?;
        let components = path.iter().map(|(_, key)| *key).collect::<Vec<_>>();
        if seen.insert((components.clone(), start, end)) {
            for (index, _) in path.iter() {
                output.in_chain[*index] = true;
            }
            output.chains.push(DerivedMatch {
                components,
                start,
                end,
            });
        }
    }
    let last_index = path.last().map(|(index, _)| *index);
    for &index in starts.get(&end).into_iter().flatten() {
        let (key, _, next_end) = matches[index];
        if Some(index) == last_index || next_end <= end {
            continue;
        }
        path.push((index, key));
        extend(matches, starts, start, next_end, path, seen, budget, output)?;
        path.pop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(index: usize) -> PatternKey {
        PatternKey::new(index).unwrap()
    }

    #[test]
    fn enumerates_every_contiguous_chain_without_crossing_gaps() {
        let derived = contiguous_chains(&[
            (key(0), 0, 2),
            (key(1), 2, 3),
            (key(2), 3, 5),
            (key(3), 6, 7),
        ])
        .unwrap();
        assert_eq!(
            derived.in_chain,
            [true, true, true, false],
            "the match past the gap is in no chain"
        );
        let sequences = derived
            .chains
            .iter()
            .map(|chain| {
                (
                    chain
                        .components
                        .iter()
                        .map(|key| key.index())
                        .collect::<Vec<_>>(),
                    chain.start,
                    chain.end,
                )
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            sequences,
            HashSet::from([
                (vec![0, 1], 0, 3),
                (vec![0, 1, 2], 0, 5),
                (vec![1, 2], 2, 5),
            ])
        );
    }

    #[test]
    fn overlapping_alternatives_produce_distinct_component_sequences() {
        let chains = contiguous_chains(&[(key(0), 0, 1), (key(1), 0, 1), (key(2), 1, 2)])
            .unwrap()
            .chains;
        assert_eq!(chains.len(), 2);
        assert!(chains.iter().any(|chain| {
            chain.components == [key(0), key(2)] && chain.start == 0 && chain.end == 2
        }));
        assert!(chains.iter().any(|chain| {
            chain.components == [key(1), key(2)] && chain.start == 0 && chain.end == 2
        }));
    }
}
