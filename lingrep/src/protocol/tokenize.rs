use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::{
    AnalyzerRun, CompleteCorpus, Coverage, ExecutionLimits, InventoryStats, ResourceHighWater,
    RunCorpus, TemporaryStorageLimits, Termination, TokenEvidence, TraversalLimits,
};

pub const SCHEMA: &str = super::ContractId::Tokenize.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<TokenizeEnvelope>(
        super::ContractId::Tokenize,
        &["tokenize"],
        "Tokenization run, sentence records, and completion evidence.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "sentences",
                    events: &["sentence"],
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
        "Canonical corpus sentence order.",
        "Complete evidence unless termination states otherwise.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure, 128+signal on cancellation.",
        &["tokenize/direct-root"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TokenizeEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: TokenizeEvent,
}
fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong tokenize schema"))
    }
}
impl TokenizeEnvelope {
    pub fn new(event: TokenizeEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TokenizeEvent {
    Run(Box<TokenizeRun>),
    Sentence(SentenceData),
    Complete(TokenizeComplete),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TokenizeRun {
    pub run_id: String,
    pub analyzer: AnalyzerRun,
    #[serde(flatten)]
    pub corpus: RunCorpus,
    pub dictionary_identity: String,
    pub reader_identity: String,
    pub token_schema_identity: String,
    pub execution_limits: ExecutionLimits,
    pub temporary_storage_limits: TemporaryStorageLimits,
    pub traversal_limits: TraversalLimits,
    pub worker_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SentenceData {
    pub corpus_sentence_ordinal: u64,
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
    pub document_ordinal: u64,
    pub document_sentence_ordinal: u64,
    pub tokens: Vec<TokenEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TokenizeComplete {
    pub coverage: Coverage,
    #[serde(flatten)]
    pub corpus: CompleteCorpus,
    pub sentences_emitted: u64,
    pub tokens_emitted: u64,
    pub elapsed_millis: u64,
    pub resources: ResourceHighWater,
    pub inventory: Option<InventoryStats>,
    pub termination: Termination,
}
