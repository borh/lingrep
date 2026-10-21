use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ContractId;

pub const SCHEMA: &str = ContractId::Dictionary.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<DictionaryEnvelope>(
        ContractId::Dictionary,
        &["dictionary install"],
        "Installed dictionary path, size, digest and license.",
        StreamShape::Single {
            events: &["installed"],
        },
        Channel::Stdout,
        "One record after the dictionary is in place.",
        "Only a dictionary whose BLAKE3 matched the pinned digest.",
        "A machine output failure after installation leaves the installed file in place.",
        "0 installed or already present, 1 download, digest or filesystem failure, 2 invalid invocation.",
        &[],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DictionaryEnvelope {
    pub schema: String,
    #[serde(flatten)]
    pub event: DictionaryEvent,
}

impl DictionaryEnvelope {
    pub fn new(data: InstalledData) -> Self {
        Self {
            schema: SCHEMA.into(),
            event: DictionaryEvent::Installed(data),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DictionaryEvent {
    Installed(InstalledData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InstalledData {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub blake3: String,
    pub license: String,
    /// The file was already installed with the pinned digest, so nothing was
    /// downloaded.
    pub already_present: bool,
    /// The installed file is the one `ling` loads when neither `--dict-path`
    /// nor `LINGREP_DICTIONARY_PATH` names a dictionary.
    pub default: bool,
}
