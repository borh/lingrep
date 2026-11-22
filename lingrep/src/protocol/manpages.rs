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

define_protocol_envelope!(
    pub struct ManpagesEnvelope(ManpagesEvent::Manpages(ManpagesData)),
    schema = SCHEMA,
    extra_derives = [Eq],
);

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
