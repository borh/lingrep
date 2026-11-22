//! The `lingrep.diff/v1` contract: the occurrences two `ling matches` runs
//! over one corpus share, and those only one of them has.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ContractId;

pub const SCHEMA: &str = ContractId::Diff.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<DiffEnvelope>(
        ContractId::Diff,
        &["diff"],
        "Occurrences shared by two `ling matches` runs over one corpus, and those only one run has.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "definitions",
                    events: &["definition", "occurrence"],
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
        "Definition pairs by name; each pair's occurrences follow it in corpus order, A-only before B-only.",
        "The sentences both runs scanned, stated in the run event; at most --examples occurrences per pair and side.",
        "A closed pipe is a machine-output failure.",
        "0 compared, with or without differences; 1 unreadable file or output failure; 2 invocation failure or runs that cannot be compared.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: DiffEvent,
}

impl DiffEnvelope {
    #[must_use]
    pub fn new(event: DiffEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DiffEvent {
    Run(DiffRun),
    Definition(DefinitionDiff),
    Occurrence(Occurrence),
    Complete(DiffComplete),
}

/// What identifies one occurrence within its sentence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// The match's token span.
    Span,
    /// The tokens each compared capture or graph binding occupies.
    Roles,
    /// The published record: span, captures, graph bindings,
    /// interpretations, lexical identity, unit and key.
    Record,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffRun {
    pub level: Level,
    pub a: RunSide,
    pub b: RunSide,
    pub corpus_kind: String,
    pub source_identity: Option<String>,
    pub analysis_identity: String,
    /// The `--where` scope both runs used, as their streams record it.
    pub scope: Option<serde_json::Value>,
    /// The `--sample` selection both runs used, as their streams record it.
    pub sample: Option<serde_json::Value>,
    pub coverage: Coverage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunSide {
    pub run_id: String,
    pub catalog_fingerprint: String,
}

/// The sentences in which occurrences were compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Coverage {
    /// Every sentence both runs admitted.
    Full { sentences_scanned: u64 },
    /// A run stopped at `--limit`: only sentences whose corpus ordinal is
    /// below this bound were compared.
    Prefix { before_corpus_sentence: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DefinitionDiff {
    pub a: String,
    pub b: String,
    /// Role names compared at the `roles` level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub shared: u64,
    pub a_only: u64,
    pub b_only: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    AOnly,
    BOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Occurrence {
    pub a: String,
    pub b: String,
    pub status: Side,
    /// Records of the side that has the occurrence.
    pub records: u64,
    pub source_name: String,
    pub document_ordinal: u64,
    pub document_sentence_ordinal: u64,
    pub corpus_sentence_ordinal: Option<u64>,
    pub span: Span,
    /// The compared identity within the sentence, as text.
    pub unit: String,
    pub surface: String,
    /// The sentence text, when the run was made with `--evidence`.
    pub sentence: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffComplete {
    pub definitions_compared: u64,
    pub definitions_differing: u64,
    pub shared: u64,
    pub a_only: u64,
    pub b_only: u64,
    /// Patterns only run A defines, with their record counts in the compared
    /// sentences. Empty when two definitions were named.
    pub a_only_definitions: Vec<UnpairedDefinition>,
    pub b_only_definitions: Vec<UnpairedDefinition>,
    /// A difference was found in the compared sentences. `false` describes
    /// those sentences at this level only.
    pub differences_found: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnpairedDefinition {
    pub name: String,
    pub records: u64,
}
