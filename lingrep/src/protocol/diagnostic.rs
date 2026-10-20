use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::SourceSpan;

pub const SCHEMA: &str = super::ContractId::Diagnostic.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<DiagnosticEnvelope>(
        super::ContractId::Diagnostic,
        &["ling"],
        "Typed invocation, catalog, input, execution, and output diagnostics.",
        StreamShape::Repeated {
            events: &["diagnostic"],
        },
        Channel::Stderr,
        "Occurrence order.",
        "Every emitted diagnostic is independently complete.",
        "A closed diagnostic pipe is an output failure.",
        "Errors accompany exit 1 or 2; warnings do not change exit status.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: DiagnosticEvent,
}
fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong diagnostic schema"))
    }
}
impl DiagnosticEnvelope {
    pub fn new(event: DiagnosticEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DiagnosticEvent {
    Diagnostic(DiagnosticData),
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Invocation,
    Catalog,
    Input,
    Matching,
    Aggregation,
    Output,
    #[default]
    Unknown,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    #[default]
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetryPolicy {
    Safe,
    AfterChange,
    Unsafe,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticLocation {
    pub source_name: Option<String>,
    pub source_span: Option<SourceSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
    pub corpus_sentence_ordinal: Option<u64>,
    pub token_index: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RelatedDiagnosticLocation {
    pub message: String,
    pub location: DiagnosticLocation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiagnosticData {
    pub run_id: Option<String>,
    /// Observational input evidence for an interrupted or failed direct scan.
    /// It is intentionally not a reusable corpus or analysis identity.
    pub observed_input_digest: Option<String>,
    pub stage: DiagnosticStage,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
    pub location: Option<DiagnosticLocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_locations: Vec<RelatedDiagnosticLocation>,
}
