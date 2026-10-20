//! Ordered, bounded sentence sources for the execution protocol.

use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::analysis::{InputUnitId, NormalizationPolicy, TextUnit};
use crate::corpus::{DocumentId, SentenceId};
use crate::fsm::CompiledCatalog;
use crate::{
    corpus::conllu::EnhancedGraphObservation,
    token_match::{
        AttributeKey, AttributeLocator, AttributeValueRef, BoundTokenSentence, TokenInfo,
        TokenSequence,
    },
};

use super::{ExecutionError, ExecutionLimits, estimate_token_info_bytes};

/// One sentence and its stable position in an execution run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceLocation {
    /// Zero-based sentence position in the complete corpus.
    pub corpus_sentence_ordinal: u64,
    /// Stable document identifier assigned at corpus admission.
    pub document_id: DocumentId,
    /// Zero-based document position in the complete corpus.
    pub document_ordinal: u64,
    /// Zero-based sentence position within the document.
    pub document_sentence_ordinal: u64,
    /// Stable logical source name used for diagnostics and result provenance.
    pub source_name: Arc<str>,
    /// Explicitly selected source metadata; absent when projection was not requested.
    pub source_metadata: Option<Arc<serde_json::Map<String, serde_json::Value>>>,
}

/// How a sentence source treats a sentence whose tokenized residency
/// estimate exceeds `ExecutionLimits::max_tokenized_bytes`.
///
/// This is source-construction policy, not an executor limit: limits stay
/// numeric bounds (with their 1:1 CLI flags and protocol serialization),
/// while the disposition belongs to the source that materializes sentences,
/// matching where the direct (unprepared) path put its equivalent in
/// `ling`.
#[derive(Debug, Default)]
pub enum TokenizedAdmission {
    /// Fail the run on the first oversized sentence.
    #[default]
    Fail,
    /// Skip oversized sentences and count the omissions instead. The handle is
    /// bundled so an `Omit` run cannot silently lose the count: the source
    /// owns the lock while it decodes, and the caller reads the summary after
    /// the run completes (the source is consumed by `execute`).
    Omit(Arc<Mutex<OversizedSentenceOmissions>>),
}

/// Omissions recorded by a source running under [`TokenizedAdmission::Omit`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OversizedSentenceOmissions {
    /// Sentences omitted as oversized.
    pub count: u64,
    /// Ordinal of the first omitted sentence (placeholder `u64::MAX` until
    /// the first omission lands).
    pub first_ordinal: u64,
    /// Logical source name of the first omitted sentence.
    pub first_source_name: Option<Arc<str>>,
    /// Tokenized residency estimate of the first omitted sentence.
    pub first_actual: usize,
    /// The bound it exceeded.
    pub limit: usize,
}

impl OversizedSentenceOmissions {
    /// Records one omission; `first_*` fields hold the first occurrence.
    pub fn record(
        &mut self,
        ordinal: u64,
        source_name: Option<Arc<str>>,
        actual: usize,
        limit: usize,
    ) {
        if self.count == 0 {
            self.first_ordinal = ordinal;
            self.first_source_name = source_name;
            self.first_actual = actual;
            self.limit = limit;
        }
        self.count += 1;
    }

    /// Omissions summary when nonempty, for degraded-run reporting.
    pub fn summary(&self) -> Option<Self> {
        (self.count > 0).then(|| self.clone())
    }
}

impl SentenceLocation {
    pub(crate) fn run_local(ordinal: u64) -> Self {
        let sentence_id = SentenceId::new(ordinal);
        Self {
            source_metadata: None,
            corpus_sentence_ordinal: sentence_id.get(),
            document_id: DocumentId::new(0),
            document_ordinal: 0,
            document_sentence_ordinal: sentence_id.get(),
            source_name: Arc::from(""),
        }
    }
}

/// One sentence and its authoritative position in an execution run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionSentence<T> {
    /// Source-owned sentence provenance carried unchanged through execution.
    pub location: SentenceLocation,
    /// Source-owned sentence payload transferred to the caller.
    pub value: T,
}

/// Whether enhanced graph evidence was requested for one bound sentence.
///
/// `NotRequested` is an execution capability choice, not a claim about the
/// corpus. An [`Observation`](Self::Observation) retains the corpus's own
/// unavailable/incomplete/complete distinction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnhancedGraphInput {
    NotRequested,
    Observation(EnhancedGraphObservation),
}

/// One schema-bound token sentence and its independently requested graph input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundSentenceInput {
    words: BoundTokenSentence,
    enhanced_graph: EnhancedGraphInput,
}

impl BoundSentenceInput {
    #[must_use]
    pub const fn new(words: BoundTokenSentence, enhanced_graph: EnhancedGraphInput) -> Self {
        Self {
            words,
            enhanced_graph,
        }
    }

    #[must_use]
    pub const fn words(&self) -> &BoundTokenSentence {
        &self.words
    }

    #[must_use]
    pub const fn enhanced_graph(&self) -> &EnhancedGraphInput {
        &self.enhanced_graph
    }

    #[must_use]
    pub fn into_words(self) -> BoundTokenSentence {
        self.words
    }

    /// Owned live bytes charged while this input is resident in execution.
    #[must_use]
    pub fn resident_bytes(&self) -> Option<usize> {
        let words = self.words.resident_bytes().ok()?;
        let graph = match &self.enhanced_graph {
            EnhancedGraphInput::NotRequested => 0,
            EnhancedGraphInput::Observation(observation) => observation.resident_bytes()?,
        };
        words.checked_add(graph)
    }
}

impl TokenSequence for BoundSentenceInput {
    fn len(&self) -> usize {
        self.words.len()
    }

    fn char_span(&self, token: usize) -> Option<crate::token_match::CharSpan> {
        self.words.char_span(token)
    }

    fn get(
        &self,
        token: usize,
        attribute: crate::token_match::TokenAttributeIndex,
    ) -> Option<&compact_str::CompactString> {
        TokenSequence::get(&self.words, token, attribute)
    }

    fn get_key(&self, token: usize, key: &AttributeKey) -> Option<&compact_str::CompactString> {
        self.words.get_key(token, key)
    }

    fn value_key(&self, token: usize, key: &AttributeKey) -> Option<AttributeValueRef<'_>> {
        self.words.value_key(token, key)
    }

    fn value_locator(
        &self,
        token: usize,
        locator: &AttributeLocator,
    ) -> Option<AttributeValueRef<'_>> {
        self.words.value_locator(token, locator)
    }
}

/// One physical input batch for the execution protocol.
#[derive(Debug)]
pub enum ExecutionBatch {
    /// Raw UTF-8 sentences that still require tokenization.
    Text(Vec<ExecutionSentence<TextUnit>>),
    /// Already-tokenized sentences, normally decoded from a cache.
    Tokenized {
        /// The batch's sentences, in ascending ordinal order.
        sentences: Vec<ExecutionSentence<Vec<TokenInfo>>>,
        /// Exact per-sentence residency charges, when the source already knows
        /// them, parallel to `sentences`.
        ///
        /// The executor otherwise derives them by walking every attribute of
        /// every token — the one piece of per-token work it does before
        /// matching, on the thread that paces a run. A prepared V6 source
        /// computes the same quantity exactly while preflighting a block, from
        /// the block's own string table and without materializing a token, so
        /// supplying it here deletes that walk rather than moving it.
        ///
        /// Supplying a charge is a claim the executor takes at face value: it
        /// checks the shape and the per-sentence limit, not the arithmetic.
        /// `None` keeps the derivation, and is right for any source that does
        /// not compute residency as a side effect of reading.
        charges: Option<Vec<usize>>,
    },
    /// Schema-bound row-major sentences ready for bound lingrep patterns execution.
    BoundTokenized {
        sentences: Vec<ExecutionSentence<BoundSentenceInput>>,
        /// Full-schema admission charges retained from the source artifact.
        charges: Vec<usize>,
    },
}

impl ExecutionBatch {
    /// A tokenized batch whose residency the executor must derive.
    #[must_use]
    pub fn tokenized(sentences: Vec<ExecutionSentence<Vec<TokenInfo>>>) -> Self {
        Self::Tokenized {
            sentences,
            charges: None,
        }
    }

    /// A tokenized batch carrying the exact residency its source computed.
    ///
    /// The charges must be parallel to `sentences` and must equal what
    /// [`estimate_token_info_bytes`](super::estimate_token_info_bytes) would
    /// return for each; the executor validates the first and trusts the second.
    #[must_use]
    pub fn tokenized_with_charges(
        sentences: Vec<ExecutionSentence<Vec<TokenInfo>>>,
        charges: Vec<usize>,
    ) -> Self {
        Self::Tokenized {
            sentences,
            charges: Some(charges),
        }
    }

    #[must_use]
    pub fn bound_tokenized_with_charges(
        sentences: Vec<ExecutionSentence<BoundSentenceInput>>,
        charges: Vec<usize>,
    ) -> Self {
        Self::BoundTokenized { sentences, charges }
    }
}

/// Supplies ordered, fallible physical sentence batches.
///
/// Implementations are obligated to enforce the applicable source limits and
/// must not retain a previously returned batch after the next pull begins.
/// This is a protocol obligation rather than a trust boundary: the executor
/// defensively revalidates every returned batch before tokenization or
/// matching.
pub trait SentenceBatchSource {
    /// Returns the next non-empty batch, or `None` at clean EOF.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ExecutionError`] for invalid limits, rejected input,
    /// source failures, or cache decode failures. Input failures are terminal
    /// and are never converted to clean EOF.
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError>;

    /// Declares whether the executor may run this source one batch ahead of
    /// publication, on its own thread.
    ///
    /// Opting in costs one extra resident batch and lets a source that stops
    /// early produce one batch nobody consumes. A source may say yes only if
    /// **how far it read is not observable** — nothing it reports, and nothing
    /// derived from it, may depend on the reader's position rather than on
    /// what was published.
    ///
    /// The prepared V6 source qualifies: its scope universe is precomputed from
    /// the whole artifact and stays `basis: Corpus` whether or not the run
    /// stopped early. A direct source does not — it reports
    /// `basis: ObservedPrefix`, whose counts are by definition whatever its
    /// reader observed — and has little to gain, since its `next_batch` reads
    /// lines while tokenization happens on the consumer side.
    ///
    /// Wrappers must delegate this alongside `next_batch` so that each source
    /// retains its admission and accounting behavior. There is no default.
    fn wants_lookahead(&self) -> bool;
}

impl<S: SentenceBatchSource + ?Sized> SentenceBatchSource for Box<S> {
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        (**self).next_batch(limits)
    }

    fn wants_lookahead(&self) -> bool {
        (**self).wants_lookahead()
    }
}

#[derive(Debug)]
enum TerminalFailure {
    Source {
        ordinal: u64,
        message: String,
    },
    SentenceTooLarge {
        ordinal: u64,
        actual: usize,
        limit: usize,
        context: String,
    },
    TokenizedSentenceTooLarge {
        ordinal: u64,
        actual: usize,
        limit: usize,
    },
}

impl TerminalFailure {
    fn execution_error(&self) -> ExecutionError {
        match self {
            Self::Source { ordinal, message } => ExecutionError::Source {
                ordinal: *ordinal,
                message: message.clone(),
            },
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
            Self::TokenizedSentenceTooLarge {
                ordinal,
                actual,
                limit,
            } => ExecutionError::TokenizedSentenceTooLarge {
                ordinal: *ordinal,
                actual: *actual,
                limit: *limit,
            },
        }
    }
}

/// A bounded adapter for an infallible raw-text iterator.
pub struct TextIteratorSource<I> {
    source: FallibleTextIteratorSource<std::iter::Map<I, fn(String) -> Result<String, Infallible>>>,
}

impl<I> TextIteratorSource<I>
where
    I: Iterator<Item = String>,
{
    /// Creates a source whose first admitted sentence receives ordinal zero.
    pub fn new(iterator: I) -> Self {
        Self {
            source: FallibleTextIteratorSource::new(
                iterator.map(infallible_text as fn(String) -> Result<String, Infallible>),
            ),
        }
    }
}

fn infallible_text(value: String) -> Result<String, Infallible> {
    Ok(value)
}

impl<I> SentenceBatchSource for TextIteratorSource<I>
where
    I: Iterator<Item = String>,
{
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        self.source.next_batch(limits)
    }

    fn wants_lookahead(&self) -> bool {
        self.source.wants_lookahead()
    }
}

/// A bounded adapter for a raw-text iterator whose next item may fail.
pub struct FallibleTextIteratorSource<I> {
    iterator: I,
    pending: Option<String>,
    next_ordinal: u64,
    terminal_failure: Option<TerminalFailure>,
}

impl<I> FallibleTextIteratorSource<I> {
    /// Creates a source whose first admitted sentence receives ordinal zero.
    pub fn new(iterator: I) -> Self {
        Self {
            iterator,
            pending: None,
            next_ordinal: 0,
            terminal_failure: None,
        }
    }
}

impl<I, E> SentenceBatchSource for FallibleTextIteratorSource<I>
where
    I: Iterator<Item = Result<String, E>>,
    E: fmt::Display,
{
    fn wants_lookahead(&self) -> bool {
        // Reads from a caller-supplied iterator, whose observed position is
        // the caller's to interpret.
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

        let mut batch = Vec::new();
        let mut input_bytes = 0usize;
        while batch.len() < limits.max_stage_sentences {
            let value = match self.pending.take() {
                Some(value) => value,
                None => match self.iterator.next() {
                    Some(Ok(value)) => value,
                    Some(Err(error)) => {
                        self.terminal_failure = Some(TerminalFailure::Source {
                            ordinal: self.next_ordinal,
                            message: error.to_string(),
                        });
                        if batch.is_empty() {
                            return Err(self
                                .terminal_failure
                                .as_ref()
                                .expect("just stored")
                                .execution_error());
                        }
                        break;
                    }
                    None => break,
                },
            };

            let value_bytes = value.len();
            if value_bytes > limits.max_sentence_bytes {
                self.terminal_failure = Some(TerminalFailure::SentenceTooLarge {
                    ordinal: self.next_ordinal,
                    actual: value_bytes,
                    limit: limits.max_sentence_bytes,
                    context: String::new(),
                });
                if batch.is_empty() {
                    return Err(self
                        .terminal_failure
                        .as_ref()
                        .expect("just stored")
                        .execution_error());
                }
                break;
            }

            let next_input_bytes = input_bytes
                .checked_add(value_bytes)
                .ok_or(ExecutionError::ResourceAccounting)?;
            if !batch.is_empty() && next_input_bytes > limits.max_stage_input_bytes {
                self.pending = Some(value);
                break;
            }

            let ordinal = self.next_ordinal;
            self.next_ordinal = self
                .next_ordinal
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            input_bytes = next_input_bytes;
            batch.push(ExecutionSentence {
                location: SentenceLocation::run_local(ordinal),
                value: TextUnit::sentence(
                    InputUnitId::new(ordinal),
                    value,
                    NormalizationPolicy::None,
                )
                .map_err(|error| ExecutionError::Source {
                    ordinal,
                    message: error.to_string(),
                })?,
            });
        }

        Ok((!batch.is_empty()).then_some(ExecutionBatch::Text(batch)))
    }
}

/// A bounded adapter for tokenized iterators that preserve read/decode errors.
pub struct FallibleTokenizedIteratorSource<I> {
    iterator: I,
    next_ordinal: u64,
    terminal_failure: Option<TerminalFailure>,
}

impl<I> FallibleTokenizedIteratorSource<I> {
    /// Creates a source whose first admitted sentence receives ordinal zero.
    pub fn new(iterator: I) -> Self {
        Self {
            iterator,
            next_ordinal: 0,
            terminal_failure: None,
        }
    }
}

impl<I, E> SentenceBatchSource for FallibleTokenizedIteratorSource<I>
where
    I: Iterator<Item = Result<Vec<TokenInfo>, E>>,
    E: fmt::Display,
{
    fn wants_lookahead(&self) -> bool {
        // Reads from a caller-supplied iterator, whose observed position is
        // the caller's to interpret.
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

        let value = match self.iterator.next() {
            Some(Ok(value)) => value,
            Some(Err(error)) => {
                self.terminal_failure = Some(TerminalFailure::Source {
                    ordinal: self.next_ordinal,
                    message: error.to_string(),
                });
                return Err(self
                    .terminal_failure
                    .as_ref()
                    .expect("just stored")
                    .execution_error());
            }
            None => return Ok(None),
        };
        let actual = estimate_token_info_bytes(&value)?;
        if actual > limits.max_tokenized_bytes {
            self.terminal_failure = Some(TerminalFailure::TokenizedSentenceTooLarge {
                ordinal: self.next_ordinal,
                actual,
                limit: limits.max_tokenized_bytes,
            });
            return Err(self
                .terminal_failure
                .as_ref()
                .expect("just stored")
                .execution_error());
        }

        let ordinal = self.next_ordinal;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        Ok(Some(ExecutionBatch::tokenized(vec![ExecutionSentence {
            location: SentenceLocation::run_local(ordinal),
            value,
        }])))
    }
}

/// Returns the cache projection required by a compiled lingrep patterns run.
///
/// Matcher-observed attributes are supplemented with `orth`, which aggregate
/// sinks use to render match and context surfaces.
#[must_use]
pub fn cached_run_attributes(fsm: &CompiledCatalog) -> Vec<AttributeKey> {
    let mut attributes = fsm.execution_attributes();
    let text = AttributeKey::new("text").expect("text is a canonical attribute");
    if !attributes.contains(&text) {
        attributes.push(text);
    }
    // Publication reads attributes matching never sees: a `key:` projection, a
    // transform guard, a `substitute` target, the written form `join` renders.
    // The compiled automaton cannot know about any of them, so a run that
    // cached only its own requirements would publish records whose derived
    // fields silently came back empty on a prepared corpus and correct on a
    // direct one.
    attributes.sort_unstable();
    attributes.dedup();
    attributes
}

#[cfg(test)]
mod bound_sentence_input_tests {
    use super::*;
    use crate::token_match::{
        AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema, TokenSequence,
    };

    #[test]
    fn wrapper_preserves_schema_key_lookup_used_by_publication() {
        let schema =
            TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()])
                .unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let words =
            BoundTokenSentence::from_projected_values(&projection, 1, vec![Some("猫".into())])
                .unwrap();
        let text = AttributeKey::new("text").unwrap();
        assert_eq!(
            words
                .get_key(0, &text)
                .map(compact_str::CompactString::as_str),
            Some("猫")
        );

        let input = BoundSentenceInput::new(words, EnhancedGraphInput::NotRequested);
        assert_eq!(
            input
                .get_key(0, &text)
                .map(compact_str::CompactString::as_str),
            Some("猫")
        );
    }
}
