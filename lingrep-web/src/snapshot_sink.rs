use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use compact_str::CompactString;
use lingrep_core::execution::{ExecutionError, MatchSink, SentenceLocation, SinkControl};
use lingrep_core::token_match::AttributeKey;
#[cfg(test)]
use lingrep_core::token_match::TokenInfo;

use crate::derived_patterns::{compound_name, contiguous_chains};
use crate::revision::ErrorCategory;
use crate::section_tally::{
    SectionAvailability, SectionRefusal, SectionTallyBuilder, SectionTallyError,
};
use crate::snapshot::{
    CaptureDescriptor, CaptureKey, CaptureSpanRef, CompoundCategory, MatchRef, PatternDescriptor,
    PatternKey, PatternMetadataValue,
};

/// What the sink is doing about sections during the scan.
///
/// The builder is boxed because it carries the sweep's whole transient state —
/// the event cursor, the open-span heap, the active-slot list — against a
/// refusal that is two integers. One allocation per scan buys a small
/// `SnapshotSink`, which is moved on every terminal path.
#[derive(Debug)]
pub(crate) enum SectionScanState {
    Building(Box<SectionTallyBuilder>),
    Refused(SectionRefusal),
}

/// Fixed, so one corpus and one catalog always retain the same sample.
///
/// A user who reruns a query to see *different* examples is not a case worth
/// trading reproducibility for: a screenshot, a permalink and a citation all
/// assume the pane shows the same rows tomorrow.
const RESERVOIR_SEED: u64 = 0x5647_5F52_4553_5631;

/// Algorithm R's draw for the `seen`-th match of one pattern.
///
/// A pure function of `(pattern, seen)` rather than a step of a mutable
/// generator, so the sample does not depend on how the scan was scheduled,
/// how many workers ran, or what any other pattern drew. Two patterns
/// reaching their tenth match draw independently because the pattern index is
/// mixed in, and the same pattern draws the same value on every rerun.
///
/// The caller reduces modulo `seen`. The bias that introduces is bounded by
/// `seen / 2^64` and a corpus cannot approach it.
fn reservoir_draw(pattern: PatternKey, seen: u64) -> u64 {
    lingrep_core::execution::reservoir_draw(RESERVOIR_SEED, pattern.index() as u64, seen)
}

#[derive(Debug)]
pub(crate) struct SnapshotSink {
    pub(crate) refs: Vec<MatchRef>,
    max_refs: usize,
    pattern_ids: HashMap<String, PatternKey>,
    compound_ids: HashMap<Vec<PatternKey>, PatternKey>,
    pub(crate) derive_compounds: bool,
    pub(crate) compound_category: CompoundCategory,
    pub(crate) patterns: Vec<PatternDescriptor>,
    /// Per pattern, the positions in `refs` this pattern's reservoir occupies.
    ///
    /// Replaces a bare retained-count: a reservoir must be able to *overwrite*
    /// a slot it already holds, which a count cannot express.
    reservoirs: HashMap<PatternKey, Vec<usize>>,
    capture_keys: HashMap<(PatternKey, CompactString, CompactString), CaptureKey>,
    pub(crate) capture_catalog: Vec<CaptureDescriptor>,
    /// Scan-time only. `finish` or `discard_sections` takes it, which is what
    /// drops the span channel before the producer is joined.
    sections: Option<SectionScanState>,
    /// The lean result `resolve_scan` publishes. `None` until `finish` runs.
    pub(crate) finalized_sections: Option<SectionAvailability>,
    /// Sentences published to this sink. The denominator, and the value
    /// `resolve_scan` checks against `ExecutionReport::sentences_matched`.
    /// Owned here rather than by the tally, because the tally does not exist
    /// in the refused branch and the gate must hold in both.
    pub(crate) observed_sentences: u64,
    last_ordinal: Option<u64>,
    /// Reused across pushes so the per-sentence distinct-pattern set costs no
    /// allocation. Emptied at the start of every push.
    matched_scratch: Vec<PatternKey>,
    /// The distinct `(pattern, surface)` pairs of one sentence, for the
    /// section tally's surface axis.
    ///
    /// Reused rather than rebuilt: `surface_used` is the live prefix and the
    /// `String`s past it keep their capacity, so joining one surface per match
    /// over a whole corpus allocates only while the longest surface grows.
    surface_scratch: Vec<(PatternKey, String)>,
    surface_used: usize,
}

/// The surface accessor, and the one the KWIC key is joined from. Matching
/// `lingrep-web/src/inspection/projector.rs`'s `FORM` is what makes a variation
/// selected in the pane name the same string this counted.
static FORM: LazyLock<AttributeKey> =
    LazyLock::new(|| AttributeKey::new("text").expect("text is canonical"));

impl SnapshotSink {
    #[cfg(test)]
    pub(crate) fn new(
        max_refs: usize,
        pattern_data: Vec<(String, Box<[(String, PatternMetadataValue)]>)>,
        sections: SectionScanState,
    ) -> Result<Self, ErrorCategory> {
        Self::new_with_compounds(
            max_refs,
            pattern_data,
            sections,
            false,
            CompoundCategory::Compound,
        )
    }

    pub(crate) fn new_with_compounds(
        max_refs: usize,
        pattern_data: Vec<(String, Box<[(String, PatternMetadataValue)]>)>,
        sections: SectionScanState,
        derive_compounds: bool,
        compound_category: CompoundCategory,
    ) -> Result<Self, ErrorCategory> {
        let mut pattern_ids = HashMap::with_capacity(pattern_data.len());
        let mut patterns = Vec::with_capacity(pattern_data.len());
        for (index, (name, metadata)) in pattern_data.into_iter().enumerate() {
            let key = PatternKey::new(index).ok_or(ErrorCategory::Execution)?;
            pattern_ids.insert(name.clone(), key);
            patterns.push(PatternDescriptor {
                key,
                name,
                exact_total: 0,
                metadata,
            });
        }
        Ok(Self {
            refs: Vec::new(),
            max_refs,
            pattern_ids,
            compound_ids: HashMap::new(),
            derive_compounds,
            compound_category,
            patterns,
            reservoirs: HashMap::new(),
            capture_keys: HashMap::new(),
            capture_catalog: Vec::new(),
            sections: Some(sections),
            finalized_sections: None,
            observed_sentences: 0,
            last_ordinal: None,
            matched_scratch: Vec::new(),
            surface_scratch: Vec::new(),
            surface_used: 0,
        })
    }

    fn add_compound(&mut self, components: &[PatternKey]) -> Result<PatternKey, ExecutionError> {
        if let Some(key) = self.compound_ids.get(components) {
            return Ok(*key);
        }
        let component_names = components
            .iter()
            .map(|key| self.patterns[key.index()].name.clone())
            .collect::<Vec<_>>();
        let mut source_categories = Vec::new();
        for key in components {
            if let Some(PatternMetadataValue::String(category)) = self.patterns[key.index()]
                .metadata
                .iter()
                .find_map(|(field, value)| (field == "category").then_some(value))
                && !source_categories.contains(category)
            {
                source_categories.push(category.clone());
            }
        }
        let source_category = source_categories.join(" ＋ ");
        let category = match self.compound_category {
            CompoundCategory::Compound => "複合項目".to_owned(),
            CompoundCategory::SourceCategories if source_category.is_empty() => "複合項目".into(),
            CompoundCategory::SourceCategories => source_category.clone(),
        };
        let components_label = component_names.join(" ＋ ");
        let name = compound_name(&component_names, |candidate| {
            self.pattern_ids.contains_key(candidate)
        });
        let key = PatternKey::new(self.patterns.len()).ok_or_else(|| {
            ExecutionError::Sink("derived pattern catalog exceeded u32 identity space".into())
        })?;
        let mut metadata: Vec<(String, PatternMetadataValue)> = vec![
            ("category".into(), PatternMetadataValue::String(category)),
            (
                "component_patterns".into(),
                PatternMetadataValue::String(components_label),
            ),
        ];
        if !source_category.is_empty() {
            metadata.push((
                "source_categories".into(),
                PatternMetadataValue::String(source_category),
            ));
        }
        metadata.sort_by(|left, right| left.0.cmp(&right.0));
        self.pattern_ids.insert(name.clone(), key);
        self.patterns.push(PatternDescriptor {
            key,
            name,
            exact_total: 0,
            metadata: metadata.into_boxed_slice(),
        });
        self.compound_ids.insert(components.to_vec(), key);
        Ok(key)
    }

    /// Completed run: drain the span stream through `End`, consume the
    /// builder, and leave the lean result in `finalized_sections`.
    ///
    /// Consuming the builder drops the channel receiver, which is what
    /// releases a producer parked on a full channel before it is joined.
    /// Called from the scan's blocking thread, never from the async
    /// coordinator: the production span source blocks on its channel.
    pub(crate) fn finish(&mut self) -> Result<(), ExecutionError> {
        let state = self
            .sections
            .take()
            .ok_or_else(|| ExecutionError::Sink("section state was already finalized".into()))?;
        self.finalized_sections = Some(match state {
            SectionScanState::Building(builder) => {
                SectionAvailability::Available(builder.finish().map_err(sink_error)?)
            }
            SectionScanState::Refused(refusal) => SectionAvailability::VocabularyTooLarge(refusal),
        });
        // A reservoir fills in arrival order and is then overwritten in place,
        // so its slots end up in no order at all. Restore corpus order, which
        // is what the KWIC pane's reading order means and what `match_ref`'s
        // ordinal indexes. Under the cap this sort is a no-op — nothing was
        // replaced, so the vector is already ascending — which is why a corpus
        // that fits retains exactly what it always did.
        self.refs.sort_by(|left, right| {
            left.sentence_index
                .cmp(&right.sentence_index)
                .then_with(|| left.span.start.cmp(&right.span.start))
                .then_with(|| left.span.end.cmp(&right.span.end))
                .then_with(|| left.pattern_key.index().cmp(&right.pattern_key.index()))
        });
        Ok(())
    }

    /// Cancelled or failed run: drop the builder undrained. Nothing will be
    /// published, but the receiver must still go away before the producer is
    /// joined, or the join waits on a task parked on a full channel.
    pub(crate) fn discard_sections(&mut self) {
        self.sections = None;
    }

    fn insert_capture_descriptor(
        &mut self,
        pattern_key: PatternKey,
        name: CompactString,
        path: CompactString,
    ) -> Result<CaptureKey, ExecutionError> {
        let key = CaptureKey::new(self.capture_catalog.len()).ok_or_else(|| {
            ExecutionError::Sink("capture catalog exceeded u32 identity space".into())
        })?;
        self.capture_catalog.push(CaptureDescriptor {
            key,
            pattern_key,
            name: name.to_string(),
            path: path.to_string(),
        });
        self.capture_keys.insert((pattern_key, name, path), key);
        Ok(key)
    }
}

impl SnapshotSink {
    /// Joins one match's surface into the scratch, keeping the prefix
    /// distinct.
    ///
    /// The join is `lingrep_core::utils::join_orth` over the matched tokens' `text`,
    /// which is exactly how `inspection::kwic_orth_split` builds the KWIC key
    /// a reader clicks. Two derivations of one string is a hazard, and it is
    /// the reason both go through `join_orth`: a divergence would silently
    /// leave a selectable variation with no counted row.
    fn stage_surface(
        &mut self,
        pattern: PatternKey,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        span: std::ops::Range<usize>,
    ) {
        if self.surface_used == self.surface_scratch.len() {
            self.surface_scratch.push((pattern, String::new()));
        }
        let (slot_pattern, joined) = &mut self.surface_scratch[self.surface_used];
        *slot_pattern = pattern;
        joined.clear();
        lingrep_core::utils::join_orth_into(
            joined,
            span.filter_map(|index| sentence.scalar_key(index, &FORM)),
        );
        // A sentence carrying one surface of one pattern twice is one
        // sentence, which is the unit the section table counts in.
        let duplicate = self.surface_scratch[..self.surface_used]
            .iter()
            .any(|(held, text)| {
                *held == pattern && text == &self.surface_scratch[self.surface_used].1
            });
        if !duplicate {
            self.surface_used += 1;
        }
    }
}

/// Collapses a sweep refusal into the sink error the runner propagates. The
/// cause is preserved in the message; the coordinator logs it and shows the
/// generic integrity text, as it does for every other scan failure.
fn sink_error(error: SectionTallyError) -> ExecutionError {
    ExecutionError::Sink(error.to_string())
}

impl MatchSink for SnapshotSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        _sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[lingrep_core::fsm::Match],
    ) -> Result<SinkControl, ExecutionError> {
        let sentence_index = location.corpus_sentence_ordinal;
        // The section sweep's cursor cannot move backwards, so an out-of-order
        // ordinal would miscount silently rather than merely produce unsorted
        // data. Refuse it whether or not sections are available.
        if let Some(previous) = self.last_ordinal
            && sentence_index < previous
        {
            return Err(ExecutionError::Sink(format!(
                "sentence ordinals must not decrease: {previous} then {sentence_index}"
            )));
        }
        self.last_ordinal = Some(sentence_index);
        self.observed_sentences += 1;
        self.matched_scratch.clear();
        self.surface_used = 0;
        let mut published_matches = Cow::Borrowed(matches);
        if self.derive_compounds {
            let mut derived_matches = matches.to_vec();
            let component_matches = matches
                .iter()
                .map(|matched| {
                    let key = self
                        .pattern_ids
                        .get(matched.pattern.as_str())
                        .copied()
                        .ok_or_else(|| {
                            ExecutionError::Sink("match referenced an unknown pattern".into())
                        })?;
                    Ok((key, matched.start, matched.end))
                })
                .collect::<Result<Vec<_>, ExecutionError>>()?;
            for derived in contiguous_chains(&component_matches) {
                let key = self.add_compound(&derived.components)?;
                derived_matches.push(lingrep_core::fsm::Match {
                    pattern: CompactString::new(self.patterns[key.index()].name.as_str()),
                    start: derived.start,
                    end: derived.end,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                });
            }
            derived_matches.sort_by(|left, right| {
                left.start
                    .cmp(&right.start)
                    .then_with(|| left.end.cmp(&right.end))
                    .then_with(|| left.pattern.cmp(&right.pattern))
            });
            published_matches = Cow::Owned(derived_matches);
            if let Some(state) = self.sections.take() {
                self.sections = Some(match state {
                    SectionScanState::Building(mut builder) => {
                        match builder.ensure_patterns(self.patterns.len()) {
                            Ok(()) => SectionScanState::Building(builder),
                            Err(refusal) => SectionScanState::Refused(refusal),
                        }
                    }
                    SectionScanState::Refused(refusal) => SectionScanState::Refused(refusal),
                });
            }
        }
        let counting_surfaces = matches!(self.sections, Some(SectionScanState::Building(_)));
        for matched in published_matches.iter() {
            let pattern = matched.pattern.to_string();
            let pattern_key = self.pattern_ids.get(&pattern).copied().ok_or_else(|| {
                ExecutionError::Sink("match referenced an unknown pattern".into())
            })?;
            self.patterns[pattern_key.index()].exact_total += 1;
            if !self.matched_scratch.contains(&pattern_key) {
                self.matched_scratch.push(pattern_key);
            }
            if counting_surfaces {
                self.stage_surface(pattern_key, _sentence, matched.start..matched.end);
            }
            // Reservoir sampling (Algorithm R). `exact_total` was just
            // incremented, so it is this match's 1-based arrival number among
            // the pattern's matches — the denominator the algorithm needs, and
            // already exact because publication was never capped.
            let seen = self.patterns[pattern_key.index()].exact_total;
            let held = self.reservoirs.get(&pattern_key).map_or(0, Vec::len);
            let slot = if held < self.max_refs {
                Some(None)
            } else {
                let draw = reservoir_draw(pattern_key, seen);
                let candidate = usize::try_from(draw % seen).unwrap_or(usize::MAX);
                (candidate < self.max_refs).then_some(Some(candidate))
            };
            if let Some(replaces) = slot {
                let captures = matched
                    .captures
                    .iter()
                    .map(|capture| {
                        let identity = (pattern_key, capture.name.clone(), capture.path.clone());
                        let capture_key = match self.capture_keys.get(&identity).copied() {
                            Some(key) => key,
                            None => self.insert_capture_descriptor(
                                pattern_key,
                                capture.name.clone(),
                                capture.path.clone(),
                            )?,
                        };
                        Ok(CaptureSpanRef {
                            capture_key,
                            span: capture.start..capture.end,
                        })
                    })
                    .collect::<Result<Vec<_>, ExecutionError>>()?;
                let reference = MatchRef {
                    sentence_index,
                    span: matched.start..matched.end,
                    pattern_key,
                    captures: captures.into_boxed_slice(),
                    graph_bindings: matched.graph_bindings.clone(),
                    lexical: matched.lexical.as_ref().map(|lexical| {
                        crate::snapshot::LexicalMatchRef {
                            term: lexical.term.to_string(),
                            matched_by: lexical
                                .matched_by
                                .iter()
                                .map(|attribute| attribute.pattern_key().to_owned())
                                .collect(),
                        }
                    }),
                };
                let reservoir = self.reservoirs.entry(pattern_key).or_default();
                match replaces {
                    // Filling: the reservoir is short, so this match is kept
                    // outright and `refs` grows by one.
                    None => {
                        reservoir.push(self.refs.len());
                        self.refs.push(reference);
                    }
                    // Full: overwrite in place, so `refs` never exceeds
                    // `patterns × max_refs` however long the corpus runs.
                    Some(index) => {
                        let position = reservoir[index];
                        self.refs[position] = reference;
                    }
                }
            }
        }
        if let Some(SectionScanState::Building(builder)) = &mut self.sections {
            // The scratch is moved out and back so the builder can borrow the
            // sink mutably while reading it.
            let matched = std::mem::take(&mut self.matched_scratch);
            let surfaces = std::mem::take(&mut self.surface_scratch);
            let result = builder.observe(sentence_index, &matched, &surfaces[..self.surface_used]);
            self.matched_scratch = matched;
            self.surface_scratch = surfaces;
            result.map_err(sink_error)?;
        }
        // `max_refs` caps retained detail, not publication: every match still
        // increments the pattern's exact total, which the snapshot publishes.
        // That is also what makes the reservoir above unbiased — the cap never
        // hid a match from the denominator.
        //
        // The runner validates this count against the supplied slice; a derived
        // compound is output built from these records, not one of them.
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_tally::{SpanEvent, SpanEvents};
    use lingrep_core::execution::MatchSink;

    /// A sink whose vocabulary is one label, `methods`, at slot 0.
    fn sink_with_sections(
        max_refs: usize,
        patterns: &[&str],
        events: Vec<SpanEvent>,
    ) -> SnapshotSink {
        let coverage = vec![lingrep_corpus::SectionCoverage {
            label: "methods".to_string(),
            coverage: 1,
        }];
        let builder =
            crate::section_tally::build(&coverage, patterns.len(), SpanEvents::from_events(events))
                .expect("the fixture vocabulary fits");
        let data = patterns
            .iter()
            .map(|name| ((*name).to_string(), Box::default()))
            .collect();
        SnapshotSink::new(
            max_refs,
            data,
            SectionScanState::Building(Box::new(builder)),
        )
        .unwrap()
    }

    /// A match over a token span, so a fixture can give the sink a surface to
    /// join rather than the empty sentence the older tests use.
    fn a_span_match(pattern: &str, span: std::ops::Range<usize>) -> lingrep_core::fsm::Match {
        lingrep_core::fsm::Match {
            pattern: CompactString::from(pattern),
            start: span.start,
            end: span.end,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    fn a_match(pattern: &str) -> lingrep_core::fsm::Match {
        lingrep_core::fsm::Match {
            pattern: CompactString::from(pattern),
            start: 0,
            end: 1,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    fn location(ordinal: u64) -> SentenceLocation {
        SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: ordinal,
            document_id: lingrep_core::corpus::DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: ordinal,
            source_name: "".into(),
        }
    }

    fn match_with_capture(pattern: &str, capture_name: &str) -> lingrep_core::fsm::Match {
        lingrep_core::fsm::Match {
            pattern: CompactString::from(pattern),
            start: 0,
            end: 1,
            captures: vec![lingrep_core::fsm::Capture {
                name: CompactString::from(capture_name),
                path: CompactString::from(capture_name),
                start: 0,
                end: 1,
            }]
            .into_boxed_slice(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    /// Drives `count` matching sentences through a sink capped at `max_refs`
    /// and returns the sentence ordinals it ended up retaining, in the order
    /// the KWIC pane would show them.
    fn retained_ordinals(max_refs: usize, count: u64) -> Vec<u64> {
        let mut sink = sink_with_sections(max_refs, &["p"], vec![]);
        for ordinal in 0..count {
            sink.push(&location(ordinal), &(&[] as &[TokenInfo]), &[a_match("p")])
                .unwrap();
        }
        sink.finish().unwrap();
        sink.refs
            .iter()
            .map(|reference| reference.sentence_index)
            .collect()
    }

    /// Below the cap nothing is sampled, so this is the old behaviour exactly:
    /// every match retained, in corpus order. The property that makes the
    /// change safe for every corpus small enough not to need it.
    #[test]
    fn a_corpus_under_the_cap_retains_every_match_in_corpus_order() {
        assert_eq!(retained_ordinals(10, 6), vec![0, 1, 2, 3, 4, 5]);
    }

    /// The defect this fixes: the pane used to show the first N matches, so a
    /// reader of a date- or document-ordered corpus was reading its opening
    /// and could not tell.
    #[test]
    fn a_corpus_over_the_cap_retains_beyond_the_opening_matches() {
        let retained = retained_ordinals(20, 4_000);
        assert_eq!(retained.len(), 20, "the cap still bounds what is retained");
        assert!(
            retained.iter().any(|ordinal| *ordinal >= 3_000),
            "a prefix could never reach the last quarter: {retained:?}"
        );
        assert!(
            retained.iter().any(|ordinal| *ordinal < 1_000),
            "nor should it abandon the opening: {retained:?}"
        );
        assert!(
            retained.windows(2).all(|pair| pair[0] < pair[1]),
            "the sample is presented in corpus order: {retained:?}"
        );
    }

    /// A screenshot, a permalink and a citation all assume the pane shows the
    /// same rows tomorrow, so the draw is a pure function of the arrival
    /// number rather than a step of a mutable generator.
    #[test]
    fn the_retained_sample_is_the_same_on_every_rerun() {
        assert_eq!(retained_ordinals(15, 900), retained_ordinals(15, 900));
    }

    /// Two patterns matching in lockstep must not retain the same rows: the
    /// pattern index is mixed into the draw precisely so their samples are
    /// independent rather than a shared coin flip.
    #[test]
    fn patterns_matching_together_still_sample_independently() {
        let mut sink = sink_with_sections(12, &["p", "q"], vec![]);
        for ordinal in 0..1_500u64 {
            sink.push(
                &location(ordinal),
                &(&[] as &[TokenInfo]),
                &[a_match("p"), a_match("q")],
            )
            .unwrap();
        }
        sink.finish().unwrap();
        let for_pattern = |index: usize| {
            sink.refs
                .iter()
                .filter(|reference| reference.pattern_key.index() == index)
                .map(|reference| reference.sentence_index)
                .collect::<Vec<_>>()
        };
        let (p, q) = (for_pattern(0), for_pattern(1));
        assert_eq!(p.len(), 12);
        assert_eq!(q.len(), 12);
        assert_ne!(p, q, "identical samples would mean one shared draw");
        assert_eq!(
            sink.refs.len(),
            24,
            "`refs` is bounded by patterns × max_refs however long the run"
        );
    }

    #[test]
    fn every_published_sentence_is_counted_whether_or_not_it_matched() {
        let mut sink = sink_with_sections(5, &["p"], vec![]);
        sink.push(&location(0), &(&[] as &[TokenInfo]), &[])
            .unwrap();
        sink.push(&location(1), &(&[] as &[TokenInfo]), &[a_match("p")])
            .unwrap();
        assert_eq!(
            sink.observed_sentences, 2,
            "the denominator counts sentences, not matches"
        );
    }

    #[test]
    fn exact_section_counts_outlive_the_retained_sample() {
        // max_refs = 1, but three sentences match: the tally must see all three.
        let mut sink = sink_with_sections(
            1,
            &["p"],
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 3,
            }],
        );
        for ordinal in 0..3 {
            sink.push(&location(ordinal), &(&[] as &[TokenInfo]), &[a_match("p")])
                .unwrap();
        }
        assert_eq!(sink.refs.len(), 1, "the retained prefix is still capped");
        sink.finish().unwrap();
        let Some(SectionAvailability::Available(tally)) = &sink.finalized_sections else {
            panic!("sections available after finish");
        };
        assert_eq!(
            tally.count(PatternKey::new(0).unwrap(), 0),
            3,
            "the count the retained refs cannot produce"
        );
    }

    #[test]
    fn derived_contiguous_chains_have_independent_totals_refs_and_section_rows() {
        let coverage = vec![lingrep_corpus::SectionCoverage {
            label: "methods".to_string(),
            coverage: 1,
        }];
        let builder = crate::section_tally::build(
            &coverage,
            3,
            SpanEvents::from_events(vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 1,
            }]),
        )
        .expect("the fixture vocabulary fits");
        let category = |value: &str| {
            vec![(
                "category".into(),
                PatternMetadataValue::String(value.into()),
            )]
            .into_boxed_slice()
        };
        let mut sink = SnapshotSink::new_with_compounds(
            10,
            vec![
                ("a".into(), category("S1A")),
                ("b".into(), category("S1B")),
                ("c".into(), category("S2A")),
            ],
            SectionScanState::Building(Box::new(builder)),
            true,
            CompoundCategory::Compound,
        )
        .unwrap();
        let matches = [
            a_span_match("a", 0..1),
            a_span_match("b", 1..2),
            a_span_match("c", 2..3),
        ];
        let sentence = [
            TokenInfo::default().with_orth("甲"),
            TokenInfo::default().with_orth("乙"),
            TokenInfo::default().with_orth("丙"),
        ];
        sink.push(&location(0), &sentence, &matches).unwrap();
        assert_eq!(sink.patterns.len(), 6, "three authored and three derived");
        let derived = sink
            .patterns
            .iter()
            .filter(|pattern| pattern.name.starts_with("複合項目"))
            .map(|pattern| (pattern.key, pattern.name.clone(), pattern.exact_total))
            .collect::<Vec<_>>();
        assert_eq!(derived.len(), 3);
        assert!(derived.iter().all(|(_, _, total)| *total == 1));
        assert!(derived.iter().all(|(key, _, _)| {
            let pattern = &sink.patterns[key.index()];
            pattern.metadata.iter().any(|(field, value)| {
                field == "category" && value == &PatternMetadataValue::String("複合項目".into())
            })
        }));
        sink.finish().unwrap();
        let Some(SectionAvailability::Available(tally)) = &sink.finalized_sections else {
            panic!("sections should include dynamic compound rows");
        };
        for (key, _, _) in derived {
            assert_eq!(tally.count(key, 0), 1);
            let surfaces = tally.surfaces(key);
            let (surface, total) = surfaces.variations().next().expect("derived surface row");
            assert_eq!(total, 1);
            assert_eq!(surfaces.count(surface, 0), Some(1));
        }
        assert_eq!(
            sink.refs.len(),
            6,
            "derived matches have independent details"
        );
    }

    #[test]
    fn derived_category_can_project_the_component_categories() {
        let metadata = |category: &str| {
            vec![(
                "category".into(),
                PatternMetadataValue::String(category.into()),
            )]
            .into_boxed_slice()
        };
        let mut sink = SnapshotSink::new_with_compounds(
            1,
            vec![
                ("a".into(), metadata("S1A")),
                ("b".into(), metadata("S2G2")),
            ],
            SectionScanState::Refused(SectionRefusal {
                labels: 0,
                patterns: 2,
            }),
            true,
            CompoundCategory::SourceCategories,
        )
        .unwrap();
        let key = sink
            .add_compound(&[PatternKey::new(0).unwrap(), PatternKey::new(1).unwrap()])
            .unwrap();
        let pattern = &sink.patterns[key.index()];
        assert!(pattern.metadata.iter().any(|(field, value)| {
            field == "category" && value == &PatternMetadataValue::String("S1A ＋ S2G2".into())
        }));
        assert!(pattern.metadata.iter().any(|(field, value)| {
            field == "source_categories"
                && value == &PatternMetadataValue::String("S1A ＋ S2G2".into())
        }));
    }

    /// The guarantee an operator lowering `--max-kwic-lines-per-pattern`
    /// depends on: the cap bounds what is **kept for reading** and nothing
    /// that is **reported**. Every exact total and every section count is
    /// identical at a cap of 2 and a cap of 1,000 over the same match stream,
    /// so the per-pattern distribution, the all-patterns comparison and the
    /// dendrogram built from it are all counts over the whole corpus whatever
    /// the cap says. Deployments run this at 20; the test asserts the property
    /// rather than a value.
    #[test]
    fn the_retained_cap_changes_the_sample_and_no_published_count() {
        // Three sections laid end to end, and two patterns matching in
        // overlapping but different parts of the corpus.
        let spans = vec![
            SpanEvent {
                slot: 0,
                first: 0,
                end: 40,
            },
            SpanEvent {
                slot: 0,
                first: 40,
                end: 90,
            },
        ];
        let run = |max_refs: usize| {
            let mut sink = sink_with_sections(max_refs, &["p", "q"], spans.clone());
            for ordinal in 0..90u64 {
                let mut matches = vec![a_match("p")];
                if ordinal % 3 == 0 {
                    matches.push(a_match("q"));
                }
                sink.push(&location(ordinal), &(&[] as &[TokenInfo]), &matches)
                    .unwrap();
            }
            let retained = sink.refs.len();
            let totals = sink
                .patterns
                .iter()
                .map(|pattern| pattern.exact_total)
                .collect::<Vec<_>>();
            sink.finish().unwrap();
            let Some(SectionAvailability::Available(tally)) = &sink.finalized_sections else {
                panic!("sections available after finish");
            };
            let counts = (0..2)
                .map(|pattern| tally.count(PatternKey::new(pattern).unwrap(), 0))
                .collect::<Vec<_>>();
            (retained, totals, counts)
        };

        let (small_retained, small_totals, small_counts) = run(2);
        let (large_retained, large_totals, large_counts) = run(1_000);

        assert_eq!(small_totals, vec![90, 30], "every match reaches the total");
        assert_eq!(small_totals, large_totals, "the cap cannot move a total");
        assert_eq!(
            small_counts, large_counts,
            "nor a section count, which is what the distributions and the \
             dendrogram are computed from"
        );
        // Without this the assertions above could hold because the cap did
        // nothing at all.
        assert_eq!(small_retained, 4, "two patterns held to two hits each");
        assert_eq!(
            large_retained, 120,
            "an uncapped run keeps every hit, so the caps really did differ"
        );
    }

    #[test]
    fn a_decreasing_sentence_ordinal_fails_the_run() {
        let mut sink = sink_with_sections(5, &["p"], vec![]);
        sink.push(&location(4), &(&[] as &[TokenInfo]), &[])
            .unwrap();
        assert!(matches!(
            sink.push(&location(3), &(&[] as &[TokenInfo]), &[]),
            Err(ExecutionError::Sink(_))
        ));
    }

    #[test]
    fn sentences_are_counted_even_when_sections_are_unavailable() {
        let refusal = SectionRefusal {
            labels: 1,
            patterns: 1,
        };
        let mut sink = SnapshotSink::new(
            5,
            vec![("p".to_string(), Box::default())],
            SectionScanState::Refused(refusal.clone()),
        )
        .unwrap();
        sink.push(&location(0), &(&[] as &[TokenInfo]), &[])
            .unwrap();
        assert_eq!(
            sink.observed_sentences, 1,
            "the publication gate must hold in the refused branch too"
        );
        sink.finish().unwrap();
        assert_eq!(
            sink.finalized_sections,
            Some(SectionAvailability::VocabularyTooLarge(refusal)),
            "finish resolves the refused branch too, so resolve_scan has a value"
        );
    }

    #[test]
    fn snapshot_sink_caps_refs_but_counts_every_match() {
        let mut sink = sink_with_sections(2, &["p"], vec![]);
        for sentence in 0..3 {
            sink.push(
                &location(sentence),
                &(&[] as &[TokenInfo]),
                &[match_with_capture("p", "grp")],
            )
            .unwrap();
        }
        assert_eq!(sink.patterns[0].exact_total, 3);
        assert_eq!(sink.refs.len(), 2);
    }

    #[test]
    fn snapshot_sink_interns_repeated_captures_once() {
        let mut sink = sink_with_sections(10, &["p"], vec![]);
        for sentence in 0..2 {
            sink.push(
                &location(sentence),
                &(&[] as &[TokenInfo]),
                &[match_with_capture("p", "grp")],
            )
            .unwrap();
        }
        assert_eq!(sink.capture_catalog.len(), 1);
        assert_eq!(sink.refs.len(), 2);
        assert!(sink.refs.iter().all(|r| r.captures.len() == 1));
    }

    #[test]
    fn snapshot_sink_rejects_a_match_for_an_unknown_pattern() {
        let mut sink = sink_with_sections(10, &["p"], vec![]);
        let error = sink
            .push(
                &location(0),
                &(&[] as &[TokenInfo]),
                &[match_with_capture("other", "grp")],
            )
            .unwrap_err();
        assert!(matches!(
            error,
            lingrep_core::execution::ExecutionError::Sink(_)
        ));
    }

    /// The surface axis is the KWIC key, joined the same way. A test that
    /// asserted some other string would pass while the pane's variation and
    /// the counted row named two different things.
    #[test]
    fn a_matched_surface_is_counted_under_the_sections_active_at_its_sentence() {
        let mut sink = sink_with_sections(
            8,
            &["p"],
            vec![SpanEvent {
                slot: 0,
                first: 0,
                end: 2,
            }],
        );
        let tokens = vec![
            TokenInfo::default().with_orth("走っ"),
            TokenInfo::default().with_orth("た"),
            TokenInfo::default().with_orth("走る"),
        ];
        // Sentences 0 and 1 are inside `methods`; sentence 2 is outside it.
        sink.push(&location(0), &tokens.as_slice(), &[a_span_match("p", 0..2)])
            .unwrap();
        sink.push(&location(1), &tokens.as_slice(), &[a_span_match("p", 2..3)])
            .unwrap();
        sink.push(&location(2), &tokens.as_slice(), &[a_span_match("p", 0..2)])
            .unwrap();
        sink.finish().unwrap();

        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections else {
            panic!("the fixture vocabulary was admitted");
        };
        let key = PatternKey::new(0).unwrap();
        let surfaces = tally.surfaces(key);
        assert_eq!(surfaces.count("走った", 0), Some(1), "methods");
        assert_eq!(surfaces.count("走った", 1), Some(1), "no section");
        assert_eq!(surfaces.count("走る", 0), Some(1));
        assert_eq!(surfaces.count("走る", 1), Some(0));
        // The two-dimensional table is unchanged by the new axis, and the
        // surfaces of one pattern sum to it here because no sentence carried
        // two of them.
        assert_eq!(tally.count(key, 0), 2);
        assert_eq!(tally.count(key, 1), 1);
    }

    /// One sentence, one surface, twice. The unit is the sentence in both
    /// tables, so a repeat cannot make the surface row exceed the row above it.
    #[test]
    fn one_sentence_carrying_a_surface_twice_counts_it_once() {
        let mut sink = sink_with_sections(8, &["p"], Vec::new());
        let tokens = vec![
            TokenInfo::default().with_orth("あ"),
            TokenInfo::default().with_orth("あ"),
        ];
        sink.push(
            &location(0),
            &tokens.as_slice(),
            &[a_span_match("p", 0..1), a_span_match("p", 1..2)],
        )
        .unwrap();
        sink.finish().unwrap();

        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections else {
            panic!("the fixture vocabulary was admitted");
        };
        let key = PatternKey::new(0).unwrap();
        assert_eq!(tally.surfaces(key).count("あ", 1), Some(1));
        assert_eq!(tally.count(key, 1), 1, "and the pattern's own row agrees");
    }
}
