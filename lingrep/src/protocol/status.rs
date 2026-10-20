use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Status.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<StatusEnvelope>(
        super::ContractId::Status,
        &["ling"],
        "Structured presentation-status records.",
        StreamShape::Repeated {
            events: &["status"],
        },
        Channel::Stderr,
        "Occurrence order.",
        "Presentation-only progress; never changes result coverage.",
        "A closed progress pipe disables further progress output.",
        "Status records do not determine command exit status.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: StatusEvent,
}

impl StatusEnvelope {
    #[must_use]
    pub fn new(data: StatusData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: StatusEvent::Status(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum StatusEvent {
    Status(StatusData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusData {
    pub stage: String,
    pub current: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    pub message: String,
}
