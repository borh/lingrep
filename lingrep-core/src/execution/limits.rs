use thiserror::Error;

use crate::token_match::TokenInfo;

/// Maximum accepted UTF-8 byte length for one input sentence by default.
pub const DEFAULT_MAX_SENTENCE_BYTES: usize = DEFAULT_MAX_INPUT_RECORD_BYTES;
/// Maximum number of sentences retained in one execution stage by default.
///
/// Chosen so that [`DEFAULT_MAX_TOKENIZED_BYTES`] normally decides retained
/// batch width before this count does.
pub const DEFAULT_MAX_STAGE_SENTENCES: usize = 2048;
/// Target input byte length retained in one execution stage by default.
pub const DEFAULT_MAX_STAGE_INPUT_BYTES: usize = 128 * 1024;
/// Maximum accepted byte length for one physical input record by default.
///
/// A record is one line of a corpus source — a JSONL document, or a plaintext
/// or Markdown line — and its length is a property of the corpus, not of how
/// the pipeline batches work. This limit is distinct from
/// [`DEFAULT_MAX_STAGE_INPUT_BYTES`]: a document larger than one execution
/// batch feeds several batches, so bounding a record by the batch budget
/// would reject corpora for a reason unrelated to residency.
pub const DEFAULT_MAX_INPUT_RECORD_BYTES: usize = 1024 * 1024;
/// Default resident-byte cap for one tokenized sentence or aggregate matching batch.
pub const DEFAULT_MAX_TOKENIZED_BYTES: usize = 32 * 1024 * 1024;

/// Explicit resource limits for a bounded execution pipeline.
///
/// Each bound is named for the thing it scopes — one sentence, one physical
/// input record, one execution stage, or resident tokenized bytes. A bound
/// belongs to exactly one scope, and this type is the single owner of every
/// rule relating them, so a caller cannot enforce one of those rules and miss
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// Maximum accepted UTF-8 byte length for one input sentence.
    pub max_sentence_bytes: usize,
    /// Maximum accepted byte length for one physical input record: a JSONL
    /// document, or a plaintext or Markdown line.
    ///
    /// A record's length is a property of the corpus, not of how the pipeline
    /// batches work, so this is independent of [`Self::max_stage_input_bytes`].
    /// A document larger than one stage is ordinary and feeds several.
    pub max_input_record_bytes: usize,
    /// Maximum number of sentences retained in one execution stage.
    pub max_stage_sentences: usize,
    /// Target input byte length retained in one execution stage.
    /// A larger accepted sentence runs alone so batching never splits a sentence.
    pub max_stage_input_bytes: usize,
    /// Maximum estimated resident bytes for one tokenized sentence and for
    /// each aggregate batch, including retained lookaround context.
    pub max_tokenized_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_sentence_bytes: DEFAULT_MAX_SENTENCE_BYTES,
            max_input_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
            max_stage_sentences: DEFAULT_MAX_STAGE_SENTENCES,
            max_stage_input_bytes: DEFAULT_MAX_STAGE_INPUT_BYTES,
            max_tokenized_bytes: DEFAULT_MAX_TOKENIZED_BYTES,
        }
    }
}

impl ExecutionLimits {
    /// Validates that every configured resource limit permits bounded progress.
    pub fn validate(&self) -> Result<(), ExecutionError> {
        for (field, value) in [
            ("max_sentence_bytes", self.max_sentence_bytes),
            ("max_input_record_bytes", self.max_input_record_bytes),
            ("max_stage_sentences", self.max_stage_sentences),
            ("max_stage_input_bytes", self.max_stage_input_bytes),
            ("max_tokenized_bytes", self.max_tokenized_bytes),
        ] {
            if value == 0 {
                return Err(ExecutionError::InvalidLimit {
                    field,
                    message: "must be greater than zero".to_owned(),
                });
            }
        }

        Ok(())
    }
}

/// Estimates the resident allocation size of a tokenized sentence.
///
/// The estimate includes the inline `TokenInfo` storage and the capacity of
/// every heap-allocated token attribute. Equal attribute contents are counted
/// once per token because each `TokenInfo` owns its own `CompactString`.
pub fn estimate_token_info_bytes(sentence: &[TokenInfo]) -> Result<usize, ExecutionError> {
    let mut bytes = std::mem::size_of_val(sentence);

    for token in sentence {
        for attribute in token.attribute_values().iter().flatten() {
            if attribute.is_heap_allocated() {
                bytes = checked_add(bytes, attribute.capacity())?;
            }
        }
    }

    Ok(bytes)
}

fn checked_add(left: usize, right: usize) -> Result<usize, ExecutionError> {
    left.checked_add(right)
        .ok_or(ExecutionError::ResourceAccounting)
}

fn source_suffix(source_name: Option<&str>) -> String {
    source_name.map_or_else(String::new, |name| format!(" in '{name}'"))
}

fn diagnostic_bytes(bytes: usize) -> String {
    for (unit, name) in [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")] {
        if bytes >= unit {
            let tenths = ((bytes as u128) * 10 + (unit as u128) / 2) / (unit as u128);
            return format!("{}.{} {name} ({bytes} bytes)", tenths / 10, tenths % 10);
        }
    }
    format!("{bytes} bytes")
}

/// Errors reported while enforcing execution resource bounds.
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error(transparent)]
    Matching(#[from] crate::fsm::BoundExecutionError),
    /// A caller-supplied execution limit is invalid.
    #[error("invalid execution limit {field}: {message}")]
    InvalidLimit {
        /// The invalid limit field.
        field: &'static str,
        /// The violated invariant.
        message: String,
    },
    /// An intermediate resource-accounting calculation overflowed.
    #[error("The estimated memory needed for analyzed tokens is too large to calculate")]
    ResourceAccounting,
    /// A source failed before yielding its next sentence.
    #[error("source failed before sentence {ordinal}: {message}")]
    Source {
        /// Ordinal of the sentence the source was about to yield.
        ordinal: u64,
        /// Source-provided failure description.
        message: String,
    },
    #[error("{source}")]
    Analysis {
        ordinal: Option<u64>,
        source_name: Option<String>,
        #[source]
        source: Box<crate::analysis::AnalysisError>,
    },
    /// An input sentence exceeds the configured byte limit.
    #[error("Sentence {ordinal}{location} is too long: {size}; the limit is {maximum}.", location = source_suffix(.source_name.as_deref()), size = diagnostic_bytes(*.actual), maximum = diagnostic_bytes(*.limit))]
    SentenceTooLarge {
        /// Ordinal of the rejected sentence.
        ordinal: u64,
        /// Exact UTF-8 byte length.
        actual: usize,
        /// Configured upper bound.
        limit: usize,
        source_name: Option<String>,
    },
    /// A bounded reader stopped after proving that an input sentence exceeds
    /// the configured byte limit, before its exact length was known.
    #[error("Sentence {ordinal}{location} is too long: at least {size}; the limit is {maximum}.", location = source_suffix(.source_name.as_deref()), size = diagnostic_bytes(*.minimum), maximum = diagnostic_bytes(*.limit))]
    SentenceTooLargeAtLeast {
        /// Ordinal of the rejected sentence.
        ordinal: u64,
        /// Smallest UTF-8 byte length proved by the reader.
        minimum: usize,
        /// Configured upper bound.
        limit: usize,
        source_name: Option<String>,
    },
    /// A physical input record exceeded its bound before a parser could
    /// allocate or interpret the complete record.
    #[error(
        "Input record {record_ordinal} in '{source_name}' is too large: at least {size}; \
         the limit is {maximum}. Reading stopped before sentence {corpus_sentence_ordinal}.",
        size = diagnostic_bytes(*.actual_at_least), maximum = diagnostic_bytes(*.limit)
    )]
    InputRecordTooLarge {
        source_name: String,
        record_ordinal: u64,
        corpus_sentence_ordinal: u64,
        actual_at_least: usize,
        limit: usize,
    },
    /// Filesystem discovery failed with a stable category and logical path.
    #[error("source discovery failed ({code}) at {logical_path}: {message}")]
    Discovery {
        code: &'static str,
        logical_path: String,
        message: String,
    },
    /// Reading or parsing a direct input source failed.
    #[error("input failed before sentence {ordinal} in {source_name}: {message}")]
    Input {
        source_name: String,
        ordinal: u64,
        kind: std::io::ErrorKind,
        message: String,
    },
    /// A tokenized sentence exceeds the configured residency estimate.
    #[error(
        "Sentence {ordinal}{location} exceeds the memory limit for analyzed tokens: estimated {size}; limit {maximum}.", location = source_suffix(.source_name.as_deref()), size = diagnostic_bytes(*.actual), maximum = diagnostic_bytes(*.limit)
    )]
    TokenizedSentenceTooLarge {
        /// Ordinal of the rejected sentence.
        ordinal: u64,
        /// Estimated resident byte length.
        actual: usize,
        /// Configured upper bound.
        limit: usize,
        source_name: Option<String>,
    },
    #[error("Matching this sentence and its lookaround context requires an estimated {size} of memory; the limit is {maximum}.", size = diagnostic_bytes(*.actual), maximum = diagnostic_bytes(*.limit))]
    LookaroundWindowTooLarge { actual: usize, limit: usize },
    /// Decoding a cache failed.
    #[error("cache decode failed: {0}")]
    Cache(#[source] std::io::Error),
    /// Writing results to a sink failed.
    #[error("result sink failed: {0}")]
    Sink(String),
    /// Writing authoritative or projected output failed without erasing its
    /// I/O category.
    #[error("output failed: {message}")]
    Output {
        kind: std::io::ErrorKind,
        message: String,
    },
    /// Observing a completed batch failed.
    #[error("batch observer failed: {0}")]
    Observer(String),
    /// A result projection required an attribute that the token schema
    /// explicitly records as absent.
    #[error(
        "missing required attribute {attribute} at {source_name}, corpus sentence \
         {corpus_sentence_ordinal}, token {token_index}"
    )]
    MissingRequiredAttribute {
        attribute: &'static str,
        source_name: String,
        corpus_sentence_ordinal: u64,
        token_index: usize,
    },
    /// The independently observed ordered logical-name stream disagreed with
    /// completed discovery.
    #[error(
        "The source files observed while reading do not match the files found during discovery"
    )]
    InventoryMismatch,
    #[error("result policy could not be applied: {0}")]
    PolicyAnchor(#[source] crate::fsm::policy::AnchorResolutionError),
}

#[cfg(test)]
mod tests {
    use super::{ExecutionError, checked_add};

    #[test]
    fn diagnostic_sizes_include_readable_units_and_exact_bytes() {
        for (bytes, expected) in [
            (0, "0 bytes"),
            (1023, "1023 bytes"),
            (1024, "1.0 KiB (1024 bytes)"),
            (1_048_576, "1.0 MiB (1048576 bytes)"),
            (1_073_741_824, "1.0 GiB (1073741824 bytes)"),
            (36_804_067, "35.1 MiB (36804067 bytes)"),
        ] {
            assert_eq!(super::diagnostic_bytes(bytes), expected);
        }
        assert!(super::diagnostic_bytes(usize::MAX).contains(&format!("({} bytes)", usize::MAX)));
    }

    #[test]
    fn checked_add_reports_resource_accounting_overflow() {
        assert!(matches!(
            checked_add(usize::MAX, 1),
            Err(ExecutionError::ResourceAccounting)
        ));
    }
}
