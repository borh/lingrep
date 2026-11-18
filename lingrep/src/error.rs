//! CLI-specific error handling

use thiserror::Error;

use crate::protocol::diagnostic::{
    DiagnosticData, DiagnosticLocation, DiagnosticSeverity, DiagnosticStage, RetryPolicy,
};

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

    #[cfg(feature = "web")]
    #[error("{0}")]
    Serve(#[from] lingrep_web::serve::ServeError),

    #[error("{command} does not support {output} output")]
    UnsupportedOutput {
        command: &'static str,
        output: &'static str,
    },

    #[error("{message}")]
    CommandFailure { code: FailureCode, message: String },

    #[error("File not found: '{0}'")]
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

    #[error("{0}")]
    Corpus(String),

    #[error("{0}")]
    CorpusSource(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("{0}")]
    Analysis(#[from] lingrep_core::analysis::AnalysisError),

    #[error("{0}")]
    ReadingSidecar(#[from] lingrep_corpus::ReadingSidecarError),

    #[error("{0}")]
    CountTable(#[from] lingrep_corpus::CountTableError),

    #[error("{0}")]
    Output(#[from] crate::protocol::OutputError),

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

    #[error("Cannot read '{path}': {source}")]
    InputIo {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Summary aggregation error: {0}")]
    Aggregation(#[source] std::io::Error),

    #[error("{0}")]
    Execution(#[from] lingrep_core::execution::ExecutionError),

    #[error(
        "Skipped {count} oversized {noun}; results are incomplete. \
         The first was sentence {first_ordinal} in '{first_source_name}': \
         {first_actual} bytes with a limit of {limit} bytes",
        noun = if *.count == 1 { "sentence" } else { "sentences" }
    )]
    OversizedSentencesSkipped {
        count: u64,
        first_source_name: String,
        first_ordinal: u64,
        first_actual: usize,
        limit: usize,
    },

    #[error("{0}")]
    Artifact(#[from] lingrep_corpus::CorpusError),

    #[error("{message}", message = path_message(.path, .source))]
    AtPath {
        path: std::path::PathBuf,
        #[source]
        source: Box<CliError>,
    },

    #[error("{source}")]
    Run {
        run_id: String,
        observed_input_digest: Option<String>,
        #[source]
        source: Box<CliError>,
    },
}

impl CliError {
    pub(crate) fn corpus(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::CorpusSource(Box::new(source))
    }

    pub(crate) fn stdout(source: std::io::Error) -> Self {
        Self::Output(crate::protocol::OutputError::Io(source))
    }

    pub(crate) fn at_path(self, path: impl Into<std::path::PathBuf>) -> Self {
        Self::AtPath {
            path: path.into(),
            source: Box::new(self),
        }
    }

    pub fn code(&self) -> &'static str {
        self.classification().code
    }

    pub fn diagnostic(&self) -> DiagnosticData {
        let class = self.classification();
        DiagnosticData {
            run_id: self.run_id().map(str::to_owned),
            observed_input_digest: self.observed_input_digest().map(str::to_owned),
            stage: class.stage,
            severity: DiagnosticSeverity::Error,
            code: class.code.into(),
            message: self.to_string(),
            suggestions: self.suggestions(),
            usage: None,
            retry: Some(class.retry),
            location: class.location,
            related_locations: Vec::new(),
        }
    }

    fn classification(&self) -> ErrorClassification {
        use DiagnosticStage::{Aggregation, Catalog, Input, Output};
        use RetryPolicy::{AfterChange, Safe, Unsafe};
        use lingrep_corpus::{CountTableError, ReadingSidecarError};
        match self {
            Self::InvalidArgs(_) => {
                ErrorClassification::usage("invalid_invocation", DiagnosticStage::Invocation)
            }
            #[cfg(feature = "web")]
            Self::Serve(lingrep_web::serve::ServeError::InvalidArguments(_)) => {
                ErrorClassification::usage("invalid_invocation", DiagnosticStage::Invocation)
            }
            #[cfg(feature = "web")]
            Self::Serve(lingrep_web::serve::ServeError::Runtime(_)) => {
                ErrorClassification::failure("serve", DiagnosticStage::Unknown, AfterChange)
            }
            Self::UnsupportedOutput { .. } => {
                ErrorClassification::usage("unsupported_output", DiagnosticStage::Invocation)
            }
            Self::CommandFailure { code, .. } => ErrorClassification::failure(
                code.as_str(),
                Output,
                match code {
                    FailureCode::PublicationDurabilityUncertain => Unsafe,
                    FailureCode::Io | FailureCode::Output | FailureCode::SignalHandler => Safe,
                    _ => AfterChange,
                },
            ),
            Self::FileNotFound(path) => ErrorClassification::failure(
                "file_not_found",
                Input,
                AfterChange,
            )
            .at(Some(path.display().to_string()), None, None),
            Self::PatternParse(_) | Self::CatalogCompile { .. } => {
                ErrorClassification::usage("catalog_validation", Catalog)
            }
            Self::InlineCatalogShape(_) => {
                ErrorClassification::usage("inline_catalog_shape", Catalog)
            }
            Self::Dictionary(_) => ErrorClassification::failure("dictionary", Input, AfterChange),
            Self::Corpus(_) | Self::CorpusSource(_) => {
                ErrorClassification::failure("corpus", Input, AfterChange)
            }
            Self::Analysis(_) => ErrorClassification::failure("analyzer", Input, AfterChange),
            Self::ReadingSidecar(ReadingSidecarError::DestinationExists(path))
            | Self::CountTable(CountTableError::DestinationExists(path)) => {
                ErrorClassification::failure("destination_exists", Output, AfterChange).at(
                    Some(path.display().to_string()),
                    None,
                    None,
                )
            }
            Self::ReadingSidecar(ReadingSidecarError::Publication(error))
            | Self::CountTable(CountTableError::Publication(error)) => {
                publication_classification(error)
            }
            Self::ReadingSidecar(ReadingSidecarError::Corpus(error)) | Self::Artifact(error) => {
                artifact_classification(error)
            }
            Self::ReadingSidecar(ReadingSidecarError::ReaderMismatch { .. }) => {
                ErrorClassification::usage("invalid_invocation", DiagnosticStage::Invocation)
            }
            Self::ReadingSidecar(ReadingSidecarError::Io(error)) => {
                io_classification(error, Output)
            }
            Self::CountTable(CountTableError::Io(error)) => io_classification(error, Aggregation),
            Self::ReadingSidecar(_) | Self::CountTable(_) => {
                ErrorClassification::failure("corpus", Input, AfterChange)
            }
            Self::Output(error) => match error {
                crate::protocol::OutputError::Io(error) => io_classification(error, Output),
                _ => ErrorClassification::failure("output", Output, AfterChange),
            },
            Self::TokenizedRecordTooLarge { ordinal, .. } => ErrorClassification::failure(
                "tokenized_record_too_large",
                Input,
                AfterChange,
            )
            .at(None, Some(*ordinal as u64), None),
            Self::TokenizedRecordMalformed { ordinal, .. } => ErrorClassification::failure(
                "tokenized_record_malformed",
                Input,
                AfterChange,
            )
            .at(None, Some(*ordinal as u64), None),
            Self::Parse(_) => ErrorClassification::failure("parse", Input, AfterChange),
            Self::Io(error) => io_classification(error, Output),
            Self::InputIo { path, source } => {
                io_classification(source, Input).at(Some(path.display().to_string()), None, None)
            }
            Self::Aggregation(error) => {
                let mut class = io_classification(error, Aggregation);
                if error.kind() == std::io::ErrorKind::StorageFull {
                    class.code = "temp_budget_exceeded";
                } else if class.code != "broken_pipe" {
                    class.code = "aggregation";
                }
                class
            }
            Self::Execution(error) => execution_classification(error),
            Self::OversizedSentencesSkipped {
                first_source_name,
                first_ordinal,
                ..
            } => ErrorClassification::failure("oversized_sentences_skipped", Input, AfterChange)
                .at(Some(first_source_name.clone()), Some(*first_ordinal), None),
            Self::AtPath { path, source } => {
                let mut class = source.classification();
                let location = class.location.get_or_insert(DiagnosticLocation {
                    source_name: None,
                    source_span: None,
                    line: None,
                    column: None,
                    corpus_sentence_ordinal: None,
                    token_index: None,
                });
                location
                    .source_name
                    .get_or_insert_with(|| path.display().to_string());
                class
            }
            Self::Run { source, .. } => source.classification(),
        }
    }

    pub fn suggestions(&self) -> Vec<String> {
        use lingrep_core::execution::ExecutionError;
        use lingrep_corpus::CorpusError;
        let raise = |flag: &str, minimum: usize| {
            vec![format!(
                "If this input size is expected, rerun the same command with {flag} {minimum} or higher."
            )]
        };
        match self.root_error() {
            Self::ReadingSidecar(lingrep_corpus::ReadingSidecarError::Publication(error))
            | Self::CountTable(lingrep_corpus::CountTableError::Publication(error)) => match error {
                lingrep_corpus::PublicationError::DurabilityUncertain { .. } => vec![
                    "Keep the created destination and inspect it before retrying. Use a new output path if you need to rebuild.".into()
                ],
                lingrep_corpus::PublicationError::DestinationExists { .. } => vec![
                    "The existing destination was preserved. Choose a new output path.".into()
                ],
                lingrep_corpus::PublicationError::Unsupported { .. } => vec![
                    "Choose an output directory on a filesystem that supports atomic rename without replacing existing files.".into()
                ],
                _ => Vec::new(),
            },
            Self::Execution(ExecutionError::Matching(lingrep_core::fsm::BoundExecutionError::Resource(
                lingrep_core::fsm::MatchResourceError::BatchOutput { .. },
            ))) => vec![
                "Narrow the pattern to produce fewer matches in one sentence.".into()
            ],
            Self::Execution(ExecutionError::SentenceTooLarge { actual, .. }) => raise("--max-sentence-bytes", *actual),
            Self::Execution(ExecutionError::SentenceTooLargeAtLeast { minimum, .. }) => raise("--max-sentence-bytes", *minimum),
            Self::Execution(ExecutionError::InputRecordTooLarge { actual_at_least, .. }) => raise("--max-record-bytes", *actual_at_least),
            Self::Execution(ExecutionError::TokenizedSentenceTooLarge { actual, .. }
                | ExecutionError::LookaroundWindowTooLarge { actual, .. }) => raise("--max-tokenized-bytes", *actual),
            Self::TokenizedRecordTooLarge { limit, .. } => raise("--max-record-bytes", limit.saturating_add(1)),
            Self::OversizedSentencesSkipped { first_actual, .. } => raise("--max-sentence-bytes", *first_actual),
            Self::CatalogCompile { error, .. } if error.diagnostics.iter().any(|diagnostic| diagnostic.code() == lingrep_core::fsm::DiagnosticCode::LimitExceeded) => vec![
                "Reduce the catalog size or expression and reference expansion. Compiler safety limits cannot be raised with CLI options.".into()
            ],
            Self::Artifact(CorpusError::TokenBlockCapacity { .. }) => vec![
                "This is a fixed artifact block limit. Check the source for missing sentence boundaries or oversized annotations, then rebuild from corrected source into a new destination.".into()
            ],
            Self::Artifact(CorpusError::ExpectedArtifactDirectory { path }) => vec![
                "--corpus accepts a prepared artifact directory. For raw input, pass ROOT directly instead of --corpus ROOT.".into(),
                format!("To prepare this input, run: ling corpus build {}", crate::examples::quote_posix(&path.to_string_lossy())),
            ],
            Self::Artifact(CorpusError::DestinationExists { .. }) => vec![
                "The existing artifact was preserved. Choose a new --output directory, or inspect the existing artifact with ling corpus inspect ARTIFACT.".into()
            ],
            Self::Artifact(CorpusError::PublicationDurabilityUncertain { .. })
            | Self::CommandFailure { code: FailureCode::PublicationDurabilityUncertain, .. } => vec![
                "Keep the created artifact. Run ling corpus inspect --verify ARTIFACT before retrying; replace ARTIFACT with its path. Use a new --output directory if you need to rebuild.".into()
            ],
            Self::Artifact(CorpusError::UnsupportedSchema { .. }
                | CorpusError::UnsupportedArtifactGeneration { .. }
                | CorpusError::PreparedArtifactRebuildRequired { .. }
                | CorpusError::UnknownManifestField { .. }) => vec![
                "Open the artifact with the ling version that wrote it or a compatible newer version. To rebuild from source, use ling corpus build ROOT --output NEW_ARTIFACT.".into()
            ],
            Self::Artifact(CorpusError::SourceInventoryMismatch | CorpusError::DiscoveryReportMismatch)
            | Self::Execution(ExecutionError::InventoryMismatch) => vec![
                "Keep the source tree unchanged while the command runs, then retry. If the mismatch persists, report it with this diagnostic.".into()
            ],
            Self::Artifact(error) if error.is_artifact_corruption() => vec![
                "Verify the artifact with ling corpus inspect --verify ARTIFACT. Restore a known-good copy, or rebuild from source with ling corpus build ROOT --output NEW_ARTIFACT.".into()
            ],
            Self::Artifact(CorpusError::TokenSchemaIdentityStale { .. } | CorpusError::NormalizationTablesStale { .. }) => vec![
                "Rebuild from source with ling corpus build ROOT --output NEW_ARTIFACT, then use the new artifact.".into()
            ],
            Self::Aggregation(error) | Self::Io(error) | Self::Artifact(CorpusError::Io(error)) if error.kind() == std::io::ErrorKind::StorageFull => vec![
                "Check free space on the filesystem used by the failing operation. If the configured temporary-storage budget was exceeded, raise --max-temp-bytes and rerun the command.".into()
            ],
            Self::Artifact(CorpusError::TempCapacity(lingrep_core::spill::TempCapacityError::LimitExceeded(_))) => vec![
                "Raise --max-temp-bytes to allow more temporary storage, then rerun the command.".into()
            ],
            Self::InputIo { source, .. } if source.kind() == std::io::ErrorKind::NotFound => vec![
                "Check the configured input path and that the file is still present.".into()
            ],
            Self::InputIo { source, .. } if source.kind() == std::io::ErrorKind::PermissionDenied => vec![
                "Check read permissions on this path and its parent directories.".into()
            ],
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn is_broken_pipe(&self) -> bool {
        self.code() == "broken_pipe"
    }

    pub fn is_stdout_broken_pipe(&self) -> bool {
        match self.root_error() {
            Self::Output(error) => error.is_broken_pipe(),
            Self::Execution(lingrep_core::execution::ExecutionError::Output { kind, .. }) => {
                *kind == std::io::ErrorKind::BrokenPipe
            }
            _ => false,
        }
    }

    pub fn retry_policy(&self) -> RetryPolicy {
        self.classification().retry
    }

    pub fn termination(&self) -> Termination {
        self.classification().termination
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
            Self::Run { source, .. } | Self::AtPath { source, .. } => source.root_error(),
            error => error,
        }
    }
}

fn path_message(path: &std::path::Path, source: &CliError) -> String {
    use lingrep_corpus::{CountTableError, PublicationError, ReadingSidecarError};
    let embedded_path = match source.root_error() {
        CliError::CountTable(
            CountTableError::KeyMismatch { path, .. }
            | CountTableError::UnsupportedFormat { path, .. }
            | CountTableError::DestinationExists(path)
            | CountTableError::PruneLeftover(path),
        ) => Some(path.as_path()),
        CliError::CountTable(CountTableError::Publication(error))
        | CliError::ReadingSidecar(ReadingSidecarError::Publication(error)) => match error {
            PublicationError::DestinationExists { path }
            | PublicationError::Unsupported { path, .. }
            | PublicationError::DurabilityUncertain { path, .. } => Some(path.as_path()),
            PublicationError::Io(_) => None,
        },
        _ => None,
    };
    if embedded_path == Some(path) {
        source.to_string()
    } else {
        format!("'{}': {source}", path.display())
    }
}

struct ErrorClassification {
    code: &'static str,
    stage: DiagnosticStage,
    retry: RetryPolicy,
    termination: Termination,
    location: Option<DiagnosticLocation>,
}

impl ErrorClassification {
    fn failure(code: &'static str, stage: DiagnosticStage, retry: RetryPolicy) -> Self {
        Self {
            code,
            stage,
            retry,
            termination: Termination::Failure,
            location: None,
        }
    }
    fn usage(code: &'static str, stage: DiagnosticStage) -> Self {
        Self {
            termination: Termination::Usage,
            ..Self::failure(code, stage, RetryPolicy::AfterChange)
        }
    }
    fn at(mut self, source_name: Option<String>, ordinal: Option<u64>, token: Option<u64>) -> Self {
        if source_name.is_some() || ordinal.is_some() || token.is_some() {
            self.location = Some(DiagnosticLocation {
                source_name,
                source_span: None,
                line: None,
                column: None,
                corpus_sentence_ordinal: ordinal,
                token_index: token,
            });
        }
        self
    }
}

fn io_classification(error: &std::io::Error, stage: DiagnosticStage) -> ErrorClassification {
    use std::io::ErrorKind;
    let retry = match error.kind() {
        ErrorKind::NotFound
        | ErrorKind::PermissionDenied
        | ErrorKind::InvalidData
        | ErrorKind::InvalidInput
        | ErrorKind::StorageFull
        | ErrorKind::Unsupported => RetryPolicy::AfterChange,
        _ => RetryPolicy::Safe,
    };
    ErrorClassification::failure(
        if error.kind() == ErrorKind::BrokenPipe {
            "broken_pipe"
        } else {
            "io"
        },
        stage,
        retry,
    )
}

fn publication_classification(error: &lingrep_corpus::PublicationError) -> ErrorClassification {
    use lingrep_corpus::PublicationError;
    let (code, retry, path) = match error {
        PublicationError::Io(error) => return io_classification(error, DiagnosticStage::Output),
        PublicationError::DestinationExists { path } => {
            ("destination_exists", RetryPolicy::AfterChange, path)
        }
        PublicationError::Unsupported { path, .. } => {
            ("unsupported_publication", RetryPolicy::AfterChange, path)
        }
        PublicationError::DurabilityUncertain { path, .. } => (
            "publication_durability_uncertain",
            RetryPolicy::Unsafe,
            path,
        ),
    };
    ErrorClassification::failure(code, DiagnosticStage::Output, retry).at(
        Some(path.display().to_string()),
        None,
        None,
    )
}

fn artifact_classification(error: &lingrep_corpus::CorpusError) -> ErrorClassification {
    use lingrep_corpus::CorpusError;
    let retry = match error {
        CorpusError::PublicationDurabilityUncertain { .. } => RetryPolicy::Unsafe,
        CorpusError::Io(error) => return io_classification(error, DiagnosticStage::Input),
        _ => RetryPolicy::AfterChange,
    };
    ErrorClassification::failure(error.code(), DiagnosticStage::Input, retry)
}

fn execution_classification(
    error: &lingrep_core::execution::ExecutionError,
) -> ErrorClassification {
    use DiagnosticStage::{Input, Matching, Output};
    use RetryPolicy::{AfterChange, Safe};
    use lingrep_core::execution::ExecutionError as E;
    let failure = ErrorClassification::failure;
    match error {
        E::Matching(lingrep_core::fsm::BoundExecutionError::Resource(
            lingrep_core::fsm::MatchResourceError::BatchOutput { .. },
        )) => failure("match_batch_too_large", Matching, AfterChange),
        E::Matching(lingrep_core::fsm::BoundExecutionError::Resource(
            lingrep_core::fsm::MatchResourceError::Sentence { .. },
        )) => failure("graph_assignment_limit", Matching, AfterChange),
        E::Matching(_) => failure("matching", Matching, AfterChange),
        E::Analysis {
            ordinal,
            source_name,
            ..
        } => failure("analyzer", Input, AfterChange).at(source_name.clone(), *ordinal, None),
        E::InputRecordTooLarge {
            source_name,
            corpus_sentence_ordinal,
            ..
        } => failure("input_record_too_large", Input, AfterChange).at(
            Some(source_name.clone()),
            Some(*corpus_sentence_ordinal),
            None,
        ),
        E::Discovery {
            code, logical_path, ..
        } => failure(code, Input, AfterChange).at(Some(logical_path.clone()), None, None),
        E::Input {
            source_name,
            ordinal,
            kind,
            ..
        } => failure(
            "input",
            Input,
            if matches!(
                kind,
                std::io::ErrorKind::InvalidData
                    | std::io::ErrorKind::InvalidInput
                    | std::io::ErrorKind::PermissionDenied
                    | std::io::ErrorKind::NotFound
            ) {
                AfterChange
            } else {
                Safe
            },
        )
        .at(Some(source_name.clone()), Some(*ordinal), None),
        E::SentenceTooLarge {
            ordinal,
            source_name,
            ..
        }
        | E::SentenceTooLargeAtLeast {
            ordinal,
            source_name,
            ..
        } => failure("sentence_too_large", Input, AfterChange).at(
            source_name.clone(),
            Some(*ordinal),
            None,
        ),
        E::TokenizedSentenceTooLarge {
            ordinal,
            source_name,
            ..
        } => failure("tokenized_sentence_too_large", Input, AfterChange).at(
            source_name.clone(),
            Some(*ordinal),
            None,
        ),
        E::MissingRequiredAttribute {
            attribute: _,
            source_name,
            corpus_sentence_ordinal,
            token_index,
        } => failure("missing_required_attribute", Matching, AfterChange).at(
            Some(source_name.clone()),
            Some(*corpus_sentence_ordinal),
            Some(*token_index as u64),
        ),
        E::Output { kind, .. } => failure(
            if *kind == std::io::ErrorKind::BrokenPipe {
                "broken_pipe"
            } else {
                "output"
            },
            Output,
            Safe,
        ),
        E::InventoryMismatch => failure("source_inventory_mismatch", Matching, AfterChange),
        E::Source { ordinal, .. } => {
            failure("execution", Matching, Safe).at(None, Some(*ordinal), None)
        }
        E::Cache(_) | E::Sink(_) | E::Observer(_) => failure("execution", Matching, Safe),
        E::InvalidLimit { .. }
        | E::ResourceAccounting
        | E::LookaroundWindowTooLarge { .. }
        | E::PolicyAnchor(_) => failure("execution", Matching, AfterChange),
    }
}

impl From<lingrep_corpus::word_vectors::VectorTableError> for CliError {
    fn from(error: lingrep_corpus::word_vectors::VectorTableError) -> Self {
        use lingrep_corpus::word_vectors::VectorTableError;
        match error {
            VectorTableError::InvalidQuery(message) => Self::InvalidArgs(message),
            VectorTableError::NotFound(path) => Self::FileNotFound(path),
            VectorTableError::Io { path, source } => Self::InputIo { path, source },
            malformed @ VectorTableError::Malformed { .. } => Self::Parse(malformed.to_string()),
        }
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliError::corpus(e)
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
    fn batch_output_error_has_a_distinct_code_and_pattern_remedy() {
        let error = CliError::Execution(lingrep_core::execution::ExecutionError::Matching(
            lingrep_core::fsm::BoundExecutionError::Resource(
                lingrep_core::fsm::MatchResourceError::BatchOutput { limit: 1024 },
            ),
        ));
        assert_eq!(error.code(), "match_batch_too_large");
        assert_eq!(error.classification().stage, DiagnosticStage::Matching);
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        assert!(error.to_string().contains("1024"));
        assert!(!error.to_string().contains("ordering constraint"));
        assert!(
            error
                .suggestions()
                .iter()
                .any(|suggestion| suggestion.contains("Narrow the pattern"))
        );
    }

    #[test]
    fn graph_resource_error_has_a_matching_code_and_pattern_remedy() {
        let error = CliError::Execution(lingrep_core::execution::ExecutionError::Matching(
            lingrep_core::fsm::BoundExecutionError::Resource(
                lingrep_core::fsm::MatchResourceError::Sentence {
                    pattern: "connected".into(),
                    resource: "graph assignments",
                    limit: 65_536,
                },
            ),
        ));
        assert_eq!(error.code(), "graph_assignment_limit");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        let description = error.to_string();
        assert!(description.contains("connected"));
        assert!(description.contains("one sentence"));
        assert!(description.contains("ordering constraint"));
    }

    #[test]
    fn descriptions_explain_resource_limits_and_saved_output_without_wrapper_labels() {
        let cases = [
            (
                CliError::from(
                    lingrep_core::execution::ExecutionError::TokenizedSentenceTooLarge {
                        ordinal: 336449,
                        actual: 36_804_067,
                        limit: 33_554_432,
                        source_name: Some("00393.md".into()),
                    },
                ),
                "Sentence 336449 in '00393.md' exceeds the memory limit for analyzed tokens: estimated 35.1 MiB (36804067 bytes); limit 32.0 MiB (33554432 bytes).",
            ),
            (
                CliError::from(lingrep_corpus::CountTableError::KeyMismatch {
                    path: "counts".into(),
                    field: "catalog fingerprint",
                    found: "old".into(),
                    expected: "new".into(),
                })
                .at_path("counts"),
                "Count table 'counts' was built with a different catalog fingerprint. Rebuild it with 'ling summarize --write-table' (saved: old; current: new)",
            ),
            (
                CliError::from(lingrep_corpus::CorpusError::DestinationExists {
                    path: "saved corpus".into(),
                }),
                "Cannot create 'saved corpus': the destination already exists and was left unchanged",
            ),
            (
                CliError::from(lingrep_corpus::CountTableError::from(
                    lingrep_corpus::PublicationError::DurabilityUncertain {
                        path: "counts".into(),
                        source: std::io::Error::other("sync failed"),
                    },
                )),
                "Created 'counts', but could not confirm it will survive a system crash: sync failed",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert_eq!(error.diagnostic().message, expected);
            assert_eq!(error.termination(), Termination::Failure);
        }
    }

    #[test]
    fn output_path_context_preserves_error_policy_and_source_chain() {
        let error = CliError::from(lingrep_corpus::CountTableError::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "disk full",
        )))
        .at_path("tables/counts");
        assert_eq!(error.code(), "io");
        assert_eq!(error.retry_policy(), RetryPolicy::AfterChange);
        assert_eq!(
            error.diagnostic().location.unwrap().source_name.as_deref(),
            Some("tables/counts")
        );
        let mut source = std::error::Error::source(&error);
        let mut found = false;
        while let Some(error) = source {
            if let Some(io) = error.downcast_ref::<std::io::Error>() {
                assert_eq!(io.kind(), std::io::ErrorKind::StorageFull);
                found = true;
            }
            source = error.source();
        }
        assert!(found);
        assert!(error.to_string().contains("tables/counts"));
    }

    #[test]
    fn display_closure_applies_only_to_stdout_failures() {
        let closed = || std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let stdout = CliError::stdout(closed());
        let other = CliError::Io(closed());
        assert!(stdout.is_stdout_broken_pipe());
        assert!(!other.is_stdout_broken_pipe());
        assert_eq!(stdout.code(), "broken_pipe");
        assert_eq!(other.termination(), Termination::Failure);
    }

    #[test]
    fn publication_diagnostics_preserve_the_commit_state_and_cause() {
        for sidecar in [false, true] {
            let publication = lingrep_corpus::PublicationError::DurabilityUncertain {
                path: "output".into(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "sync failed"),
            };
            let error = if sidecar {
                CliError::from(lingrep_corpus::ReadingSidecarError::from(publication))
            } else {
                CliError::from(lingrep_corpus::CountTableError::from(publication))
            };
            let diagnostic = error.diagnostic();
            assert_eq!(diagnostic.code, "publication_durability_uncertain");
            assert_eq!(diagnostic.stage, DiagnosticStage::Output);
            assert_eq!(diagnostic.retry, Some(RetryPolicy::Unsafe));
            assert_eq!(
                diagnostic.location.unwrap().source_name.as_deref(),
                Some("output")
            );
            assert_eq!(error.termination(), Termination::Failure);
            assert!(diagnostic.suggestions[0].contains("before retrying"));
            assert!(!diagnostic.suggestions[0].contains("corpus inspect"));
            let mut source = std::error::Error::source(&error);
            let mut found = false;
            while let Some(error) = source {
                if let Some(io) = error.downcast_ref::<std::io::Error>() {
                    assert_eq!(io.kind(), std::io::ErrorKind::PermissionDenied);
                    found = true;
                }
                source = error.source();
            }
            assert!(found);
        }
    }

    #[test]
    fn sentence_location_preserves_source_names_without_parsing_display_text() {
        let error = CliError::from(lingrep_core::execution::ExecutionError::SentenceTooLarge {
            ordinal: 7,
            actual: 100,
            limit: 50,
            source_name: Some(" in folder/in source.md".into()),
        });
        let location = error.diagnostic().location.unwrap();
        assert_eq!(
            location.source_name.as_deref(),
            Some(" in folder/in source.md")
        );
        assert_eq!(location.corpus_sentence_ordinal, Some(7));
    }

    #[test]
    fn resource_errors_suggest_the_option_for_the_exceeded_limit() {
        use lingrep_core::execution::ExecutionError;
        for (error, flag) in [
            (
                ExecutionError::SentenceTooLarge {
                    ordinal: 3,
                    actual: 123,
                    limit: 100,
                    source_name: None,
                },
                "--max-sentence-bytes",
            ),
            (
                ExecutionError::SentenceTooLargeAtLeast {
                    ordinal: 3,
                    minimum: 123,
                    limit: 100,
                    source_name: None,
                },
                "--max-sentence-bytes",
            ),
            (
                ExecutionError::InputRecordTooLarge {
                    source_name: "a.md".into(),
                    record_ordinal: 2,
                    corpus_sentence_ordinal: 3,
                    actual_at_least: 123,
                    limit: 100,
                },
                "--max-record-bytes",
            ),
            (
                ExecutionError::TokenizedSentenceTooLarge {
                    ordinal: 3,
                    actual: 123,
                    limit: 100,
                    source_name: Some("a.md".into()),
                },
                "--max-tokenized-bytes",
            ),
            (
                ExecutionError::LookaroundWindowTooLarge {
                    actual: 123,
                    limit: 100,
                },
                "--max-tokenized-bytes",
            ),
        ] {
            let error = CliError::Execution(error).with_run_id("run".into(), None);
            assert!(error.suggestions()[0].contains(&format!("{flag} 123")));
            assert_eq!(error.termination(), Termination::Failure);
        }
    }

    #[test]
    fn artifact_errors_include_safe_recovery_guidance() {
        use lingrep_corpus::CorpusError;
        let uncertain = CliError::Artifact(CorpusError::PublicationDurabilityUncertain {
            path: "artifact".into(),
        });
        assert_eq!(uncertain.retry_policy(), RetryPolicy::Unsafe);
        assert!(uncertain.suggestions()[0].contains("before retrying"));
        assert!(uncertain.suggestions()[0].contains("--verify"));
        let exists = CliError::Artifact(CorpusError::DestinationExists {
            path: "artifact".into(),
        });
        assert!(exists.suggestions()[0].contains("preserved"));
        assert!(exists.suggestions()[0].contains("new --output"));
        let corrupt = CliError::Artifact(CorpusError::TruncatedComponent {
            component: "tokens",
        });
        assert!(corrupt.suggestions()[0].contains("NEW_ARTIFACT"));
        assert!(!corrupt.suggestions()[0].contains("--prune"));
        let fixed = CliError::Artifact(CorpusError::TokenBlockCapacity {
            minimum: 123,
            limit: 100,
        });
        assert!(fixed.suggestions()[0].contains("fixed"));
        assert!(!fixed.suggestions()[0].contains("--max-tokenized-bytes"));
    }

    #[test]
    fn vector_io_errors_preserve_the_path_and_os_error() {
        let error = CliError::from(lingrep_corpus::word_vectors::VectorTableError::Io {
            path: "vectors/words.bin".into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
        });
        assert!(error.to_string().contains("vectors/words.bin"));
        assert!(error.to_string().contains("permission denied"));
        assert_eq!(error.code(), "io");
        assert!(error.suggestions()[0].contains("permissions"));
    }

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
        assert!(error.to_string().contains("Skipped 2 oversized sentences"));
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
