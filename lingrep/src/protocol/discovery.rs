use clap::ValueEnum;
use clap::builder::PossibleValue;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

macro_rules! contract_ids {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum ContractId {
            $($variant),+
        }

        impl ContractId {
            pub const ALL: &'static [Self; contract_ids!(@count $($variant),+)] = &[
                $(Self::$variant),+
            ];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl ValueEnum for ContractId {
            fn value_variants<'a>() -> &'a [Self] {
                Self::ALL
            }

            fn to_possible_value(&self) -> Option<PossibleValue> {
                Some(PossibleValue::new(self.as_str()))
            }
        }
    };
    (@count $($variant:ident),+) => {
        <[()]>::len(&[$(contract_ids!(@unit $variant)),+])
    };
    (@unit $variant:ident) => { () };
}

contract_ids! {
    Attributes => "lingrep.attributes/v1",
    AttributesTsv => "lingrep.attributes-tsv/v1",
    Capabilities => "lingrep.capabilities/v1",
    Check => "lingrep.check/v1",
    Completions => "lingrep.completions/v1",
    Diagnostic => "lingrep.diagnostic/v1",
    Dictionary => "lingrep.dictionary/v1",
    Doctor => "lingrep.doctor/v1",
    Invocation => "lingrep.invocation/v1",
    Manpages => "lingrep.manpages/v1",
    MatchesTsv => "lingrep.matches-tsv/v1",
    ProtocolContract => "lingrep.protocol-contract/v1",
    Query => "lingrep.query/v2",
    Sources => "lingrep.sources/v1",
    SourcesTsv => "lingrep.sources-tsv/v1",
    Status => "lingrep.status/v1",
    SummarizeTsv => "lingrep.summarize-tsv/v1",
    Tokenize => "lingrep.tokenize/v1",
    TokenizeTsv => "lingrep.tokenize-tsv/v1",
    Variants => "lingrep.variants/v1",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    Jsonl,
    Tsv,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Stdout,
    Stderr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractSummary<'a> {
    pub id: ContractId,
    pub kind: ContractKind,
    pub owners: &'a [&'a str],
    pub summary: &'a str,
}

pub enum ContractSpec {
    Jsonl(JsonContractSpec),
    Tsv(TsvContractSpec),
}

pub struct JsonContractSpec {
    pub summary: ContractSummary<'static>,
    pub schema: fn(ContractId) -> Result<serde_json::Value, ContractBuildError>,
    pub stream: StreamShape,
    pub channel: Channel,
    pub ordering: &'static str,
    pub coverage: &'static str,
    pub broken_pipe: &'static str,
    pub exit_status: &'static str,
    pub example_ids: &'static [&'static str],
}

pub struct TsvContractSpec {
    pub summary: ContractSummary<'static>,
    pub columns: &'static [ColumnSpec],
    pub header: HeaderPolicy,
    pub escaping: &'static str,
    pub ordering: &'static str,
    pub coverage: &'static str,
    pub broken_pipe: &'static str,
    pub exit_status: &'static str,
    pub example_ids: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HeaderPolicy {
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamPhase {
    pub name: &'static str,
    pub events: &'static [&'static str],
    pub min: u64,
    pub max: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamShape {
    Single { events: &'static [&'static str] },
    Repeated { events: &'static [&'static str] },
    Phased { phases: &'static [StreamPhase] },
}

#[derive(Debug, Error)]
pub enum ContractBuildError {
    #[error("failed to serialize derived JSON Schema: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("derived schema for {0:?} has no unique top-level schema property")]
    MissingSchemaProperty(ContractId),
}

pub fn envelope_schema<T: JsonSchema>(
    id: ContractId,
) -> Result<serde_json::Value, ContractBuildError> {
    let mut schema = serde_json::to_value(schemars::schema_for!(T))?;
    let property = schema
        .pointer_mut("/properties/schema")
        .ok_or(ContractBuildError::MissingSchemaProperty(id))?;
    *property = serde_json::json!({"type": "string", "const": id.as_str()});
    Ok(schema)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn json_contract<T: JsonSchema>(
    id: ContractId,
    owners: &'static [&'static str],
    summary: &'static str,
    stream: StreamShape,
    channel: Channel,
    ordering: &'static str,
    coverage: &'static str,
    broken_pipe: &'static str,
    exit_status: &'static str,
    example_ids: &'static [&'static str],
) -> ContractSpec {
    ContractSpec::Jsonl(JsonContractSpec {
        summary: ContractSummary {
            id,
            kind: ContractKind::Jsonl,
            owners,
            summary,
        },
        schema: envelope_schema::<T>,
        stream,
        channel,
        ordering,
        coverage,
        broken_pipe,
        exit_status,
        example_ids,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn tsv_contract(
    id: ContractId,
    owners: &'static [&'static str],
    summary: &'static str,
    columns: &'static [ColumnSpec],
    ordering: &'static str,
    coverage: &'static str,
    example_ids: &'static [&'static str],
) -> ContractSpec {
    ContractSpec::Tsv(TsvContractSpec {
        summary: ContractSummary {
            id,
            kind: ContractKind::Tsv,
            owners,
            summary,
        },
        columns,
        header: HeaderPolicy::None,
        escaping: "UTF-8, LF-terminated, RFC-4180-style quoted fields",
        ordering,
        coverage,
        broken_pipe: "A closed pipe is a machine-output failure.",
        exit_status: "0 complete, 1 runtime/output failure, 2 invocation failure.",
        example_ids,
    })
}

impl ContractSpec {
    pub fn id(&self) -> ContractId {
        self.summary().id
    }

    pub fn summary(&self) -> ContractSummary<'static> {
        match self {
            Self::Jsonl(spec) => spec.summary,
            Self::Tsv(spec) => spec.summary,
        }
    }

    pub fn stream(&self) -> Option<&StreamShape> {
        match self {
            Self::Jsonl(spec) => Some(&spec.stream),
            Self::Tsv(_) => None,
        }
    }

    pub fn to_detail(&self) -> Result<super::contract::ContractDetailData, ContractBuildError> {
        Ok(match self {
            Self::Jsonl(spec) => super::contract::ContractDetailData {
                id: spec.summary.id.as_str().into(),
                kind: spec.summary.kind,
                owners: strings(spec.summary.owners),
                summary: spec.summary.summary.into(),
                channel: Some(spec.channel),
                json_schema: Some((spec.schema)(spec.summary.id)?),
                stream: Some(super::contract::StreamShapeData::from(&spec.stream)),
                tsv: None,
                ordering: spec.ordering.into(),
                coverage: spec.coverage.into(),
                broken_pipe: spec.broken_pipe.into(),
                exit_status: spec.exit_status.into(),
                example_ids: strings(spec.example_ids),
            },
            Self::Tsv(spec) => super::contract::ContractDetailData {
                id: spec.summary.id.as_str().into(),
                kind: spec.summary.kind,
                owners: strings(spec.summary.owners),
                summary: spec.summary.summary.into(),
                channel: Some(Channel::Stdout),
                json_schema: None,
                stream: None,
                tsv: Some(super::contract::TsvLayoutData {
                    columns: spec
                        .columns
                        .iter()
                        .map(|column| super::contract::ColumnData {
                            name: column.name.into(),
                            description: column.description.into(),
                        })
                        .collect(),
                    header: spec.header,
                    escaping: spec.escaping.into(),
                }),
                ordering: spec.ordering.into(),
                coverage: spec.coverage.into(),
                broken_pipe: spec.broken_pipe.into(),
                exit_status: spec.exit_status.into(),
                example_ids: strings(spec.example_ids),
            },
        })
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}
