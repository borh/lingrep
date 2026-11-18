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
        "Superseded and older-format realizations, their count tables, and interrupted prunes, deleted or planned.",
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
        "Kept entries in bytewise path order; removals of interrupted prunes, then of tables, then of realizations, each in bytewise path order.",
        "Entries immediately in the scanned directory at run time.",
        "A failed deletion ends the removals with its removal record, carrying `failure`, then the complete record; deletions already made remain made. A closed pipe is a machine-output failure.",
        "0 complete, 1 a deletion failed or a runtime/output failure, 2 invocation failure or no realization to keep.",
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
    /// Whether realizations the default prune keeps as `reader_variant`,
    /// `source_revision` or `source_unknown` are deleted too.
    pub include_all_superseded: bool,
    /// Whether this run deletes, rather than only listing what it would.
    pub execute: bool,
}

/// A superseded, older-format or damaged realization, a count table bound
/// only to removed realizations, or an interrupted prune, that this run
/// keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneKept {
    pub path: String,
    pub reason: PruneKeep,
    pub differences: Vec<VariantDifference>,
    /// Apparent size of the files under `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_bytes: Option<u64>,
    /// For a `damaged` realization, the validation failure; for
    /// `foreign_entries`, the entries that are not components; for
    /// `kept_table`, the kept table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One entry this run deletes, would delete without `--execute`, or failed
/// to delete.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneRemoval {
    pub path: String,
    pub reason: RemovalReason,
    /// For a superseded realization, the dimensions on which it differs from
    /// the current request.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<VariantDifference>,
    /// Apparent size of the files under `path`: measured immediately before
    /// the deletion with `--execute`, and at scan time otherwise or when the
    /// deletion failed. A file hard-linked elsewhere is counted although its
    /// deletion frees no space.
    pub apparent_bytes: u64,
    /// With `--execute`, why the deletion failed. It is the last removal
    /// record; the entry is left as `lingrep.disk/v1` then reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
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
    /// A realization or count table an earlier `corpus prune --execute`
    /// renamed for deletion and did not finish deleting, as
    /// `lingrep.disk/v1` reports it.
    InterruptedPrune,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PruneComplete {
    pub executed: bool,
    /// Removals planned.
    pub removals: u64,
    /// Removals deleted; zero without `--execute`.
    pub deleted: u64,
    /// Removals whose deletion failed: zero, or one, after which no further
    /// removal is attempted.
    pub failed: u64,
    /// Apparent size of the planned removals when the scan measured them.
    pub planned_apparent_bytes: u64,
    /// Apparent size of the deleted removals.
    pub deleted_apparent_bytes: u64,
}
