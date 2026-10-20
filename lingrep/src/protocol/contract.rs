use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ContractId;
use super::discovery::{Channel, ContractKind, HeaderPolicy, StreamShape};

pub const SCHEMA: &str = ContractId::ProtocolContract.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<ContractEnvelope>(
        ContractId::ProtocolContract,
        &["contracts"],
        "Compact contract index or one complete contract detail.",
        StreamShape::Single {
            events: &["contract_index", "contract"],
        },
        Channel::Stdout,
        "Contract IDs are lexically ordered in the index.",
        "One complete index or selected detail record.",
        "A closed pipe is a machine-output failure.",
        "0 described, 1 schema/output failure, 2 unknown contract or invalid invocation.",
        &["contracts/query-detail"],
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: ContractEvent,
}

impl ContractEnvelope {
    pub fn new(event: ContractEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ContractEvent {
    ContractIndex(ContractIndexData),
    Contract(Box<ContractDetailData>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractIndexData {
    pub contracts: Vec<ContractSummaryData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractSummaryData {
    pub id: String,
    pub kind: ContractKind,
    pub owners: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractDetailData {
    pub id: String,
    pub kind: ContractKind,
    pub owners: Vec<String>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamShapeData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tsv: Option<TsvLayoutData>,
    pub ordering: String,
    pub coverage: String,
    pub broken_pipe: String,
    pub exit_status: String,
    pub example_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TsvLayoutData {
    pub columns: Vec<ColumnData>,
    pub header: HeaderPolicy,
    pub escaping: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ColumnData {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum StreamShapeData {
    Single { events: Vec<String> },
    Repeated { events: Vec<String> },
    Phased { phases: Vec<StreamPhaseData> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StreamPhaseData {
    pub name: String,
    pub events: Vec<String>,
    pub min: u64,
    pub max: Option<u64>,
}

impl From<&StreamShape> for StreamShapeData {
    fn from(value: &StreamShape) -> Self {
        let strings = |values: &[&str]| values.iter().map(|value| (*value).into()).collect();
        match value {
            StreamShape::Single { events } => Self::Single {
                events: strings(events),
            },
            StreamShape::Repeated { events } => Self::Repeated {
                events: strings(events),
            },
            StreamShape::Phased { phases } => Self::Phased {
                phases: phases
                    .iter()
                    .map(|phase| StreamPhaseData {
                        name: phase.name.into(),
                        events: strings(phase.events),
                        min: phase.min,
                        max: phase.max,
                    })
                    .collect(),
            },
        }
    }
}
