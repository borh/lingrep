use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::{Coverage, InventoryStats, Termination, TraversalLimits};

pub const SCHEMA: &str = super::ContractId::Sources.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<SourcesEnvelope>(
        super::ContractId::Sources,
        &["corpus build", "corpus inspect", "corpus inventory"],
        "Corpus inventory run, source records, and completion evidence.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "sources",
                    events: &["source"],
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
        "Stable source ordinal order.",
        "Live or recorded inventory as identified by the run record.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &[
            "corpus inventory/direct-root",
            "corpus inspect/prepared-artifact",
        ],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourcesEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: SourcesEvent,
}
fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong sources schema"))
    }
}
impl SourcesEnvelope {
    pub fn new(event: SourcesEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SourcesEvent {
    Run(SourcesRun),
    Source(SourceData),
    Complete(SourcesComplete),
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SourcesKind {
    #[default]
    Live,
    Artifact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AggregateState {
    Pending,
    Known { inventory: InventoryStats },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraversalHighWater {
    pub max_depth: u64,
    pub examined_entries: u64,
    pub max_directory_entries: u64,
    pub index_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourcesRun {
    pub run_id: String,
    pub source_kind: SourcesKind,
    pub reader_identity: String,
    pub admission_policy_identity: String,
    pub traversal_limits: TraversalLimits,
    pub aggregate_state: AggregateState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceData {
    pub ordinal: u64,
    pub logical_name: String,
    pub target_group_ordinal: u64,
    pub external_target: bool,
    pub target_multiplicity: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourcesComplete {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MetadataReport>,
    pub coverage: Coverage,
    #[serde(flatten)]
    pub inventory: InventoryStats,
    pub traversal_high_water: TraversalHighWater,
    pub ordered_inventory_digest: String,
    pub elapsed_millis: u64,
    pub termination: Termination,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<ArtifactVerification>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArtifactVerification {
    pub components: Vec<ComponentVerification>,
    pub compared_location_rows: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ComponentVerification {
    pub name: String,
    pub length_matches: bool,
    pub blake3_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetadataReport {
    pub divisions: std::collections::BTreeMap<String, DivisionMetadataReport>,
    pub division_kinds_truncated: bool,
    pub documents: MetadataGrain,
    pub sentences: MetadataGrain,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetadataGrain {
    pub units: u64,
    pub fields: std::collections::BTreeMap<String, MetadataField>,
    pub fields_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetadataField {
    pub present: u64,
    pub missing: u64,
    pub shapes: std::collections::BTreeMap<String, u64>,
    pub examples: Vec<MetadataExample>,
    pub examples_complete: bool,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct MetadataExample {
    pub preview: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DivisionMetadataReport {
    #[serde(flatten)]
    pub metadata: MetadataGrain,
    pub empty_units: u64,
    pub observed_documents: u64,
    pub unavailable_documents: u64,
}
