//! The `lingrep.contrast/v1` contract: each pattern's entries ranked by G²
//! between two document scopes, read from a count table written by
//! `ling summarize --write-table`.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::SummaryGroupData;

pub const SCHEMA: &str = super::ContractId::Contrast.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<ContrastEnvelope>(
        super::ContractId::Contrast,
        &["contrast"],
        "Each pattern's entries ranked by G², UCREL's two-cell log-likelihood, between the documents --where admits and those --versus admits, with each scope's counts, rates per million tokens and document reach, read from a count table.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "scopes",
                    events: &["scopes"],
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
        "One pattern record per pattern in catalog order, then rows: patterns in catalog order; within a pattern, descending g2 with null last, then descending where.count + versus.count, then the group order of `ling summarize`.",
        "Every entry with an occurrence in either scope is ranked; rows are the first --top of each pattern's ranking, or all of them. Every pattern is ranked before the first pattern record, which carries its entry count: a --top up to 1,048,576 holds that many rows per pattern in memory, and a larger --top, --top all, or JSON without --top holds every pattern's ranking in temporary files at once, up to 1,048,576 keys per pattern in memory, within --max-temp-bytes.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: ContrastEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong contrast schema"))
    }
}

impl ContrastEnvelope {
    #[must_use]
    pub fn new(event: ContrastEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ContrastEvent {
    Run(ContrastRun),
    Scopes(ContrastScopes),
    Pattern(ContrastPattern),
    Row(ContrastRow),
    Complete(ContrastComplete),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastRun {
    pub table: String,
    /// The key the table was checked against: the artifact's analysis
    /// identity, the catalog fingerprint and the matcher revision.
    pub analysis_identity: String,
    pub catalog_fingerprint: String,
    pub matcher_revision: String,
    /// The --where clause; `null` admits every document.
    #[serde(rename = "where")]
    pub where_clause: Option<String>,
    /// The --versus clause; `null` admits every document.
    pub versus: Option<String>,
    pub requested_top: Option<u64>,
}

/// Documents, sentences and tokens a scope admits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeAdmission {
    pub documents: u64,
    pub sentences: u64,
    pub tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRelation {
    /// No document is in both scopes.
    Disjoint,
    /// Some documents are in both scopes; G² is still computed over the
    /// scopes as given, so it is not a comparison of independent samples.
    Overlapping,
    /// Both scopes admit the same documents.
    Identical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastScopes {
    #[serde(rename = "where")]
    pub where_scope: ScopeAdmission,
    pub versus: ScopeAdmission,
    /// What both scopes admit.
    pub shared: ScopeAdmission,
    pub relation: ScopeRelation,
}

/// A count and the admitted documents holding at least one occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeCount {
    pub count: u64,
    pub works: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastPattern {
    pub pattern: String,
    /// Occurrences of every entry of the pattern in each scope.
    #[serde(rename = "where")]
    pub where_scope: ScopeCount,
    pub versus: ScopeCount,
    /// Entries with an occurrence in either scope: the length of the ranking.
    pub entries: u64,
}

/// One scope's side of a row.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastSide {
    pub count: u64,
    /// Admitted documents with at least one occurrence.
    pub works: u64,
    /// Occurrences per million admitted tokens; `null` when the scope admits
    /// no tokens.
    pub per_million: Option<f64>,
}

/// The scope whose rate is higher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Where,
    Versus,
    Equal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastRow {
    pub pattern: String,
    pub rank: u64,
    pub group: SummaryGroupData,
    #[serde(rename = "where")]
    pub where_scope: ContrastSide,
    pub versus: ContrastSide,
    /// Occurrences in documents both scopes admit, counted in both sides.
    pub shared: ScopeCount,
    /// G² = 2 Σ O·ln(O/E) over the two scopes, E_i = N_i·(a+b)/(N1+N2) with
    /// N_i the tokens scope i admits; a cell with O = 0 contributes 0. An
    /// exploratory score, not a significance test. `null` when either scope
    /// admits no tokens.
    pub g2: Option<f64>,
    /// The smaller expected count E_i; `null` with g2.
    pub min_expected: Option<f64>,
    /// log2((a/N1)/(b/N2)), unsmoothed; `null` when either count is 0.
    pub log2_ratio: Option<f64>,
    /// `null` with g2.
    pub direction: Option<Direction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ContrastComplete {
    pub rows: u64,
    /// Time to answer from the opened table, excluding catalog compilation,
    /// table opening and clause resolution.
    pub query_millis: u64,
    pub elapsed_millis: u64,
}
