//! Bounded sentence batching over admitted corpus events.

use std::io;
use std::sync::{Arc, Mutex};

use lingrep_core::analysis::{
    InputUnitId, NormalizationPolicy, SegmentationMode, TextUnit, TextUnitKind,
};
use lingrep_core::corpus::{AdmittedCorpusEvent, DiscoveryError};
use lingrep_core::execution::{
    BoundSentenceInput, BoundedRecordError, EnhancedGraphInput, ExecutionBatch, ExecutionError,
    ExecutionLimits, ExecutionSentence, SentenceBatchSource, SentenceLocation,
};
use lingrep_core::token_match::BoundProjection;

use super::discovery::{SourceInputError, terminal_failure_from_discovery};

/// Observable bounded-state facts retained after the source is moved into the
/// shared executor.
#[derive(Clone, Default)]
pub struct DirectSourceStats {
    state: Arc<Mutex<DirectSourceStatsState>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OversizedSentenceOmissionSummary {
    pub(crate) count: u64,
    pub(crate) first_source_name: String,
    pub(crate) first_ordinal: u64,
    pub(crate) first_actual: usize,
    pub(crate) limit: usize,
}

#[derive(Clone, Default)]
struct DirectSourceStatsState {
    documents_admitted: u64,
    last_admitted_document: Option<u64>,
    max_staged_raw_bytes: usize,
    oversized_sentence_omissions: Option<OversizedSentenceOmissionSummary>,
    observed_division_kinds: std::collections::BTreeSet<compact_str::CompactString>,
}

impl DirectSourceStats {
    /// The documents with at least one sentence this source has returned.
    pub fn documents_admitted(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.documents_admitted)
            .unwrap_or_default()
    }

    pub(crate) fn observed_division_kinds(
        &self,
    ) -> std::collections::BTreeSet<compact_str::CompactString> {
        self.state
            .lock()
            .map(|state| state.observed_division_kinds.clone())
            .unwrap_or_default()
    }

    pub fn max_staged_raw_bytes(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.max_staged_raw_bytes)
            .unwrap_or_default()
    }

    pub(crate) fn oversized_sentence_omissions(
        &self,
    ) -> Result<Option<OversizedSentenceOmissionSummary>, ExecutionError> {
        self.state
            .lock()
            .map(|state| state.oversized_sentence_omissions.clone())
            .map_err(|_| ExecutionError::ResourceAccounting)
    }

    fn record_oversized_sentence(
        &self,
        location: &SentenceLocation,
        actual: usize,
        limit: usize,
    ) -> Result<(), ExecutionError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ExecutionError::ResourceAccounting)?;
        match &mut state.oversized_sentence_omissions {
            Some(summary) => {
                summary.count = summary
                    .count
                    .checked_add(1)
                    .ok_or(ExecutionError::ResourceAccounting)?;
            }
            None => {
                state.oversized_sentence_omissions = Some(OversizedSentenceOmissionSummary {
                    count: 1,
                    first_source_name: location.source_name.to_string(),
                    first_ordinal: location.corpus_sentence_ordinal,
                    first_actual: actual,
                    limit,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum OversizedSentenceDisposition {
    #[default]
    Stop,
    SkipAndFail,
}

/// Converts shared admitted corpus events into location-bearing bounded text
/// batches. A document end is a physical batch boundary, allowing a result
/// limit to stop before discovering the next document.
pub struct DirectSentenceSource<I> {
    events: I,
    pending: Option<DirectSentence>,
    current_source: Option<(lingrep_core::corpus::DocumentId, Arc<str>)>,
    source_metadata_projection: Option<lingrep_core::corpus::SourceMetadataProjection>,
    lookaround_division_kinds: Box<[compact_str::CompactString]>,
    document_metadata: lingrep_core::corpus::Metadata,
    next_sentence_ordinal: u64,
    terminal_failure: Option<TerminalFailure>,
    stats: DirectSourceStats,
    oversized_sentence_disposition: OversizedSentenceDisposition,
    normalization: NormalizationPolicy,
    segmentation: SegmentationMode,
    projection: Option<BoundProjection>,
}

enum DirectSentenceValue {
    Text(TextUnit),
    Bound(BoundSentenceInput),
}

struct DirectSentence {
    location: SentenceLocation,
    value: DirectSentenceValue,
    raw_bytes: usize,
}

impl DirectSentence {
    fn is_bound(&self) -> bool {
        matches!(self.value, DirectSentenceValue::Bound(_))
    }
}

impl<I> DirectSentenceSource<I> {
    #[must_use]
    pub fn with_source_metadata(
        mut self,
        projection: Option<lingrep_core::corpus::SourceMetadataProjection>,
    ) -> Self {
        self.source_metadata_projection = projection;
        self
    }

    /// Records each sentence's innermost division of each of `kinds` in
    /// [`SentenceLocation::lookaround_divisions`].
    #[must_use]
    pub fn with_lookaround_divisions(mut self, kinds: Vec<compact_str::CompactString>) -> Self {
        self.lookaround_division_kinds = kinds.into();
        self
    }

    #[must_use]
    pub fn with_normalization(mut self, normalization: NormalizationPolicy) -> Self {
        self.normalization = normalization;
        self
    }

    #[must_use]
    pub fn with_segmentation(mut self, segmentation: SegmentationMode) -> Self {
        self.segmentation = segmentation;
        self
    }

    #[must_use]
    pub fn with_projection(mut self, projection: BoundProjection) -> Self {
        self.projection = Some(projection);
        self
    }

    pub fn stats(&self) -> DirectSourceStats {
        self.stats.clone()
    }
}

pub(super) enum TerminalFailure {
    SentenceTooLarge {
        ordinal: u64,
        actual: usize,
        limit: usize,
        source_name: Option<String>,
    },
    InputRecordTooLarge {
        source_name: String,
        record_ordinal: u64,
        corpus_sentence_ordinal: u64,
        actual_at_least: usize,
        limit: usize,
    },
    Discovery {
        code: &'static str,
        logical_path: String,
        message: String,
    },
    Input {
        source_name: String,
        ordinal: u64,
        kind: io::ErrorKind,
        message: String,
    },
}

impl TerminalFailure {
    pub(super) fn execution_error(&self) -> ExecutionError {
        match self {
            Self::SentenceTooLarge {
                ordinal,
                actual,
                limit,
                source_name,
            } => ExecutionError::SentenceTooLarge {
                ordinal: *ordinal,
                actual: *actual,
                limit: *limit,
                source_name: source_name.clone(),
            },
            Self::InputRecordTooLarge {
                source_name,
                record_ordinal,
                corpus_sentence_ordinal,
                actual_at_least,
                limit,
            } => ExecutionError::InputRecordTooLarge {
                source_name: source_name.clone(),
                record_ordinal: *record_ordinal,
                corpus_sentence_ordinal: *corpus_sentence_ordinal,
                actual_at_least: *actual_at_least,
                limit: *limit,
            },
            Self::Discovery {
                code,
                logical_path,
                message,
            } => ExecutionError::Discovery {
                code,
                logical_path: logical_path.clone(),
                message: message.clone(),
            },
            Self::Input {
                source_name,
                ordinal,
                kind,
                message,
            } => ExecutionError::Input {
                source_name: source_name.clone(),
                ordinal: *ordinal,
                kind: *kind,
                message: message.clone(),
            },
        }
    }
}

fn source_from_events_with_disposition<I>(
    events: I,
    oversized_sentence_disposition: OversizedSentenceDisposition,
) -> DirectSentenceSource<I> {
    DirectSentenceSource {
        events,
        pending: None,
        current_source: None,
        source_metadata_projection: None,
        lookaround_division_kinds: Box::default(),
        document_metadata: Default::default(),
        next_sentence_ordinal: 0,
        terminal_failure: None,
        stats: DirectSourceStats::default(),
        oversized_sentence_disposition,
        normalization: NormalizationPolicy::Nfc,
        segmentation: SegmentationMode::Input,
        projection: None,
    }
}

pub fn source_from_events<I>(events: I) -> DirectSentenceSource<I> {
    source_from_events_with_disposition(events, OversizedSentenceDisposition::Stop)
}

pub(crate) fn source_from_events_skipping_oversized<I>(events: I) -> DirectSentenceSource<I> {
    source_from_events_with_disposition(events, OversizedSentenceDisposition::SkipAndFail)
}

pub(crate) type BoxSentenceSource = Box<dyn SentenceBatchSource + Send>;

pub(crate) fn boxed_direct_source<I>(
    source: Box<DirectSentenceSource<I>>,
) -> (BoxSentenceSource, DirectSourceStats)
where
    DirectSentenceSource<I>: SentenceBatchSource + Send + 'static,
{
    let stats = source.stats();
    (source, stats)
}

impl<I> SentenceBatchSource for DirectSentenceSource<I>
where
    I: Iterator<Item = io::Result<AdmittedCorpusEvent>>,
{
    fn wants_lookahead(&self) -> bool {
        // A direct run reports `basis: ObservedPrefix`, so how far this reader
        // got is published. Reading ahead of a stop would move that count.
        false
    }

    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if let Some(failure) = &self.terminal_failure {
            return Err(failure.execution_error());
        }
        limits.validate()?;

        let mut batch: Vec<DirectSentence> = Vec::new();
        let mut raw_bytes = 0usize;
        loop {
            let sentence = match self.pending.take() {
                Some(sentence) => Some(sentence),
                None => match self.events.next() {
                    Some(Ok(AdmittedCorpusEvent::DocumentStart {
                        id,
                        source_name,
                        metadata,
                    })) => {
                        self.current_source = Some((id, Arc::from(source_name)));
                        if self.source_metadata_projection.is_some() {
                            self.document_metadata = metadata;
                        }
                        None
                    }
                    Some(Ok(AdmittedCorpusEvent::Sentence(sentence))) => {
                        let Some((document_id, source_name)) = &self.current_source else {
                            return Err(source_error(
                                sentence.id.get(),
                                "admitted sentence has no active source",
                            ));
                        };
                        if *document_id != sentence.document_id {
                            return Err(source_error(
                                sentence.id.get(),
                                "admitted sentence document differs from active source",
                            ));
                        }
                        if sentence.id.get() != self.next_sentence_ordinal {
                            return Err(source_error(
                                self.next_sentence_ordinal,
                                "admitted sentence ordinal is not contiguous",
                            ));
                        }
                        self.next_sentence_ordinal = self
                            .next_sentence_ordinal
                            .checked_add(1)
                            .ok_or(ExecutionError::ResourceAccounting)?;
                        for kind in &self.lookaround_division_kinds {
                            if sentence.structure.observes(kind) {
                                let mut stats = self
                                    .stats
                                    .state
                                    .lock()
                                    .map_err(|_| ExecutionError::ResourceAccounting)?;
                                stats.observed_division_kinds.insert(kind.clone());
                            }
                        }
                        let location = SentenceLocation {
                            lookaround_divisions: (!self.lookaround_division_kinds.is_empty())
                                .then(|| {
                                    self.lookaround_division_kinds
                                        .iter()
                                        .map(|kind| sentence.structure.innermost(kind))
                                        .collect()
                                }),
                            source_metadata: self
                                .source_metadata_projection
                                .as_ref()
                                .map(|projection| {
                                    projection
                                        .project(
                                            &self.document_metadata,
                                            &sentence.metadata,
                                            &sentence.structure,
                                        )
                                        .map(Arc::new)
                                })
                                .transpose()
                                .map_err(|error| {
                                    source_error(sentence.id.get(), error.to_string())
                                })?,
                            corpus_sentence_ordinal: sentence.id.get(),
                            document_id: sentence.document_id,
                            document_ordinal: sentence.document_id.get(),
                            document_sentence_ordinal: sentence.document_sentence_ordinal,
                            source_name: Arc::clone(source_name),
                        };
                        let raw_bytes = sentence.text.len();
                        let value = if let Some(words) = sentence.words {
                            let projection = self.projection.as_ref().ok_or_else(|| {
                                source_error(
                                    sentence.id.get(),
                                    "pretokenized input requires an execution projection",
                                )
                            })?;
                            let words = words.reproject(projection).map_err(|error| {
                                source_error(sentence.id.get(), error.to_string())
                            })?;
                            let graph_requested = projection.providers().contains(
                                &lingrep_core::token_match::AttributeProvider::EnhancedGraph,
                            );
                            let graph = if graph_requested {
                                EnhancedGraphInput::Observation(sentence.enhanced_graph.unwrap_or(
                                    lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable,
                                ))
                            } else {
                                EnhancedGraphInput::NotRequested
                            };
                            DirectSentenceValue::Bound(BoundSentenceInput::new(words, graph))
                        } else {
                            DirectSentenceValue::Text(if sentence.annotations.is_empty() {
                                match self.segmentation {
                                    SegmentationMode::Input => TextUnit::sentence(
                                        InputUnitId::new(sentence.id.get()),
                                        sentence.text,
                                        self.normalization,
                                    ),
                                    SegmentationMode::Analyzer => TextUnit::admit(
                                        InputUnitId::new(sentence.id.get()),
                                        TextUnitKind::Segment,
                                        sentence.text,
                                        self.normalization,
                                    ),
                                }
                                .map_err(|error| {
                                    source_error(sentence.id.get(), error.to_string())
                                })?
                            } else {
                                TextUnit::annotated(
                                    InputUnitId::new(sentence.id.get()),
                                    match self.segmentation {
                                        SegmentationMode::Input => TextUnitKind::Sentence,
                                        SegmentationMode::Analyzer => TextUnitKind::Segment,
                                    },
                                    lingrep_core::analysis::AnnotatedText::new(
                                        sentence.text,
                                        sentence.annotations.into_vec(),
                                    )
                                    .map_err(|error| {
                                        source_error(sentence.id.get(), error.to_string())
                                    })?,
                                    self.normalization,
                                )
                                .map_err(|error| {
                                    source_error(sentence.id.get(), error.to_string())
                                })?
                            })
                        };
                        Some(DirectSentence {
                            location,
                            value,
                            raw_bytes,
                        })
                    }
                    Some(Ok(AdmittedCorpusEvent::DocumentEnd { id })) => {
                        if self
                            .current_source
                            .as_ref()
                            .is_none_or(|(active, _)| *active != id)
                        {
                            return Err(source_error(
                                id.get(),
                                "document end has no active source",
                            ));
                        }
                        self.current_source = None;
                        self.document_metadata = Default::default();
                        if !batch.is_empty() {
                            break;
                        }
                        None
                    }
                    Some(Ok(_)) => None,
                    Some(Err(error)) => {
                        let ordinal = self.next_sentence_ordinal;
                        let source_name = self
                            .current_source
                            .as_ref()
                            .map_or("", |(_, source_name)| source_name.as_ref());
                        self.terminal_failure =
                            Some(terminal_failure_from_io(ordinal, source_name, &error));
                        if batch.is_empty() {
                            return Err(self
                                .terminal_failure
                                .as_ref()
                                .expect("terminal failure was stored")
                                .execution_error());
                        }
                        break;
                    }
                    None => {
                        if self.current_source.is_some() {
                            return Err(source_error(
                                self.next_sentence_ordinal,
                                "admitted corpus ended with an active source",
                            ));
                        }
                        break;
                    }
                },
            };
            let Some(sentence) = sentence else {
                continue;
            };

            let sentence_bytes = sentence.raw_bytes;
            if sentence_bytes > limits.max_sentence_bytes {
                if self.oversized_sentence_disposition == OversizedSentenceDisposition::SkipAndFail
                {
                    self.stats.record_oversized_sentence(
                        &sentence.location,
                        sentence_bytes,
                        limits.max_sentence_bytes,
                    )?;
                    continue;
                }
                let failure = TerminalFailure::SentenceTooLarge {
                    ordinal: sentence.location.corpus_sentence_ordinal,
                    actual: sentence_bytes,
                    limit: limits.max_sentence_bytes,
                    source_name: Some(sentence.location.source_name.to_string()),
                };
                if batch.is_empty() {
                    let error = failure.execution_error();
                    self.terminal_failure = Some(failure);
                    return Err(error);
                }
                self.terminal_failure = Some(failure);
                break;
            }
            let annotation_bytes = match &sentence.value {
                DirectSentenceValue::Text(unit) => {
                    lingrep_core::analysis::AnnotatedText::annotation_bytes(unit.annotations())
                }
                DirectSentenceValue::Bound(_) => 0,
            };
            let next_bytes = raw_bytes
                .checked_add(sentence_bytes.saturating_add(annotation_bytes))
                .ok_or(ExecutionError::ResourceAccounting)?;
            if !batch.is_empty()
                && (batch.len() == limits.max_stage_sentences
                    || next_bytes > limits.max_stage_input_bytes
                    || batch[0].is_bound() != sentence.is_bound())
            {
                self.pending = Some(sentence);
                break;
            }
            raw_bytes = next_bytes;
            batch.push(sentence);
            if batch.len() == limits.max_stage_sentences {
                break;
            }
        }

        if !batch.is_empty() {
            let mut stats = self
                .stats
                .state
                .lock()
                .map_err(|_| ExecutionError::ResourceAccounting)?;
            stats.max_staged_raw_bytes = stats.max_staged_raw_bytes.max(raw_bytes);
            for sentence in &batch {
                let document = sentence.location.document_ordinal;
                if stats.last_admitted_document != Some(document) {
                    stats.last_admitted_document = Some(document);
                    stats.documents_admitted = stats
                        .documents_admitted
                        .checked_add(1)
                        .ok_or(ExecutionError::ResourceAccounting)?;
                }
            }
        }
        if batch.is_empty() {
            return Ok(None);
        }
        if batch[0].is_bound() {
            let mut sentences = Vec::with_capacity(batch.len());
            let mut charges = Vec::with_capacity(batch.len());
            for sentence in batch {
                let words = match sentence.value {
                    DirectSentenceValue::Bound(words) => words,
                    DirectSentenceValue::Text(_) => {
                        return Err(ExecutionError::Source {
                            ordinal: sentence.location.corpus_sentence_ordinal,
                            message: "mixed direct sentence batch crossed its analysis boundary"
                                .to_owned(),
                        });
                    }
                };
                let charge = words
                    .resident_bytes()
                    .ok_or(ExecutionError::ResourceAccounting)?;
                sentences.push(ExecutionSentence {
                    location: sentence.location,
                    value: words,
                });
                charges.push(charge);
            }
            Ok(Some(ExecutionBatch::bound_tokenized_with_charges(
                sentences, charges,
            )))
        } else {
            let mut sentences = Vec::with_capacity(batch.len());
            for sentence in batch {
                let value = match sentence.value {
                    DirectSentenceValue::Text(value) => value,
                    DirectSentenceValue::Bound(_) => {
                        return Err(ExecutionError::Source {
                            ordinal: sentence.location.corpus_sentence_ordinal,
                            message: "mixed direct sentence batch crossed its analysis boundary"
                                .to_owned(),
                        });
                    }
                };
                sentences.push(ExecutionSentence {
                    location: sentence.location,
                    value,
                });
            }
            Ok(Some(ExecutionBatch::Text(sentences)))
        }
    }
}

fn source_error(ordinal: u64, message: impl ToString) -> ExecutionError {
    ExecutionError::Source {
        ordinal,
        message: message.to_string(),
    }
}

fn terminal_failure_from_io(ordinal: u64, source_name: &str, error: &io::Error) -> TerminalFailure {
    if let Some(error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<BoundedRecordError>())
    {
        TerminalFailure::InputRecordTooLarge {
            source_name: error.source_name.clone(),
            record_ordinal: error.record_ordinal,
            corpus_sentence_ordinal: ordinal,
            actual_at_least: error.actual_at_least,
            limit: error.limit,
        }
    } else if let Some(error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<DiscoveryError>())
    {
        terminal_failure_from_discovery(error)
    } else if let Some(error) = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<SourceInputError>())
    {
        TerminalFailure::Input {
            source_name: error.source_name.clone(),
            ordinal,
            kind: error.source.kind(),
            message: error.source.to_string(),
        }
    } else {
        TerminalFailure::Input {
            source_name: source_name.to_owned(),
            ordinal,
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use lingrep_core::corpus::{AdmittedSentence, SentenceId};

    use super::*;

    fn sentence(
        document: lingrep_core::corpus::DocumentId,
        ordinal: u64,
        text: &str,
    ) -> AdmittedCorpusEvent {
        AdmittedCorpusEvent::Sentence(AdmittedSentence {
            annotations: Box::new([]),
            structure: Default::default(),
            id: SentenceId::new(ordinal),
            document_id: document,
            document_sentence_ordinal: ordinal,
            text: text.to_owned(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        })
    }

    fn small_limits() -> ExecutionLimits {
        ExecutionLimits {
            max_sentence_bytes: 4,
            max_input_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            max_stage_sentences: 8,
            max_stage_input_bytes: 16,
            max_tokenized_bytes: 1024,
        }
    }

    #[test]
    fn direct_bound_source_preserves_requested_enhanced_observation() {
        let conllu = "# text = saw\n1\tsaw\tsee\tVERB\t_\t_\t0\troot\t0:root\t_\n1.1\tpro\tpro\tPRON\t_\t_\t_\t_\t1:obj\t_\n";
        let mut reader = lingrep_core::corpus::conllu::Reader::new(
            std::io::BufReader::new(conllu.as_bytes()),
            "direct.conllu",
        );
        let parsed = reader.next().unwrap().unwrap();
        let schema = lingrep_core::corpus::conllu::token_schema();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let document = lingrep_core::corpus::DocumentId::new(0);
        let expected = parsed.enhanced_graph().clone();
        let events = vec![
            Ok(AdmittedCorpusEvent::DocumentStart {
                id: document,
                source_name: "direct.conllu".into(),
                metadata: Box::new([]),
            }),
            Ok(AdmittedCorpusEvent::Sentence(AdmittedSentence {
                annotations: Box::new([]),
                structure: Default::default(),
                id: SentenceId::new(0),
                document_id: document,
                document_sentence_ordinal: 0,
                text: parsed.text().to_owned(),
                words: Some(parsed.project(&projection).unwrap()),
                enhanced_graph: Some(expected.clone()),
                active_labels: Box::new([]),
                metadata: Box::new([]),
            })),
            Ok(AdmittedCorpusEvent::DocumentEnd { id: document }),
        ];
        let mut source = source_from_events(events.into_iter()).with_projection(projection);
        let Some(ExecutionBatch::BoundTokenized { sentences, charges }) =
            source.next_batch(&ExecutionLimits::default()).unwrap()
        else {
            panic!("expected one bound batch");
        };
        assert_eq!(
            sentences[0].value.enhanced_graph(),
            &EnhancedGraphInput::Observation(expected)
        );
        assert_eq!(charges[0], sentences[0].value.resident_bytes().unwrap());
    }

    #[test]
    fn continuation_skips_oversized_sentences_without_renumbering_later_input() {
        let document = lingrep_core::corpus::DocumentId::new(0);
        let events = vec![
            Ok(AdmittedCorpusEvent::DocumentStart {
                id: document,
                source_name: "a.md".into(),
                metadata: Box::new([]),
            }),
            Ok(sentence(document, 0, "a")),
            Ok(sentence(document, 1, "over?size")),
            Ok(sentence(document, 2, "b")),
            Ok(AdmittedCorpusEvent::DocumentEnd { id: document }),
        ];
        let mut source = source_from_events_skipping_oversized(events.into_iter());
        let stats = source.stats();

        let Some(ExecutionBatch::Text(batch)) = source.next_batch(&small_limits()).unwrap() else {
            panic!("expected one text batch");
        };
        assert_eq!(
            batch
                .iter()
                .map(|sentence| sentence.location.corpus_sentence_ordinal)
                .collect::<Vec<_>>(),
            [0, 2]
        );
        assert!(source.next_batch(&small_limits()).unwrap().is_none());
        assert_eq!(
            stats.oversized_sentence_omissions().unwrap(),
            Some(OversizedSentenceOmissionSummary {
                count: 1,
                first_source_name: "a.md".into(),
                first_ordinal: 1,
                first_actual: 9,
                limit: 4,
            })
        );
    }

    #[test]
    fn omission_stats_count_many_skips_but_retain_only_the_first_location() {
        let document = lingrep_core::corpus::DocumentId::new(0);
        let mut events = vec![Ok(AdmittedCorpusEvent::DocumentStart {
            id: document,
            source_name: "first.md".into(),
            metadata: Box::new([]),
        })];
        events.extend((0..1_000).map(|ordinal| Ok(sentence(document, ordinal, "over?size"))));
        events.push(Ok(AdmittedCorpusEvent::DocumentEnd { id: document }));
        let mut source = source_from_events_skipping_oversized(events.into_iter());
        let stats = source.stats();

        assert!(source.next_batch(&small_limits()).unwrap().is_none());
        let summary = stats.oversized_sentence_omissions().unwrap().unwrap();
        assert_eq!(summary.count, 1_000);
        assert_eq!(summary.first_source_name, "first.md");
        assert_eq!(summary.first_ordinal, 0);
        assert_eq!(summary.first_actual, 9);
    }

    #[test]
    fn omission_stats_fail_closed_when_the_lock_is_poisoned() {
        let stats = DirectSourceStats::default();
        let state = Arc::clone(&stats.state);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.lock().unwrap();
            panic!("poison omission stats");
        }));

        assert!(matches!(
            stats.oversized_sentence_omissions(),
            Err(ExecutionError::ResourceAccounting)
        ));
    }
}
