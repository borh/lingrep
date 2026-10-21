//! The `lingrep.variants/v1` contract: prepared realizations of one source.
//!
//! Current prepared artifacts can differ in analyzer, reader or admission
//! policy. Each difference is reported as its own named dimension. Discovery
//! reports an artifact of another generation as a diagnostic, not a
//! realization; it requires rebuilding from source. A diagnostic of the
//! target's own source in an earlier format is `prunable`, and `--prune`
//! deletes exactly those.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

pub const SCHEMA: &str = super::ContractId::Variants.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<VariantsEnvelope>(
        super::ContractId::Variants,
        &["corpus variants"],
        "Prepared realizations beside one source, and how each differs.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "variants",
                    events: &["variant"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "complete",
                    events: &["complete"],
                    min: 1,
                    max: Some(1),
                },
            ],
        },
        Channel::Stdout,
        "Bytewise artifact path order.",
        "Realizations present in the scanned directory at run time.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &["corpus variants/source-or-artifact"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantsEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: VariantsEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong variants schema"))
    }
}

impl VariantsEnvelope {
    #[must_use]
    pub fn new(event: VariantsEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum VariantsEvent {
    Run(VariantsRun),
    Variant(Box<VariantRecord>),
    Complete(VariantsComplete),
}

/// What the run was asked about, and what it is comparing against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantsRun {
    pub run_id: String,
    /// Directory scanned, non-recursively.
    pub directory: String,
    /// Which subject the differences are expressed relative to.
    pub reference: VariantsReference,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VariantsReference {
    /// The argument was a source root; nothing is a reference, so every
    /// realization is reported on its own terms and grouped by source.
    Source { root: String },
    /// The argument was an artifact; every difference is relative to it.
    Artifact {
        path: String,
        source_identity: String,
        analysis_identity: String,
    },
}

/// One prepared realization, and how it differs from the reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantRecord {
    pub path: String,
    /// The recorded lingrep identity scheme (`a1`).
    pub identity_scheme: String,
    pub format_generation: u32,
    pub source_identity: String,
    pub analysis_identity: String,
    pub reader_identity: String,
    pub admission_identity: String,
    pub token_schema_identity: String,
    pub dictionary_identity: String,
    pub trust: String,
    pub analyzer: VariantAnalyzer,
    pub document_count: u64,
    pub sentence_count: u64,
    pub token_count: u64,
    /// Whether this realization is of the same source content as the
    /// reference. A realization of different content is an older local
    /// snapshot, not a variant analysis of the same corpus.
    pub same_source: bool,
    /// The dimensions on which this realization differs, in a fixed order.
    pub differences: Vec<VariantDifference>,
}

/// A named dimension on which two realizations differ.
///
/// Closed and ordered so that machine output over unchanged bytes is
/// reproducible.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VariantDifference {
    /// Different source bytes at the same local name.
    Source,
    /// A different reader policy — the case `--input-format` creates, where
    /// one tree read two ways is two artifacts with no analyzer difference.
    Reader,
    /// A different sentence-admission policy.
    Admission,
    /// A different analyzer, by its own recorded semantic identity.
    Analyzer,
    /// A different stored token schema.
    TokenSchema,
    /// A different trust boundary.
    Trust,
}

/// The analyzer semantics a manifest recorded.
///
/// Deliberately not `query::AnalyzerRun`: that type carries the stored token
/// schema, which lives in the token component rather than the manifest.
/// Including it would force the scan to open every candidate's token file,
/// turning a bounded manifest read into a full artifact open per sibling — for
/// a field this command never needed. These are exactly the fields the
/// manifest has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantAnalyzer {
    pub family: String,
    pub profile: String,
    pub segmentation: String,
    pub normalization: String,
    pub language_policy: String,
    pub boundary_policy: String,
    pub semantic_identity: String,
}

impl VariantAnalyzer {
    #[must_use]
    pub fn from_record(record: &lingrep_corpus::AnalyzerRecord) -> Self {
        Self {
            family: record.family.clone(),
            profile: record.profile.clone(),
            segmentation: record.segmentation.clone(),
            normalization: record.normalization.clone(),
            language_policy: record.language_policy.clone(),
            boundary_policy: record.boundary_policy.clone(),
            semantic_identity: record.semantic_identity.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantsComplete {
    /// Realizations found, whether or not each was reported.
    pub realizations: u64,
    /// Realizations reported, which `--limit` may cap below `realizations`.
    pub reported: u64,
    /// Candidates whose manifest could not be read.
    pub unreadable: u64,
    /// One entry per unreadable candidate, in bytewise path order.
    pub diagnostics: Vec<VariantDiagnostic>,
    /// Diagnostics deleted by `--prune`.
    #[serde(default)]
    pub pruned: u64,
    /// Bytes those deletions freed.
    #[serde(default)]
    pub freed_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VariantDiagnostic {
    pub path: String,
    pub message: String,
    /// Size of the files under `path`; absent when it could not be measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Written for the target's source in a format older than this build
    /// reads, so only a rebuild from source can replace it.
    #[serde(default)]
    pub prunable: bool,
    /// Deleted by this run.
    #[serde(default)]
    pub pruned: bool,
}
