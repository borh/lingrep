//! Bounded execution primitives shared by streaming runners.

mod admission;
mod sampling;
// Unix, not Linux. Every platform-specific call here is POSIX — `setpgid` via
// `Command::process_group`, `kill(-pgid, …)`, and `O_NOFOLLOW` — with no
// `prctl`, `pidfd`, `/proc` or `clone` anywhere, and symlink rejection goes
// through `symlink_metadata` rather than trusting `ELOOP`. Gating on Linux cost
// darwin the live adapter path and, because this module is the only consumer of
// several `analysis::external` items, left nine dead-code findings that failed
// `clippy -D warnings` there.
#[cfg(unix)]
mod external_process;
mod external_source;
mod limits;
mod native_source;
mod policy_sink;
mod record_reader;
mod runner;
mod source;

pub use admission::SentenceAdmission;
#[cfg(unix)]
pub use external_process::{
    DiagnosticCallback, ExternalAdapterCancellation, ExternalAdapterCommand,
    ExternalAdapterProcess, ExternalProcessError, VerifiedAdapterManifest,
};
#[cfg(unix)]
pub use external_source::LiveExternalSource;
pub use external_source::{ExternalAttestation, OfflineExternalSource};
pub use limits::{
    DEFAULT_MAX_INPUT_RECORD_BYTES, DEFAULT_MAX_SENTENCE_BYTES, DEFAULT_MAX_STAGE_INPUT_BYTES,
    DEFAULT_MAX_STAGE_SENTENCES, DEFAULT_MAX_TOKENIZED_BYTES, ExecutionError, ExecutionLimits,
    estimate_token_info_bytes,
};
pub use native_source::NativeAnalysisSource;
pub use policy_sink::{PolicySelection, PolicySink, Publication};
pub use record_reader::{BoundedRecordError, BoundedRecordReader, BoundedRecordStats};
pub use runner::{
    AnalysisBinding, CancellationFlag, ExecutionProgress, ExecutionReport, MatchSink,
    PARTITION_PUBLICATION_WIDTH, ProgressSink, RunOutcome, SinkControl, TokenizedBatchObservation,
    TokenizedBatchObserver, execute, execute_analyzed,
};
pub use sampling::{CorpusSample, RATE_SCALE, SampledSentences, reservoir_draw};
pub use source::{
    BoundSentenceInput, EnhancedGraphInput, ExecutionBatch, ExecutionSentence,
    FallibleTextIteratorSource, FallibleTokenizedIteratorSource, OversizedSentenceOmissions,
    SentenceBatchSource, SentenceLocation, TextIteratorSource, TokenizedAdmission,
    cached_run_attributes,
};

#[cfg(unix)]
mod external_session;
#[cfg(unix)]
pub use external_session::ExternalAnalyzerSession;
