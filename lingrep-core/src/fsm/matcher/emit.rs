//! Match emission: construction, dedup, and the terminal overlap filter.

use super::resource::{MatchResourceError, OutputBudget, compact_heap_bytes, match_heap_bytes};

use super::pool::FastBuild;
use crate::fsm::Match;
use compact_str::CompactString;
use std::collections::{HashMap, HashSet};

type MatchKey = (CompactString, usize, usize);

/// Constructs and deduplicates matches before applying the overlap filter.
pub(crate) struct MatchSink<'a> {
    dedup: HashSet<MatchKey, FastBuild>,
    out: Vec<Match>,
    output: &'a OutputBudget,
}

impl<'a> MatchSink<'a> {
    pub(crate) fn with_output(output: &'a OutputBudget) -> Self {
        Self {
            dedup: HashSet::default(),
            out: Vec::new(),
            output,
        }
    }

    /// Inserts the first match for each `(pattern, start, end)` tuple.
    /// Captures are populated by a later pass; callers validate the span.
    #[inline]
    pub(crate) fn emit(
        &mut self,
        pattern: &CompactString,
        start: usize,
        end: usize,
    ) -> Result<(), MatchResourceError> {
        self.output.reserve(compact_heap_bytes(pattern))?;
        let key = (pattern.clone(), start, end);
        if self.dedup.contains(&key) {
            return Ok(());
        }
        self.output.reserve_vec(&mut self.out, 1)?;
        self.output.reserve(compact_heap_bytes(pattern))?;
        self.dedup.insert(key);
        self.out.push(Match {
            pattern: pattern.clone(),
            start,
            end,
            captures: Box::default(),
            graph_bindings: Box::default(),
            interpretations: Box::default(),
            lexical: None,
        });
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<Vec<Match>, MatchResourceError> {
        filter_and_collect_matches(&self.out, self.output)
    }
}

/// The sweep over one definition's published spans in ascending start order
/// that applies the nested-match rule: a span is dropped when a span of the
/// same definition from an earlier start ends strictly later.
///
/// Spans from one start never drop each other, so a caller publishing several
/// spans from one start tests all of them before it calls [`Self::publish`]
/// for any.
#[derive(Default)]
pub(crate) struct EarlierStarts {
    longest_end: usize,
}

impl EarlierStarts {
    /// Whether a span from an earlier start ends strictly after `end`.
    #[inline]
    pub(crate) const fn extends_past(&self, end: usize) -> bool {
        end < self.longest_end
    }

    #[inline]
    pub(crate) fn publish(&mut self, end: usize) {
        self.longest_end = self.longest_end.max(end);
    }
}

/// For each pattern, discard zero-width matches, keep the longest match from
/// each start, and apply [`EarlierStarts`]. Overlaps between different
/// patterns remain independent.
pub(crate) fn filter_and_collect_matches(
    matches: &[Match],
    output: &OutputBudget,
) -> Result<Vec<Match>, MatchResourceError> {
    if matches.is_empty() {
        return Ok(Vec::new());
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

    output.reserve_buffer::<Match>(matches.len())?;
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

        output.reserve_buffer::<Match>(longest_by_start.len())?;
        let mut filtered_matches = Vec::with_capacity(longest_by_start.len());
        let mut earlier = EarlierStarts::default();
        for candidate in longest_by_start.into_values() {
            if !earlier.extends_past(candidate.end) {
                earlier.publish(candidate.end);
                output.reserve(match_heap_bytes(candidate))?;
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

    Ok(all_matches)
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
        let output = super::OutputBudget::default();
        let mut sink = MatchSink::with_output(&output);
        sink.emit(&key("p"), 2, 4).unwrap();
        sink.emit(&key("p"), 2, 4).unwrap();
        sink.emit(&key("p"), 3, 5).unwrap();
        let out = sink.finish().unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn emit_produces_a_capture_free_match() {
        let output = super::OutputBudget::default();
        let mut sink = MatchSink::with_output(&output);
        sink.emit(&key("p"), 0, 1).unwrap();
        let out = sink.finish().unwrap();
        assert_eq!((out[0].start, out[0].end), (0, 1));
        assert!(out[0].captures.is_empty());
    }

    #[test]
    fn finish_applies_the_longest_per_start_overlap_policy() {
        // Two matches at start 0 with ends 1 and 2: only the longer survives.
        let output = super::OutputBudget::default();
        let mut sink = MatchSink::with_output(&output);
        sink.emit(&key("p"), 0, 1).unwrap();
        sink.emit(&key("p"), 0, 2).unwrap();
        let out = sink.finish().unwrap();
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
            interpretations: Box::default(),
            lexical: None,
        };
        assert!(
            filter_and_collect_matches(&[zero], &super::OutputBudget::default())
                .unwrap()
                .is_empty()
        );
    }
}
