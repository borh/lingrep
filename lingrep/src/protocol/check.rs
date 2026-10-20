use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::SourceSpan;

pub const SCHEMA: &str = super::ContractId::Check.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<CheckEnvelope>(
        super::ContractId::Check,
        &["check"],
        "One compiled-catalog report with warnings and drift evidence.",
        StreamShape::Single { events: &["check"] },
        Channel::Stdout,
        "Authored catalog order.",
        "The complete catalog report.",
        "A closed pipe is a machine-output failure.",
        "0 valid catalog and checked claims hold, 1 violated claims or runtime/output failure, 2 invalid invocation or catalog.",
        &["check/inline-pattern"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: CheckEvent,
}
fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong check schema"))
    }
}
impl CheckEnvelope {
    pub fn new(event: CheckEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CheckEvent {
    Check(CheckData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DefinitionDescriptor {
    pub name: String,
    pub catalog_ordinal: u64,
    pub body: super::query::DefinitionBody,
    pub metadata: serde_json::Value,
    pub source_file: String,
    pub source_span: SourceSpan,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompileStats {
    pub source_bytes: u64,
    pub definitions: u64,
    pub lexicon_members: u64,
    pub expression_nodes: u64,
    pub max_depth: u64,
    pub metadata_bytes: u64,
    pub regex_bytes: u64,
    pub reference_edges: u64,
    pub expansion_work: u64,
    pub generated_owned_bytes: u64,
    pub nfa_states: u64,
    pub transitions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompileWarning {
    pub code: String,
    pub message: String,
    pub location: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceLocation {
    pub source_file: String,
    pub source_span: SourceSpan,
}

/// One catalog carrying one body in a drift report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftCarrierData {
    pub catalog: String,
    /// `"pattern"` or `"fragment"`. Reported, never compared.
    pub kind: String,
}

/// One distinct body.
///
/// An object rather than a bare `Vec<DriftCarrierData>`: the
/// additive-fields-keep-`/v1` rule only holds where every wire node has
/// somewhere to put a field. A nested bare list would be the one node in this
/// protocol that cannot absorb one, so adding a body fingerprint or structural
/// diff would mean turning a JSON array into an object
/// — a breaking change, and exactly the schema bump the rule exists to avoid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftBodyData {
    pub carriers: Vec<DriftCarrierData>,
}

/// One definition name whose reference-closed canonical bodies differ across
/// the compared catalogs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftData {
    pub name: String,
    pub bodies: Vec<DriftBodyData>,
}

/// One referenced attribute a named schema cannot supply, with the definition
/// sites that reference it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnavailableAttributeData {
    pub attribute: String,
    /// Empty when only the catalog's publication policy references it.
    pub sites: Vec<BindingSiteData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BindingSiteData {
    pub definition: String,
    pub source_file: String,
    /// 1-indexed line of the definition's own declaration.
    pub line: u64,
}

/// The `--schema` finding: how this catalog binds against a named schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BindingData {
    /// The requested schema name, e.g. `"unidic-morphology"`.
    pub schema: String,
    /// Referenced attributes the schema does not carry. The catalog still
    /// binds; positive tests over them match nothing there.
    pub unavailable: Vec<UnavailableAttributeData>,
    /// Present when the catalog cannot bind at all — dependency `where`
    /// clauses without their edge attributes, or a value-kind conflict — and
    /// the check then fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckData {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comparison: Vec<ComparedDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<ExampleVerification>,
    pub catalog_fingerprint: String,
    pub definitions: Vec<DefinitionDescriptor>,
    pub compile_stats: CompileStats,
    pub warnings: Vec<CompileWarning>,
    /// Cross-catalog drift, present only when `--compare-with` found any.
    ///
    /// Additive fields keep the `lingrep.check/v1` schema string: nothing in
    /// this protocol sets `deny_unknown_fields`, and `serde(default)` keeps
    /// previously-written records readable. The string bumps only on a
    /// breaking change — a removed field, a changed type, or a changed
    /// meaning for an existing field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<DriftData>,
    /// The `--schema` binding finding, present only when one was requested.
    /// Additive under the rule above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExampleVerification {
    pub analyzer: super::query::AnalyzerRun,
    pub resource_identity: String,
    pub checked: u64,
    pub failures: Vec<ExampleFailure>,
    pub post_policy_checked: u64,
    pub post_policy_failures: Vec<ExampleFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExampleFailure {
    pub definition: String,
    pub example: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ComparedDefinition {
    pub name: String,
    pub revisions: Vec<DefinitionRevision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DefinitionRevision {
    pub catalog: String,
    pub kind: String,
    pub body_group: u64,
    pub expanded: String,
}
