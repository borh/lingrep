//! Match emission: construction, dedup, and the terminal overlap filter.
//!
//! State-advancement code (`nfa_matcher.rs`) calls `MatchSink::emit` and never
//! touches the dedup set, the context-window arithmetic, or the
//! longest-match-per-start filter. This module is the single place those
//! concerns live.

use super::pool::FastBuild;
use crate::fsm::Match;
use compact_str::CompactString;
use std::collections::{HashMap, HashSet};

type MatchKey = (CompactString, usize, usize);

/// Constructs and deduplicates matches before applying the overlap filter.
pub(crate) struct MatchSink {
    dedup: HashSet<MatchKey, FastBuild>,
    out: Vec<Match>,
}

impl MatchSink {
    pub(crate) fn new() -> Self {
        Self {
            dedup: HashSet::default(),
            out: Vec::with_capacity(32),
        }
    }

    /// Inserts the first match for each `(pattern, start, end)` tuple.
    /// Captures are populated by a later pass; callers validate the span.
    #[inline]
    pub(crate) fn emit(&mut self, pattern: &CompactString, start: usize, end: usize) {
        let m = Match {
            pattern: pattern.clone(),
            start,
            end,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        };
        if self.dedup.insert((pattern.clone(), start, end)) {
            self.out.push(m);
        }
    }

    pub(crate) fn finish(self) -> Vec<Match> {
        filter_and_collect_matches(&self.out)
    }
}

/// For each pattern, discard zero-width matches and matches contained by a
/// longer match. Overlaps between different patterns remain independent.
pub(crate) fn filter_and_collect_matches(matches: &[Match]) -> Vec<Match> {
    if matches.is_empty() {
        return Vec::new();
    }

    #[cfg(test)]
    log::trace!("filter_and_collect_matches: input matches: {matches:?}");

    let mut pattern_matches: HashMap<&CompactString, Vec<&Match>> =
        HashMap::with_capacity(matches.len() / 2);

    for m in matches {
        pattern_matches.entry(&m.pattern).or_default().push(m);
    }

    #[cfg(test)]
    log::trace!("Grouped by pattern: {pattern_matches:?}");

    let mut all_matches: Vec<Match> = Vec::with_capacity(matches.len());

    let mut pattern_names: Vec<_> = pattern_matches.keys().copied().collect();
    pattern_names.sort();

    for pattern_name in pattern_names {
        #[expect(
            clippy::unwrap_used,
            reason = "pattern_name keys are cloned from pattern_matches, so the key is present"
        )]
        let pattern_group = pattern_matches.remove(&pattern_name).unwrap();

        if pattern_group.is_empty() {
            continue;
        }

        // The previous implementation checked every retained interval for
        // every candidate. Dense single-token patterns therefore turned a
        // linear stream of matches into quadratic work. The same rule has
        // a linear scan after grouping by start: retain the longest end at
        // each start, then reject only when an earlier retained start has
        // a strictly greater end. Equal end positions remain distinct,
        // preserving the established overlap policy.
        let mut longest_by_start = std::collections::BTreeMap::<usize, &Match>::new();
        for candidate in pattern_group {
            // Public matches always identify at least one consumed token.
            // Nullable expressions remain useful inside sequences, but a
            // top-level epsilon path must not escape as a zero-width match.
            if candidate.start >= candidate.end {
                continue;
            }
            longest_by_start
                .entry(candidate.start)
                .and_modify(|current| {
                    if candidate.end > current.end {
                        *current = candidate;
                    }
                })
                .or_insert(candidate);
        }

        let mut filtered_matches = Vec::with_capacity(longest_by_start.len());
        let mut longest_earlier_end: Option<usize> = None;
        for candidate in longest_by_start.into_values() {
            if longest_earlier_end.is_none_or(|end| end <= candidate.end) {
                longest_earlier_end =
                    Some(longest_earlier_end.map_or(candidate.end, |end| end.max(candidate.end)));
                filtered_matches.push(candidate.clone());
            }
        }

        #[cfg(test)]
        log::trace!("  Filtered matches for pattern {pattern_name}: {filtered_matches:?}");

        all_matches.extend(filtered_matches);
    }

    all_matches.sort_by_key(|a| a.start);

    #[cfg(test)]
    log::trace!("Final matches: {all_matches:?}");

    all_matches
}

#[cfg(test)]
mod tests {
    use super::{MatchSink, filter_and_collect_matches};
    use crate::fsm::Match;

    fn key(p: &str) -> compact_str::CompactString {
        p.into()
    }

    #[test]
    fn emit_dedups_identical_pattern_span_and_pushes_once() {
        let mut sink = MatchSink::new();
        sink.emit(&key("p"), 2, 4);
        sink.emit(&key("p"), 2, 4); // duplicate key — dropped
        sink.emit(&key("p"), 3, 5); // distinct — kept
        let out = sink.finish();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn emit_produces_a_capture_free_match() {
        let mut sink = MatchSink::new();
        sink.emit(&key("p"), 0, 1);
        let out = sink.finish();
        assert_eq!((out[0].start, out[0].end), (0, 1));
        assert!(out[0].captures.is_empty());
    }

    #[test]
    fn finish_applies_the_longest_per_start_overlap_policy() {
        // Two matches at start 0 with ends 1 and 2: only the longer survives.
        let mut sink = MatchSink::new();
        sink.emit(&key("p"), 0, 1);
        sink.emit(&key("p"), 0, 2);
        let out = sink.finish();
        assert_eq!(
            out.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
            vec![(0, 2)]
        );
    }

    #[test]
    fn filter_free_function_drops_zero_width_matches() {
        let zero = Match {
            pattern: key("p"),
            start: 2,
            end: 2,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        };
        assert!(filter_and_collect_matches(&[zero]).is_empty());
    }
}
