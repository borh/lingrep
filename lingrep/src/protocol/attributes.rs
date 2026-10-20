//! The `attributes` stream: what this corpus contains, and which authored
//! exclusions it exposes.

use super::query::AnalyzerRun;
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Attributes.as_str();

/// One aggregate run. Not a live stream: every count is final before the first
/// record is written, and the phases exist to give the three record shapes
/// separate contracts rather than to promise incremental delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AttributesEvent {
    Scope(Box<ScopeData>),
    Coverage(CoverageData),
    Exclusion(ExclusionData),
    Projection(ProjectionData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeData {
    pub analyzer: AnalyzerRun,
    pub token_schema_identity: String,
    pub admitted_tokens: u64,
    pub admitted_sentences: u64,
    pub admitted_documents: u64,
    /// Present only when `--where` was given; `corpus` for a prepared run,
    /// `observed_prefix` for a limited direct one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<serde_json::Value>,
    /// Which dictionary produced these tokens, as a stable identity rather
    /// than a path — a prepared run has no dictionary path at all. A coverage
    /// number is a fact about a binding, not about an attribute.
    pub dictionary_identity: String,
    /// The catalog fingerprint the exclusions came from, absent when no
    /// catalog was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CoverageData {
    pub attribute: String,
    /// Set to a value other than `*`.
    pub present: u64,
    /// Set to `*` — a supplied value, not an absence.
    pub unspecified: u64,
    pub absent: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExclusionData {
    pub definition: String,
    pub attribute: String,
    pub source_name: String,
    pub declaration_line: usize,
    pub site_count: usize,
    pub reachable: bool,
    /// Tokens of this corpus on which the attribute is absent. Exposure, not
    /// impact: it does not claim a match was lost.
    pub absent_in_corpus: u64,
    /// `clean`, `exposed`, `inert`, or `unknown`.
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProjectionData {
    pub definition: String,
    pub attribute: String,
    pub source_name: String,
    pub declaration_line: usize,
    pub reachable: bool,
    pub missing_in_corpus: u64,
    pub empty_in_corpus: u64,
    pub status: String,
}

/// The wire envelope. **Must** be a struct with a `schema` field:
/// [`super::discovery::envelope_schema`] rewrites `/properties/schema` and
/// returns `MissingSchemaProperty` when it is absent, so an alias for
/// [`AttributesEvent`] fails at contract construction. Mirrors
/// [`super::tokenize::TokenizeEnvelope`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttributesEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: AttributesEvent,
}

impl AttributesEnvelope {
    pub fn new(event: AttributesEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<AttributesEnvelope>(
        super::ContractId::Attributes,
        &["coverage"],
        "Attribute presence over admitted tokens and catalog exclusions affected by attribute absence.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "scope",
                    events: &["scope"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "projections",
                    events: &["projection"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "coverage",
                    events: &["coverage"],
                    min: 1,
                    max: Some(u8::MAX as u64),
                },
                StreamPhase {
                    name: "exclusions",
                    events: &["exclusion"],
                    min: 0,
                    max: None,
                },
            ],
        },
        Channel::Stdout,
        "One scope record, then every attribute in token-schema order, then \
         exclusions by source, line, definition, attribute.",
        "Every attribute is always reported; exclusions only when a catalog was supplied.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure, 128+signal on cancellation.",
        &["coverage/direct-root"],
    )
}
