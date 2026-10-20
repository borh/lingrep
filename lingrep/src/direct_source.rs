//! Bounded direct-input adapters for the shared execution protocol.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use blake3::Hasher;
use lingrep_core::analysis::{
    InputUnitId, NormalizationPolicy, SegmentationMode, TextUnit, TextUnitKind,
};
use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusDiscovery, CorpusEvent, CorpusReadConfig,
    DiscoveryError, DiscoveryReport, InputFormat, MarkdownDiagnostics, ScopeObservation,
    SourceDocument, TraversalLimits, ValidatedCorpusRoot, stream_input_events,
};
use lingrep_core::execution::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionBatch, ExecutionError, ExecutionLimits,
    ExecutionSentence, SentenceBatchSource, SentenceLocation,
};
use lingrep_core::scope::ScopeClause;
use lingrep_core::token_match::{BoundProjection, TokenInfo};
use lingrep_corpus::{IdentityError, SourceObservation};

/// The bounded record reader lives in `lingrep-core` so `ling` and `lingrep-web`
/// enforce one record bound and report one diagnostic. It is re-exported here
/// because every direct-input caller reaches it through this module.
pub use lingrep_core::execution::{BoundedRecordError, BoundedRecordReader, BoundedRecordStats};

#[derive(Debug, thiserror::Error)]
#[error("input failed in {source_name}: {source}")]
struct SourceInputError {
    source_name: String,
    #[source]
    source: io::Error,
}

/// A reader that observes the exact raw chunks consumed from one logical file.
pub struct HashingReader<R> {
    reader: R,
    observation: SourceObservation,
    ended: bool,
}

impl<R: Read> HashingReader<R> {
    pub fn new(
        reader: R,
        observation: SourceObservation,
        logical_name: &str,
    ) -> Result<Self, IdentityError> {
        observation.begin_source(logical_name)?;
        Ok(Self {
            reader,
            observation,
            ended: false,
        })
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        if read == 0 {
            if !self.ended {
                self.observation.end_source().map_err(identity_io_error)?;
                self.ended = true;
            }
        } else {
            self.observation
                .update_source_bytes(&buffer[..read])
                .map_err(identity_io_error)?;
        }
        Ok(read)
    }
}

/// Run-local observation of stdin bytes. It cannot produce a durable corpus
/// identity because stdin has no reopenable logical source name.
#[derive(Clone, Default)]
pub struct StdinObservation {
    state: Arc<Mutex<StdinObservationState>>,
}

#[derive(Clone, Default)]
struct StdinObservationState {
    digest: Hasher,
    eof: bool,
}

impl StdinObservation {
    pub fn reader<R: Read>(&self, reader: R) -> StdinReader<R> {
        StdinReader {
            reader,
            observation: self.clone(),
        }
    }

    /// Returns the digest only after an observed EOF.
    pub fn observed_content_digest(&self) -> Option<String> {
        let state = self.state.lock().ok()?;
        state
            .eof
            .then(|| encode_digest(state.digest.clone().finalize().as_bytes()))
    }
}

pub struct StdinReader<R> {
    reader: R,
    observation: StdinObservation,
}

impl<R: Read> Read for StdinReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        let mut state = self
            .observation
            .state
            .lock()
            .map_err(|_| io::Error::other("stdin observation lock poisoned"))?;
        if read == 0 {
            state.eof = true;
        } else {
            state.digest.update(&buffer[..read]);
        }
        Ok(read)
    }
}

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
    max_staged_raw_bytes: usize,
    oversized_sentence_omissions: Option<OversizedSentenceOmissionSummary>,
}

impl DirectSourceStats {
    pub fn documents_admitted(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.documents_admitted)
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
}

enum TerminalFailure {
    SentenceTooLarge {
        ordinal: u64,
        actual: usize,
        limit: usize,
        context: String,
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
    fn execution_error(&self) -> ExecutionError {
        match self {
            Self::SentenceTooLarge {
                ordinal,
                actual,
                limit,
                context,
            } => ExecutionError::SentenceTooLarge {
                ordinal: *ordinal,
                actual: *actual,
                limit: *limit,
                context: context.clone(),
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

#[derive(Clone, Default)]
pub struct OrderedInventoryObservation {
    state: Arc<Mutex<(blake3::Hasher, u64)>>,
}

impl OrderedInventoryObservation {
    pub fn observe(&self, logical_name: &str) -> Result<(), IdentityError> {
        let length =
            u64::try_from(logical_name.len()).map_err(|_| IdentityError::LengthOverflow)?;
        let mut state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        state.0.update(&length.to_le_bytes());
        state.0.update(logical_name.as_bytes());
        state.1 = state
            .1
            .checked_add(1)
            .ok_or(IdentityError::LengthOverflow)?;
        Ok(())
    }

    fn snapshot(&self) -> Result<([u8; 32], u64), IdentityError> {
        let state = self.state.lock().map_err(|_| IdentityError::InvalidState)?;
        Ok((*state.0.clone().finalize().as_bytes(), state.1))
    }
}

/// Proves the sources a run actually read are the sources discovery found,
/// in the same order.
///
/// The two digests must be *independently* derived for this to mean
/// anything: `observed` accumulates during the read, and
/// `discovery.ordered_inventory_digest` comes from the traversal that
/// enumerated the inventory. Feeding it the observation's own digest turns
/// this into a tautology — see the BCCWJ arm of `FilesystemCorpusEvents`.
pub fn validate_ordered_inventory(
    observed: &OrderedInventoryObservation,
    discovery: &DiscoveryReport,
) -> Result<(), ExecutionError> {
    let (digest, documents) = observed
        .snapshot()
        .map_err(|_| ExecutionError::ResourceAccounting)?;
    if documents != discovery.inventory.documents || digest != discovery.ordered_inventory_digest {
        Err(ExecutionError::InventoryMismatch)
    } else {
        Ok(())
    }
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

impl<I> DirectSentenceSource<I> {
    pub fn stats(&self) -> DirectSourceStats {
        self.stats.clone()
    }
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
                        let mut stats = self
                            .stats
                            .state
                            .lock()
                            .map_err(|_| ExecutionError::ResourceAccounting)?;
                        stats.documents_admitted = stats
                            .documents_admitted
                            .checked_add(1)
                            .ok_or(ExecutionError::ResourceAccounting)?;
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
                        let location = SentenceLocation {
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
                            DirectSentenceValue::Text(
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
                                })?,
                            )
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
                    context: format!(" in {}", sentence.location.source_name),
                };
                if batch.is_empty() {
                    let error = failure.execution_error();
                    self.terminal_failure = Some(failure);
                    return Err(error);
                }
                self.terminal_failure = Some(failure);
                break;
            }
            let next_bytes = raw_bytes
                .checked_add(sentence_bytes)
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

#[cfg(test)]
mod tests {

    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;
    use lingrep_core::corpus::{AdmittedSentence, SentenceId};

    fn sentence(
        document: lingrep_core::corpus::DocumentId,
        ordinal: u64,
        text: &str,
    ) -> AdmittedCorpusEvent {
        AdmittedCorpusEvent::Sentence(AdmittedSentence {
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

fn terminal_failure_from_discovery(error: &DiscoveryError) -> TerminalFailure {
    let (code, logical_path) = match error {
        DiscoveryError::Cycle { logical_path } => (
            "directory_cycle",
            logical_path.to_string_lossy().into_owned(),
        ),
        DiscoveryError::DanglingLink { logical_path } => {
            ("dangling_link", logical_path.to_string_lossy().into_owned())
        }
        DiscoveryError::NonUtf8Name { path } => {
            ("non_utf8_source_name", path.to_string_lossy().into_owned())
        }
        DiscoveryError::Limit { logical_path, .. } => (
            "traversal_limit",
            logical_path.to_string_lossy().into_owned(),
        ),
        DiscoveryError::IoAt { logical_path, .. } => {
            ("discovery_io", logical_path.to_string_lossy().into_owned())
        }
    };
    TerminalFailure::Discovery {
        code,
        logical_path,
        message: error.to_string(),
    }
}

pub(crate) fn execution_discovery_error(error: &DiscoveryError) -> ExecutionError {
    terminal_failure_from_discovery(error).execution_error()
}

/// Final identity and traversal state shared with the command after execution.
#[derive(Clone)]
pub struct FilesystemObservation {
    pub source: SourceObservation,
    pub inventory: OrderedInventoryObservation,
    report: Arc<Mutex<Option<DiscoveryReport>>>,
}

impl FilesystemObservation {
    pub fn discovery_report(&self) -> Option<DiscoveryReport> {
        self.report.lock().ok()?.clone()
    }
}

/// Raw multi-file corpus stream built on the shared bounded discovery cursor.
pub struct FilesystemCorpusEvents {
    discovery: DiscoveryState,
    current: Option<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>>,
    current_source_name: Option<String>,
    config: CorpusReadConfig,
    record_limit: usize,
    observation: FilesystemObservation,
    /// One sink for the whole root, so the labels reported at the end are the
    /// labels the *run* observed rather than the last document's.
    diagnostics: MarkdownDiagnostics,
}

enum DiscoveryState {
    Validated(Box<ValidatedCorpusRoot>),
    Open(Box<CorpusDiscovery>),
    /// The BCCWJ distribution: one document stream per inventory zip.
    ///
    /// The cursor and the per-run stream facts live in the variant that
    /// needs them. Holding them as `Option` fields beside it let the type
    /// say "a BCCWJ walk with no metadata table", which nothing could
    /// construct and a runtime guard had to refuse anyway.
    Bccwj {
        cursor: crate::bccwj_seam::BccwjCursor,
        stream: crate::bccwj_seam::BccwjStream,
    },
    Exhausted,
}

impl FilesystemCorpusEvents {
    /// Validates only the selected root. Directory enumeration and recursive
    /// failures remain deferred until the event stream is pulled after `run`.
    pub fn new(
        root: &Path,
        config: CorpusReadConfig,
        limits: TraversalLimits,
        record_limit: usize,
    ) -> Result<(Self, FilesystemObservation, MarkdownDiagnostics), ExecutionError> {
        let validated = CorpusDiscovery::validate_root(root, limits.clone())
            .map_err(|error| execution_discovery_error(&error))?;
        if validated.is_directory() && matches!(config, CorpusReadConfig::Soranoha) {
            let error = DiscoveryError::IoAt {
                logical_path: PathBuf::from("."),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "soranoha input must be a bulk archive file (the .zip downloaded from soranoha.org)",
                ),
            };
            return Err(execution_discovery_error(&error));
        }
        if validated.is_directory()
            && !matches!(
                config,
                CorpusReadConfig::Markdown(_) | CorpusReadConfig::Conllu | CorpusReadConfig::Bccwj
            )
        {
            let error = DiscoveryError::IoAt {
                logical_path: PathBuf::from("."),
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a directory corpus requires the Markdown, CoNLL-U, or BCCWJ reader",
                ),
            };
            return Err(execution_discovery_error(&error));
        }
        let observation = FilesystemObservation {
            source: SourceObservation::new(),
            inventory: OrderedInventoryObservation::default(),
            report: Arc::new(Mutex::new(None)),
        };
        let diagnostics = MarkdownDiagnostics::default();
        let discovery = if matches!(config, CorpusReadConfig::Bccwj) {
            if !validated.is_directory() {
                return Err(ExecutionError::InvalidLimit {
                    field: "input",
                    message: "bccwj input must be a distribution root (a directory containing C-XML/ and DOC/)".into(),
                });
            }
            let inventory = lingrep_core::corpus::bccwj::BccwjInventory::discover(root, &limits)
                .map_err(|error| {
                    execution_discovery_error(&DiscoveryError::IoAt {
                        logical_path: root.to_path_buf(),
                        source: error,
                    })
                })?;
            // The context tables are part of the source identity: observe
            // them before the first zip's begin_source (the API enforces the
            // phase), so the live run, the naming pass and the build pass
            // all hash the same bytes in the same order.
            crate::bccwj_seam::observe_bccwj_context_files(&inventory, root, &observation.source)
                .map_err(|error| {
                execution_discovery_error(&DiscoveryError::IoAt {
                    logical_path: root.to_path_buf(),
                    source: error,
                })
            })?;
            DiscoveryState::Bccwj {
                cursor: crate::bccwj_seam::BccwjCursor::new(inventory.zips),
                stream: crate::bccwj_seam::BccwjStream::new(root.to_path_buf(), record_limit),
            }
        } else {
            DiscoveryState::Validated(Box::new(validated))
        };
        Ok((
            Self {
                discovery,
                current: None,
                current_source_name: None,
                config,
                record_limit,
                observation: observation.clone(),
                diagnostics: diagnostics.clone(),
            },
            observation,
            diagnostics,
        ))
    }

    fn open_document(
        &self,
        document: SourceDocument,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        open_corpus_document(
            &self.observation.source,
            Some(&self.observation.inventory),
            Some(&self.diagnostics),
            &self.config,
            self.record_limit,
            &document,
        )
    }
}

impl Iterator for FilesystemCorpusEvents {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.current.as_mut().and_then(Iterator::next) {
                return Some(event.map_err(|error| {
                    contextualize_input_error(
                        error,
                        self.current_source_name.as_deref().unwrap_or_default(),
                    )
                }));
            }
            self.current = None;
            self.current_source_name = None;
            if matches!(self.discovery, DiscoveryState::Validated(_)) {
                let DiscoveryState::Validated(validated) =
                    std::mem::replace(&mut self.discovery, DiscoveryState::Exhausted)
                else {
                    #[expect(
                        clippy::unreachable,
                        reason = "the Exhausted/Validated/Open state was checked above"
                    )]
                    {
                        unreachable!("discovery state was checked")
                    }
                };
                let file_kind = self
                    .config
                    .directory_file_kind()
                    .unwrap_or(lingrep_core::corpus::CorpusFileKind::Markdown);
                match CorpusDiscovery::from_validated_root_for_kind(*validated, file_kind) {
                    Ok(discovery) => self.discovery = DiscoveryState::Open(Box::new(discovery)),
                    Err(error) => return Some(Err(discovery_io_error(error))),
                }
            }
            if let DiscoveryState::Bccwj { cursor, stream } = &mut self.discovery {
                let Some(document) = cursor.next_document() else {
                    // The inventory is exhausted: publish the report the
                    // build derives over the same inventory — the zip
                    // (source) names, not the entry (document) names.
                    //
                    // Both halves come from the *inventory*, never from the
                    // observation. `validate_ordered_inventory` compares the
                    // observation's digest against this one, so deriving this
                    // one from the observation made that check compare a
                    // value with itself and it could not fail — while for
                    // every other format it proves the sources read are the
                    // sources discovered, in order.
                    if let Ok(mut stored) = self.observation.report.lock() {
                        *stored = Some(cursor.report());
                    }
                    return None;
                };
                let zip = match cursor.zip_for(&document) {
                    Ok(zip) => zip.clone(),
                    Err(error) => return Some(Err(error)),
                };
                let opened = stream.open_observing_labels(
                    &self.observation.source,
                    Some(&self.observation.inventory),
                    &zip,
                    &self.diagnostics,
                );
                match opened {
                    Ok(events) => {
                        self.current = Some(events);
                        self.current_source_name = Some(document.logical_name);
                    }
                    Err(error) => {
                        return Some(Err(contextualize_input_error(
                            error,
                            &document.logical_name,
                        )));
                    }
                }
                continue;
            }
            let DiscoveryState::Open(discovery) = &mut self.discovery else {
                return None;
            };
            match discovery.next_document() {
                Ok(Some(document)) => {
                    let logical_name = document.logical_name.clone();
                    match self.open_document(document) {
                        Ok(events) => {
                            self.current = Some(events);
                            self.current_source_name = Some(logical_name);
                        }
                        Err(error) => {
                            return Some(Err(contextualize_input_error(error, &logical_name)));
                        }
                    }
                }
                Ok(None) => {
                    let DiscoveryState::Open(discovery) =
                        std::mem::replace(&mut self.discovery, DiscoveryState::Exhausted)
                    else {
                        #[expect(clippy::unreachable, reason = "open discovery was just matched")]
                        {
                            unreachable!("open discovery was just matched")
                        }
                    };
                    match discovery.finish() {
                        Ok(report) => {
                            if let Ok(mut stored) = self.observation.report.lock() {
                                *stored = Some(report);
                            }
                            return None;
                        }
                        Err(error) => return Some(Err(discovery_io_error(error))),
                    }
                }
                Err(error) => return Some(Err(discovery_io_error(error))),
            }
        }
    }
}

fn contextualize_input_error(error: io::Error, source_name: &str) -> io::Error {
    if error.get_ref().is_some_and(
        <dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static>::is::<
            BoundedRecordError,
        >,
    ) {
        return error;
    }
    let kind = error.kind();
    io::Error::new(
        kind,
        SourceInputError {
            source_name: source_name.to_owned(),
            source: error,
        },
    )
}

pub type AdmittedFilesystemEvents = AdmittedCorpus<FilesystemCorpusEvents>;

pub fn admitted_filesystem_events(events: FilesystemCorpusEvents) -> AdmittedFilesystemEvents {
    AdmittedCorpus::new(events)
}

/// Applies a scope clause at sentence admission, if one was given.
///
/// The caller keeps its own clone of `observation`: this reader is handed to a
/// source by value and the source is moved into the execution runner, so the
/// counts have to leave through a shared handle.
///
pub fn scoped<I>(
    corpus: AdmittedCorpus<I>,
    clause: Option<&ScopeClause>,
    observation: &ScopeObservation,
) -> AdmittedCorpus<I> {
    match clause {
        Some(clause) => corpus.with_scope(clause.clone(), observation.clone()),
        None => corpus,
    }
}

/// Return bounded context without modifying the matched token slice.
pub fn project_context(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: Range<usize>,
    requested: usize,
) -> (Vec<TokenInfo>, Vec<TokenInfo>) {
    let start = span.start.min(sentence.len());
    let end = span.end.min(sentence.len()).max(start);
    let left_start = start.saturating_sub(requested);
    let right_end = end.saturating_add(requested).min(sentence.len());
    (
        (left_start..start)
            .filter_map(|index| TokenInfo::from_sequence(sentence, index))
            .collect(),
        (end..right_end)
            .filter_map(|index| TokenInfo::from_sequence(sentence, index))
            .collect(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "missing required attribute {attribute} at {source_name}, corpus sentence \
     {corpus_sentence_ordinal}, token {token_index}"
)]
pub struct MissingRequiredAttribute {
    pub attribute: &'static str,
    pub source_name: String,
    pub corpus_sentence_ordinal: u64,
    pub token_index: usize,
}

impl From<MissingRequiredAttribute> for ExecutionError {
    fn from(error: MissingRequiredAttribute) -> Self {
        Self::MissingRequiredAttribute {
            attribute: error.attribute,
            source_name: error.source_name,
            corpus_sentence_ordinal: error.corpus_sentence_ordinal,
            token_index: error.token_index,
        }
    }
}

pub fn matched_surface(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
) -> Result<String, MissingRequiredAttribute> {
    let mut surface = String::new();
    visit_matched_orths(sentence, span, location, |orth| surface.push_str(orth))?;
    Ok(surface)
}

pub fn matched_orth_tokens(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
) -> Result<Vec<String>, MissingRequiredAttribute> {
    let mut result = Vec::with_capacity(span.end.saturating_sub(span.start));
    visit_matched_orths(sentence, span, location, |orth| {
        result.push(orth.to_owned());
    })?;
    Ok(result)
}

fn visit_matched_orths(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: Range<usize>,
    location: &SentenceLocation,
    mut visit: impl FnMut(&str),
) -> Result<(), MissingRequiredAttribute> {
    let end = span.end.min(sentence.len());
    for index in span.start.min(end)..end {
        let text = sentence
            .get_key(
                index,
                &lingrep_core::token_match::AttributeKey::new("text")
                    .expect("form is a canonical attribute key"),
            )
            .ok_or_else(|| MissingRequiredAttribute {
                attribute: "text",
                source_name: location.source_name.to_string(),
                corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                token_index: index,
            })?;
        visit(text);
    }
    Ok(())
}

pub(crate) fn input_format(config: &CorpusReadConfig) -> InputFormat {
    match config {
        CorpusReadConfig::Plain => InputFormat::Plaintext,
        CorpusReadConfig::Jsonl => InputFormat::Jsonl,
        CorpusReadConfig::Conllu => InputFormat::Conllu,
        CorpusReadConfig::Markdown(config) => InputFormat::Markdown(config.clone()),
        // BCCWJ is a directory of zips and a Soranoha archive is a seekable
        // container: neither reader consumes one `BufRead` stream, so both
        // stream through their own arms instead of this mapping. This function
        // is reached only for the file formats above.
        CorpusReadConfig::Bccwj | CorpusReadConfig::Soranoha => {
            #[expect(
                clippy::unreachable,
                reason = "bccwj and soranoha documents stream through their dedicated arms, not the file-format mapping"
            )]
            {
                unreachable!("the bccwj and soranoha configs arrive through their dedicated arms")
            }
        }
    }
}

/// The shared stdin seam: an observation, a record-bounded reader named
/// `"<stdin>"`, and the format-typed event stream.
///
/// Every command that accepts `-` builds exactly these three things, and they
/// have to stay identical or the same bytes would report different identities
/// under different subcommands. Only the *admission* wrapper around the
/// returned events differs — scope clause, projection, source metadata — so
/// that part stays with the caller. The observation is returned rather than
/// held because a command that reports a stream digest keeps it past the run;
/// the reader owns its own clone either way.
pub(crate) fn stdin_events(
    config: &CorpusReadConfig,
    max_input_record_bytes: usize,
) -> (
    StdinObservation,
    Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>,
) {
    let observation = StdinObservation::default();
    let reader = observation.reader(io::stdin());
    let (reader, _) =
        BoundedRecordReader::new(BufReader::new(reader), max_input_record_bytes, "<stdin>");
    let events = stream_input_events(input_format(config), reader, "<stdin>".into(), None);
    (observation, events)
}

/// The shared per-document open used by both corpus seams: open the file,
/// record the ordered-inventory observation (direct seam only), wrap the
/// handle in the identity-hash reader and the record bound, and stream the
/// format-typed events with the diagnostics sink (direct seam only).
///
/// The seam **loops** that feed this stay separate: their side channels
/// genuinely differ (the `DiscoveredDocuments` queue in prepare, lazy versus
/// eager discovery open, error styles), and extracting the loop would force
/// a callback interface larger than either existing one.
pub(crate) fn open_corpus_document(
    source: &SourceObservation,
    inventory: Option<&OrderedInventoryObservation>,
    diagnostics: Option<&MarkdownDiagnostics>,
    config: &CorpusReadConfig,
    record_limit: usize,
    document: &SourceDocument,
) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
    if matches!(config, CorpusReadConfig::Soranoha) {
        return crate::soranoha_seam::open(source, inventory, document, record_limit);
    }
    let file = File::open(&document.path)?;
    if let Some(inventory) = inventory {
        inventory
            .observe(&document.logical_name)
            .map_err(identity_io_error)?;
    }
    let reader = HashingReader::new(file, source.clone(), &document.logical_name)
        .map_err(identity_io_error)?;
    let (reader, _) =
        BoundedRecordReader::new(BufReader::new(reader), record_limit, &document.logical_name);
    Ok(stream_input_events(
        input_format(config),
        reader,
        document.logical_name.clone(),
        diagnostics.cloned(),
    ))
}

pub(crate) fn identity_io_error(error: IdentityError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn discovery_io_error(error: DiscoveryError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

pub fn encode_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
