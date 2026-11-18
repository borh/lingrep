//! The `lingrep.prune/v1` contract: what `corpus prune` deletes, or would.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::disk::{DiskSource, PruneKeep};
use super::variants::VariantDifference;

pub const SCHEMA: &str = super::ContractId::Prune.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<PruneEnvelope>(
        super::ContractId::Prune,
        &["corpus prune"],
        "Superseded and older-format realizations, and their count tables, deleted or planned.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "sources",
                    events: &["source"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "kept",
                    events: &["kept"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "removals",
                    events: &["removal"],
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
        "Kept realizations in bytewise path order; removed tables in bytewise path order, then removed realizations in bytewise path order.",
        "Entries immediately in the scanned directory at run time.",
        "A closed pipe is a machine-output failure; deletions already made remain made.",
        "0 complete, 1 runtime/output failure, 2 invocation failure or no realization to keep.",
        &["corpus prune/dry-run"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: PruneEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong prune schema"))
    }
}

impl PruneEnvelope {
    #[must_use]
    pub fn new(event: PruneEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum PruneEvent {
    Run(PruneRun),
    Source(DiskSource),
    Kept(PruneKept),
    Removal(PruneRemoval),
    Complete(PruneComplete),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneRun {
    pub run_id: String,
    /// Directory scanned, non-recursively.
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Artifacts kept by `--keep` in addition to each source's current one.
    pub keep: Vec<String>,
    /// Whether superseded realizations that differ in no analysis dimension
    /// are deleted too.
    pub include_all_superseded: bool,
    /// Whether this run deletes, rather than only listing what it would.
    pub execute: bool,
}

/// A superseded or damaged realization this run keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneKept {
    pub path: String,
    pub reason: PruneKeep,
    pub differences: Vec<VariantDifference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// For a `damaged` realization, the validation failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One entry this run deletes, or would delete without `--execute`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneRemoval {
    pub path: String,
    pub reason: RemovalReason,
    /// For a superseded realization, the dimensions on which it differs from
    /// the current request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<VariantDifference>,
    /// Without `--execute`, the size measured now; with it, the size the
    /// deletion freed.
    pub bytes: u64,
    pub deleted: bool,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    /// A superseded realization, as `lingrep.disk/v1` defines it.
    Superseded,
    /// A realization written in an older format.
    OlderFormat,
    /// A count table whose bound artifacts are all removed by this run.
    TableOfRemoved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneComplete {
    pub executed: bool,
    pub removals: u64,
    /// Bytes the planned removals occupied when the run measured them.
    pub expected_freed_bytes: u64,
    /// Bytes the deletions freed; zero without `--execute`.
    pub freed_bytes: u64,
}
