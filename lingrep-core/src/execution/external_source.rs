use std::fmt;
use std::io::BufRead;
use std::sync::Arc;

use crate::analysis::{
    ExternalBatchBounds, ExternalProtocolLimits, ExternalTranscript, ExternalValidatedBatch,
    ExternalWarning,
};
use crate::corpus::DocumentId;

use super::{
    ExecutionBatch, ExecutionError, ExecutionLimits, ExecutionSentence, SentenceBatchSource,
    SentenceLocation,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAttestation {
    authority: Arc<str>,
    statement_digest: [u8; 32],
}

impl ExternalAttestation {
    pub fn new(
        authority: impl Into<Arc<str>>,
        statement_digest: [u8; 32],
    ) -> Result<Self, &'static str> {
        let authority = authority.into();
        if authority.is_empty() {
            return Err("external attestation authority must be nonempty");
        }
        Ok(Self {
            authority,
            statement_digest,
        })
    }

    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    #[must_use]
    pub const fn statement_digest(&self) -> &[u8; 32] {
        &self.statement_digest
    }
}

pub struct OfflineExternalSource<R> {
    transcript: ExternalTranscript<R>,
    attestation: ExternalAttestation,
    next_corpus_sentence: u64,
    next_document_ordinal: u64,
    last_warnings: Box<[ExternalWarning]>,
    failed: bool,
}

#[cfg(unix)]
pub struct LiveExternalSource<I> {
    process: Option<super::ExternalAdapterProcess>,
    batches: I,
    source_name: Arc<str>,
    next_corpus_sentence: u64,
    next_document_ordinal: u64,
    last_warnings: Box<[ExternalWarning]>,
    finished: bool,
    failed: bool,
}

#[cfg(unix)]
impl<I> fmt::Debug for LiveExternalSource<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveExternalSource")
            .field("source_name", &self.source_name)
            .field("next_corpus_sentence", &self.next_corpus_sentence)
            .field("next_document_ordinal", &self.next_document_ordinal)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl<R: BufRead> fmt::Debug for OfflineExternalSource<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfflineExternalSource")
            .field("attestation", &self.attestation)
            .field("next_corpus_sentence", &self.next_corpus_sentence)
            .field("next_document_ordinal", &self.next_document_ordinal)
            .finish_non_exhaustive()
    }
}

impl<R: BufRead> OfflineExternalSource<R> {
    pub fn open(
        reader: R,
        attestation: ExternalAttestation,
        limits: ExternalProtocolLimits,
    ) -> Result<Self, crate::analysis::ExternalProtocolError> {
        Ok(Self {
            transcript: ExternalTranscript::open(reader, limits)?,
            attestation,
            next_corpus_sentence: 0,
            next_document_ordinal: 0,
            last_warnings: Box::new([]),
            failed: false,
        })
    }

    #[must_use]
    pub const fn trust_class(&self) -> &'static str {
        "externally-attested"
    }

    #[must_use]
    pub const fn attestation(&self) -> &ExternalAttestation {
        &self.attestation
    }

    #[must_use]
    pub fn request_digest(&self) -> [u8; 32] {
        self.transcript.request_digest()
    }

    #[must_use]
    pub fn whole_file_digest(&self) -> Option<[u8; 32]> {
        self.transcript.whole_file_digest()
    }

    #[must_use]
    pub fn last_warnings(&self) -> &[ExternalWarning] {
        &self.last_warnings
    }

    fn flatten(
        &mut self,
        batch: ExternalValidatedBatch,
        limits: &ExecutionLimits,
    ) -> Result<ExecutionBatch, ExecutionError> {
        flatten_external_batch(
            batch,
            limits,
            Arc::clone(&self.attestation.authority),
            &mut self.next_corpus_sentence,
            &mut self.next_document_ordinal,
            &mut self.last_warnings,
        )
    }
}

#[cfg(unix)]
impl<I> LiveExternalSource<I>
where
    I: Iterator<Item = Vec<crate::analysis::TextUnit>>,
{
    pub fn new(
        process: super::ExternalAdapterProcess,
        batches: I,
        source_name: impl Into<Arc<str>>,
    ) -> Result<Self, &'static str> {
        let source_name = source_name.into();
        if source_name.is_empty() {
            return Err("live external source name must be nonempty");
        }
        Ok(Self {
            process: Some(process),
            batches,
            source_name,
            next_corpus_sentence: 0,
            next_document_ordinal: 0,
            last_warnings: Box::new([]),
            finished: false,
            failed: false,
        })
    }

    #[must_use]
    pub const fn trust_class(&self) -> &'static str {
        "locally-executed"
    }

    #[must_use]
    pub fn last_warnings(&self) -> &[ExternalWarning] {
        &self.last_warnings
    }
}

#[cfg(unix)]
impl<I> SentenceBatchSource for LiveExternalSource<I>
where
    I: Iterator<Item = Vec<crate::analysis::TextUnit>>,
{
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if self.finished {
            return Ok(None);
        }
        if self.failed {
            return Err(terminal_source_error(
                self.next_corpus_sentence,
                "live external source previously failed",
            ));
        }
        limits.validate()?;
        let Some(inputs) = self.batches.next() else {
            let process = self.process.take().ok_or_else(|| ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: "live adapter process is missing".to_owned(),
            })?;
            return match process.finish() {
                Ok(()) => {
                    self.finished = true;
                    Ok(None)
                }
                Err(error) => {
                    self.failed = true;
                    Err(terminal_source_error(
                        self.next_corpus_sentence,
                        error.to_string(),
                    ))
                }
            };
        };
        let batch = self
            .process
            .as_mut()
            .ok_or_else(|| ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: "live adapter process is missing".to_owned(),
            })?
            .exchange_batch(inputs, limits)
            .map_err(|error| terminal_source_error(self.next_corpus_sentence, error.to_string()));
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                self.failed = true;
                self.process.take();
                return Err(error);
            }
        };
        let result = flatten_external_batch(
            batch,
            limits,
            Arc::clone(&self.source_name),
            &mut self.next_corpus_sentence,
            &mut self.next_document_ordinal,
            &mut self.last_warnings,
        );
        match result {
            Ok(batch) => Ok(Some(batch)),
            Err(error) => {
                self.failed = true;
                self.process.take();
                Err(error)
            }
        }
    }

    fn wants_lookahead(&self) -> bool {
        false
    }
}

fn flatten_external_batch(
    batch: ExternalValidatedBatch,
    limits: &ExecutionLimits,
    source_name: Arc<str>,
    next_corpus_sentence: &mut u64,
    next_document_ordinal: &mut u64,
    last_warnings: &mut Box<[ExternalWarning]>,
) -> Result<ExecutionBatch, ExecutionError> {
    limits.validate()?;
    let (analysis, warnings, input_record_bytes) = batch.into_parts();
    let sentence_count = analysis
        .inputs()
        .iter()
        .map(|input| input.sentences().len())
        .sum::<usize>();
    if sentence_count > limits.max_stage_sentences {
        return Err(ExecutionError::Source {
            ordinal: *next_corpus_sentence,
            message: format!(
                "external batch has {sentence_count} sentences; stage limit is {}",
                limits.max_stage_sentences
            ),
        });
    }
    if let Some((record_ordinal, actual)) = input_record_bytes
        .iter()
        .copied()
        .enumerate()
        .find(|(_, actual)| *actual > limits.max_input_record_bytes)
    {
        return Err(ExecutionError::InputRecordTooLarge {
            source_name: source_name.to_string(),
            record_ordinal: record_ordinal as u64,
            corpus_sentence_ordinal: *next_corpus_sentence,
            actual_at_least: actual,
            limit: limits.max_input_record_bytes,
        });
    }
    let input_bytes = analysis.inputs().iter().try_fold(0_usize, |total, input| {
        total
            .checked_add(input.input().len())
            .ok_or(ExecutionError::ResourceAccounting)
    })?;
    if analysis.inputs().len() > 1 && input_bytes > limits.max_stage_input_bytes {
        return Err(ExecutionError::Source {
            ordinal: *next_corpus_sentence,
            message: format!(
                "external batch retains {input_bytes} input bytes; stage limit is {}",
                limits.max_stage_input_bytes
            ),
        });
    }

    let mut output = Vec::with_capacity(sentence_count);
    let mut charges = Vec::with_capacity(sentence_count);
    let mut candidate_corpus_sentence = *next_corpus_sentence;
    let mut candidate_document_ordinal = *next_document_ordinal;
    let mut aggregate_charge = 0_usize;
    for analyzed_input in analysis.into_inputs().into_vec() {
        let (input, analyzed_sentences) = analyzed_input.into_parts();
        let document_ordinal = candidate_document_ordinal;
        candidate_document_ordinal = candidate_document_ordinal
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        for analyzed in analyzed_sentences.into_vec() {
            let sentence_bytes = analyzed.source_range().as_usize().len();
            if sentence_bytes > limits.max_sentence_bytes {
                return Err(ExecutionError::SentenceTooLarge {
                    ordinal: candidate_corpus_sentence,
                    actual: sentence_bytes,
                    limit: limits.max_sentence_bytes,
                    context: format!(" in {source_name}"),
                });
            }
            let document_sentence_ordinal = u64::from(analyzed.analyzer_sentence_ordinal());
            let words = analyzed.into_words();
            let charge = words
                .resident_bytes()
                .map_err(|_| ExecutionError::ResourceAccounting)?;
            if charge > limits.max_tokenized_bytes {
                return Err(ExecutionError::TokenizedSentenceTooLarge {
                    ordinal: candidate_corpus_sentence,
                    actual: charge,
                    limit: limits.max_tokenized_bytes,
                });
            }
            aggregate_charge = aggregate_charge
                .checked_add(charge)
                .ok_or(ExecutionError::ResourceAccounting)?;
            if aggregate_charge > limits.max_tokenized_bytes {
                return Err(ExecutionError::TokenizedSentenceTooLarge {
                    ordinal: candidate_corpus_sentence,
                    actual: aggregate_charge,
                    limit: limits.max_tokenized_bytes,
                });
            }
            output.push(ExecutionSentence {
                location: SentenceLocation {
                    source_metadata: None,
                    corpus_sentence_ordinal: candidate_corpus_sentence,
                    document_id: DocumentId::new(input.provenance().document_identity()),
                    document_ordinal,
                    document_sentence_ordinal,
                    source_name: Arc::clone(&source_name),
                },
                value: super::BoundSentenceInput::new(
                    words,
                    super::EnhancedGraphInput::NotRequested,
                ),
            });
            charges.push(charge);
            candidate_corpus_sentence = candidate_corpus_sentence
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
        }
    }
    *next_corpus_sentence = candidate_corpus_sentence;
    *next_document_ordinal = candidate_document_ordinal;
    *last_warnings = warnings;
    Ok(ExecutionBatch::bound_tokenized_with_charges(
        output, charges,
    ))
}

impl<R: BufRead> SentenceBatchSource for OfflineExternalSource<R> {
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if self.failed {
            return Err(terminal_source_error(
                self.next_corpus_sentence,
                "offline external source previously failed",
            ));
        }
        limits.validate()?;
        let bounds = execution_bounds(limits);
        let batch = self
            .transcript
            .next_validated_batch_with_bounds(bounds)
            .map_err(|error| terminal_source_error(self.next_corpus_sentence, error.to_string()));
        let result = match batch {
            Ok(Some(batch)) => self.flatten(batch, limits).map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn wants_lookahead(&self) -> bool {
        false
    }
}

fn execution_bounds(limits: &ExecutionLimits) -> ExternalBatchBounds {
    ExternalBatchBounds {
        max_input_record_bytes: limits.max_input_record_bytes,
        max_stage_input_bytes: limits.max_stage_input_bytes,
        max_stage_sentences: limits.max_stage_sentences,
        max_sentence_bytes: limits.max_sentence_bytes,
        max_tokenized_bytes: limits.max_tokenized_bytes,
    }
}

fn terminal_source_error(ordinal: u64, message: impl Into<String>) -> ExecutionError {
    ExecutionError::Source {
        ordinal,
        message: message.into(),
    }
}
