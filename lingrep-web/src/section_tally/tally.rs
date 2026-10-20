use std::cmp::Reverse;
use std::collections::BinaryHeap;

use lingrep_corpus::SectionCoverage;

use crate::section_tally::span_events::{SpanEvents, SpanStreamError};
use crate::section_tally::surface::{SurfaceSections, SurfaceTally, SurfaceTallyBuilder};
use crate::snapshot::PatternKey;

/// Bytes this feature may retain per workbench. Multiplied by
/// `DEFAULT_MAX_ENTRIES = 256`, so 64 KB is 16 MB across a full registry.
pub const RETAINED_CEILING_BYTES: usize = 65_536;

/// A column of the count table. Explicit rather than positional, so `None`
/// cannot be mistaken for a label slot by a consumer reading an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionSlot {
    Label(String),
    /// The sentence carried no label at all.
    None,
}

/// Why an exact section projection was refused. Both inputs are carried so the
/// panel can say which corpus and which catalog produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionRefusal {
    pub labels: usize,
    pub patterns: usize,
}

/// What a completed scan knows about sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionAvailability {
    Available(SectionTally),
    VocabularyTooLarge(SectionRefusal),
}

/// Why a sweep refused to keep counting. Every variant is a hard failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionTallyError {
    OutOfOrderSentence { previous: u64, found: u64 },
    OverlappingSpan { slot: u32, ordinal: u64 },
    Stream(SpanStreamError),
}

impl std::fmt::Display for SectionTallyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfOrderSentence { previous, found } => write!(
                formatter,
                "sentence ordinals must not decrease: {previous} then {found}"
            ),
            Self::OverlappingSpan { slot, ordinal } => write!(
                formatter,
                "section slot {slot} already had an open span at sentence {ordinal}"
            ),
            Self::Stream(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<SpanStreamError> for SectionTallyError {
    fn from(error: SpanStreamError) -> Self {
        Self::Stream(error)
    }
}

/// Conservative allocation size for one owned label.
fn label_allocation(label: &str) -> usize {
    label.len().max(1).next_multiple_of(16)
}

/// `8·P·S + 8·S + 32·S + B + 128`, checked. `None` means it would overflow,
/// which is a refusal rather than a wrapped comparison that admits.
fn retained_bytes(patterns: usize, slots: usize, label_bytes: usize) -> Option<usize> {
    let counts = patterns.checked_mul(slots)?.checked_mul(8)?;
    let universe = slots.checked_mul(8)?;
    let inline = slots.checked_mul(32)?;
    counts
        .checked_add(universe)?
        .checked_add(inline)?
        .checked_add(label_bytes)?
        .checked_add(128)
}

/// The retained result: exactly the three allocations `retained(P, S, B)`
/// measures. No channel, no heap, no cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionTally {
    slots: Box<[SectionSlot]>,
    /// Row-major `patterns × slots`: sentences in which the pattern matched.
    counts: Box<[u64]>,
    /// Per slot: sentences observed in it, whether or not anything matched.
    universe: Box<[u64]>,
    /// The same counts split by matched surface, so a selected variation can
    /// narrow the panel. Separately bounded and separately refusable; see
    /// [`surface`](crate::section_tally::surface).
    surfaces: SurfaceTally,
}

impl SectionTally {
    /// The tally a run over a corpus with no section labels produces: one
    /// `None` slot and no observations.
    ///
    /// Test-only. Snapshot fixtures that assert nothing about sections need a
    /// value that is honest rather than a refusal they would then have to
    /// explain.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn unlabelled() -> Self {
        Self {
            slots: vec![SectionSlot::None].into_boxed_slice(),
            counts: Box::new([]),
            universe: vec![0].into_boxed_slice(),
            surfaces: SurfaceTallyBuilder::new(0, 1).finish(),
        }
    }

    #[must_use]
    pub fn slots(&self) -> &[SectionSlot] {
        &self.slots
    }

    /// Sentences in which `pattern` matched, within `slot`. Zero out of range.
    #[must_use]
    pub fn count(&self, pattern: PatternKey, slot: usize) -> u64 {
        self.counts
            .get(pattern.index() * self.slots.len() + slot)
            .copied()
            .unwrap_or_default()
    }

    /// Sentences observed within `slot`, whatever matched. Zero out of range.
    #[must_use]
    pub fn universe(&self, slot: usize) -> u64 {
        self.universe.get(slot).copied().unwrap_or_default()
    }

    /// One pattern's counts split by matched surface, or why it has none.
    #[must_use]
    pub fn surfaces(&self, pattern: PatternKey) -> SurfaceSections<'_> {
        self.surfaces.pattern(pattern)
    }
}

/// Scan-time sweep state. Everything here except the three count arrays is
/// transient, which is why `finish` consumes `self`.
#[derive(Debug)]
pub struct SectionTallyBuilder {
    slots: Box<[SectionSlot]>,
    counts: Vec<Vec<u64>>,
    universe: Box<[u64]>,
    events: SpanEvents,
    /// `end` ordinal of each open span, soonest first.
    open: BinaryHeap<Reverse<(u64, u32)>>,
    active: Box<[bool]>,
    active_slots: Vec<u32>,
    previous_ordinal: Option<u64>,
    surfaces: SurfaceTallyBuilder,
}

/// Decides availability and, only if the vocabulary is admitted, obtains the
/// event stream from `start_stream`.
///
/// The single entry point for building a sweep, and the reason
/// [`SectionTallyBuilder`]'s constructor is private: the admission test must
/// run before anything opens a stream. A producer whose consumer was never built fills its
/// channel and parks forever, so `start_stream` is **not called** on the
/// refused branch — which is what makes that obligation testable rather than
/// merely asserted at the call site.
pub fn admit<F>(
    vocabulary: &[SectionCoverage],
    patterns: usize,
    start_stream: F,
) -> Result<SectionTallyBuilder, SectionRefusal>
where
    F: FnOnce() -> SpanEvents,
{
    if SectionTallyBuilder::fits(vocabulary, patterns) {
        Ok(SectionTallyBuilder::new(
            vocabulary,
            patterns,
            start_stream(),
        ))
    } else {
        Err(SectionRefusal {
            labels: vocabulary.len(),
            patterns,
        })
    }
}

/// [`admit`] over an already-built stream, discarding the refusal. For callers
/// that have no producer to protect — tests, and the sample-equivalent tally.
#[must_use]
pub fn build(
    vocabulary: &[SectionCoverage],
    patterns: usize,
    events: SpanEvents,
) -> Option<SectionTallyBuilder> {
    admit(vocabulary, patterns, || events).ok()
}

impl SectionTallyBuilder {
    /// Decides availability *before* allocating, and before the caller starts a
    /// span producer: a refused admission must not leave a producer parked on a
    /// channel nothing will drain.
    ///
    /// Public because the refusal is a reportable outcome; [`admit`] is how a
    /// caller acts on it.
    #[must_use]
    pub fn fits(vocabulary: &[SectionCoverage], patterns: usize) -> bool {
        let Some(slots) = vocabulary.len().checked_add(1) else {
            return false;
        };
        vocabulary
            .iter()
            .try_fold(0usize, |total, entry| {
                total.checked_add(label_allocation(&entry.label))
            })
            .and_then(|bytes| retained_bytes(patterns, slots, bytes))
            .is_some_and(|bytes| bytes <= RETAINED_CEILING_BYTES)
    }

    /// Private: `patterns * slots` is unchecked here, and [`admit`] is the one
    /// path that reaches it, having already run [`Self::fits`]. A `pub`
    /// constructor whose safety rests on a doc comment is a footgun on a
    /// `pub mod`.
    fn new(vocabulary: &[SectionCoverage], patterns: usize, events: SpanEvents) -> Self {
        let slots = vocabulary.len() + 1;
        let mut slot_list = vocabulary
            .iter()
            .map(|entry| SectionSlot::Label(entry.label.clone()))
            .collect::<Vec<_>>();
        slot_list.push(SectionSlot::None);
        Self {
            slots: slot_list.into_boxed_slice(),
            counts: (0..patterns).map(|_| vec![0; slots]).collect(),
            universe: vec![0; slots].into_boxed_slice(),
            events,
            open: BinaryHeap::new(),
            active: vec![false; slots].into_boxed_slice(),
            active_slots: Vec::new(),
            previous_ordinal: None,
            surfaces: SurfaceTallyBuilder::new(patterns, slots),
        }
    }

    #[must_use]
    pub fn slots(&self) -> &[SectionSlot] {
        &self.slots
    }

    /// Adds rows for patterns discovered during the scan, or refuses the
    /// section projection before its retained count table exceeds its budget.
    pub(crate) fn ensure_patterns(&mut self, patterns: usize) -> Result<(), SectionRefusal> {
        if patterns <= self.counts.len() {
            return Ok(());
        }
        let labels = self.slots.len().saturating_sub(1);
        let label_bytes = self.slots[..labels]
            .iter()
            .filter_map(|slot| match slot {
                SectionSlot::Label(label) => Some(label_allocation(label)),
                SectionSlot::None => None,
            })
            .sum();
        if retained_bytes(patterns, self.slots.len(), label_bytes)
            .is_none_or(|bytes| bytes > RETAINED_CEILING_BYTES)
        {
            return Err(SectionRefusal { labels, patterns });
        }
        let slots = self.slots.len();
        self.counts.resize_with(patterns, || vec![0; slots]);
        self.surfaces.ensure_patterns(patterns);
        Ok(())
    }

    /// Sentences in which `pattern` matched, within `slot`, so far.
    #[must_use]
    pub fn count(&self, pattern: PatternKey, slot: usize) -> u64 {
        self.counts
            .get(pattern.index())
            .and_then(|row| row.get(slot))
            .copied()
            .unwrap_or_default()
    }

    /// Sentences observed within `slot` so far, whatever matched.
    #[must_use]
    pub fn universe(&self, slot: usize) -> u64 {
        self.universe.get(slot).copied().unwrap_or_default()
    }

    /// One published sentence, the patterns that matched in it, and the
    /// distinct `(pattern, surface)` pairs those matches carried. Ordinals
    /// must not decrease; a repeated key counts once.
    ///
    /// The two lists are the same fact at two grains, so they are taken
    /// together: a caller that could supply one without the other could leave
    /// the panel and the surface-filtered panel disagreeing about one
    /// sentence.
    ///
    /// # Errors
    ///
    /// Returns an error when the ordinal decreases, when the span stream
    /// opens a second span for an already-active slot, or when the stream
    /// itself failed.
    pub fn observe(
        &mut self,
        ordinal: u64,
        matched: &[PatternKey],
        surfaces: &[(PatternKey, String)],
    ) -> Result<(), SectionTallyError> {
        // `<=`, not `<`: a repeated ordinal double-counts in the tally while
        // leaving `observed_sentences` and `sentences_matched` in agreement, so
        // the publication gate would not catch it. Enforce the stream contract
        // here rather than relying on its producer.
        if let Some(previous) = self.previous_ordinal
            && ordinal <= previous
        {
            return Err(SectionTallyError::OutOfOrderSentence {
                previous,
                found: ordinal,
            });
        }
        self.previous_ordinal = Some(ordinal);
        self.retire(ordinal);
        self.admit_spans(ordinal)?;
        // Once per sentence, before the per-slot pass below: a sentence inside
        // two sections is recorded in two slots and is still one sentence, so
        // the whole-corpus figure per surface has to be counted here.
        for (pattern, surface) in surfaces {
            self.surfaces.record_sentence(*pattern, surface);
        }
        if self.active_slots.is_empty() {
            let none_slot = self.slots.len() - 1;
            self.record(none_slot, matched, surfaces);
        } else {
            for index in 0..self.active_slots.len() {
                let slot = self.active_slots[index] as usize;
                self.record(slot, matched, surfaces);
            }
        }
        Ok(())
    }

    /// Drains the remainder of the event stream through `End` and returns the
    /// lean retained value.
    ///
    /// Draining is required for liveness: trailing events left unread park the
    /// producer on a full channel while the coordinator waits to join it.
    /// Consuming `self` is required for the memory bound: the channel, the
    /// open-span heap and the active-slot list must not reach the snapshot.
    /// It validates ordering and termination only — an event after the
    /// observed universe never becomes active, so it can affect neither a
    /// count nor the open-span bound.
    ///
    /// # Errors
    ///
    /// Returns an error when the remainder is out of order or the stream
    /// closed without its `End` sentinel.
    pub fn finish(mut self) -> Result<SectionTally, SectionTallyError> {
        self.events.drain()?;
        let counts = self
            .counts
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(SectionTally {
            slots: self.slots,
            counts,
            universe: self.universe,
            surfaces: self.surfaces.finish(),
        })
    }

    fn record(&mut self, slot: usize, matched: &[PatternKey], surfaces: &[(PatternKey, String)]) {
        self.universe[slot] += 1;
        for (pattern, surface) in surfaces {
            self.surfaces.record(*pattern, surface, slot);
        }
        for (position, pattern) in matched.iter().enumerate() {
            // A repeated key counts once. The sink supplies distinct keys;
            // this makes the guarantee local rather than an assumption.
            if matched[..position].contains(pattern) {
                continue;
            }
            if let Some(cell) = self
                .counts
                .get_mut(pattern.index())
                .and_then(|row| row.get_mut(slot))
            {
                *cell += 1;
            }
        }
    }

    fn retire(&mut self, ordinal: u64) {
        while let Some(Reverse((end, slot))) = self.open.peek().copied() {
            if end > ordinal {
                break;
            }
            self.open.pop();
            self.active[slot as usize] = false;
            if let Some(position) = self.active_slots.iter().position(|value| *value == slot) {
                self.active_slots.swap_remove(position);
            }
        }
    }

    fn admit_spans(&mut self, ordinal: u64) -> Result<(), SectionTallyError> {
        while let Some(event) = self.events.peek()? {
            if event.first > ordinal {
                break;
            }
            self.events.take();
            if event.end <= ordinal {
                // Wholly behind the cursor: it covered only sentences the run
                // omitted as oversized. It never becomes active, so it is not
                // checked for overlap either — overlap is a property of two
                // spans being active at one sentence, and this one has no
                // sentence to be active at. It can therefore affect neither a
                // count nor the `<= S` open-span bound.
                continue;
            }
            let slot = event.slot as usize;
            if self.active.get(slot).copied().unwrap_or(false) {
                return Err(SectionTallyError::OverlappingSpan {
                    slot: event.slot,
                    ordinal,
                });
            }
            let Some(flag) = self.active.get_mut(slot) else {
                // A producer only ever emits slots it mapped from this
                // vocabulary, so this is unreachable in the wired path; the
                // sweep still refuses rather than indexing out of bounds.
                return Err(SectionTallyError::OverlappingSpan {
                    slot: event.slot,
                    ordinal,
                });
            };
            *flag = true;
            self.active_slots.push(event.slot);
            self.open.push(Reverse((event.end, event.slot)));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_tally::span_events::SpanEvent;

    fn coverage(labels: &[&str]) -> Vec<SectionCoverage> {
        labels
            .iter()
            .map(|label| SectionCoverage {
                label: (*label).to_string(),
                coverage: 1,
            })
            .collect()
    }

    fn tally(labels: &[&str], patterns: usize, events: Vec<SpanEvent>) -> SectionTallyBuilder {
        build(&coverage(labels), patterns, SpanEvents::from_events(events))
            .expect("the fixture vocabulary fits")
    }

    fn key(index: usize) -> PatternKey {
        PatternKey::new(index).unwrap()
    }

    /// The whole-corpus figure per surface is not the row sum. A sentence held
    /// by two sections is recorded in two slots and is still one sentence, so
    /// summing the slots would report it twice — which is what the variation
    /// list would then rank on.
    #[test]
    fn a_sentence_in_two_sections_counts_once_in_the_surface_total() {
        let mut builder = tally(
            &["methods", "results"],
            1,
            vec![
                SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 1,
                },
                SpanEvent {
                    slot: 1,
                    first: 0,
                    end: 1,
                },
            ],
        );
        let staged = [(key(0), "走った".to_string())];
        builder.observe(0, &[key(0)], &staged).unwrap();
        let finished = builder.finish().unwrap();
        let surfaces = finished.surfaces(key(0));
        assert_eq!(surfaces.count("走った", 0), Some(1));
        assert_eq!(surfaces.count("走った", 1), Some(1));
        assert_eq!(
            surfaces.variations().collect::<Vec<_>>(),
            vec![("走った", 1)],
            "one sentence, however many sections held it"
        );
    }

    /// The complement: two sentences under one section do accumulate.
    #[test]
    fn the_surface_total_accumulates_across_sentences() {
        let mut builder = tally(
            &["methods"],
            1,
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 2,
            }],
        );
        let staged = [(key(0), "走った".to_string())];
        builder.observe(0, &[key(0)], &staged).unwrap();
        builder.observe(1, &[key(0)], &staged).unwrap();
        let finished = builder.finish().unwrap();
        assert_eq!(
            finished.surfaces(key(0)).variations().collect::<Vec<_>>(),
            vec![("走った", 2)]
        );
    }

    #[test]
    fn a_sentence_counts_under_every_active_label() {
        // methods covers 0..4; quote covers 2..3. Sentence 2 is in both.
        let mut t = tally(
            &["methods", "quote"],
            1,
            vec![
                SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 4,
                },
                SpanEvent {
                    slot: 1,
                    first: 2,
                    end: 3,
                },
            ],
        );
        for ordinal in 0..4 {
            t.observe(ordinal, &[key(0)], &[]).unwrap();
        }
        assert_eq!(t.count(key(0), 0), 4, "four sentences in methods");
        assert_eq!(t.count(key(0), 1), 1, "one sentence in quote");
        assert_eq!(t.universe(0), 4);
        assert_eq!(t.universe(1), 1);
    }

    #[test]
    fn a_sentence_outside_every_span_lands_in_the_none_slot() {
        let mut t = tally(
            &["methods"],
            1,
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 1,
            }],
        );
        t.observe(0, &[key(0)], &[]).unwrap();
        t.observe(1, &[key(0)], &[]).unwrap();
        let none = t.slots().len() - 1;
        assert!(matches!(t.slots()[none], SectionSlot::None));
        assert_eq!(t.count(key(0), 0), 1);
        assert_eq!(t.count(key(0), none), 1);
        assert_eq!(t.universe(none), 1);
    }

    #[test]
    fn an_adjacent_reopening_stays_active_across_the_boundary() {
        // Same label closes at 3 and reopens at 3: retire-then-admit must leave
        // it active at sentence 3. No reference counting is involved.
        let mut t = tally(
            &["methods"],
            1,
            vec![
                SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 3,
                },
                SpanEvent {
                    slot: 0,
                    first: 3,
                    end: 6,
                },
            ],
        );
        for ordinal in 0..6 {
            t.observe(ordinal, &[key(0)], &[]).unwrap();
        }
        assert_eq!(t.count(key(0), 0), 6, "no gap at the document boundary");
    }

    #[test]
    fn an_overlapping_same_label_span_is_refused() {
        let mut t = tally(
            &["methods"],
            1,
            vec![
                SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 9,
                },
                SpanEvent {
                    slot: 0,
                    first: 2,
                    end: 4,
                },
            ],
        );
        t.observe(0, &[key(0)], &[]).unwrap();
        assert!(matches!(
            t.observe(2, &[key(0)], &[]),
            Err(SectionTallyError::OverlappingSpan {
                slot: 0,
                ordinal: 2
            })
        ));
    }

    #[test]
    fn a_decreasing_sentence_ordinal_is_refused() {
        let mut t = tally(
            &["methods"],
            1,
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 9,
            }],
        );
        t.observe(4, &[key(0)], &[]).unwrap();
        assert!(matches!(
            t.observe(3, &[key(0)], &[]),
            Err(SectionTallyError::OutOfOrderSentence {
                previous: 4,
                found: 3
            })
        ));
    }

    #[test]
    fn one_sentence_counts_once_per_pattern_however_many_matches() {
        let mut t = tally(
            &["methods"],
            2,
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 9,
            }],
        );
        // The caller supplies distinct keys; passing a repeat must still count once.
        t.observe(0, &[key(0), key(0), key(1)], &[]).unwrap();
        assert_eq!(t.count(key(0), 0), 1);
        assert_eq!(t.count(key(1), 0), 1);
        assert_eq!(t.universe(0), 1);
    }

    #[test]
    fn finish_drains_trailing_events_beyond_the_last_sentence() {
        let mut t = tally(
            &["methods"],
            1,
            vec![
                SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 2,
                },
                SpanEvent {
                    slot: 0,
                    first: 900,
                    end: 910,
                },
            ],
        );
        t.observe(0, &[key(0)], &[]).unwrap();
        let retained = t
            .finish()
            .expect("trailing events drain rather than deadlock");
        assert_eq!(
            retained.count(key(0), 0),
            1,
            "a drained event changes no count"
        );
    }

    #[test]
    fn a_vocabulary_past_the_ceiling_is_refused_on_label_count() {
        let labels = (0..200)
            .map(|index| format!("s{index:03}"))
            .collect::<Vec<_>>();
        let refs = labels.iter().map(String::as_str).collect::<Vec<_>>();
        assert!(!SectionTallyBuilder::fits(&coverage(&refs), 100));
    }

    #[test]
    fn a_vocabulary_past_the_ceiling_is_refused_on_label_bytes_alone() {
        // Two labels and one pattern: admitted on count many times over, and
        // refused on size alone. 40_000 bytes — the figure the plan carried —
        // would have fitted: the ceiling is 65_536.
        let long = "x".repeat(70_000);
        assert!(
            SectionTallyBuilder::fits(&coverage(&["short", "methods"]), 1),
            "the same shape with short labels is admitted"
        );
        assert!(
            !SectionTallyBuilder::fits(&coverage(&[&long, "methods"]), 1),
            "label bytes are part of the bound, not an assumed constant"
        );
    }

    /// Spec obligation 10. The stream is not merely unused on the refused
    /// branch — it is never started, because a producer with no consumer fills
    /// its channel and parks forever. Pinned by a closure that records whether
    /// it ran.
    #[test]
    fn a_refused_vocabulary_never_starts_the_stream() {
        let started = std::cell::Cell::new(false);
        let long = "x".repeat(70_000);
        let refusal = admit(&coverage(&[&long]), 1, || {
            started.set(true);
            SpanEvents::from_events(vec![])
        })
        .expect_err("70_000 label bytes exceed the 65_536 ceiling");
        assert_eq!(
            refusal,
            SectionRefusal {
                labels: 1,
                patterns: 1
            }
        );
        assert!(
            !started.get(),
            "no stream may be opened when the projection is refused"
        );
    }

    #[test]
    fn an_admitted_vocabulary_starts_the_stream_exactly_once() {
        let started = std::cell::Cell::new(0usize);
        admit(&coverage(&["methods"]), 1, || {
            started.set(started.get() + 1);
            SpanEvents::from_events(vec![])
        })
        .expect("one short label fits");
        assert_eq!(started.get(), 1, "the control for the refusal above");
    }

    #[test]
    fn the_retained_value_carries_no_inline_scan_state() {
        // Narrow by construction: this proves the struct is three slice handles
        // plus the surface table's own two fields, and no additional inline
        // field — not that the heap matches `retained(P, S, B)`, which
        // `retained_bytes_is_checked_at_its_boundary` covers. Together they
        // are the memory claim. The surface table is bounded separately, by
        // `SURFACE_CEILING_BYTES`.
        let mut builder = tally(
            &["methods"],
            1,
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 2,
            }],
        );
        builder.observe(0, &[key(0)], &[]).unwrap();
        let retained: SectionTally = builder.finish().unwrap();
        assert_eq!(retained.count(key(0), 0), 1);
        assert_eq!(retained.slots().len(), 2);
        assert_eq!(
            std::mem::size_of_val(&retained),
            std::mem::size_of::<Box<[SectionSlot]>>()
                + std::mem::size_of::<Box<[u64]>>() * 2
                + std::mem::size_of::<SurfaceTally>(),
            "no channel, heap or cursor survives finalization inline"
        );
        assert_eq!(
            std::mem::size_of::<SurfaceTally>(),
            std::mem::size_of::<usize>() * 3,
            "the surface table is a width and one slice handle"
        );
    }

    #[test]
    fn retained_bytes_is_checked_at_its_boundary() {
        // 100 patterns, S slots, B = 32·L: 872·S + 96 ≤ 65_536 admits S = 75.
        let at = retained_bytes(100, 75, 32 * 74).expect("no overflow");
        let past = retained_bytes(100, 76, 32 * 75).expect("no overflow");
        assert!(at <= RETAINED_CEILING_BYTES, "{at} must fit");
        assert!(past > RETAINED_CEILING_BYTES, "{past} must not");
        // Overflow is a refusal, not a wrap that admits.
        assert_eq!(retained_bytes(usize::MAX, 2, 0), None);
        assert_eq!(retained_bytes(1, 1, usize::MAX), None);
    }

    #[test]
    fn dynamically_discovered_pattern_rows_refuse_before_exceeding_the_budget() {
        let coverage = coverage(&["methods"]);
        let mut builder = build(&coverage, 1, SpanEvents::from_events(vec![])).unwrap();
        assert_eq!(
            builder.ensure_patterns(usize::MAX),
            Err(SectionRefusal {
                labels: 1,
                patterns: usize::MAX,
            })
        );
        assert_eq!(builder.count(key(0), 0), 0, "existing rows remain intact");
    }
}
