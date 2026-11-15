//! The `lingrep.neighbors/v1` contract: the values nearest one word in a
//! word-vector file.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ContractId;

pub const SCHEMA: &str = ContractId::Neighbors.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<NeighborsEnvelope>(
        ContractId::Neighbors,
        &["neighbors"],
        "Values ranked by cosine similarity to one word in a word-vector file.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "neighbors",
                    events: &["neighbor"],
                    min: 1,
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
        "Descending similarity, then bytewise value order.",
        "Values meeting --min-similarity, limited by --top.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 unreadable or malformed file or output failure, 2 invocation failure or a word without a vector.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NeighborsEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: NeighborsEvent,
}

impl NeighborsEnvelope {
    #[must_use]
    pub fn new(event: NeighborsEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum NeighborsEvent {
    Run(NeighborsRun),
    Neighbor(Neighbor),
    Complete(NeighborsComplete),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NeighborsRun {
    pub word: String,
    pub vectors: String,
    pub dimensions: u64,
    pub min_similarity: Option<f32>,
    pub top: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Neighbor {
    /// One-based position in the published order.
    pub rank: u64,
    pub value: String,
    /// Cosine between this value's vector and the word's, from -1 to 1.
    pub similarity: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NeighborsComplete {
    pub neighbors: u64,
    /// Rows read from the file.
    pub values: u64,
    /// Rows whose vector is all zeros; they have no direction and are never
    /// a neighbor.
    pub values_without_vector: u64,
}
