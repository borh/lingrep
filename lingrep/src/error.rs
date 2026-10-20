//! CLI-specific error handling

use thiserror::Error;

use crate::protocol::diagnostic::RetryPolicy;

/// Process termination states with authoritative shell exit codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    Success,
    Usage,
    Failure,
    SigInt,
    SigTerm,
}

/// Stable failure categories whose process projection is fixed at the CLI
/// boundary. Later command implementations may add detail without changing
/// this mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCode {
    CommandNotImplemented,
    Io,
    Output,
    SignalHandler,
    PublicationDurabilityUncertain,
    PublicationCommittedOutputFailed,
    ManpageCleanupFailed,
    ProtocolContract,
}

impl FailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CommandNotImplemented => "command_not_implemented",
            Self::Io => "io",
            Self::Output => "output",
            Self::SignalHandler => "signal_handler",
            Self::PublicationDurabilityUncertain => "publication_durability_uncertain",
            Self::PublicationCommittedOutputFailed => "publication_committed_output_failed",
            Self::ManpageCleanupFailed => "manpage_cleanup_failed",
            Self::ProtocolContract => "protocol_contract",
        }
    }
}

/// Convert a typed terminal state to the process status promised by the public
/// command contract.
pub const fn exit_code(termination: Termination) -> i32 {
    match termination {
        Termination::Success => 0,
        Termination::Usage => 2,
        Termination::Failure => 1,
        Termination::SigInt => 130,
        Termination::SigTerm => 143,
    }
}

/// Artifact, input, execution, and output failures all exit one. In
/// particular, publication durability uncertainty is not a usage error even
/// though retrying blindly is unsafe.
pub const fn termination_for_failure(_code: FailureCode) -> Termination {
    Termination::Failure
}

/// Project only the two cooperatively handled process signals.
#[cfg(unix)]
pub const fn termination_for_signal(signal: i32) -> Option<Termination> {
    match signal {
        signal_hook::consts::SIGINT => Some(Termination::SigInt),
        signal_hook::consts::SIGTERM => Some(Termination::SigTerm),
        _ => None,
    }
}

#[cfg(not(unix))]
pub const fn termination_for_signal(_signal: i32) -> Option<Termination> {
    None
}

#[derive(Error, Debug)]
pub enum CliError {
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("{command} does not support {output} output")]
    UnsupportedOutput {
        command: &'static str,
        output: &'static str,
    },

    #[error("{message}")]
    CommandFailure { code: FailureCode, message: String },

    #[error("File not found: {0}")]
    FileNotFound(std::path::PathBuf),

    #[error("Pattern parse error: {0}")]
    PatternParse(String),

    #[error("Pattern parse error: {error}")]
    CatalogCompile {
        #[source]
        error: lingrep_core::fsm::CompileError,
        source_text: String,
    },

    #[error("Invalid inline catalog: {0}")]
    InlineCatalogShape(String),

    #[error("Dictionary error: {0}")]
    Dictionary(String),

    #[error("Corpus error: {0}")]
    Corpus(String),

    #[error("Tokenized JSON record before sentence {ordinal} exceeds encoded byte limit {limit}")]
    TokenizedRecordTooLarge { ordinal: usize, limit: usize },

    #[error("Tokenized JSON record before sentence {ordinal} is malformed: {source}")]
    TokenizedRecordMalformed {
        ordinal: usize,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Summary aggregation error: {0}")]
    Aggregation(#[source] std::io::Error),

    #[error("Execution error: {0}")]
    Execution(#[from] lingrep_core::execution::ExecutionError),

    #[error(
        "execution continued after skipping oversized admitted sentences: count={count}; \
         first was sentence {first_ordinal} from {first_source_name}, \
         {first_actual} bytes with limit {limit}"
    )]
    OversizedSentencesSkipped {
        count: u64,
        first_source_name: String,
        first_ordinal: u64,
        first_actual: usize,
        limit: usize,
    },

    #[error("Prepared corpus error: {0}")]
    Artifact(#[from] lingrep_corpus::CorpusError),

    #[error("{source}")]
    Run {
        run_id: String,
        observed_input_digest: Option<String>,
        #[source]
        source: Box<CliError>,
    },
}

impl CliError {
    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Run { source, .. } => source.code(),
            Self::InvalidArgs(_) => "invalid_invocation",
            Self::UnsupportedOutput { .. } => "unsupported_output",
            Self::CommandFailure { code, .. } => code.as_str(),
            Self::FileNotFound(_) => "file_not_found",
            Self::PatternParse(_) | Self::CatalogCompile { .. } => "catalog_validation",
            Self::InlineCatalogShape(_) => "inline_catalog_shape",
            Self::Dictionary(_) => "dictionary",
            Self::Corpus(_) => "corpus",
            Self::TokenizedRecordTooLarge { .. } => "tokenized_record_too_large",
            Self::TokenizedRecordMalformed { .. } => "tokenized_record_malformed",
            Self::Parse(_) => "parse",
            Self::Io(_) => "io",
            Self::Aggregation(error) if error.kind() == std::io::ErrorKind::StorageFull => {
                "temp_budget_exceeded"
            }
            Self::Aggregation(_) => "aggregation",
            Self::Execution(
                lingrep_core::execution::ExecutionError::MissingRequiredAttribute { .. },
            ) => "missing_required_attribute",
            Self::Execution(lingrep_core::execution::ExecutionError::InputRecordTooLarge {
                ..
            }) => "input_record_too_large",
            Self::Execution(lingrep_core::execution::ExecutionError::Discovery {
                code, ..
            }) => code,
            Self::Execution(lingrep_core::execution::ExecutionError::Input { .. }) => "input",
            Self::Execution(
                lingrep_core::execution::ExecutionError::SentenceTooLarge { .. }
                | lingrep_core::execution::ExecutionError::SentenceTooLargeAtLeast { .. },
            ) => "sentence_too_large",
            Self::Execution(
                lingrep_core::execution::ExecutionError::TokenizedSentenceTooLarge { .. },
            ) => "tokenized_sentence_too_large",
            Self::Execution(lingrep_core::execution::ExecutionError::Output {
                kind: std::io::ErrorKind::BrokenPipe,
                ..
            }) => "broken_pipe",
            Self::Execution(lingrep_core::execution::ExecutionError::Output { .. }) => "output",
            Self::Execution(lingrep_core::execution::ExecutionError::InventoryMismatch) => {
                "source_inventory_mismatch"
            }
            Self::Execution(_) => "execution",
            Self::OversizedSentencesSkipped { .. } => "oversized_sentences_skipped",
            Self::Artifact(error) => error.code(),
        }
    }

    #[must_use]
    pub fn is_broken_pipe(&self) -> bool {
        self.code() == "broken_pipe"
    }

    /// Whether repeating an operation is safe, requires a changed condition,
    /// or risks compounding an uncertain publication.
    #[must_use]
    pub fn retry_policy(&self) -> RetryPolicy {
        match self {
            Self::Run { source, .. } => source.retry_policy(),
            Self::Artifact(lingrep_corpus::CorpusError::PublicationDurabilityUncertain {
                ..
            })
            | Self::CommandFailure {
                code: FailureCode::PublicationDurabilityUncertain,
                ..
            } => RetryPolicy::Unsafe,
            Self::Io(_)
            | Self::Aggregation(_)
            | Self::Execution(
                lingrep_core::execution::ExecutionError::Source { .. }
                | lingrep_core::execution::ExecutionError::Input { .. }
                | lingrep_core::execution::ExecutionError::Cache(_)
                | lingrep_core::execution::ExecutionError::Sink(_)
                | lingrep_core::execution::ExecutionError::Output { .. }
                | lingrep_core::execution::ExecutionError::Observer(_),
            )
            | Self::Artifact(lingrep_corpus::CorpusError::Io(_))
            | Self::CommandFailure {
                code: FailureCode::Io | FailureCode::Output | FailureCode::SignalHandler,
                ..
            } => RetryPolicy::Safe,
            Self::InvalidArgs(_)
            | Self::UnsupportedOutput { .. }
            | Self::FileNotFound(_)
            | Self::PatternParse(_)
            | Self::CatalogCompile { .. }
            | Self::InlineCatalogShape(_)
            | Self::Dictionary(_)
            | Self::Corpus(_)
            | Self::TokenizedRecordTooLarge { .. }
            | Self::TokenizedRecordMalformed { .. }
            | Self::Parse(_)
            | Self::Execution(_)
            | Self::OversizedSentencesSkipped { .. }
            | Self::Artifact(_)
            | Self::CommandFailure {
                code: FailureCode::CommandNotImplemented,
                ..
            }
            | Self::CommandFailure {
                code:
                    FailureCode::PublicationCommittedOutputFailed
                    | FailureCode::ManpageCleanupFailed
                    | FailureCode::ProtocolContract,
                ..
            } => RetryPolicy::AfterChange,
        }
    }

    /// Invocation and catalog validation are usage failures; all runtime,
    /// resource, artifact, and output failures exit one.
    pub fn termination(&self) -> Termination {
        match self {
            Self::Run { source, .. } => source.termination(),
            Self::InvalidArgs(_)
            | Self::UnsupportedOutput { .. }
            | Self::PatternParse(_)
            | Self::CatalogCompile { .. }
            | Self::InlineCatalogShape(_) => Termination::Usage,
            Self::CommandFailure { code, .. } => termination_for_failure(*code),
            Self::FileNotFound(_)
            | Self::Dictionary(_)
            | Self::Corpus(_)
            | Self::TokenizedRecordTooLarge { .. }
            | Self::TokenizedRecordMalformed { .. }
            | Self::Parse(_)
            | Self::Io(_)
            | Self::Aggregation(_)
            | Self::Execution(_)
            | Self::OversizedSentencesSkipped { .. }
            | Self::Artifact(_) => Termination::Failure,
        }
    }

    pub fn with_run_id(self, run_id: String, observed_input_digest: Option<String>) -> Self {
        match self {
            Self::Run { .. } => self,
            source => Self::Run {
                run_id,
                observed_input_digest,
                source: Box::new(source),
            },
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        match self {
            Self::Run { run_id, .. } => Some(run_id),
            _ => None,
        }
    }

    pub fn observed_input_digest(&self) -> Option<&str> {
        match self {
            Self::Run {
                observed_input_digest,
                ..
            } => observed_input_digest.as_deref(),
            _ => None,
        }
    }

    pub fn root_error(&self) -> &Self {
        match self {
            Self::Run { source, .. } => source.root_error(),
            error => error,
        }
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliError::Corpus(format!("JSON parse error: {e}"))
    }
}

impl From<String> for CliError {
    fn from(s: String) -> Self {
        CliError::Parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_sentence_omission_is_a_stable_after_change_failure() {
        let error = CliError::OversizedSentencesSkipped {
            count: 2,
            first_source_name: "00027.md".into(),
            first_ordinal: 7,
            first_actual: 187_965,
            limit: 65_536,
        };

        assert_eq!(error.code(), "oversized_sentences_skipped");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        assert_eq!(error.termination(), Termination::Failure);
        assert!(error.to_string().contains("count=2"));
        assert!(error.to_string().contains("00027.md"));
    }

    #[test]
    fn retry_policy_distinguishes_changed_inputs_transient_io_and_unsafe_publication() {
        assert_eq!(
            CliError::UnsupportedOutput {
                command: "explore",
                output: "json",
            }
            .retry_policy(),
            RetryPolicy::AfterChange
        );
        assert_eq!(
            CliError::Io(std::io::Error::other("transient")).retry_policy(),
            RetryPolicy::Safe
        );
        assert_eq!(
            CliError::Artifact(
                lingrep_corpus::CorpusError::PublicationDurabilityUncertain {
                    path: "artifact".into(),
                }
            )
            .retry_policy(),
            RetryPolicy::Unsafe
        );
    }
}
