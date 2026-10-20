use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = super::ContractId::Doctor.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<DoctorEnvelope>(
        super::ContractId::Doctor,
        &["doctor"],
        "Ordered runtime-readiness probe results.",
        StreamShape::Single {
            events: &["doctor"],
        },
        Channel::Stdout,
        "Dictionary, catalog, corpus, then binding probe order.",
        "All explicitly requested probes run independently; the binding probe \
         joins them when a catalog and a corpus are both supplied and both \
         admitted.",
        "A closed pipe is a machine-output failure.",
        "0 all probes pass, 1 any probe or output fails, 2 invalid invocation.",
        &["doctor/explicit-inputs"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DoctorEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: DoctorEvent,
}

impl DoctorEnvelope {
    #[must_use]
    pub fn new(data: DoctorData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: DoctorEvent::Doctor(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DoctorEvent {
    Doctor(DoctorData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DoctorData {
    pub ready: bool,
    pub probes: Vec<ProbeData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProbeData {
    pub name: String,
    pub ok: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProbeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ProbeEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    Dictionary,
    Catalog,
    Corpus,
    /// The catalog bound against the corpus's stored token schema — the
    /// `/v3` addition, and the reason for the version.
    Binding,
}

/// One referenced attribute the corpus's schema does not carry, named with
/// its referencing definitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnavailableBindingAttribute {
    pub attribute: String,
    /// Empty when only the catalog's publication policy references it.
    pub definitions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeEvidence {
    Dictionary {
        blake3: String,
    },
    Catalog {
        fingerprint: String,
        definition_count: u64,
        warning_count: u64,
    },
    Corpus {
        source_identity: String,
        analysis_identity: String,
        token_schema_identity: String,
        document_count: u64,
        sentence_count: u64,
        token_count: u64,
    },
    Binding {
        /// Referenced attributes the corpus cannot supply. The catalog still
        /// binds; positive tests over them match nothing there.
        unavailable: Vec<UnavailableBindingAttribute>,
        /// Keys whose absence refuses the bind outright — dependency `where`
        /// clause edges. Non-empty means the probe failed.
        required_missing: Vec<String>,
    },
}
