//! Streaming corpus readers shared by command-line and web front ends.

pub mod divisions;
pub use divisions::{Division, DivisionId, DivisionObservation, StructuralContext};

mod analysis_identity;
pub mod bccwj;
pub mod conllu;
pub mod discovery;
pub mod markdown_dialect;
pub mod scope_observation;
pub mod section_policy;
pub mod soranoha;
pub mod source;
pub mod structured;
pub mod text_source;
pub mod zip;

pub use analysis_identity::{
    admission_identity_digest, reader_identity_digest, token_schema_identity_digest,
};
pub use discovery::{
    CorpusDiscovery, CorpusFileKind, DiscoveryError, DiscoveryReport, InventoryStats,
    SourceDocument, TraversalHighWater, TraversalLimits, ValidatedCorpusRoot,
};
pub use markdown_dialect::{
    DEFAULT_SELF_LABELS, MARKDOWN_READER_SCHEMA_REVISION, MarkdownDiagnostics,
    MarkdownReaderConfig, read_markdown_events,
};
pub use scope_observation::{ScopeCounts, ScopeObservation};
pub use section_policy::{DEFAULT_EXCLUDED_SECTIONS, SectionPolicy};
pub use source::{
    CorpusFormat, CorpusReadConfig, CorpusSource, CorpusStream, CorpusStructuredStream,
    resolve_input_format,
};
pub use structured::{
    AdmittedCorpus, AdmittedCorpusEvent, AdmittedSentence, CorpusEvent, DocumentId, Metadata,
    MetadataScalar, MetadataValue, SentenceId, SourceMetadataProjection, metadata_from_json,
    sentence_texts,
};
pub use text_source::{InputFormat, stream_input_events, stream_input_reader};
