use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Capabilities.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<CapabilityEnvelope>(
        super::ContractId::Capabilities,
        &["describe"],
        "Parser-derived command capabilities and semantic descriptors.",
        StreamShape::Repeated {
            events: &["capability"],
        },
        Channel::Stdout,
        "Stable command-path order.",
        "One record for the requested command or every public command.",
        "A closed pipe is a machine-output failure.",
        "0 described, 1 output failure, 2 unknown command or invalid invocation.",
        &["describe/machine-capabilities"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CapabilityEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: CapabilityEvent,
}

impl CapabilityEnvelope {
    #[must_use]
    pub fn new(data: CapabilityData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: CapabilityEvent::Capability(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CapabilityEvent {
    Capability(CapabilityData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CapabilityData {
    pub command: String,
    pub purpose: String,
    pub output_modes: Vec<String>,
    pub schemas: Vec<String>,
    pub tsv_contracts: Vec<String>,
    pub input_modes: Vec<String>,
    pub arguments: Vec<ArgumentData>,
    pub environment: Vec<String>,
    pub defaults: Vec<String>,
    pub exit_codes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<ExampleData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArgumentData {
    pub name: String,
    pub required: bool,
    pub values: Vec<String>,
    pub conflicts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExampleData {
    pub id: String,
    pub purpose: String,
    pub argv: Vec<String>,
    pub placeholders: BTreeMap<String, String>,
}
