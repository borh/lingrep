use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ContractId;

pub const SCHEMA: &str = ContractId::Manpages.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<ManpagesEnvelope>(
        ContractId::Manpages,
        &["manpages"],
        "Committed manual-page directory and ordered page sizes.",
        StreamShape::Single {
            events: &["manpages"],
        },
        Channel::Stdout,
        "Depth-first public command order.",
        "Only pages committed by the successful no-replace publication.",
        "A post-commit machine output failure is reported as committed state uncertainty.",
        "0 published, 1 publication/output failure, 2 invalid invocation.",
        &["manpages/generate-all"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManpagesEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: ManpagesEvent,
}

impl ManpagesEnvelope {
    pub fn new(data: ManpagesData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: ManpagesEvent::Manpages(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ManpagesEvent {
    Manpages(ManpagesData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManpagesData {
    pub output: String,
    pub pages: Vec<PageData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PageData {
    pub name: String,
    pub bytes: u64,
}
