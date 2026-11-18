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
        Err(D::Error::custom("wrong disk schema"))
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
    /// Apparent size of the files under `path`; absent when it could not be
    /// measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
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
    /// For a superseded or older-format realization that the default
    /// `corpus prune` keeps, why it keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by_prune: Option<PruneKeep>,
    /// Why an unreadable entry could not be opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What a realization is, relative to the analysis inputs of the run.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RealizationStatus {
    /// Readable, and the realization `corpus build` would reuse for its
    /// source under the given analysis inputs.
    Current,
    /// Readable, recording the analysis identity and trust its name records,
    /// and not current.
    Superseded,
    /// Readable; no analysis inputs were given, so neither current nor
    /// superseded.
    Unclassified,
    /// Written in an earlier artifact generation, manifest schema or
    /// provenance schema than this build reads.
    OlderFormat,
    /// Cannot be opened for another reason: a newer format, or damage.
    Unreadable,
    /// Readable, but its name records a different analysis identity or trust
    /// than its manifest.
    Misnamed,
    /// A symbolic link with a canonical artifact name.
    SymbolicLink,
    /// A `.lingcorpus` directory whose name is not a canonical artifact name,
    /// such as an explicit `--output`.
    NonCanonicalName,
}

/// Why `corpus prune` keeps a superseded or older-format realization by
/// default.
///
/// The default prune deletes a superseded realization of the current source
/// content that differs from the current request in `analyzer` or
/// `token_schema`, and an older-format realization that records the current
/// source identity.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PruneKeep {
    /// Differs in reader or admission policy and in no analysis dimension,
    /// such as the artifact lingrep-web reads a Markdown source into.
    /// Deleted with `--include-all-superseded`.
    ReaderVariant,
    /// Differs in source content, whatever else differs: an analysis of an
    /// earlier version of the source, which no rebuild of the current source
    /// reproduces. Deleted with `--include-all-superseded`.
    SourceRevision,
    /// Not known to analyze the current source content: the source is
    /// missing, or an older-format manifest records no source identity equal
    /// to the current one. Deleted with `--include-all-superseded`.
    SourceUnknown,
    /// Has another trust class. `corpus build` does not produce it, so no
    /// local rebuild replaces it; `corpus prune` never deletes it.
    OtherTrust,
    /// Differs from the current request in no dimension, yet `corpus build`
    /// would not select it. `corpus prune` never deletes it.
    IdenticalToCurrent,
    /// Holds an entry that is not one of the components its manifest lists,
    /// a component that is not a regular file, or an entry on another
    /// filesystem, or is itself a mount point. `corpus prune` never deletes
    /// it.
    ForeignEntries,
    /// Its manifest is readable but it fails the structural validation
    /// `corpus inspect` runs; the realization is reported as `unreadable`.
    /// `corpus prune` never deletes it.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
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
    /// The header cannot be read, for example a different table version.
    Unreadable,
}

/// A staging directory or lock file left by a build that did not publish, or
/// a realization that `corpus prune` renamed for deletion and did not finish
/// deleting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskLeftover {
    /// The name the staging directory would have been published under, or
    /// the name the pruned realization had.
    pub published_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staging: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
    /// The hidden directory of an `interrupted_prune`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruning: Option<String>,
    /// For an `interrupted_prune` that `corpus prune` keeps, why it keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_by_prune: Option<PruneKeep>,
    pub status: LeftoverStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
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
    /// A realization `corpus prune --execute` renamed to a hidden name and
    /// did not finish deleting; `corpus build` does not read it, and the next
    /// `corpus prune --execute` deletes it.
    InterruptedPrune,
}

/// One cache entry, reported only when requested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskCache {
    pub kind: CacheKind,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Files counted in `bytes`.
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
    /// Count and bytes for each item kind and status present, ordered by
    /// kind, then status.
    pub totals: Vec<DiskTotal>,
    /// Sum of every measured item.
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiskTotal {
    pub item: DiskItem,
    /// The item's status or kind, as its own records spell it.
    pub status: String,
    pub count: u64,
    pub bytes: u64,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DiskItem {
    Realization,
    Table,
    Leftover,
    Cache,
}
