use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use super::ContractId;

pub const SCHEMA: &str = ContractId::WindowCounts.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<WindowCountsEnvelope>(
        ContractId::WindowCounts,
        &["summarize"],
        "Counts by sentence-aligned document window, selected by --by-window.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "windows",
                    events: &["window"],
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
        "Ascending document ordinal, then ascending window ordinal.",
        "Every window containing a sentence, including windows with zero matches.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure, 128+signal on cancellation.",
        &[],
    )
}

#[derive(Serialize, JsonSchema)]
pub struct WindowCountsEnvelope {
    schema: &'static str,
    #[serde(flatten)]
    event: WindowCountsEvent,
}

impl WindowCountsEnvelope {
    pub const fn new(event: WindowCountsEvent) -> Self {
        Self {
            schema: SCHEMA,
            event,
        }
    }
}

#[derive(Serialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WindowCountsEvent {
    Run(WindowCountsRun),
    Window(WindowCountsData),
    Complete(WindowCountsComplete),
}

#[derive(Serialize, JsonSchema)]
pub struct WindowCountsRun {
    pub run_id: String,
    pub source_identity: String,
    pub analysis_identity: String,
    pub catalog_fingerprint: String,
    pub window_size: u64,
    pub patterns: Vec<String>,
}

#[derive(Default, Serialize, JsonSchema)]
pub struct WindowCountsData {
    pub document_ordinal: u64,
    pub window_ordinal: u64,
    pub sentences: u64,
    pub tokens: u64,
    pub counts: BTreeMap<String, u64>,
}

#[derive(Serialize, JsonSchema)]
pub struct WindowCountsComplete {
    pub windows: u64,
}
