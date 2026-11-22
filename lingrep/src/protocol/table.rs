//! The `lingrep.table/v1` contract: per-pattern counts read from a count
//! table written by `ling summarize --write-table`.

use serde::{Deserialize, Serialize};

use super::query::{DocumentData, SummaryGroupData};

pub const SCHEMA: &str = super::ContractId::Table.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<TableEnvelope>(
        super::ContractId::Table,
        &["table"],
        "Per-pattern counts under a --where clause, or with --entry (JSON only) one entry's counts per document-metadata value as metadata_value records, or with --by-document the full sparse document-by-entry matrix, read from a count table.",
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
                    name: "subset",
                    events: &["subset"],
                    min: 0,
                    max: Some(1),
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
                    name: "documents",
                    events: &["document"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "entries",
                    events: &["entry"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "cells",
                    events: &["cell"],
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
        "One pattern record per pattern in catalog order (only the --select pattern under --select), then under --key one subset record, then rows: patterns in catalog order; within a pattern, descending count, then the group order of `ling summarize`. Under --entry, the records between run and complete are metadata_value records, in the value order of `ling summarize --by-metadata`. Under --by-document: admitted documents in corpus order, every entry in table order, then nonzero cells in entry order and ascending stable document ID.",
        "Every pattern's totals under the clause, then every entry (under --key, every entry whose key has the given field values, with its share of the subset) with a nonzero count under the clause, or the first --top of each pattern. Under --entry, one record per value with at least one admitted document; a final value: null record appears when admitted documents have no value. Under --by-document, documents include zero-count works, the entry dictionary covers the entire table even under empty scopes, and every admitted nonzero cell is emitted. Only a complete record confirms the matrix is complete; rows counts cells and entries counts dictionary records.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &[],
    )
}

define_protocol_envelope!(
    pub struct TableEnvelope(TableEvent),
    schema = SCHEMA,
    error = "wrong table schema",
);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TableEvent {
    Run(TableRun),
    Pattern(TablePattern),
    Subset(TableSubset),
    Row(TableRow),
    MetadataValue(TableMetadataValue),
    Document(TableDocument),
    Entry(TableEntry),
    Cell(TableCell),
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub by_document: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published_source_metadata: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<TableKeyEquals>,
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

/// One `--key FIELD=VALUE`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableKeyEquals {
    pub name: String,
    pub value: String,
}

/// The entries of the `--select` pattern whose key fields equal every
/// `--key` value, summed under the clause before `--top`. A subset that
/// admits nothing is 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableSubset {
    pub pattern: String,
    pub key: Vec<TableKeyEquals>,
    pub occurrences: u64,
    /// Admitted documents with at least one occurrence of the subset.
    pub works: u64,
    /// Entries of the subset with a nonzero count.
    pub entries: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableRow {
    pub pattern: String,
    /// Under --key, the rank within the subset.
    pub rank: u64,
    pub group: SummaryGroupData,
    pub count: u64,
    /// Admitted documents with at least one occurrence.
    pub works: u64,
    /// Under --key, `count` divided by the subset's `occurrences`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<f64>,
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
pub struct TableDocument {
    /// Stable ID used by cells, bound to the run's corpus identity.
    pub document_id: u64,
    #[serde(flatten)]
    pub data: DocumentData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableEntry {
    /// Stable within this count table; unchanged by document scopes.
    pub entry: u32,
    pub pattern: String,
    pub group: SummaryGroupData,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableCell {
    pub document_id: u64,
    pub entry: u32,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TableComplete {
    /// Admitted documents with at least one sentence.
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    pub tokens_admitted: u64,
    pub rows: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<u64>,
    /// Time to answer from the opened table, excluding catalog compilation,
    /// table opening and clause resolution.
    pub query_millis: u64,
    pub elapsed_millis: u64,
}
