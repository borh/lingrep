use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::SentenceFinal.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<SentenceFinalEnvelope>(
        super::ContractId::SentenceFinal,
        &["sentence-final"],
        "Sentence-final run, aggregate rows, and completion evidence.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "rows",
                    events: &["row"],
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
        "Pattern then UTF-8 surface order.",
        "Complete aggregate after successful execution.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure, 128+signal on cancellation.",
        &["sentence-final/catalog-file-direct-root"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SentenceFinalEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: SentenceFinalEvent,
}

impl SentenceFinalEnvelope {
    #[must_use]
    pub fn new(event: SentenceFinalEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SentenceFinalEvent {
    Run(RunData),
    Row(RowData),
    Complete(CompleteData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunData {
    pub run_id: String,
    pub dictionary_identity: String,
    pub catalog_fingerprint: String,
    pub input_kind: String,
    pub limits: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RowData {
    pub pattern: String,
    pub surface: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompleteData {
    pub coverage: String,
    pub rows: u64,
    pub matches_found: u64,
    pub elapsed_millis: u64,
    pub resources: serde_json::Value,
}
