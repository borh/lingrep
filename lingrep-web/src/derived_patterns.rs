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

/// Enumerates every contiguous path through the base matches in one sentence.
/// A derived item is identified by its ordered component keys and full span.
pub(crate) fn contiguous_chains(matches: &[(PatternKey, usize, usize)]) -> Vec<DerivedMatch> {
    let mut chains = Vec::new();
    let mut seen = HashSet::new();
    let mut starts = HashMap::<usize, Vec<usize>>::new();
    for (index, &(_, start, _)) in matches.iter().enumerate() {
        starts.entry(start).or_default().push(index);
    }
    for (index, &(key, start, end)) in matches.iter().enumerate() {
        let mut path = vec![key];
        extend(
            matches,
            &starts,
            index,
            start,
            end,
            &mut path,
            &mut seen,
            &mut chains,
        );
    }
    chains
}

fn extend(
    matches: &[(PatternKey, usize, usize)],
    starts: &HashMap<usize, Vec<usize>>,
    last_index: usize,
    start: usize,
    end: usize,
    path: &mut Vec<PatternKey>,
    seen: &mut HashSet<(Vec<PatternKey>, usize, usize)>,
    output: &mut Vec<DerivedMatch>,
) {
    if path.len() >= 2 && seen.insert((path.clone(), start, end)) {
        output.push(DerivedMatch {
            components: path.clone(),
            start,
            end,
        });
    }
    for &index in starts.get(&end).into_iter().flatten() {
        let (key, _, next_end) = matches[index];
        if index == last_index || next_end <= end {
            continue;
        }
        path.push(key);
        extend(matches, starts, index, start, next_end, path, seen, output);
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
        let chains = contiguous_chains(&[
            (key(0), 0, 2),
            (key(1), 2, 3),
            (key(2), 3, 5),
            (key(3), 6, 7),
        ]);
        let sequences = chains
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
        let chains = contiguous_chains(&[(key(0), 0, 1), (key(1), 0, 1), (key(2), 1, 2)]);
        assert_eq!(chains.len(), 2);
        assert!(chains.iter().any(|chain| {
            chain.components == [key(0), key(2)] && chain.start == 0 && chain.end == 2
        }));
        assert!(chains.iter().any(|chain| {
            chain.components == [key(1), key(2)] && chain.start == 0 && chain.end == 2
        }));
    }
}
