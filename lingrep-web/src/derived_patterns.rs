use std::collections::{HashMap, HashSet};

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

pub(crate) struct ContiguousChains {
    pub(crate) chains: Vec<DerivedMatch>,
    /// Per input match, whether it is a component of any chain.
    pub(crate) in_chain: Vec<bool>,
}

/// Enumerates every contiguous path through the base matches in one sentence.
/// A derived item is identified by its ordered component keys and full span.
pub(crate) fn contiguous_chains(matches: &[(PatternKey, usize, usize)]) -> ContiguousChains {
    let mut output = ContiguousChains {
        chains: Vec::new(),
        in_chain: vec![false; matches.len()],
    };
    let mut seen = HashSet::new();
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
            &mut seen,
            &mut output,
        );
    }
    output
}

fn extend(
    matches: &[(PatternKey, usize, usize)],
    starts: &HashMap<usize, Vec<usize>>,
    start: usize,
    end: usize,
    path: &mut Vec<(usize, PatternKey)>,
    seen: &mut HashSet<(Vec<PatternKey>, usize, usize)>,
    output: &mut ContiguousChains,
) {
    if path.len() >= 2 {
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
        extend(matches, starts, start, next_end, path, seen, output);
        path.pop();
    }
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
        ]);
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
        let chains = contiguous_chains(&[(key(0), 0, 1), (key(1), 0, 1), (key(2), 1, 2)]).chains;
        assert_eq!(chains.len(), 2);
        assert!(chains.iter().any(|chain| {
            chain.components == [key(0), key(2)] && chain.start == 0 && chain.end == 2
        }));
        assert!(chains.iter().any(|chain| {
            chain.components == [key(1), key(2)] && chain.start == 0 && chain.end == 2
        }));
    }
}
