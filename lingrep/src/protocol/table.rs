//! The `lingrep.table/v1` contract: per-pattern counts read from a count
//! table written by `ling summarize --write-table`.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::SummaryGroupData;

pub const SCHEMA: &str = super::ContractId::Table.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<TableEnvelope>(
        super::ContractId::Table,
        &["table"],
        "Per-pattern counts under a --where clause, read from a count table.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "patterns",
                    events: &["pattern"],
                    min: 0,
                    max: None,
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
        "One pattern record per pattern in catalog order, then rows: patterns in catalog order; within a pattern, descending count, then the group order of `ling summarize`.",
        "Every pattern's totals under the clause, then every entry with a nonzero count under the clause, or the first --top of each pattern.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: TableEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong table schema"))
    }
}

impl TableEnvelope {
    #[must_use]
    pub fn new(event: TableEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TableEvent {
    Run(TableRun),
    Pattern(TablePattern),
    Row(TableRow),
    Complete(TableComplete),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableRun {
    pub table: String,
    /// The key the table was checked against: the artifact's analysis
    /// identity, the catalog fingerprint and the matcher revision.
    pub analysis_identity: String,
    pub catalog_fingerprint: String,
    pub matcher_revision: String,
    /// The `--where` clause, if any.
    pub clause: Option<String>,
    pub requested_top: Option<u64>,
}

/// One pattern's totals over all of its entries under the clause, whatever
/// `--top` keeps; 0 when the clause admits none of its occurrences.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TablePattern {
    pub pattern: String,
    pub occurrences: u64,
    /// Admitted documents with at least one occurrence of the pattern.
    pub works: u64,
}

/// One entry of one pattern under the clause.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableRow {
    pub pattern: String,
    pub rank: u64,
    /// Shaped as the `group` of a `lingrep.query/v2` summary record.
    pub group: SummaryGroupData,
    pub count: u64,
    /// Admitted documents with at least one occurrence.
    pub works: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableComplete {
    /// Admitted documents with at least one sentence.
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    pub tokens_admitted: u64,
    pub rows: u64,
    /// Time to answer from the opened table, excluding catalog compilation,
    /// table opening and clause resolution.
    pub query_millis: u64,
    pub elapsed_millis: u64,
}
