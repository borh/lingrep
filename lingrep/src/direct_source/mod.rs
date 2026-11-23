//! Bounded direct-input adapters for the shared execution protocol.

mod batching;
mod discovery;
mod display;
mod hashing;

/// The bounded record reader lives in `lingrep-core` so `ling` and `lingrep-web`
/// enforce one record bound and report one diagnostic. It is re-exported here
/// because every direct-input caller reaches it through this module.
pub use lingrep_core::execution::{BoundedRecordError, BoundedRecordReader, BoundedRecordStats};

pub(crate) use batching::{
    BoxSentenceSource, OversizedSentenceOmissionSummary, source_from_events_skipping_oversized,
};
pub use batching::{DirectSentenceSource, DirectSourceStats, source_from_events};

pub use discovery::{
    AdmittedFilesystemEvents, FilesystemCorpusEvents, FilesystemObservation,
    admitted_filesystem_events, scoped,
};
pub(crate) use discovery::{
    directory_file_kind, execution_discovery_error, input_format, open_bccwj_discovery,
    open_corpus_document, stdin_events, validate_root_reader_compatibility,
};

pub use display::{
    MissingRequiredAttribute, displayed_text, matched_orth_tokens, matched_surface,
    matched_surface_into, project_context,
};

pub(crate) use hashing::identity_io_error;
pub use hashing::{
    HashingReader, OrderedInventoryObservation, StdinObservation, StdinReader, encode_digest,
    validate_ordered_inventory,
};
