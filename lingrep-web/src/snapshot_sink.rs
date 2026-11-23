use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use compact_str::CompactString;
use lingrep_core::execution::{ExecutionError, MatchSink, SentenceLocation, SinkControl};
use lingrep_core::token_match::AttributeKey;
#[cfg(test)]
use lingrep_core::token_match::TokenInfo;

use crate::derived_patterns::{
    CompoundLimitExceeded, compound_name, contiguous_chains, max_derived_patterns,
};
#[cfg(test)]
use crate::revision::ErrorCategory;
use crate::section_tally::{
    SectionAvailability, SectionRefusal, SectionTallyBuilder, SectionTallyError,
};
use crate::snapshot::{
    CaptureDescriptor, CaptureKey, CaptureSpanRef, CompletedSnapshot, CompoundCategory, MatchRef,
    PatternDescriptor, PatternKey, PatternMetadataValue, PatternOrigin, UnavailableAttributes,
};

#[derive(Debug)]
pub(crate) enum SectionScanState {
    Building(Box<SectionTallyBuilder>),
    Refused(SectionRefusal),
}

/// Lifecycle phase of the per-scan section tally.
///
/// The span channel receiver lives only inside `Collecting`. Transitioning to
/// `Finalized` or `Discarded` drops that receiver before the producer is
/// joined, and `Finalized` is the single state that `into_completed_snapshot`
/// accepts for publication.
#[derive(Debug)]
enum SectionTallyState {
    Collecting(Box<SectionTallyBuilder>),
    Refused(SectionRefusal),
    Finalized(SectionAvailability),
    Discarded,
}

impl From<SectionScanState> for SectionTallyState {
    fn from(state: SectionScanState) -> Self {
        match state {
            SectionScanState::Building(builder) => Self::Collecting(builder),
            SectionScanState::Refused(refusal) => Self::Refused(refusal),
        }
    }
}

impl SectionTallyState {
    fn is_collecting(&self) -> bool {
        matches!(self, Self::Collecting(_))
    }

    fn ensure_patterns(&mut self, count: usize) {
        if let Self::Collecting(builder) = self
            && let Err(refusal) = builder.ensure_patterns(count)
        {
            *self = Self::Refused(refusal);
        }
    }

    fn observe(
        &mut self,
        sentence_index: u64,
        matched: &[PatternKey],
        surfaces: &[(PatternKey, String)],
    ) -> Result<(), ExecutionError> {
        if let Self::Collecting(builder) = self {
            builder
                .observe(sentence_index, matched, surfaces)
                .map_err(sink_error)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ExecutionError> {
        let finalized = match std::mem::replace(self, Self::Discarded) {
            Self::Collecting(builder) => {
                SectionAvailability::Available(builder.finish().map_err(sink_error)?)
            }
            Self::Refused(refusal) => SectionAvailability::VocabularyTooLarge(refusal),
            Self::Finalized(_) | Self::Discarded => {
                return Err(ExecutionError::Sink(
                    "section state was already finalized".into(),
                ));
            }
        };
        *self = Self::Finalized(finalized);
        Ok(())
    }

    fn discard(&mut self) {
        *self = Self::Discarded;
    }

    #[cfg(test)]
    fn finalized(&self) -> Option<&SectionAvailability> {
        match self {
            Self::Finalized(availability) => Some(availability),
            _ => None,
        }
    }

    fn into_finalized(self) -> Option<SectionAvailability> {
        match self {
            Self::Finalized(availability) => Some(availability),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct ResultIndexRecorder {
    journal: crate::result_index::ResultIndexWriter,
    preview: Vec<crate::result_index::ResultOccurrence>,
    preview_bytes: usize,
    publication_attributes: Arc<[AttributeKey]>,
}

/// Lifecycle phase of the optional full-result index and its live preview.
#[derive(Debug, Default)]
enum ResultIndexState {
    #[default]
    Absent,
    Recording(ResultIndexRecorder),
    Finalized {
        index: Arc<crate::result_index::ResultIndex>,
        preview: Vec<crate::result_index::ResultOccurrence>,
        publication_attributes: Arc<[AttributeKey]>,
    },
}

impl ResultIndexState {
    fn open(
        publication_attributes: Arc<[AttributeKey]>,
    ) -> Result<Self, crate::result_index::ResultIndexError> {
        let journal =
            crate::result_index::ResultIndexWriter::new(Arc::clone(&publication_attributes))?;
        Ok(Self::Recording(ResultIndexRecorder {
            journal,
            preview: Vec::new(),
            preview_bytes: 0,
            publication_attributes,
        }))
    }

    fn is_recording(&self) -> bool {
        matches!(self, Self::Recording(_))
    }

    fn record(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        reference: MatchRef,
        seen: u64,
        published: Option<lingrep_core::fsm::RecordView<'_>>,
    ) -> Result<(), ExecutionError> {
        let Self::Recording(recorder) = self else {
            return Ok(());
        };
        let mut occurrence = crate::result_index::ResultOccurrence::from_reference(
            location.document_ordinal,
            reference,
        );
        occurrence.document_sentence_ordinal = location.document_sentence_ordinal;
        occurrence.source_name = location.source_name.clone();
        occurrence.source_metadata = location.source_metadata.clone();
        occurrence.char_span = character_span(sentence, occurrence.reference.span.clone());
        occurrence.capture_char_spans = occurrence
            .reference
            .captures
            .iter()
            .map(|capture| character_span(sentence, capture.span.clone()))
            .collect();
        if let Some(published) = published {
            occurrence.unit = published.unit.map(str::to_owned);
            occurrence.key = published.key.map(<[_]>::to_vec);
            occurrence.lexical_key = published.lexical_key.map(<[_]>::to_vec);
            occurrence.tags = published.tags.to_vec();
        }
        recorder
            .journal
            .append_occurrence(&mut occurrence)
            .map_err(|error| ExecutionError::Sink(error.to_string()))?;
        let preview_bytes = occurrence_bytes(&occurrence);
        if seen <= 30 && recorder.preview_bytes.saturating_add(preview_bytes) <= 4 * 1024 * 1024 {
            recorder.preview.push(occurrence);
            recorder.preview_bytes += preview_bytes;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), crate::result_index::ResultIndexError> {
        if !matches!(self, Self::Recording(_)) {
            return Ok(());
        }
        if let Self::Recording(recorder) = std::mem::take(self) {
            let index = recorder.journal.finish()?;
            *self = Self::Finalized {
                index,
                preview: recorder.preview,
                publication_attributes: recorder.publication_attributes,
            };
        }
        Ok(())
    }

    fn discard(&mut self) {
        *self = Self::Absent;
    }

    fn preview(&self) -> &[crate::result_index::ResultOccurrence] {
        match self {
            Self::Absent => &[],
            Self::Recording(recorder) => &recorder.preview,
            Self::Finalized { preview, .. } => preview,
        }
    }

    fn publication_attributes(&self) -> Arc<[AttributeKey]> {
        match self {
            Self::Absent => Arc::from([]),
            Self::Recording(recorder) => Arc::clone(&recorder.publication_attributes),
            Self::Finalized {
                publication_attributes,
                ..
            } => Arc::clone(publication_attributes),
        }
    }

    #[cfg(test)]
    fn index(&self) -> Option<&Arc<crate::result_index::ResultIndex>> {
        match self {
            Self::Finalized { index, .. } => Some(index),
            _ => None,
        }
    }

    fn into_index(self) -> Option<Arc<crate::result_index::ResultIndex>> {
        match self {
            Self::Finalized { index, .. } => Some(index),
            _ => None,
        }
    }
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

/// Immutable scan-level configuration and pass-through metadata supplied when
/// a [`SnapshotSink`] is constructed and carried straight into the published
/// [`CompletedSnapshot`].
#[derive(Debug)]
pub(crate) struct SnapshotSinkMeta {
    pub(crate) search_scope: crate::search_scope::SearchScope,
    pub(crate) admission: lingrep_core::execution::SentenceAdmission,
    pub(crate) excluded_sentences: u64,
    pub(crate) cli_arguments: String,
    pub(crate) overlaps: lingrep_core::fsm::policy_program::Overlaps,
    pub(crate) unavailable_attributes: Option<UnavailableAttributes>,
    pub(crate) vectors_binding: Option<crate::patterns::BoundWordVectors>,
    pub(crate) derive_compounds: bool,
    pub(crate) compound_category: CompoundCategory,
    pub(crate) document_reach: bool,
}

impl Default for SnapshotSinkMeta {
    fn default() -> Self {
        Self {
            search_scope: Default::default(),
            admission: Default::default(),
            excluded_sentences: 0,
            cli_arguments: String::new(),
            overlaps: Default::default(),
            unavailable_attributes: None,
            vectors_binding: None,
            derive_compounds: false,
            compound_category: CompoundCategory::Compound,
            document_reach: false,
        }
    }
}

#[derive(Debug)]
pub(crate) enum SnapshotSinkInitError {
    PatternSpaceExhausted,
    ResultIndex(crate::result_index::ResultIndexError),
}

/// Authored and dynamically derived compound patterns, exact match counts, and
/// same-span coincidences between compounds and authored patterns.
#[derive(Debug)]
struct PatternCatalog {
    pattern_ids: HashMap<String, PatternKey>,
    compound_ids: HashMap<Vec<PatternKey>, PatternKey>,
    /// Same-span coincidences per `(compound, authored)` pair, written into
    /// the compound descriptors by `finalize_coincidences`.
    coincidences: HashMap<(PatternKey, PatternKey), u64>,
    patterns: Vec<PatternDescriptor>,
    compound_category: CompoundCategory,
}

impl PatternCatalog {
    fn new(
        pattern_data: Vec<(String, Box<[(String, PatternMetadataValue)]>)>,
        mut metadata_conflicts: BTreeMap<String, Box<[String]>>,
        compound_category: CompoundCategory,
    ) -> Result<Self, SnapshotSinkInitError> {
        let mut pattern_ids = HashMap::with_capacity(pattern_data.len());
        let mut patterns = Vec::with_capacity(pattern_data.len());
        for (index, (name, metadata)) in pattern_data.into_iter().enumerate() {
            let key = PatternKey::new(index).ok_or(SnapshotSinkInitError::PatternSpaceExhausted)?;
            let conflicts = metadata_conflicts.remove(&name).unwrap_or_default();
            pattern_ids.insert(name.clone(), key);
            patterns.push(PatternDescriptor {
                key,
                name,
                exact_total: 0,
                metadata_conflicts: conflicts,
                metadata,
                origin: PatternOrigin::Authored { chained_total: 0 },
            });
        }
        Ok(Self {
            pattern_ids,
            compound_ids: HashMap::new(),
            coincidences: HashMap::new(),
            patterns,
            compound_category,
        })
    }

    fn key_of(&self, name: &str) -> Result<PatternKey, ExecutionError> {
        self.pattern_ids
            .get(name)
            .copied()
            .ok_or_else(|| ExecutionError::Sink("match referenced an unknown pattern".into()))
    }

    fn record_match(&mut self, key: PatternKey, in_chain: bool) -> u64 {
        let descriptor = &mut self.patterns[key.index()];
        descriptor.exact_total += 1;
        if in_chain && let PatternOrigin::Authored { chained_total } = &mut descriptor.origin {
            *chained_total += 1;
        }
        descriptor.exact_total
    }

    fn add_compound(&mut self, components: &[PatternKey]) -> Result<PatternKey, ExecutionError> {
        if let Some(key) = self.compound_ids.get(components) {
            return Ok(*key);
        }
        if self.compound_ids.len() >= max_derived_patterns() {
            return Err(compound_limit(CompoundLimitExceeded));
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
        let name = compound_name(&component_names, |candidate| {
            self.pattern_ids.contains_key(candidate)
        });
        let key = PatternKey::new(self.patterns.len()).ok_or_else(|| {
            ExecutionError::Sink("derived pattern catalog exceeded u32 identity space".into())
        })?;
        let mut metadata: Vec<(String, PatternMetadataValue)> =
            vec![("category".into(), PatternMetadataValue::String(category))];
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
            metadata_conflicts: Box::default(),
            metadata: metadata.into_boxed_slice(),
            origin: PatternOrigin::Compound {
                components: components.into(),
                coincident: Box::default(),
            },
        });
        self.compound_ids.insert(components.to_vec(), key);
        Ok(key)
    }

    fn record_coincidence(&mut self, compound: PatternKey, authored: PatternKey) {
        *self.coincidences.entry((compound, authored)).or_default() += 1;
    }

    fn finalize_coincidences(&mut self) {
        let mut by_compound = HashMap::<PatternKey, Vec<(PatternKey, u64)>>::new();
        for (&(compound, authored), &count) in &self.coincidences {
            by_compound
                .entry(compound)
                .or_default()
                .push((authored, count));
        }
        for (compound, mut entries) in by_compound {
            entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            if let PatternOrigin::Compound { coincident, .. } =
                &mut self.patterns[compound.index()].origin
            {
                *coincident = entries.into_boxed_slice();
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ReservoirSlot {
    /// Filling: the reservoir is short, so this match appends to `refs`.
    Append,
    /// Full: overwrite the existing position in `refs` held by this reservoir slot.
    Replace(usize),
}

/// Per-pattern Algorithm R reservoir retaining at most `max_refs` matches per
/// pattern while keeping retained memory bounded by `patterns × max_refs`.
#[derive(Debug)]
struct MatchReservoir {
    refs: Vec<MatchRef>,
    max_refs: usize,
    /// Per pattern, the positions in `refs` this pattern's reservoir occupies.
    ///
    /// Replaces a bare retained-count: a reservoir must be able to *overwrite*
    /// a slot it already holds, which a count cannot express.
    slots: HashMap<PatternKey, Vec<usize>>,
}

impl MatchReservoir {
    fn new(max_refs: usize) -> Self {
        Self {
            refs: Vec::new(),
            max_refs,
            slots: HashMap::new(),
        }
    }

    fn decide(&self, pattern_key: PatternKey, seen: u64) -> Option<ReservoirSlot> {
        let held = self.slots.get(&pattern_key).map_or(0, Vec::len);
        if held < self.max_refs {
            Some(ReservoirSlot::Append)
        } else {
            let draw = reservoir_draw(pattern_key, seen);
            let candidate = usize::try_from(draw % seen).unwrap_or(usize::MAX);
            (candidate < self.max_refs).then_some(ReservoirSlot::Replace(candidate))
        }
    }

    fn insert(&mut self, pattern_key: PatternKey, slot: ReservoirSlot, reference: MatchRef) {
        let reservoir = self.slots.entry(pattern_key).or_default();
        match slot {
            ReservoirSlot::Append => {
                reservoir.push(self.refs.len());
                self.refs.push(reference);
            }
            ReservoirSlot::Replace(index) => {
                let position = reservoir[index];
                self.refs[position] = reference;
            }
        }
    }

    /// A reservoir fills in arrival order and is then overwritten in place,
    /// so its slots end up in no order at all. Restore corpus order, which
    /// is what the KWIC pane's reading order means and what `match_ref`'s
    /// ordinal indexes. Under the cap this sort is a no-op — nothing was
    /// replaced, so the vector is already ascending — which is why a corpus
    /// that fits retains exactly what it always did.
    fn sort_corpus_order(&mut self) {
        self.refs.sort_by(|left, right| {
            left.sentence_index
                .cmp(&right.sentence_index)
                .then_with(|| left.span.start.cmp(&right.span.start))
                .then_with(|| left.span.end.cmp(&right.span.end))
                .then_with(|| left.pattern_key.index().cmp(&right.pattern_key.index()))
        });
    }
}

/// Deduplicated capture descriptors shared across retained references and the
/// result index.
#[derive(Debug, Default)]
struct CaptureInterner {
    keys: HashMap<(PatternKey, CompactString, CompactString), CaptureKey>,
    catalog: Vec<CaptureDescriptor>,
}

impl CaptureInterner {
    fn intern_spans(
        &mut self,
        pattern_key: PatternKey,
        captures: &[lingrep_core::fsm::Capture],
    ) -> Result<Box<[CaptureSpanRef]>, ExecutionError> {
        captures
            .iter()
            .map(|capture| {
                let identity = (pattern_key, capture.name.clone(), capture.path.clone());
                let capture_key = match self.keys.get(&identity).copied() {
                    Some(key) => key,
                    None => self.insert_descriptor(
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
            .collect::<Result<Vec<_>, ExecutionError>>()
            .map(Vec::into_boxed_slice)
    }

    fn insert_descriptor(
        &mut self,
        pattern_key: PatternKey,
        name: CompactString,
        path: CompactString,
    ) -> Result<CaptureKey, ExecutionError> {
        let key = CaptureKey::new(self.catalog.len()).ok_or_else(|| {
            ExecutionError::Sink("capture catalog exceeded u32 identity space".into())
        })?;
        self.catalog.push(CaptureDescriptor {
            key,
            pattern_key,
            name: name.to_string(),
            path: path.to_string(),
        });
        self.keys.insert((pattern_key, name, path), key);
        Ok(key)
    }
}

/// Reused per-sentence scratch buffers so neither distinct-pattern tracking
/// nor section-tally surface joining allocates in steady state.
#[derive(Debug, Default)]
struct SentenceScratch {
    matched: Vec<PatternKey>,
    /// The distinct `(pattern, surface)` pairs of one sentence, for the
    /// section tally's surface axis.
    ///
    /// Reused rather than rebuilt: `surface_used` is the live prefix and the
    /// `String`s past it keep their capacity, so joining one surface per match
    /// over a whole corpus allocates only while the longest surface grows.
    surfaces: Vec<(PatternKey, String)>,
    surface_used: usize,
}

impl SentenceScratch {
    fn reset(&mut self) {
        self.matched.clear();
        self.surface_used = 0;
    }

    fn note_pattern(&mut self, pattern_key: PatternKey) {
        if !self.matched.contains(&pattern_key) {
            self.matched.push(pattern_key);
        }
    }

    /// Joins one match's surface into the scratch, keeping the prefix
    /// distinct.
    ///
    /// The join is `lingrep_core::token_match::push_display_text`, and
    /// `inspection::kwic_orth_split` builds the KWIC key a reader clicks from
    /// the same per-token texts and spaces. Two derivations of one string is a
    /// hazard: a divergence would silently leave a selectable variation with
    /// no counted row.
    fn stage_surface(
        &mut self,
        pattern: PatternKey,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        span: std::ops::Range<usize>,
    ) {
        if self.surface_used == self.surfaces.len() {
            self.surfaces.push((pattern, String::new()));
        }
        let (slot_pattern, joined) = &mut self.surfaces[self.surface_used];
        *slot_pattern = pattern;
        joined.clear();
        lingrep_core::token_match::push_display_text(joined, sentence, span);
        // A sentence carrying one surface of one pattern twice is one
        // sentence, which is the unit the section table counts in.
        let duplicate = self.surfaces[..self.surface_used]
            .iter()
            .any(|(held, text)| *held == pattern && text == &self.surfaces[self.surface_used].1);
        if !duplicate {
            self.surface_used += 1;
        }
    }

    fn matched(&self) -> &[PatternKey] {
        &self.matched
    }

    fn surfaces(&self) -> &[(PatternKey, String)] {
        &self.surfaces[..self.surface_used]
    }
}

#[derive(Debug)]
pub(crate) struct SnapshotSink {
    meta: SnapshotSinkMeta,
    catalog: PatternCatalog,
    reservoir: MatchReservoir,
    captures: CaptureInterner,
    result_index: ResultIndexState,
    sections: SectionTallyState,
    document_tally: Option<DocumentTally>,
    observed_sentences: u64,
    /// Observed sentences that published at least one match, counted once
    /// per sentence however many matches or compounds it holds.
    matched_sentences: u64,
    last_ordinal: Option<u64>,
    scratch: SentenceScratch,
}

impl SnapshotSink {
    pub(crate) fn new_for_scan(
        max_refs: usize,
        pattern_data: Vec<(String, Box<[(String, PatternMetadataValue)]>)>,
        metadata_conflicts: BTreeMap<String, Box<[String]>>,
        sections: SectionScanState,
        result_index_attributes: Option<Arc<[AttributeKey]>>,
        meta: SnapshotSinkMeta,
    ) -> Result<Self, SnapshotSinkInitError> {
        let catalog =
            PatternCatalog::new(pattern_data, metadata_conflicts, meta.compound_category)?;
        let result_index = match result_index_attributes {
            Some(attributes) => {
                ResultIndexState::open(attributes).map_err(SnapshotSinkInitError::ResultIndex)?
            }
            None => ResultIndexState::Absent,
        };
        let document_tally = meta.document_reach.then(DocumentTally::default);
        Ok(Self {
            meta,
            catalog,
            reservoir: MatchReservoir::new(max_refs),
            captures: CaptureInterner::default(),
            result_index,
            sections: SectionTallyState::from(sections),
            document_tally,
            observed_sentences: 0,
            matched_sentences: 0,
            last_ordinal: None,
            scratch: SentenceScratch::default(),
        })
    }

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
            false,
        )
    }

    #[cfg(test)]
    pub(crate) fn new_with_compounds(
        max_refs: usize,
        pattern_data: Vec<(String, Box<[(String, PatternMetadataValue)]>)>,
        sections: SectionScanState,
        derive_compounds: bool,
        compound_category: CompoundCategory,
        document_reach: bool,
    ) -> Result<Self, ErrorCategory> {
        Self::new_for_scan(
            max_refs,
            pattern_data,
            BTreeMap::new(),
            sections,
            None,
            SnapshotSinkMeta {
                derive_compounds,
                compound_category,
                document_reach,
                ..SnapshotSinkMeta::default()
            },
        )
        .map_err(|_| ErrorCategory::Execution)
    }

    #[cfg(test)]
    fn enable_result_index(
        &mut self,
        publication_attributes: Arc<[AttributeKey]>,
    ) -> Result<(), crate::result_index::ResultIndexError> {
        self.result_index = ResultIndexState::open(publication_attributes)?;
        Ok(())
    }

    pub(crate) fn observed_sentences(&self) -> u64 {
        self.observed_sentences
    }

    pub(crate) fn excluded_sentences(&self) -> u64 {
        self.meta.excluded_sentences
    }

    pub(crate) fn patterns(&self) -> &[PatternDescriptor] {
        &self.catalog.patterns
    }

    pub(crate) fn preview(&self) -> &[crate::result_index::ResultOccurrence] {
        self.result_index.preview()
    }

    pub(crate) fn publication_attributes(&self) -> Arc<[AttributeKey]> {
        self.result_index.publication_attributes()
    }

    pub(crate) fn capture_catalog(&self) -> &[CaptureDescriptor] {
        &self.captures.catalog
    }

    #[cfg(test)]
    pub(crate) fn refs(&self) -> &[MatchRef] {
        &self.reservoir.refs
    }

    #[cfg(test)]
    pub(crate) fn patterns_mut(&mut self) -> &mut [PatternDescriptor] {
        &mut self.catalog.patterns
    }

    #[cfg(test)]
    pub(crate) fn finalized_sections(&self) -> Option<&SectionAvailability> {
        self.sections.finalized()
    }

    #[cfg(test)]
    pub(crate) fn result_index(&self) -> Option<&Arc<crate::result_index::ResultIndex>> {
        self.result_index.index()
    }

    #[cfg(test)]
    pub(crate) fn into_result_index(self) -> Option<Arc<crate::result_index::ResultIndex>> {
        self.result_index.into_index()
    }

    #[cfg(test)]
    pub(crate) fn unavailable_attributes(&self) -> Option<&UnavailableAttributes> {
        self.meta.unavailable_attributes.as_ref()
    }

    /// Completed run: drain the span stream through `End`, consume the
    /// builder, and leave the lean result in `SectionTallyState::Finalized`.
    ///
    /// Consuming the builder drops the channel receiver, which is what
    /// releases a producer parked on a full channel before it is joined.
    /// Called from the scan's blocking thread, never from the async
    /// coordinator: the production span source blocks on its channel.
    pub(crate) fn finish(&mut self) -> Result<(), ExecutionError> {
        if let Some(tally) = &mut self.document_tally
            && tally.current.take().is_some()
        {
            tally.close_document(&self.catalog.patterns);
        }
        self.sections.finish()?;
        self.reservoir.sort_corpus_order();
        self.catalog.finalize_coincidences();
        Ok(())
    }

    pub(crate) fn finish_result_index(
        &mut self,
    ) -> Result<(), crate::result_index::ResultIndexError> {
        self.result_index.finish()
    }

    /// Cancelled or failed run: drop the builder undrained. Nothing will be
    /// published, but the receiver must still go away before the producer is
    /// joined, or the join waits on a task parked on a full channel.
    pub(crate) fn discard_sections(&mut self) {
        self.sections.discard();
        self.result_index.discard();
    }

    /// Consumes a finalized sink to produce the immutable [`CompletedSnapshot`].
    ///
    /// Returns `None` if `finish` never ran (or sections were discarded), which
    /// acts as the lifecycle guard preventing an unfinalized scan from publishing.
    pub(crate) fn into_completed_snapshot(
        self,
        revision: crate::revision::RevisionId,
        corpus_identity: crate::corpus_runtime::CorpusIdentity,
        source: Arc<str>,
        oversized_omissions: Option<lingrep_core::execution::OversizedSentenceOmissions>,
    ) -> Option<CompletedSnapshot> {
        let documents = self.document_summary();
        let sections = self.sections.into_finalized()?;
        Some(CompletedSnapshot {
            search_scope: self.meta.search_scope,
            admission: self.meta.admission,
            cli_arguments: self.meta.cli_arguments,
            excluded_sentences: self.meta.excluded_sentences,
            revision,
            corpus_identity,
            source,
            documents,
            derive_compounds: self.meta.derive_compounds,
            overlaps: self.meta.overlaps,
            compound_category: self.meta.compound_category,
            patterns: self.catalog.patterns.into_boxed_slice(),
            match_refs: self.reservoir.refs.into_boxed_slice(),
            result_index: self.result_index.into_index(),
            capture_catalog: self.captures.catalog.into_boxed_slice(),
            sections,
            observed_sentences: self.observed_sentences,
            matched_sentences: self.matched_sentences,
            oversized_omissions,
            unavailable_attributes: self.meta.unavailable_attributes,
            vectors_binding: self.meta.vectors_binding,
        })
    }

    /// Derives this sentence's compound matches and reports, per base match,
    /// whether it is a component of any chain. Registers new compounds and
    /// their same-span coincidences, and widens the section tally to them.
    fn derive_chains(
        &mut self,
        matches: &[lingrep_core::fsm::Match],
    ) -> Result<(Vec<lingrep_core::fsm::Match>, Vec<bool>), ExecutionError> {
        let component_matches = matches
            .iter()
            .map(|matched| {
                let key = self.catalog.key_of(matched.pattern.as_str())?;
                Ok((key, matched.start, matched.end))
            })
            .collect::<Result<Vec<_>, ExecutionError>>()?;
        let derived = contiguous_chains(&component_matches).map_err(compound_limit)?;
        let mut compounds = Vec::with_capacity(derived.chains.len());
        for chain in derived.chains {
            let key = self.catalog.add_compound(&chain.components)?;
            // Published matches arrive in canonical `(start, end, pattern)`
            // order, so the authored matches on exactly this span are one
            // contiguous run.
            let same_span = component_matches
                .partition_point(|&(_, start, end)| (start, end) < (chain.start, chain.end))..;
            for &(authored, start, end) in &component_matches[same_span] {
                if (start, end) != (chain.start, chain.end) {
                    break;
                }
                self.catalog.record_coincidence(key, authored);
            }
            compounds.push(lingrep_core::fsm::Match {
                pattern: CompactString::new(self.catalog.patterns[key.index()].name.as_str()),
                start: chain.start,
                end: chain.end,
                captures: Box::default(),
                graph_bindings: Box::default(),
                interpretations: Box::default(),
                lexical: None,
            });
        }
        self.sections.ensure_patterns(self.catalog.patterns.len());
        Ok((compounds, derived.in_chain))
    }

    /// Counts one published match and retains it when its pattern's
    /// reservoir admits it.
    fn observe_match(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matched: &lingrep_core::fsm::Match,
        in_chain: bool,
        counting_surfaces: bool,
        published: Option<lingrep_core::fsm::RecordView<'_>>,
    ) -> Result<(), ExecutionError> {
        let sentence_index = location.corpus_sentence_ordinal;
        let pattern_key = self.catalog.key_of(matched.pattern.as_str())?;
        // Reservoir sampling (Algorithm R). `record_match` increments
        // `exact_total` and returns this match's 1-based arrival number among
        // the pattern's matches — the denominator the algorithm needs, and
        // already exact because publication was never capped.
        let seen = self.catalog.record_match(pattern_key, in_chain);
        self.scratch.note_pattern(pattern_key);
        if counting_surfaces {
            self.scratch
                .stage_surface(pattern_key, sentence, matched.start..matched.end);
        }
        let slot = self.reservoir.decide(pattern_key, seen);
        let recording = self.result_index.is_recording();
        if slot.is_none() && !recording {
            return Ok(());
        }
        let captures = self.captures.intern_spans(pattern_key, &matched.captures)?;
        let reference = MatchRef {
            sentence_index,
            span: matched.start..matched.end,
            pattern_key,
            captures,
            graph_bindings: matched.graph_bindings.clone(),
            interpretations: matched.interpretations.clone(),
            lexical: matched
                .lexical
                .as_ref()
                .map(|lexical| crate::snapshot::LexicalMatchRef {
                    term: lexical.term.to_string(),
                    matched_by: lexical
                        .matched_by
                        .iter()
                        .map(|attribute| attribute.pattern_key().to_owned())
                        .collect(),
                }),
            in_chain,
        };
        match (recording, slot) {
            (true, Some(slot)) => {
                self.result_index
                    .record(location, sentence, reference.clone(), seen, published)?;
                self.reservoir.insert(pattern_key, slot, reference);
            }
            (true, None) => {
                self.result_index
                    .record(location, sentence, reference, seen, published)?;
            }
            (false, Some(slot)) => {
                self.reservoir.insert(pattern_key, slot, reference);
            }
            (false, None) => {}
        }
        Ok(())
    }
}

/// Per-document occurrence counts arise by diffing each touched pattern's
/// `exact_total` against `total_at_close` when `current` advances, so only
/// patterns matched in the open document are visited on document close.
#[derive(Debug, Default)]
struct DocumentTally {
    current: Option<u64>,
    documents_scanned: u64,
    per_pattern: Vec<PatternDocumentState>,
    touched: Vec<PatternKey>,
}

#[derive(Debug, Default, Clone)]
struct PatternDocumentState {
    last_document: Option<u64>,
    documents: u64,
    total_at_close: u64,
    top_counts: [u64; TOP_DOCUMENTS],
}

const TOP_DOCUMENTS: usize = 5;

impl DocumentTally {
    fn touch(&mut self, document: u64, matched: &[PatternKey]) {
        for &key in matched {
            if self.per_pattern.len() <= key.index() {
                self.per_pattern
                    .resize_with(key.index() + 1, PatternDocumentState::default);
            }
            let state = &mut self.per_pattern[key.index()];
            if state.last_document != Some(document) {
                state.last_document = Some(document);
                state.documents += 1;
                self.touched.push(key);
            }
        }
    }

    fn close_document(&mut self, patterns: &[PatternDescriptor]) {
        for key in self.touched.drain(..) {
            let state = &mut self.per_pattern[key.index()];
            let total = patterns[key.index()].exact_total;
            let count = total - state.total_at_close;
            state.total_at_close = total;
            if let Some(slot) = state.top_counts.iter_mut().min()
                && count > *slot
            {
                *slot = count;
            }
        }
    }

    fn summary(&self, pattern_count: usize) -> crate::snapshot::DocumentSummary {
        let by_pattern = (0..pattern_count)
            .map(|index| {
                let state = self.per_pattern.get(index).cloned().unwrap_or_default();
                let mut top = state
                    .top_counts
                    .into_iter()
                    .filter(|count| *count > 0)
                    .collect::<Vec<_>>();
                top.sort_unstable_by(|a, b| b.cmp(a));
                crate::snapshot::DocumentReach {
                    documents: state.documents,
                    top_counts: top.into_boxed_slice(),
                }
            })
            .collect();
        crate::snapshot::DocumentSummary {
            documents_scanned: self.documents_scanned,
            by_pattern,
        }
    }
}

impl SnapshotSink {
    pub(crate) fn document_summary(&self) -> Option<crate::snapshot::DocumentSummary> {
        self.document_tally
            .as_ref()
            .map(|tally| tally.summary(self.catalog.patterns.len()))
    }
}

fn compound_limit(error: CompoundLimitExceeded) -> ExecutionError {
    ExecutionError::Sink(error.to_string())
}

fn sink_error(error: SectionTallyError) -> ExecutionError {
    ExecutionError::Sink(error.to_string())
}

fn character_span(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: std::ops::Range<usize>,
) -> Option<std::ops::Range<u32>> {
    if span.is_empty() {
        return None;
    }
    let first = sentence.char_span(span.start)?;
    let last = sentence.char_span(span.end - 1)?;
    Some(first.start..last.end)
}

fn occurrence_bytes(row: &crate::result_index::ResultOccurrence) -> usize {
    reference_bytes(&row.reference)
        + row.source_name.len()
        + row.source_metadata.as_ref().map_or(0, |metadata| {
            serde_json::to_vec(metadata)
                .map_or(usize::MAX / 8, |bytes| bytes.len().saturating_mul(8))
        })
        + std::mem::size_of_val(row.capture_char_spans.as_ref())
        + row.unit.as_ref().map_or(0, String::len)
        + row.key.as_ref().map_or(0, |fields| {
            std::mem::size_of_val(fields.as_slice())
                + fields
                    .iter()
                    .map(|field| field.name.len() + field.value.len())
                    .sum::<usize>()
        })
        + row.lexical_key.as_ref().map_or(0, |fields| {
            std::mem::size_of_val(fields.as_slice())
                + fields
                    .iter()
                    .map(|field| {
                        field.name.len()
                            + match &field.value {
                                lingrep_core::fsm::LexicalProjectedKeyValue::Scalar(value) => {
                                    value.len()
                                }
                                lingrep_core::fsm::LexicalProjectedKeyValue::Segments(values) => {
                                    std::mem::size_of_val(values.as_slice())
                                        + values.iter().map(String::len).sum::<usize>()
                                }
                            }
                    })
                    .sum::<usize>()
        })
        + std::mem::size_of_val(row.tags.as_slice())
        + row
            .tags
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
}

fn reference_bytes(reference: &MatchRef) -> usize {
    use lingrep_core::fsm::GraphBinding;
    std::mem::size_of::<crate::result_index::ResultOccurrence>()
        + std::mem::size_of_val(reference.captures.as_ref())
        + std::mem::size_of_val(reference.graph_bindings.as_ref())
        + reference
            .interpretations
            .iter()
            .map(lingrep_core::fsm::Interpretation::retained_bytes)
            .sum::<usize>()
        + reference
            .graph_bindings
            .iter()
            .map(|binding| match binding {
                GraphBinding::Node {
                    name, attributes, ..
                } => {
                    name.len()
                        + std::mem::size_of_val(attributes.as_ref())
                        + attributes
                            .iter()
                            .map(|attribute| attribute.name.as_str().len() + attribute.value.len())
                            .sum::<usize>()
                }
                GraphBinding::Count { name, .. } | GraphBinding::Measure { name, .. } => name.len(),
                GraphBinding::Edge { name, relation, .. } => name.len() + relation.len(),
            })
            .sum::<usize>()
        + reference.lexical.as_ref().map_or(0, |lexical| {
            std::mem::size_of_val(lexical)
                + lexical.term.len()
                + std::mem::size_of_val(lexical.matched_by.as_ref())
                + lexical.matched_by.iter().map(String::len).sum::<usize>()
        })
}

impl SnapshotSink {
    fn push_matches(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[lingrep_core::fsm::Match],
        records: Option<&[lingrep_core::fsm::PublishedRecord]>,
    ) -> Result<SinkControl, ExecutionError> {
        let sentence_index = location.corpus_sentence_ordinal;
        // The section sweep's cursor cannot move backwards, so an out-of-order
        // ordinal would miscount silently rather than merely produce unsorted
        // data. Reject it whether or not sections are available.
        if let Some(previous) = self.last_ordinal
            && sentence_index < previous
        {
            return Err(ExecutionError::Sink(format!(
                "sentence ordinals must not decrease: {previous} then {sentence_index}"
            )));
        }
        self.last_ordinal = Some(sentence_index);
        if let Some(tally) = &mut self.document_tally
            && tally.current != Some(location.document_ordinal)
        {
            if let Some(previous) = tally.current {
                if location.document_ordinal < previous {
                    return Err(ExecutionError::Sink(format!(
                        "document ordinals must not decrease: {previous} then {}",
                        location.document_ordinal
                    )));
                }
                tally.close_document(&self.catalog.patterns);
            }
            tally.current = Some(location.document_ordinal);
            tally.documents_scanned += 1;
        }
        self.observed_sentences += 1;
        if !matches.is_empty() {
            self.matched_sentences += 1;
        }
        self.scratch.reset();
        let derived = if self.meta.derive_compounds {
            Some(self.derive_chains(matches)?)
        } else {
            None
        };
        let counting_surfaces = self.sections.is_collecting();
        match &derived {
            Some((compounds, in_chain)) => {
                let mut published = matches
                    .iter()
                    .zip(in_chain.iter().copied())
                    .enumerate()
                    .map(|(index, (matched, in_chain))| {
                        (
                            matched,
                            in_chain,
                            records.map(|records| {
                                lingrep_core::fsm::RecordView::published(&records[index])
                            }),
                        )
                    })
                    .chain(compounds.iter().map(|matched| (matched, false, None)))
                    .collect::<Vec<_>>();
                published.sort_by_key(|(matched, _, _)| {
                    (matched.start, matched.end, matched.pattern.as_str())
                });
                for (matched, in_chain, record) in published {
                    self.observe_match(
                        location,
                        sentence,
                        matched,
                        in_chain,
                        counting_surfaces,
                        record,
                    )?;
                }
            }
            None => {
                for (index, matched) in matches.iter().enumerate() {
                    self.observe_match(
                        location,
                        sentence,
                        matched,
                        false,
                        counting_surfaces,
                        records.map(|records| {
                            lingrep_core::fsm::RecordView::published(&records[index])
                        }),
                    )?;
                }
            }
        }
        if let Some(tally) = &mut self.document_tally {
            tally.touch(location.document_ordinal, self.scratch.matched());
        }
        self.sections.observe(
            sentence_index,
            self.scratch.matched(),
            self.scratch.surfaces(),
        )?;
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

impl MatchSink for SnapshotSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[lingrep_core::fsm::Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.push_matches(location, sentence, matches, None)
    }

    fn push_records(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<lingrep_core::fsm::PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        if !self.result_index.is_recording() {
            let matches = records
                .into_iter()
                .map(lingrep_core::fsm::PublishedRecord::into_match)
                .collect::<Vec<_>>();
            return self.push_matches(location, sentence, &matches, None);
        }
        let matches = records
            .iter()
            .map(|record| lingrep_core::fsm::Match {
                pattern: record.name.clone(),
                start: record.start,
                end: record.end,
                captures: record.components.clone(),
                graph_bindings: record.raw.graph_bindings.clone(),
                interpretations: record.raw.interpretations.clone(),
                lexical: record.raw.lexical.clone(),
            })
            .collect::<Vec<_>>();
        self.push_matches(location, sentence, &matches, Some(&records))
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
            interpretations: Box::default(),
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
            interpretations: Box::default(),
            lexical: None,
        }
    }

    fn location(ordinal: u64) -> SentenceLocation {
        SentenceLocation {
            lookaround_divisions: None,
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
            interpretations: Box::default(),
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
        sink.refs()
            .iter()
            .map(|reference| reference.sentence_index)
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn indexed_policy_records_preserve_derived_fields_surviving_captures_and_offsets() {
        use lingrep_core::fsm::{
            LexicalProjectedKeyField, LexicalProjectedKeyValue, ProjectedKeyField, PublishedRecord,
        };
        let (index, expected, captures) = tokio::task::spawn_blocking(|| {
            let mut sink = sink_with_sections(1, &["p"], vec![]);
            sink.enable_result_index(std::sync::Arc::from([])).unwrap();
            let words = vec![TokenInfo::default().with_orth("猫")];
            let schema = lingrep_core::token_match::TokenSchema::from_keys(["text"]).unwrap();
            let projection = schema
                .bind_projection([lingrep_core::token_match::AttributeKey::new("text").unwrap()])
                .unwrap();
            let sentence =
                lingrep_core::token_match::BoundTokenSentence::project(&projection, &words)
                    .unwrap()
                    .with_char_offsets(Some(std::sync::Arc::new(vec![
                        lingrep_core::token_match::CharSpan::new(2, 3),
                    ])));
            let record = PublishedRecord {
                raw: match_with_capture("p", "item"),
                start: 0,
                end: 1,
                name: "p".into(),
                unit: Some("normalized".into()),
                key: Some(vec![ProjectedKeyField {
                    name: "item.lemma".into(),
                    value: "猫".into(),
                }]),
                lexical_key: Some(vec![LexicalProjectedKeyField {
                    name: "lemma".into(),
                    value: LexicalProjectedKeyValue::Segments(vec!["ネコ".into(), "猫".into()]),
                }]),
                tags: vec![("kind".into(), "animal".into())],
                components: match_with_capture("p", "item").captures,
                overrides: vec![],
            };
            let mut location = location(7);
            location.document_ordinal = 4;
            location.document_sentence_ordinal = 2;
            location.source_name = "book.md".into();
            sink.push_records(&location, &sentence, vec![record.clone()])
                .unwrap();
            let expected = sink.preview()[0].clone();
            assert!(sink.result_index().is_none());
            sink.finish().unwrap();
            sink.finish_result_index().unwrap();
            let captures = sink.capture_catalog().to_vec();
            (
                sink.into_result_index().unwrap(),
                (expected, record),
                captures,
            )
        })
        .await
        .unwrap();
        let row = index.reference(0).await.unwrap().unwrap();
        assert_eq!(row, expected.0);
        assert_eq!(
            (row.document_ordinal, row.document_sentence_ordinal),
            (4, 2)
        );
        assert_eq!(row.reference.sentence_index, 7);
        assert_eq!(row.source_name.as_ref(), "book.md");
        assert_eq!(row.char_span, Some(2..3));
        assert_eq!(row.capture_char_spans.as_ref(), &[Some(2..3)]);
        assert_eq!(row.unit, expected.1.unit);
        assert_eq!(row.key, expected.1.key);
        assert_eq!(row.lexical_key, expected.1.lexical_key);
        assert_eq!(row.tags, expected.1.tags);
        assert_eq!(
            captures[row.reference.captures[0].capture_key.index()].name,
            "item"
        );
    }

    /// Below the cap nothing is sampled: every match is retained, in corpus
    /// order.
    #[test]
    fn a_corpus_under_the_cap_retains_every_match_in_corpus_order() {
        assert_eq!(retained_ordinals(10, 6), vec![0, 1, 2, 3, 4, 5]);
    }

    /// Over the cap the pane does not show only the first N matches, which in a
    /// date- or document-ordered corpus would be its opening.
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
            sink.refs()
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
            sink.refs().len(),
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
        assert_eq!(sink.matched_sentences, 1);
    }

    #[test]
    fn a_sentence_is_covered_once_however_many_matches_it_publishes() {
        for derive_compounds in [false, true] {
            let mut sink = SnapshotSink::new_with_compounds(
                5,
                vec![("a".into(), Box::default()), ("b".into(), Box::default())],
                SectionScanState::Refused(SectionRefusal {
                    labels: 0,
                    patterns: 2,
                }),
                derive_compounds,
                CompoundCategory::Compound,
                false,
            )
            .unwrap();
            let tokens = [
                TokenInfo::default().with_orth("甲"),
                TokenInfo::default().with_orth("乙"),
            ];
            sink.push(
                &location(0),
                &tokens,
                &[a_span_match("a", 0..1), a_span_match("b", 1..2)],
            )
            .unwrap();
            sink.push(&location(1), &tokens, &[a_span_match("a", 0..1)])
                .unwrap();
            sink.push(&location(2), &tokens, &[]).unwrap();
            assert_eq!(
                (sink.matched_sentences, sink.observed_sentences),
                (2, 3),
                "derive_compounds={derive_compounds}"
            );
        }
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
        assert_eq!(sink.refs().len(), 1, "the retained prefix is still capped");
        sink.finish().unwrap();
        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections() else {
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
            false,
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
        assert_eq!(sink.patterns().len(), 6, "three authored and three derived");
        let derived = sink
            .patterns()
            .iter()
            .filter(|pattern| pattern.name.starts_with("複合項目"))
            .map(|pattern| (pattern.key, pattern.name.clone(), pattern.exact_total))
            .collect::<Vec<_>>();
        assert_eq!(derived.len(), 3);
        assert!(derived.iter().all(|(_, _, total)| *total == 1));
        assert!(derived.iter().all(|(key, _, _)| {
            let pattern = &sink.patterns()[key.index()];
            pattern.metadata.iter().any(|(field, value)| {
                field == "category" && value == &PatternMetadataValue::String("複合項目".into())
            })
        }));
        let key = |index: usize| PatternKey::new(index).unwrap();
        let component_chains = derived
            .iter()
            .map(|(key, _, _)| sink.patterns()[key.index()].components().unwrap().to_vec())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            component_chains,
            std::collections::HashSet::from([
                vec![key(0), key(1)],
                vec![key(1), key(2)],
                vec![key(0), key(1), key(2)],
            ]),
            "each compound carries its ordered component keys"
        );
        assert!(
            sink.patterns()[..3]
                .iter()
                .all(|pattern| pattern.components().is_none()),
            "authored patterns have no components"
        );
        assert!(
            sink.patterns()[..3]
                .iter()
                .all(|pattern| { pattern.origin == PatternOrigin::Authored { chained_total: 1 } }),
            "each base occurrence is counted once however many chains nest it"
        );
        sink.finish().unwrap();
        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections() else {
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
            sink.refs().len(),
            6,
            "derived matches have independent details"
        );
    }

    /// `こと` `が` chains with `できる` on exactly the span the authored
    /// `ことができる` covers, so every such occurrence is a same-span
    /// coincidence; a sentence where the longer pattern did not fire is not.
    #[test]
    fn a_compound_counts_its_same_span_coincidences_per_authored_pattern() {
        let mut sink = SnapshotSink::new_with_compounds(
            10,
            vec![
                ("こと".into(), Box::default()),
                ("できる".into(), Box::default()),
                ("ことができる".into(), Box::default()),
            ],
            SectionScanState::Refused(SectionRefusal {
                labels: 0,
                patterns: 3,
            }),
            true,
            CompoundCategory::Compound,
            false,
        )
        .unwrap();
        let sentence = [
            TokenInfo::default().with_orth("こと"),
            TokenInfo::default().with_orth("が"),
            TokenInfo::default().with_orth("できる"),
        ];
        sink.push(
            &location(0),
            &sentence,
            &[
                a_span_match("こと", 0..2),
                a_span_match("ことができる", 0..3),
                a_span_match("できる", 2..3),
            ],
        )
        .unwrap();
        sink.push(
            &location(1),
            &sentence,
            &[a_span_match("こと", 0..2), a_span_match("できる", 2..3)],
        )
        .unwrap();
        sink.finish().unwrap();
        let compound = sink
            .patterns()
            .iter()
            .find(|pattern| pattern.components().is_some())
            .expect("one compound");
        assert_eq!(compound.exact_total, 2);
        assert_eq!(
            compound.origin,
            PatternOrigin::Compound {
                components: Box::from([PatternKey::new(0).unwrap(), PatternKey::new(1).unwrap()]),
                coincident: Box::from([(PatternKey::new(2).unwrap(), 1)]),
            }
        );
    }

    /// `ことができる` ends where `だろう` starts in the first sentence and
    /// stands alone in the second, so one of its two retained occurrences is
    /// a chain component and the other is not.
    #[test]
    fn a_retained_occurrence_records_whether_it_is_a_chain_component() {
        let patterns = || {
            vec![
                ("ことができる".into(), Box::default()),
                ("だろう".into(), Box::default()),
            ]
        };
        let refused = || {
            SectionScanState::Refused(SectionRefusal {
                labels: 0,
                patterns: 2,
            })
        };
        let chained = [
            TokenInfo::default().with_orth("こと"),
            TokenInfo::default().with_orth("が"),
            TokenInfo::default().with_orth("できる"),
            TokenInfo::default().with_orth("だろう"),
        ];
        let alone = [
            TokenInfo::default().with_orth("こと"),
            TokenInfo::default().with_orth("が"),
            TokenInfo::default().with_orth("できる"),
        ];
        let push_both = |sink: &mut SnapshotSink| {
            sink.push(
                &location(0),
                &chained,
                &[
                    a_span_match("ことができる", 0..3),
                    a_span_match("だろう", 3..4),
                ],
            )
            .unwrap();
            sink.push(&location(1), &alone, &[a_span_match("ことができる", 0..3)])
                .unwrap();
            sink.finish().unwrap();
        };

        let mut sink = SnapshotSink::new_with_compounds(
            10,
            patterns(),
            refused(),
            true,
            CompoundCategory::Compound,
            false,
        )
        .unwrap();
        push_both(&mut sink);
        let compound = sink
            .patterns()
            .iter()
            .find(|pattern| pattern.components().is_some())
            .expect("one compound");
        assert_eq!(compound.name, "複合項目〔ことができる ＋ だろう〕");
        assert_eq!(compound.exact_total, 1);
        assert_eq!(
            sink.patterns()[0].origin,
            PatternOrigin::Authored { chained_total: 1 }
        );
        assert_eq!(sink.patterns()[0].exact_total, 2);
        let flags = sink
            .refs()
            .iter()
            .map(|reference| {
                (
                    reference.sentence_index,
                    reference.pattern_key.index(),
                    reference.in_chain,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            flags,
            vec![(0, 0, true), (0, 2, false), (0, 1, true), (1, 0, false)],
            "components are flagged, the compound's own match and the standalone occurrence are not"
        );

        let mut sink = SnapshotSink::new_with_compounds(
            10,
            patterns(),
            refused(),
            false,
            CompoundCategory::Compound,
            false,
        )
        .unwrap();
        push_both(&mut sink);
        assert_eq!(sink.patterns().len(), 2);
        assert!(
            sink.refs().iter().all(|reference| !reference.in_chain),
            "without derivation no occurrence is a chain component"
        );
    }

    fn located(ordinal: u64, document: u64) -> SentenceLocation {
        SentenceLocation {
            lookaround_divisions: None,
            document_ordinal: document,
            document_id: lingrep_core::corpus::DocumentId::new(document),
            ..location(ordinal)
        }
    }

    #[test]
    fn document_reach_and_top_counts_match_a_per_document_oracle() {
        let corpus: Vec<(u64, Vec<usize>)> = vec![
            (0, vec![0, 1]),
            (0, vec![0]),
            (0, vec![0, 0, 1]),
            (1, vec![]),
            (2, vec![1]),
            (3, vec![0, 1]),
            (3, vec![0]),
            (4, vec![0]),
            (5, vec![0]),
            (6, vec![0, 0, 0, 0]),
            (7, vec![0]),
        ];
        let names = ["a", "b", "never"];
        for derive_compounds in [false, true] {
            let mut sink = SnapshotSink::new_with_compounds(
                3,
                names
                    .iter()
                    .map(|name| ((*name).to_string(), Box::default()))
                    .collect(),
                SectionScanState::Refused(SectionRefusal {
                    labels: 0,
                    patterns: 3,
                }),
                derive_compounds,
                CompoundCategory::Compound,
                true,
            )
            .unwrap();
            let mut oracle = std::collections::BTreeMap::<(String, u64), u64>::new();
            let tokens = [
                TokenInfo::default().with_orth("甲"),
                TokenInfo::default().with_orth("乙"),
                TokenInfo::default().with_orth("丙"),
                TokenInfo::default().with_orth("丁"),
            ];
            for (ordinal, (document, hits)) in corpus.iter().enumerate() {
                let matches = hits
                    .iter()
                    .enumerate()
                    .map(|(offset, &pattern)| a_span_match(names[pattern], offset..offset + 1))
                    .collect::<Vec<_>>();
                sink.push(&located(ordinal as u64, *document), &tokens, &matches)
                    .unwrap();
                for matched in &matches {
                    *oracle
                        .entry((matched.pattern.to_string(), *document))
                        .or_default() += 1;
                }
                if derive_compounds {
                    let component_matches = matches
                        .iter()
                        .map(|m| (sink.catalog.pattern_ids[m.pattern.as_str()], m.start, m.end))
                        .collect::<Vec<_>>();
                    for chain in contiguous_chains(&component_matches).unwrap().chains {
                        let name = sink.patterns()
                            [sink.catalog.compound_ids[&chain.components].index()]
                        .name
                        .clone();
                        *oracle.entry((name, *document)).or_default() += 1;
                    }
                }
            }
            sink.finish().unwrap();
            let summary = sink.document_summary().expect("the run counted documents");
            assert_eq!(
                summary.documents_scanned, 8,
                "documents with a scanned sentence"
            );
            assert_eq!(summary.by_pattern.len(), sink.patterns().len());
            for pattern in sink.patterns() {
                let mut counts = oracle
                    .iter()
                    .filter(|((name, _), _)| *name == pattern.name)
                    .map(|(_, count)| *count)
                    .collect::<Vec<_>>();
                counts.sort_unstable_by(|a, b| b.cmp(a));
                let reach = summary.reach(pattern.key).unwrap();
                assert_eq!(
                    reach.documents,
                    counts.len() as u64,
                    "{}: documents reached (derive_compounds={derive_compounds})",
                    pattern.name
                );
                assert_eq!(
                    reach.top_counts.to_vec(),
                    counts.into_iter().take(5).collect::<Vec<_>>(),
                    "{}: top counts",
                    pattern.name
                );
                assert!(reach.top_counts.iter().sum::<u64>() <= pattern.exact_total);
            }
            let never = &sink.patterns()[2];
            assert_eq!(
                summary.reach(never.key).unwrap(),
                &crate::snapshot::DocumentReach {
                    documents: 0,
                    top_counts: Box::default()
                }
            );
        }
    }

    #[test]
    fn a_run_without_the_option_keeps_no_document_state() {
        let mut sink =
            SnapshotSink::new(5, vec![("p".into(), Box::default())], no_refusal()).unwrap();
        sink.push(&located(0, 0), &(&[] as &[TokenInfo]), &[a_match("p")])
            .unwrap();
        sink.push(&located(1, 1), &(&[] as &[TokenInfo]), &[a_match("p")])
            .unwrap();
        sink.finish().unwrap();
        assert!(sink.document_tally.is_none());
        assert!(sink.document_summary().is_none());
    }

    #[test]
    fn a_single_document_corpus_puts_everything_in_the_top_document() {
        let mut sink = SnapshotSink::new_with_compounds(
            5,
            vec![("p".into(), Box::default())],
            no_refusal(),
            false,
            CompoundCategory::Compound,
            true,
        )
        .unwrap();
        for ordinal in 0..4 {
            sink.push(
                &located(ordinal, 0),
                &(&[] as &[TokenInfo]),
                &[a_match("p")],
            )
            .unwrap();
        }
        sink.finish().unwrap();
        let summary = sink.document_summary().unwrap();
        assert_eq!(summary.documents_scanned, 1);
        assert_eq!(
            summary.reach(PatternKey::new(0).unwrap()).unwrap(),
            &crate::snapshot::DocumentReach {
                documents: 1,
                top_counts: Box::from([4])
            }
        );
    }

    #[test]
    fn non_contiguous_document_ordinals_fail_the_run() {
        let mut sink = SnapshotSink::new_with_compounds(
            5,
            vec![("p".into(), Box::default())],
            no_refusal(),
            false,
            CompoundCategory::Compound,
            true,
        )
        .unwrap();
        sink.push(&located(0, 3), &(&[] as &[TokenInfo]), &[])
            .unwrap();
        sink.push(&located(1, 4), &(&[] as &[TokenInfo]), &[])
            .unwrap();
        let error = sink
            .push(&located(2, 3), &(&[] as &[TokenInfo]), &[])
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("document ordinals must not decrease"),
            "{error}"
        );
    }

    fn no_refusal() -> SectionScanState {
        SectionScanState::Refused(SectionRefusal {
            labels: 0,
            patterns: 1,
        })
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
            false,
        )
        .unwrap();
        let key = sink
            .catalog
            .add_compound(&[PatternKey::new(0).unwrap(), PatternKey::new(1).unwrap()])
            .unwrap();
        let pattern = &sink.patterns()[key.index()];
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
            let retained = sink.refs().len();
            let totals = sink
                .patterns()
                .iter()
                .map(|pattern| pattern.exact_total)
                .collect::<Vec<_>>();
            sink.finish().unwrap();
            let Some(SectionAvailability::Available(tally)) = sink.finalized_sections() else {
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
            sink.finalized_sections(),
            Some(&SectionAvailability::VocabularyTooLarge(refusal)),
            "finish resolves the refused branch too, so resolve_scan has a value"
        );
    }

    fn compound_sink(patterns: &[String]) -> SnapshotSink {
        SnapshotSink::new_with_compounds(
            10,
            patterns
                .iter()
                .map(|name| (name.clone(), Box::default()))
                .collect(),
            SectionScanState::Refused(crate::section_tally::SectionRefusal {
                labels: 0,
                patterns: 0,
            }),
            true,
            CompoundCategory::Compound,
            false,
        )
        .unwrap()
    }

    #[test]
    fn deriving_compounds_fails_a_sentence_whose_chains_exceed_the_budget() {
        let mut sink = compound_sink(&["a".into(), "b".into()]);
        let sentence = vec![TokenInfo::default().with_orth("語"); 19];
        let matches = (0..sentence.len())
            .flat_map(|token| {
                [
                    a_span_match("a", token..token + 1),
                    a_span_match("b", token..token + 1),
                ]
            })
            .collect::<Vec<_>>();
        let error = sink
            .push(&location(0), &sentence, &matches)
            .expect_err("2^19 chains in one sentence exceed the chain budget");
        assert!(error.to_string().contains("Derive compounds"), "{error}");
    }

    #[test]
    fn deriving_compounds_fails_a_run_registering_too_many_derived_patterns() {
        let names = (0..65).map(|index| format!("p{index}")).collect::<Vec<_>>();
        let mut sink = compound_sink(&names);
        let sentence = vec![TokenInfo::default().with_orth("語"); 2];
        let mut ordinal = 0;
        let mut failure = None;
        'pairs: for first in &names {
            for second in &names {
                let matches = [a_span_match(first, 0..1), a_span_match(second, 1..2)];
                if let Err(error) = sink.push(&location(ordinal), &sentence, &matches) {
                    failure = Some(error);
                    break 'pairs;
                }
                ordinal += 1;
            }
        }
        let error = failure.expect("65 × 65 distinct pairs exceed the derived-pattern limit");
        assert!(error.to_string().contains("Derive compounds"), "{error}");
        assert_eq!(
            sink.patterns().len(),
            names.len() + crate::derived_patterns::MAX_DERIVED_PATTERNS
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
        assert_eq!(sink.patterns()[0].exact_total, 3);
        assert_eq!(sink.refs().len(), 2);
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
        assert_eq!(sink.capture_catalog().len(), 1);
        assert_eq!(sink.refs().len(), 2);
        assert!(sink.refs().iter().all(|r| r.captures.len() == 1));
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

        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections() else {
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

        let Some(SectionAvailability::Available(tally)) = sink.finalized_sections() else {
            panic!("the fixture vocabulary was admitted");
        };
        let key = PatternKey::new(0).unwrap();
        assert_eq!(tally.surfaces(key).count("あ", 1), Some(1));
        assert_eq!(tally.count(key, 1), 1, "and the pattern's own row agrees");
    }

    #[test]
    fn section_phase_rejects_double_finish_and_discarded_publication() {
        let mut sink = sink_with_sections(4, &["p"], Vec::new());
        sink.finish().unwrap();
        assert!(sink.finish().is_err());

        let mut discarded = sink_with_sections(4, &["p"], Vec::new());
        discarded.discard_sections();
        assert!(discarded.finalized_sections().is_none());
        assert!(discarded.finish().is_err());
        assert!(
            discarded
                .into_completed_snapshot(
                    crate::revision::RevisionId(1),
                    crate::corpus_runtime::CorpusIdentity::new("fixture"),
                    Arc::from("p := []."),
                    None,
                )
                .is_none()
        );
    }
}
