//! Immutable prepared-corpus storage primitives.
//!
//! A published prepared-corpus component must remain byte-for-byte immutable for the
//! duration of every open reader and execution run. V1 projected decode checks a
//! stored block's checksum whenever it loads that block; complete verification remains
//! the explicit whole-artifact operation.
//!
//! ```compile_fail
//! use lingrep_corpus::TokenEncoding;
//! ```
//!
//! The physical prepared-token generation is private. Execution consumers use
//! [`PreparedSentenceSource`] values obtained from [`CorpusArtifact`].
//!
//! ```
//! fn accept_prepared_source(_: lingrep_corpus::PreparedSentenceSource) {}
//! ```
//!
//! ```compile_fail
//! use lingrep_corpus::TokenV6Reader;
//! ```
//!
//! ```compile_fail
//! use lingrep_corpus::publish_file_noreplace;
//! ```
//!
//! ```compile_fail
//! use lingrep_corpus::{LocationDocument, LocationWriter, MappedLocations};
//! ```
//!
//! ```compile_fail
//! use lingrep_corpus::ProvenanceWriter;
//! ```

mod artifact;
mod enhanced;
mod error;
mod identity;
mod locations;
mod manifest;
pub use manifest::{SentenceTruncation, is_prepared_artifact};
mod postings;
pub use postings::PreparedFilterStats;
mod provenance;
mod publication;
mod scratch;
mod siblings;
mod tokens;
mod verify;

pub use artifact::{
    AnalyzerRecord, BuildConfig, CORPUS_ARTIFACT_FORMAT_GENERATION, CorpusArtifact, CorpusBuilder,
    IdentityScheme, MAX_PROVENANCE_PAGE_SIZE, ParsedArtifactName, PreparedIdentity,
    SentenceProvenance, StoredDocument, TrustClass, canonical_artifact_path, format_identity,
    parse_artifact_name, vibrato_fixture_descriptor,
};
pub use error::CorpusError;
pub use identity::{
    AdmissionIdentity, AnalysisIdentity, AnalysisInputs, DictionaryFingerprint, IdentityError,
    ObservedSources, ReaderIdentity, SourceIdentity, SourceObservation, TokenSchemaIdentity,
    derive_analysis_identity,
};
pub use lingrep_core::corpus::{DocumentId, SentenceId};
pub use lingrep_core::execution::SentenceLocation;
pub use provenance::{
    ActiveRegion, BlockingProvenance, DivisionMetadataRow, DocumentMetadataRow, MAX_SECTION_LABELS,
    ParagraphRange, ProvenanceView, RegionSpanRow, RichSentenceMetadata, RichSentenceProvenance,
    SectionCoverage, SentenceReadingRanges, StoredSentenceAnalysis, StoredSurfaceAlignment,
};
pub use siblings::{
    BuildOutcome, Realization, RealizationRequest, SiblingRealization, SiblingReport, SiblingScan,
    artifact_bytes, containing_directory, is_superseded, remove_superseded,
};
pub use tokens::{PreparedScopeStats, PreparedSentenceSource};
pub use verify::{ComponentVerification, VerificationReport};
