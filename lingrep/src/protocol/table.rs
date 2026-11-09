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
        "Per-pattern counts under a --where clause, or with --entry (JSON only) one entry's counts per document-metadata value as metadata_value records, read from a count table.",
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
                    name: "metadata_values",
                    events: &["metadata_value"],
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
        "One pattern record per pattern in catalog order, then rows: patterns in catalog order; within a pattern, descending count, then the group order of `ling summarize`. Under --entry, the records between run and complete are metadata_value records, in the value order of `ling summarize --by-metadata`.",
        "Every pattern's totals under the clause, then every entry with a nonzero count under the clause, or the first --top of each pattern. Under --entry, one record per value with at least one admitted document; a final value: null record appears when admitted documents have no value.",
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
    MetadataValue(TableMetadataValue),
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
    pub clause: Option<String>,
    pub requested_top: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_metadata: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableRow {
    pub pattern: String,
    pub rank: u64,
    pub group: SummaryGroupData,
    pub count: u64,
    /// Admitted documents with at least one occurrence.
    pub works: u64,
}

/// One value of the `--by-metadata` key, shaped as a `lingrep.query/v2`
/// `metadata_value` record whose pattern counts are the one `--entry`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableMetadataValue {
    pub key: String,
    /// The value as `--where` compares it; `null` for documents without the
    /// key or with an empty list.
    pub value: Option<String>,
    /// Admitted documents with this value.
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    pub tokens_admitted: u64,
    pub entry: TableEntryCount,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableEntryCount {
    pub pattern: String,
    pub group: SummaryGroupData,
    pub count: u64,
    /// Admitted documents with this value and at least one occurrence.
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
