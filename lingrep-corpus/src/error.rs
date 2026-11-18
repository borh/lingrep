use std::path::{Path, PathBuf};

use crate::IdentityError;

/// Failures while reading or writing a prepared corpus component.
#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("identity error: {0}")]
    Identity(#[from] IdentityError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("temporary storage capacity exhausted: {0}")]
    TempCapacity(#[from] lingrep_core::spill::TempCapacityError),
    #[error("provenance page exceeds the shared limit of {limit} sentences")]
    ProvenancePageCapacity { limit: usize },
    #[error("section label result exceeds the shared limit of {limit} labels")]
    SectionLabelCapacity { limit: usize },
    #[error(
        "prepared sentence requires at least {minimum} raw block bytes, exceeding the {limit}-byte limit"
    )]
    TokenBlockCapacity { minimum: u64, limit: u64 },
    #[error("truncated {component} at byte offset {offset}")]
    Truncated {
        component: &'static str,
        offset: u64,
    },
    #[error("listed sentence ordinals do not ascend")]
    UnsortedOrdinals,
    #[error("invalid {component} format: {message}")]
    InvalidFormat {
        component: &'static str,
        message: String,
    },
    #[error("unsupported {component} schema version {version}")]
    UnsupportedSchema {
        component: &'static str,
        version: u32,
    },
    #[error(
        "prepared artifact manifest schema {version} is unsupported; rebuild with the current `ling corpus build`"
    )]
    PreparedArtifactRebuildRequired { version: u32 },
    #[error(
        "lingrep artifact generation {generation} is unsupported; rebuild from source with `ling corpus build`"
    )]
    UnsupportedArtifactGeneration { generation: u32 },
    #[error(
        "prepared artifact manifest has the field `{field}`, which ling {reader_version} does not read; the artifact was written by ling {producer_version}. Open it with that version or later, or rebuild it with `ling corpus build`"
    )]
    UnknownManifestField {
        field: String,
        producer_version: String,
        reader_version: &'static str,
    },
    #[error(
        "'{path}' is not a prepared corpus: expected a directory containing manifest.json, created by 'ling corpus build'"
    )]
    ExpectedArtifactDirectory { path: PathBuf },
    #[error("artifact component {component} is a symbolic link")]
    SymlinkComponent { component: &'static str },
    #[error("artifact component {component} has the wrong recorded length")]
    ComponentLengthMismatch { component: &'static str },
    #[error("artifact component {component} has a different analysis identity")]
    AnalysisIdentityMismatch { component: &'static str },
    #[error(
        "artifact was written under token schema {recorded}, but this build produces {expected}; rebuild the artifact from source"
    )]
    TokenSchemaIdentityStale { recorded: String, expected: String },
    #[error(
        "artifact was normalized with Unicode tables {recorded}, but this build uses {expected}; rebuild the artifact from source"
    )]
    NormalizationTablesStale { recorded: String, expected: String },
    #[error("artifact component {component} belongs to a different artifact instance")]
    ArtifactBindingMismatch { component: &'static str },
    #[error("artifact component {component} has a different sentence count")]
    SentenceCountMismatch { component: &'static str },
    #[error("artifact component {component} checksum differs from the manifest")]
    ComponentChecksumMismatch { component: &'static str },
    #[error("immutable provenance has a SQLite sidecar")]
    SqliteSidecar,
    #[error("artifact component {component} is truncated")]
    TruncatedComponent { component: &'static str },
    #[error(
        "The source files observed while reading do not match the files found during discovery"
    )]
    SourceInventoryMismatch,
    #[error("builder source inventory and discovery report differ")]
    DiscoveryReportMismatch,
    #[error("Cannot create '{path}': the destination already exists and was left unchanged")]
    DestinationExists { path: PathBuf },
    #[error(
        "Cannot save '{path}' safely: atomic rename without replacement is not supported (OS error {errno})"
    )]
    UnsupportedPublication { path: PathBuf, errno: i32 },
    #[error("Created '{path}', but could not confirm it will survive a system crash")]
    PublicationDurabilityUncertain { path: PathBuf },
    #[error("verification mismatch in {component}: {message}")]
    VerificationMismatch {
        component: &'static str,
        sentence_ordinal: Option<u64>,
        message: String,
    },
}

/// An I/O failure with the operation and path that produced it.
#[derive(Debug, thiserror::Error)]
#[error("cannot {operation} '{}': {source}", path.display())]
struct OperationFailed {
    operation: String,
    path: PathBuf,
    source: Box<dyn std::error::Error + Send + Sync>,
}

/// An I/O error whose message names `operation` and `path`, with kind `kind` and
/// `source` as its source.
pub(crate) fn io_at(
    operation: impl Into<String>,
    path: &Path,
    kind: std::io::ErrorKind,
    source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
) -> std::io::Error {
    std::io::Error::new(
        kind,
        OperationFailed {
            operation: operation.into(),
            path: path.to_path_buf(),
            source: source.into(),
        },
    )
}

impl CorpusError {
    /// Returns true for damaged or inconsistent stored bytes; OS, resource,
    /// unsupported-version and directory-layout errors return false.
    #[must_use]
    pub fn is_artifact_corruption(&self) -> bool {
        match self {
            Self::Truncated { .. }
            | Self::InvalidFormat { .. }
            | Self::ComponentLengthMismatch { .. }
            | Self::AnalysisIdentityMismatch { .. }
            | Self::ArtifactBindingMismatch { .. }
            | Self::SentenceCountMismatch { .. }
            | Self::ComponentChecksumMismatch { .. }
            | Self::TruncatedComponent { .. }
            | Self::DiscoveryReportMismatch
            | Self::VerificationMismatch { .. } => true,
            Self::Identity(_)
            | Self::Io(_)
            | Self::TempCapacity(_)
            | Self::ProvenancePageCapacity { .. }
            | Self::SectionLabelCapacity { .. }
            | Self::TokenBlockCapacity { .. }
            | Self::UnsortedOrdinals
            | Self::UnsupportedSchema { .. }
            | Self::PreparedArtifactRebuildRequired { .. }
            | Self::UnsupportedArtifactGeneration { .. }
            | Self::UnknownManifestField { .. }
            | Self::ExpectedArtifactDirectory { .. }
            | Self::SymlinkComponent { .. }
            | Self::TokenSchemaIdentityStale { .. }
            | Self::NormalizationTablesStale { .. }
            | Self::SqliteSidecar
            | Self::SourceInventoryMismatch
            | Self::DestinationExists { .. }
            | Self::UnsupportedPublication { .. }
            | Self::PublicationDurabilityUncertain { .. } => false,
        }
    }

    /// Stable machine-readable error category.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Identity(IdentityError::InvalidState) => "identity_invalid_state",
            Self::Identity(IdentityError::LengthOverflow) => "identity_length_overflow",
            Self::Identity(IdentityError::TokenSchemaMismatch) => "identity_token_schema_mismatch",
            Self::Io(_) => "io",
            Self::TempCapacity(_) => "temp_capacity",
            Self::ProvenancePageCapacity { .. } => "provenance_page_capacity",
            Self::SectionLabelCapacity { .. } => "section_label_capacity",
            Self::TokenBlockCapacity { .. } => "token_block_capacity",
            Self::Truncated { .. } => "truncated",
            Self::UnsortedOrdinals => "unsorted_ordinals",
            Self::InvalidFormat { .. } => "invalid_format",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::PreparedArtifactRebuildRequired { .. }
            | Self::UnsupportedArtifactGeneration { .. } => "unsupported_schema",
            Self::UnknownManifestField { .. } => "unknown_manifest_field",
            Self::ExpectedArtifactDirectory { .. } => "expected_artifact_directory",
            Self::SymlinkComponent { .. } => "symlink_component",
            Self::ComponentLengthMismatch { .. } => "component_length_mismatch",
            Self::AnalysisIdentityMismatch { .. } => "analysis_identity_mismatch",
            Self::TokenSchemaIdentityStale { .. } => "token_schema_identity_stale",
            Self::NormalizationTablesStale { .. } => "normalization_tables_stale",
            Self::ArtifactBindingMismatch { .. } => "artifact_binding_mismatch",
            Self::SentenceCountMismatch { .. } => "sentence_count_mismatch",
            Self::ComponentChecksumMismatch { .. } => "component_checksum_mismatch",
            Self::SqliteSidecar => "sqlite_sidecar",
            Self::TruncatedComponent { .. } => "truncated_component",
            Self::SourceInventoryMismatch => "source_inventory_mismatch",
            Self::DiscoveryReportMismatch => "discovery_report_mismatch",
            Self::DestinationExists { .. } => "destination_exists",
            Self::UnsupportedPublication { .. } => "unsupported_publication",
            Self::PublicationDurabilityUncertain { .. } => "publication_durability_uncertain",
            Self::VerificationMismatch { .. } => "verification_mismatch",
        }
    }
}
