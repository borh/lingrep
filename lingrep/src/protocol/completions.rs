use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Completions.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<CompletionEnvelope>(
        super::ContractId::Completions,
        &["completions"],
        "Generated shell-completion payload and shell identity.",
        StreamShape::Single {
            events: &["completion"],
        },
        Channel::Stdout,
        "One completion payload.",
        "Complete generated script for the selected shell.",
        "A closed pipe is a machine-output failure.",
        "0 generated, 1 output failure, 2 invalid shell or invocation.",
        &["completions/bash"],
    )
}

define_protocol_envelope!(
    pub struct CompletionEnvelope(CompletionEvent::Completion(CompletionData)),
    schema = SCHEMA,
    extra_derives = [Eq],
);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CompletionEvent {
    Completion(CompletionData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompletionData {
    pub shell: String,
    pub script: String,
}
