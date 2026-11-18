//! The `lingrep.disk/v1` contract: disk use of prepared artifacts in one
//! directory.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::variants::VariantDifference;

pub const SCHEMA: &str = super::ContractId::Disk.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<DiskEnvelope>(
        super::ContractId::Disk,
        &["corpus disk"],
        "Sizes and status of prepared artifacts, count tables, staging leftovers and caches.",
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
                    name: "realizations",
                    events: &["realization"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "tables",
                    events: &["table"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "leftovers",
                    events: &["leftover"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "caches",
                    events: &["cache"],
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
        "Bytewise path order within each phase; sources by name.",
        "Entries immediately in the scanned directory at run time, plus requested caches.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure.",
        &["corpus disk/root"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: DiskEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("expected the lingrep.disk/v1 schema"))
    }
}

impl DiskEnvelope {
    #[must_use]
    pub fn new(event: DiskEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum DiskEvent {
    Run(DiskRun),
    Source(DiskSource),
    Realization(DiskRealization),
    Table(DiskTable),
    Leftover(DiskLeftover),
    Cache(DiskCache),
    Complete(DiskComplete),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskRun {
    pub run_id: String,
    /// Directory scanned, non-recursively.
    pub directory: String,
    /// The source whose items are reported; absent when every source in the
    /// directory is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Whether realizations were classified as current or superseded, which
    /// requires the analysis inputs `corpus build` takes.
    pub current_classified: bool,
}

/// One source with realizations in the scanned directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskSource {
    /// The source's file name, as its realizations' names record it.
    pub name: String,
    /// The path `corpus build` would be given to select the current
    /// realization.
    pub path: String,
    pub current: SourceCurrent,
}

/// Which realization `corpus build` would select for a source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceCurrent {
    /// No analysis inputs were given.
    NotClassified,
    /// `corpus build` would reuse the realization at `path`.
    Present { path: String },
    /// `corpus build` would publish a new realization at `path`.
    Absent { path: String },
    /// The source is not present, so nothing can be selected for it.
    SourceMissing,
}

/// One entry that is, or is named like, a prepared artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskRealization {
    pub path: String,
    /// The source the entry's name records; absent for a non-canonical name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub status: RealizationStatus,
    /// Sum of file lengths under `path`, counting hard-linked files in each entry.
    /// Omitted if the scan could not measure the size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
    /// Recorded in a readable manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_generation: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyzer_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_identity: Option<String>,
    /// For a superseded realization, the dimensions on which it differs from
    /// the current request, in a fixed order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub differences: Vec<VariantDifference>,
    /// The reason the default prune retains this realization, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by_prune: Option<PruneKeep>,
    /// The error returned when opening the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RealizationStatus {
    /// Readable, and the realization `corpus build` would reuse for its
    /// source under the given analysis inputs.
    Current,
    /// A readable realization with a matching name and manifest that the
    /// current analysis request would skip.
    Superseded,
    /// Readable; no analysis inputs were given, so neither current nor
    /// superseded.
    Unclassified,
    /// Written in an earlier artifact generation, manifest schema or
    /// provenance schema than this build reads.
    OlderFormat,
    /// A later artifact generation, manifest schema, provenance schema, or
    /// an unsupported manifest field.
    NewerFormat,
    /// Cannot be opened for any other reason: its manifest or a component
    /// fails the validation `corpus inspect` runs.
    Damaged,
    /// Readable, but its name records a different analysis identity or trust
    /// than its manifest.
    Misnamed,
    /// A symbolic link with a canonical artifact name.
    SymbolicLink,
    /// A `.lingcorpus` directory whose name is not a canonical artifact name,
    /// such as an explicit `--output`.
    NonCanonicalName,
}

/// Reasons the default prune excludes an entry from removal.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PruneKeep {
    /// Differs in reader or admission policy and in no analysis dimension,
    /// such as the artifact lingrep-web reads a Markdown source into.
    /// Deleted with `--include-all-superseded`.
    ReaderVariant,
    /// An earlier source revision, eligible with `--include-all-superseded`.
    SourceRevision,
    /// Not known to analyze the current source content: the source is
    /// missing, or an older-format manifest records no source identity equal
    /// to the current one. Deleted with `--include-all-superseded`.
    SourceUnknown,
    /// A different trust class, excluded from removal.
    OtherTrust,
    /// Differs from the current request in no dimension, yet `corpus build`
    /// would not select it. `corpus prune` never deletes it.
    IdenticalToCurrent,
    /// An unexpected entry, a non-regular component, an entry on another
    /// filesystem, or a mount point. These entries are excluded from removal.
    ForeignEntries,
    /// A realization retained because a protected table would otherwise
    /// have no bound artifact in the scanned directory.
    KeptTable,
    /// The realization is `damaged`. `corpus prune` never deletes it.
    Damaged,
}

impl PruneKeep {
    /// Whether `--include-all-superseded` deletes a realization kept for this
    /// reason.
    #[must_use]
    pub fn deleted_with_include_all_superseded(self) -> bool {
        matches!(
            self,
            Self::ReaderVariant | Self::SourceRevision | Self::SourceUnknown
        )
    }
}

/// One count table immediately in the scanned directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskTable {
    pub path: String,
    /// Apparent size of the files under `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
    /// The analysis identity the table's header binds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_identity: Option<String>,
    pub binding: TableBinding,
    /// Entries in the scanned directory, whatever their names, that record
    /// the bound analysis identity in a readable manifest, or are unreadable
    /// canonical names other than an older format that record it in their
    /// name. A symbolic link is listed by its own path.
    pub artifacts: Vec<String>,
    /// `foreign_entries` for a superseded table excluded from removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by_prune: Option<PruneKeep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The state of the artifacts a count table binds. A table whose binding is
/// `superseded` or `absent` is orphaned.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TableBinding {
    /// A bound artifact is current.
    Current,
    /// A bound artifact is present and is neither current nor superseded.
    Present,
    /// Every bound artifact is superseded.
    Superseded,
    /// No artifact in the scanned directory records the bound identity; one
    /// may exist elsewhere.
    Absent,
    /// The table header could not be opened.
    Unreadable,
}

/// A staging directory or lock file left by a build that did not publish, or
/// a realization or count table that `corpus prune` renamed for deletion and
/// did not finish deleting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskLeftover {
    /// The name the staging directory would have been published under, or
    /// the name the pruned realization or count table had.
    pub published_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// The hidden directory of an `interrupted_prune`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruning: Option<String>,
    /// Reason an interrupted removal is excluded from cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by_prune: Option<PruneKeep>,
    pub status: LeftoverStatus,
    /// Apparent size of the files under the reported paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LeftoverStatus {
    /// A process holds the lock.
    Live,
    /// The lock records this host and is free: its build has stopped, and the
    /// next build of the same name on this host removes it.
    Abandoned,
    /// No lock file, or one recording another or no host name.
    Unknown,
    /// A realization or count table left under its hidden removal name.
    /// An eligible leftover is removed on a subsequent execute run.
    InterruptedPrune,
}

/// One cache entry, reported only when requested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskCache {
    pub kind: CacheKind,
    pub path: String,
    /// Apparent size of the files counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apparent_bytes: Option<u64>,
    /// Files counted in `apparent_bytes`.
    pub files: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_seconds: Option<u64>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CacheKind {
    /// A decompressed copy of a `.dic.zst` dictionary, named by a hash of the
    /// compressed file's metadata.
    DictionaryCopy,
    /// Every other file in the dictionary cache directory, such as validation
    /// markers, counted together.
    DictionaryCacheOther,
    /// An analyzer directory named on the command line.
    Analyzer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskComplete {
    /// Count and apparent size for each item kind and status present,
    /// ordered by item, then by status in the order the status types list
    /// them.
    pub totals: Vec<DiskTotal>,
    /// Sum of every measured item's apparent size.
    pub apparent_bytes: u64,
}

/// The items of one kind and status, by the field their own records carry
/// it in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum DiskTotal {
    Realization {
        status: RealizationStatus,
        count: u64,
        apparent_bytes: u64,
    },
    Table {
        binding: TableBinding,
        count: u64,
        apparent_bytes: u64,
    },
    Leftover {
        status: LeftoverStatus,
        count: u64,
        apparent_bytes: u64,
    },
    Cache {
        kind: CacheKind,
        count: u64,
        apparent_bytes: u64,
    },
}
