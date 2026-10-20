use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Invocation.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<InvocationEnvelope>(
        super::ContractId::Invocation,
        &[
            "matches",
            "summarize",
            "tokenize",
            "sentence-final",
            "corpus build",
            "corpus inventory",
        ],
        "Resolved invocation plan produced by --plan.",
        StreamShape::Single {
            events: &["invocation"],
        },
        Channel::Stdout,
        "One resolved plan.",
        "Static invocation facts only; no corpus traversal.",
        "A closed pipe is a machine-output failure.",
        "0 resolved, 1 input/readiness failure, 2 invalid invocation.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InvocationEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: InvocationEvent,
}

impl InvocationEnvelope {
    #[must_use]
    pub fn new(data: InvocationData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: InvocationEvent::Invocation(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum InvocationEvent {
    Invocation(InvocationData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InvocationData {
    pub command: String,
    pub catalog_source: Option<String>,
    pub catalog_fingerprint: Option<String>,
    pub dictionary_path: Option<String>,
    pub dictionary_source: Option<String>,
    pub input_kind: String,
    pub output_mode: String,
    pub color: String,
    pub progress: String,
    pub limits: serde_json::Value,
    pub schemas: Vec<String>,
}
