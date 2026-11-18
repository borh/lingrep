//! Directory inventory for `corpus disk` and `corpus prune`.
//! Classification reads each source to derive its identity without decoding token blocks.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lingrep_corpus::{
    AnalysisIdentity, BuildOutcome, CorpusArtifact, CorpusError, CountTable, CountTableError,
    PruneLeftoverKind, Realization, RealizationRequest, SiblingRealization, SiblingScan,
    SourceIdentity, StagingStatus, TrustClass, artifact_bytes, foreign_entries, is_newer_format,
    is_older_format, parse_artifact_name, prune_leftovers, recorded_source_identity,
    staging_leftovers,
};

use super::CommandOutcome;
use super::corpus::{
    BuildInputs, BuildRequest, canonical_destination, observe_root, resolve_build_inputs,
};
use crate::cli::{CorpusDiskArgs, CurrentInputArgs, ExecutionCliConfig};
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::presentation::{OutputMode, ResultDelivery};
use crate::protocol::disk::{
    CacheKind, DiskCache, DiskComplete, DiskEnvelope, DiskEvent, DiskLeftover, DiskRealization,
    DiskRun, DiskSource, DiskTable, DiskTotal, LeftoverStatus, PruneKeep, RealizationStatus,
    SourceCurrent, TableBinding,
};
use crate::protocol::variants::VariantDifference;
use crate::protocol::write_jsonl;
use crate::terminal::{PresentationProgress, TerminalArbiter};

/// What a scan covers: one directory, optionally narrowed to one source.
pub(crate) struct Scope {
    pub directory: PathBuf,
    /// The source as spelled on the command line, and its file name.
    pub source: Option<(PathBuf, String)>,
}

impl Scope {
    pub(crate) fn new(directory: Option<&Path>, source: Option<&Path>) -> Result<Self, CliError> {
        if let Some(source) = source {
            // `.`, `..` and `/` have no file name; `corpus build` resolves them
            // before naming an artifact, and so does this.
            let named = if source.file_name().is_some() {
                source.to_path_buf()
            } else {
                fs::canonicalize(source).map_err(|error| CliError::InputIo {
                    path: source.to_path_buf(),
                    source: error,
                })?
            };
            let name = named
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    CliError::InvalidArgs(format!(
                        "{} has no UTF-8 file name to match artifact names against",
                        source.display()
                    ))
                })?
                .to_owned();
            return Ok(Self {
                directory: lingrep_corpus::containing_directory(&named).to_path_buf(),
                source: Some((named, name)),
            });
        }
        let directory =
            directory.ok_or_else(|| CliError::InvalidArgs("pass DIR or --source SOURCE".into()))?;
        // Without trailing separators, so that `link/` names the link
        // itself rather than the directory it points to.
        let named = directory.components().collect::<PathBuf>();
        match fs::symlink_metadata(&named) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CliError::InvalidArgs(format!(
                    "{} is a symbolic link; pass the directory it points to",
                    directory.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CliError::InvalidArgs(format!(
                    "{} is not a directory; pass --source to name one source",
                    directory.display()
                )));
            }
            _ => {}
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            source: None,
        })
    }

    fn includes(&self, source_name: &str) -> bool {
        self.source
            .as_ref()
            .is_none_or(|(_, name)| name == source_name)
    }

    fn source_path(&self, source_name: &str) -> PathBuf {
        match &self.source {
            Some((path, _)) => path.clone(),
            None => self.directory.join(source_name),
        }
    }
}

/// What `corpus build` asked of the scan for one source.
pub(crate) struct CurrentRequest {
    source_identity: SourceIdentity,
    inputs: BuildInputs,
}

pub(crate) enum CurrentSelection {
    NotClassified,
    Present(PathBuf),
    Absent(PathBuf),
    SourceMissing,
}

pub(crate) struct SourceEntry {
    pub name: String,
    pub path: PathBuf,
    pub current: CurrentSelection,
    request: Option<CurrentRequest>,
}

impl SourceEntry {
    pub(crate) fn record(&self) -> DiskSource {
        DiskSource {
            name: self.name.clone(),
            path: display(&self.path),
            current: match &self.current {
                CurrentSelection::NotClassified => SourceCurrent::NotClassified,
                CurrentSelection::Present(path) => SourceCurrent::Present {
                    path: display(path),
                },
                CurrentSelection::Absent(path) => SourceCurrent::Absent {
                    path: display(path),
                },
                CurrentSelection::SourceMissing => SourceCurrent::SourceMissing,
            },
        }
    }
}

pub(crate) struct RealizationEntry {
    pub path: PathBuf,
    pub source: Option<String>,
    pub status: RealizationStatus,
    pub bytes: Option<u64>,
    pub recorded: Option<SiblingRealization>,
    pub differences: Vec<VariantDifference>,
    /// For a superseded or older-format realization classified against the
    /// analysis inputs, why the default prune keeps it.
    pub kept_by_prune: Option<PruneKeep>,
    /// For a deletion candidate, its [`foreign_entries`].
    pub foreign: Vec<String>,
    pub message: Option<String>,
}

pub(crate) struct TableEntry {
    pub path: PathBuf,
    pub bytes: Option<u64>,
    pub analysis_identity: Option<String>,
    pub binding: TableBinding,
    pub artifacts: Vec<PathBuf>,
    /// For a `superseded` table, why `corpus prune` keeps it.
    pub kept_by_prune: Option<PruneKeep>,
    /// For a `superseded` table, its [`CountTable::foreign_entries`].
    pub foreign: Vec<String>,
    pub message: Option<String>,
}

pub(crate) struct LeftoverEntry {
    pub published_name: String,
    pub staging: Option<PathBuf>,
    pub lock: Option<PathBuf>,
    pub pruning: Option<PathBuf>,
    pub status: LeftoverStatus,
    pub bytes: Option<u64>,
    /// For an interrupted prune, why `corpus prune` keeps it.
    pub kept_by_prune: Option<PruneKeep>,
    /// For an interrupted prune, its [`foreign_entries`].
    pub foreign: Vec<String>,
}

/// Everything one scan found, classified.
pub(crate) struct Scan {
    pub classified: bool,
    pub sources: Vec<SourceEntry>,
    pub realizations: Vec<RealizationEntry>,
    pub tables: Vec<TableEntry>,
    pub leftovers: Vec<LeftoverEntry>,
}

/// Scans `scope`. With `inputs`, selects each source's current realization
/// exactly as `corpus build` would. `Ok(None)` means the run was cancelled.
pub(crate) fn scan(
    scope: &Scope,
    inputs: Option<&CurrentInputArgs>,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<Option<Scan>, CliError> {
    let directory = scope.directory.as_path();
    let report = SiblingScan::scan(directory).map_err(|error| {
        CliError::Corpus(format!(
            "cannot scan {} for prepared realizations: {error}",
            directory.display()
        ))
    })?;
    let entries = directory_entries(directory)?;

    let mut source_names = BTreeSet::new();
    let realization_names = report
        .realizations
        .iter()
        .map(|realization| &realization.path)
        .chain(report.diagnostics.iter().map(|(path, _)| path))
        .chain(
            entries
                .iter()
                .filter(|(_, kind)| *kind == EntryKind::Symlink)
                .map(|(path, _)| path),
        );
    for path in realization_names {
        if let Some(parsed) = file_name(path).and_then(parse_artifact_name)
            && scope.includes(&parsed.source_name)
        {
            source_names.insert(parsed.source_name);
        }
    }
    if let Some((_, name)) = &scope.source {
        source_names.insert(name.clone());
    }

    let mut sources = Vec::with_capacity(source_names.len());
    for name in source_names {
        let path = scope.source_path(&name);
        let (current, request) = match inputs {
            None => (CurrentSelection::NotClassified, None),
            Some(_) if fs::symlink_metadata(&path).is_err() => {
                (CurrentSelection::SourceMissing, None)
            }
            Some(inputs) => {
                let Some((current, request)) =
                    select_current(&path, inputs, &report, config, terminal, output)?
                else {
                    return Ok(None);
                };
                (current, Some(request))
            }
        };
        sources.push(SourceEntry {
            name,
            path,
            current,
            request,
        });
    }
    let current_paths = sources
        .iter()
        .filter_map(|source| match &source.current {
            CurrentSelection::Present(path) => Some(path.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();

    let mut realizations = Vec::new();
    for realization in &report.realizations {
        let Some(parsed) = file_name(&realization.path).and_then(parse_artifact_name) else {
            continue;
        };
        let is_current = current_paths.contains(&realization.path);
        if !is_current && !scope.includes(&parsed.source_name) {
            continue;
        }
        let request = sources
            .iter()
            .find(|source| source.name == parsed.source_name)
            .and_then(|source| source.request.as_ref());
        let validation = if is_current {
            Ok(())
        } else {
            CorpusArtifact::open(&realization.path).map(drop)
        };
        let mut foreign = Vec::new();
        let (status, differences, kept_by_prune) = if is_current {
            (RealizationStatus::Current, Vec::new(), None)
        } else if validation.is_err() {
            (
                RealizationStatus::Damaged,
                Vec::new(),
                Some(PruneKeep::Damaged),
            )
        } else if !realization.is_canonically_named() {
            (RealizationStatus::Misnamed, Vec::new(), None)
        } else if inputs.is_some() {
            let differences = request.map(|request| differences(realization, request));
            let keep = superseded_keep(realization, differences.as_deref());
            foreign = foreign_of(&realization.path);
            (
                RealizationStatus::Superseded,
                differences.unwrap_or_default(),
                keep,
            )
        } else {
            (RealizationStatus::Unclassified, Vec::new(), None)
        };
        realizations.push(RealizationEntry {
            path: realization.path.clone(),
            source: Some(parsed.source_name),
            status,
            bytes: artifact_bytes(&realization.path).ok(),
            recorded: Some(realization.clone()),
            differences,
            kept_by_prune: foreign_keep(&foreign).or(kept_by_prune),
            foreign,
            message: validation.err().map(|error| error.to_string()),
        });
    }
    for (path, error) in &report.diagnostics {
        let Some(parsed) = file_name(path).and_then(parse_artifact_name) else {
            continue;
        };
        if !scope.includes(&parsed.source_name) {
            continue;
        }
        let older = is_older_format(error);
        let foreign = if older && inputs.is_some() {
            foreign_of(path)
        } else {
            Vec::new()
        };
        let kept_by_prune = if older && inputs.is_some() {
            let current = sources
                .iter()
                .find(|source| source.name == parsed.source_name)
                .and_then(|source| source.request.as_ref())
                .map(|request| request.source_identity);
            (current.is_none() || recorded_source_identity(path) != current)
                .then_some(PruneKeep::SourceUnknown)
        } else {
            None
        };
        realizations.push(RealizationEntry {
            path: path.clone(),
            source: Some(parsed.source_name),
            status: if older {
                RealizationStatus::OlderFormat
            } else if is_newer_format(error) {
                RealizationStatus::NewerFormat
            } else {
                RealizationStatus::Damaged
            },
            bytes: artifact_bytes(path).ok(),
            recorded: None,
            differences: Vec::new(),
            kept_by_prune: foreign_keep(&foreign)
                .or(kept_by_prune)
                .or((!older && !is_newer_format(error)).then_some(PruneKeep::Damaged)),
            foreign,
            message: Some(error.to_string()),
        });
    }
    for (path, kind) in &entries {
        let Some(name) = file_name(path) else {
            continue;
        };
        let parsed = parse_artifact_name(name);
        let status = match (kind, &parsed) {
            (EntryKind::Symlink, Some(parsed)) if scope.includes(&parsed.source_name) => {
                RealizationStatus::SymbolicLink
            }
            (EntryKind::Directory, None)
                if name.ends_with(".lingcorpus")
                    && scope
                        .source
                        .as_ref()
                        .is_none_or(|(_, source)| name.starts_with(&format!("{source}."))) =>
            {
                RealizationStatus::NonCanonicalName
            }
            _ => continue,
        };
        realizations.push(RealizationEntry {
            path: path.clone(),
            source: parsed.map(|parsed| parsed.source_name),
            status,
            bytes: artifact_bytes(path).ok(),
            recorded: None,
            differences: Vec::new(),
            kept_by_prune: None,
            foreign: Vec::new(),
            message: None,
        });
    }
    realizations.sort_by(|left, right| left.path.cmp(&right.path));

    let bindable = bindable(&entries, &report);
    let tables = tables(scope, &entries, &bindable, &realizations);
    let leftovers = leftovers(scope, directory, &bindable, &realizations)?;
    Ok(Some(Scan {
        classified: inputs.is_some(),
        sources,
        realizations,
        tables,
        leftovers,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Symlink,
    Other,
}

/// Immediate entries in bytewise path order, with symlink metadata.
fn directory_entries(directory: &Path) -> Result<Vec<(PathBuf, EntryKind)>, CliError> {
    let read = |error: io::Error| CliError::InputIo {
        path: directory.to_path_buf(),
        source: error,
    };
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(directory).map_err(read)? {
        let entry = entry.map_err(read)?;
        let kind = entry.file_type().map_err(read)?;
        let kind = if kind.is_symlink() {
            EntryKind::Symlink
        } else if kind.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::Other
        };
        entries.insert(entry.path(), kind);
    }
    Ok(entries.into_iter().collect())
}

/// Selects the realization `corpus build` would reuse for these inputs.
fn select_current(
    source: &Path,
    inputs: &CurrentInputArgs,
    report: &lingrep_corpus::SiblingReport,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<Option<(CurrentSelection, CurrentRequest)>, CliError> {
    let request = BuildRequest {
        root: source.to_path_buf(),
        output: None,
        dict_path: inputs.dict_path.clone(),
        markdown: inputs.markdown.resolve().map_err(CliError::InvalidArgs)?,
        input_format: inputs.input_format,
    };
    let resolved = resolve_build_inputs(&request, config, terminal)?;
    let progress = PresentationProgress::stderr(
        &config.stderr,
        config.progress,
        output,
        ResultDelivery::Deferred,
    );
    let Some(observed) = observe_root(
        source,
        config,
        &resolved.reader_config,
        terminal.cancellation(),
        &progress,
        &mut |_| {},
    )?
    else {
        return Ok(None);
    };
    progress.finish();
    let destination = canonical_destination(source, &observed, &resolved.analysis)?;
    let classification = Realization::classify(
        &RealizationRequest {
            source_identity: *observed.source_identity(),
            analysis: &resolved.analysis,
            analyzer: &resolved.analyzer_record,
            trust: TrustClass::LocallyExecuted,
            destination: &destination,
        },
        report,
    );
    let current = match classification {
        BuildOutcome::ExactReuse(path) => CurrentSelection::Present(path),
        BuildOutcome::SiblingOf(_)
        | BuildOutcome::Fresh
        | BuildOutcome::ContradictoryDestination(_) => CurrentSelection::Absent(destination),
    };
    Ok(Some((
        current,
        CurrentRequest {
            source_identity: *observed.source_identity(),
            inputs: resolved,
        },
    )))
}

/// The dimensions on which a realization differs from what the current
/// request would build, in [`VariantDifference`] order.
fn differences(
    realization: &SiblingRealization,
    request: &CurrentRequest,
) -> Vec<VariantDifference> {
    let analysis = &request.inputs.analysis;
    let mut differences = Vec::new();
    if realization.source_identity != request.source_identity {
        differences.push(VariantDifference::Source);
    }
    if realization.reader != analysis.reader() {
        differences.push(VariantDifference::Reader);
    }
    if realization.admission != analysis.admission() {
        differences.push(VariantDifference::Admission);
    }
    if realization.analyzer.semantic_identity != request.inputs.analyzer_record.semantic_identity {
        differences.push(VariantDifference::Analyzer);
    }
    if realization.token_schema != analysis.token_schema() {
        differences.push(VariantDifference::TokenSchema);
    }
    if realization.trust != TrustClass::LocallyExecuted {
        differences.push(VariantDifference::Trust);
    }
    differences
}

/// Classifies retention for the default prune. Pass `None` for a missing source.
/// The [`foreign_entries`] of a deletion candidate; a failure to list them is
/// reported as one entry.
fn foreign_of(path: &Path) -> Vec<String> {
    listed_foreign(foreign_entries(path))
}

fn listed_foreign(listed: Result<Vec<String>, CorpusError>) -> Vec<String> {
    listed.unwrap_or_else(|error| vec![format!("(cannot be listed: {error})")])
}

/// Returns removable tables and artifacts retained to preserve a protected
/// table's binding within the scanned directory.
pub(crate) fn plan_tables<'a>(
    tables: &'a [TableEntry],
    removed: &BTreeSet<&Path>,
) -> (Vec<&'a TableEntry>, BTreeMap<&'a Path, &'a TableEntry>) {
    let bound_only = |table: &&TableEntry| {
        table.binding == TableBinding::Superseded
            && !table.artifacts.is_empty()
            && table
                .artifacts
                .iter()
                .all(|artifact| removed.contains(artifact.as_path()))
    };
    let mut protected = BTreeMap::new();
    for table in tables
        .iter()
        .filter(bound_only)
        .filter(|table| table.kept_by_prune.is_some())
    {
        for artifact in &table.artifacts {
            protected.entry(artifact.as_path()).or_insert(table);
        }
    }
    let deleted = tables
        .iter()
        .filter(bound_only)
        .filter(|table| {
            table
                .artifacts
                .iter()
                .all(|artifact| !protected.contains_key(artifact.as_path()))
        })
        .collect();
    (deleted, protected)
}

fn foreign_keep(foreign: &[String]) -> Option<PruneKeep> {
    (!foreign.is_empty()).then_some(PruneKeep::ForeignEntries)
}

fn superseded_keep(
    realization: &SiblingRealization,
    differences: Option<&[VariantDifference]>,
) -> Option<PruneKeep> {
    use VariantDifference::{Admission, Analyzer, Reader, Source, TokenSchema};
    let Some(differences) = differences else {
        return Some(if realization.trust == TrustClass::LocallyExecuted {
            PruneKeep::SourceUnknown
        } else {
            PruneKeep::OtherTrust
        });
    };
    if realization.trust != TrustClass::LocallyExecuted {
        Some(PruneKeep::OtherTrust)
    } else if differences.is_empty() {
        Some(PruneKeep::IdenticalToCurrent)
    } else if differences.contains(&Source) {
        Some(PruneKeep::SourceRevision)
    } else if differences
        .iter()
        .any(|difference| matches!(difference, Analyzer | TokenSchema))
    {
        None
    } else if differences
        .iter()
        .any(|difference| matches!(difference, Reader | Admission))
    {
        Some(PruneKeep::ReaderVariant)
    } else {
        None
    }
}

/// Every entry in the directory, whatever its name or scope, that a count
/// table can bind, with the analysis identity it binds: each realization with
/// a readable manifest, including explicit outputs and symbolic-link targets,
/// and each unreadable canonical name other than an older format, by the
/// identity its name records.
fn bindable(
    entries: &[(PathBuf, EntryKind)],
    report: &lingrep_corpus::SiblingReport,
) -> Vec<(PathBuf, AnalysisIdentity)> {
    let mut bindable = report
        .realizations
        .iter()
        .map(|realization| (realization.path.clone(), realization.analysis_identity))
        .collect::<Vec<_>>();
    bindable.extend(report.diagnostics.iter().filter_map(|(path, error)| {
        let parsed = file_name(path).and_then(parse_artifact_name)?;
        (!is_older_format(error)).then(|| (path.clone(), parsed.analysis_identity))
    }));
    for (path, kind) in entries {
        let candidate = match kind {
            EntryKind::Symlink => true,
            EntryKind::Directory => file_name(path)
                .is_some_and(|name| !name.starts_with('.') && parse_artifact_name(name).is_none()),
            EntryKind::Other => false,
        };
        if candidate
            && fs::metadata(path.join("manifest.json")).is_ok_and(|metadata| metadata.is_file())
            && let Ok(realization) = SiblingRealization::read(path)
        {
            bindable.push((path.clone(), realization.analysis_identity));
        }
    }
    bindable.sort_by(|left, right| left.0.cmp(&right.0));
    bindable
}

/// Count tables immediately in the directory, bound by analysis identity to
/// the [`bindable`] entries beside them.
///
/// A directory is a table candidate when it holds a regular `header.json`;
/// one whose header names another format is not a table and is skipped.
fn tables(
    scope: &Scope,
    entries: &[(PathBuf, EntryKind)],
    bindable: &[(PathBuf, AnalysisIdentity)],
    reported: &[RealizationEntry],
) -> Vec<TableEntry> {
    let status_of = |path: &Path| {
        reported
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.status)
    };
    let mut tables = Vec::new();
    for (path, kind) in entries {
        if *kind != EntryKind::Directory
            || file_name(path)
                .is_none_or(|name| name.starts_with('.') || parse_artifact_name(name).is_some())
            || !fs::symlink_metadata(path.join("header.json")).is_ok_and(|meta| meta.is_file())
        {
            continue;
        }
        let key = match CountTable::read_key(path) {
            Ok(key) => key,
            Err(CountTableError::UnsupportedFormat { found, .. })
                if found == lingrep_corpus::COUNT_TABLE_FORMAT =>
            {
                if scope.source.is_none() {
                    tables.push(TableEntry {
                        path: path.clone(),
                        bytes: artifact_bytes(path).ok(),
                        analysis_identity: None,
                        binding: TableBinding::Unreadable,
                        artifacts: Vec::new(),
                        kept_by_prune: None,
                        foreign: Vec::new(),
                        message: Some(
                            CountTable::read_key(path)
                                .err()
                                .map_or_else(String::new, |error| error.to_string()),
                        ),
                    });
                }
                continue;
            }
            Err(_) => continue,
        };
        let artifacts = bindable
            .iter()
            .filter(|(_, identity)| encode_digest(identity.as_bytes()) == key.analysis_identity)
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let statuses = artifacts
            .iter()
            .map(|artifact| status_of(artifact))
            .collect::<Vec<_>>();
        if scope.source.is_some() && statuses.iter().all(Option::is_none) {
            continue;
        }
        let binding = if statuses.is_empty() {
            TableBinding::Absent
        } else if statuses.contains(&Some(RealizationStatus::Current)) {
            TableBinding::Current
        } else if statuses
            .iter()
            .all(|status| *status == Some(RealizationStatus::Superseded))
        {
            TableBinding::Superseded
        } else {
            TableBinding::Present
        };
        let foreign = if binding == TableBinding::Superseded {
            listed_foreign(CountTable::foreign_entries(path))
        } else {
            Vec::new()
        };
        tables.push(TableEntry {
            path: path.clone(),
            bytes: artifact_bytes(path).ok(),
            analysis_identity: Some(key.analysis_identity),
            binding,
            artifacts,
            kept_by_prune: foreign_keep(&foreign),
            foreign,
            message: None,
        });
    }
    tables
}

/// The leftovers in `directory`. With `--source`, an interrupted realization
/// prune is in scope when its name records the source, and an interrupted
/// table prune when its header binds a `reported` realization.
fn leftovers(
    scope: &Scope,
    directory: &Path,
    bindable: &[(PathBuf, AnalysisIdentity)],
    reported: &[RealizationEntry],
) -> Result<Vec<LeftoverEntry>, CliError> {
    let found = staging_leftovers(directory).map_err(|error| CliError::InputIo {
        path: directory.to_path_buf(),
        source: error,
    })?;
    let pruning = prune_leftovers(directory).map_err(|error| CliError::InputIo {
        path: directory.to_path_buf(),
        source: error,
    })?;
    let in_scope = |published_name: &str| {
        scope.source.is_none()
            || parse_artifact_name(published_name)
                .is_some_and(|parsed| scope.includes(&parsed.source_name))
    };
    let mut entries = found
        .into_iter()
        .filter(|leftover| in_scope(&leftover.published_name))
        .map(|leftover| {
            let bytes = [leftover.staging.as_deref(), leftover.lock.as_deref()]
                .into_iter()
                .flatten()
                .map(|path| artifact_bytes(path).ok())
                .sum::<Option<u64>>();
            LeftoverEntry {
                status: match leftover.status {
                    StagingStatus::Live => LeftoverStatus::Live,
                    StagingStatus::Abandoned => LeftoverStatus::Abandoned,
                    StagingStatus::Unknown => LeftoverStatus::Unknown,
                },
                published_name: leftover.published_name,
                staging: leftover.staging,
                lock: leftover.lock,
                pruning: None,
                bytes,
                kept_by_prune: None,
                foreign: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    let table_in_scope = |path: &Path| {
        scope.source.is_none()
            || CountTable::read_key(path).is_ok_and(|key| {
                bindable.iter().any(|(artifact, identity)| {
                    encode_digest(identity.as_bytes()) == key.analysis_identity
                        && reported.iter().any(|entry| entry.path == *artifact)
                })
            })
    };
    entries.extend(
        pruning
            .into_iter()
            .filter(|leftover| match leftover.kind {
                PruneLeftoverKind::Realization => in_scope(&leftover.published_name),
                PruneLeftoverKind::CountTable => table_in_scope(&leftover.path),
            })
            .map(|leftover| {
                let foreign = listed_foreign(leftover.foreign_entries());
                LeftoverEntry {
                    status: LeftoverStatus::InterruptedPrune,
                    bytes: artifact_bytes(&leftover.path).ok(),
                    published_name: leftover.published_name,
                    staging: None,
                    lock: None,
                    pruning: Some(leftover.path),
                    kept_by_prune: foreign_keep(&foreign),
                    foreign,
                }
            }),
    );
    entries.sort_by(|left, right| left.path().cmp(&right.path()));
    Ok(entries)
}

impl LeftoverEntry {
    /// The path the leftover is reported under.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.staging
            .as_deref()
            .or(self.lock.as_deref())
            .or(self.pruning.as_deref())
    }
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn modified(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs())
}

pub(crate) fn display(path: &Path) -> String {
    path.display().to_string()
}

pub(crate) fn identity(identity: &AnalysisIdentity) -> String {
    encode_digest(identity.as_bytes())
}

pub fn run(
    args: &CorpusDiskArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let stdout = io::stdout();
    run_to(args, config, terminal, output, &mut stdout.lock())
}

pub(crate) fn run_to<W: Write>(
    args: &CorpusDiskArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
    writer: &mut W,
) -> Result<CommandOutcome, CliError> {
    let scope = Scope::new(args.directory.as_deref(), args.source.as_deref())?;
    let inputs = args.classifies_current().then_some(&args.inputs);
    let Some(scan) = scan(&scope, inputs, config, terminal, output)? else {
        return Ok(CommandOutcome::Cancelled);
    };
    let caches = caches(args);

    let mut events = vec![DiskEvent::Run(DiskRun {
        run_id: crate::commands::query_support::random_id(),
        directory: display(&scope.directory),
        source: scope.source.as_ref().map(|(path, _)| display(path)),
        current_classified: scan.classified,
    })];
    events.extend(
        scan.sources
            .iter()
            .map(|source| DiskEvent::Source(source.record())),
    );
    let removed = scan
        .realizations
        .iter()
        .filter(|entry| {
            matches!(
                entry.status,
                RealizationStatus::Superseded | RealizationStatus::OlderFormat
            ) && entry.kept_by_prune.is_none()
        })
        .map(|entry| entry.path.as_path())
        .collect();
    let kept_by_table = plan_tables(&scan.tables, &removed).1;
    events.extend(scan.realizations.iter().map(|entry| {
        let recorded = entry.recorded.as_ref();
        DiskEvent::Realization(DiskRealization {
            path: display(&entry.path),
            source: entry.source.clone(),
            status: entry.status,
            apparent_bytes: entry.bytes,
            modified_unix_seconds: modified(&entry.path),
            analysis_identity: recorded.map(|recorded| identity(&recorded.analysis_identity)),
            format_generation: recorded.map(|recorded| recorded.format_generation),
            analyzer_family: recorded.map(|recorded| recorded.analyzer.family.clone()),
            dictionary_identity: recorded
                .map(|recorded| encode_digest(recorded.dictionary.as_bytes())),
            differences: entry.differences.clone(),
            kept_by_prune: entry.kept_by_prune.or(kept_by_table
                .contains_key(entry.path.as_path())
                .then_some(PruneKeep::KeptTable)),
            message: entry.message.clone(),
        })
    }));
    events.extend(scan.tables.iter().map(|table| {
        DiskEvent::Table(DiskTable {
            path: display(&table.path),
            apparent_bytes: table.bytes,
            modified_unix_seconds: modified(&table.path),
            analysis_identity: table.analysis_identity.clone(),
            binding: table.binding,
            artifacts: table.artifacts.iter().map(|path| display(path)).collect(),
            kept_by_prune: table.kept_by_prune,
            message: table.message.clone(),
        })
    }));
    events.extend(scan.leftovers.iter().map(|leftover| {
        DiskEvent::Leftover(DiskLeftover {
            published_name: leftover.published_name.clone(),
            staging: leftover.staging.as_deref().map(display),
            lock: leftover.lock.as_deref().map(display),
            pruning: leftover.pruning.as_deref().map(display),
            kept_by_prune: leftover.kept_by_prune,
            status: leftover.status,
            apparent_bytes: leftover.bytes,
            modified_unix_seconds: leftover.path().and_then(modified),
        })
    }));
    events.extend(caches.into_iter().map(DiskEvent::Cache));
    events.push(DiskEvent::Complete(complete(&events)));

    let human = Human {
        directory: &scope.directory,
    };
    for event in events {
        let envelope = DiskEnvelope::new(event);
        if output == OutputMode::Jsonl {
            write_jsonl(writer, &envelope).map_err(CliError::corpus)?;
        } else {
            human
                .write(writer, &envelope.event)
                .map_err(CliError::stdout)?;
        }
    }
    Ok(CommandOutcome::Success)
}

/// The dictionary cache when requested, then each named analyzer directory.
fn caches(args: &CorpusDiskArgs) -> Vec<DiskCache> {
    let mut caches = Vec::new();
    if args.caches
        && let Some(directory) = vibrato_rkyv::dictionary::GLOBAL_CACHE_DIR.as_deref()
        && let Ok(entries) = fs::read_dir(directory)
    {
        let mut copies = BTreeMap::new();
        let (mut other_files, mut other_bytes) = (0_u64, 0_u64);
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "dic") {
                copies.insert(path, metadata.len());
            } else {
                other_files += 1;
                other_bytes += metadata.len();
            }
        }
        caches.extend(copies.into_iter().map(|(path, bytes)| DiskCache {
            kind: CacheKind::DictionaryCopy,
            modified_unix_seconds: modified(&path),
            path: display(&path),
            apparent_bytes: Some(bytes),
            files: 1,
        }));
        caches.push(DiskCache {
            kind: CacheKind::DictionaryCacheOther,
            path: display(directory),
            apparent_bytes: Some(other_bytes),
            files: other_files,
            modified_unix_seconds: modified(directory),
        });
    }
    for directory in &args.analyzer_dir {
        let (files, bytes) =
            count_files(directory).map_or((0, None), |(files, bytes)| (files, Some(bytes)));
        caches.push(DiskCache {
            kind: CacheKind::Analyzer,
            path: display(directory),
            apparent_bytes: bytes,
            files,
            modified_unix_seconds: modified(directory),
        });
    }
    caches
}

/// Files and their total apparent size under `path`, without following
/// symbolic links.
fn count_files(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok((1, metadata.len()));
    }
    let mut total = (0, 0);
    for entry in fs::read_dir(path)? {
        let (files, bytes) = count_files(&entry?.path())?;
        total.0 += files;
        total.1 += bytes;
    }
    Ok(total)
}

fn complete(events: &[DiskEvent]) -> DiskComplete {
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    enum Key {
        Realization(RealizationStatus),
        Table(TableBinding),
        Leftover(LeftoverStatus),
        Cache(CacheKind),
    }
    let mut totals = BTreeMap::<Key, (u64, u64)>::new();
    let mut add = |key: Key, bytes: Option<u64>| {
        let slot = totals.entry(key).or_default();
        slot.0 += 1;
        slot.1 += bytes.unwrap_or_default();
    };
    for event in events {
        match event {
            DiskEvent::Realization(record) => {
                add(Key::Realization(record.status), record.apparent_bytes);
            }
            DiskEvent::Table(record) => add(Key::Table(record.binding), record.apparent_bytes),
            DiskEvent::Leftover(record) => {
                add(Key::Leftover(record.status), record.apparent_bytes);
            }
            DiskEvent::Cache(record) => add(Key::Cache(record.kind), record.apparent_bytes),
            DiskEvent::Run(_) | DiskEvent::Source(_) | DiskEvent::Complete(_) => {}
        }
    }
    DiskComplete {
        apparent_bytes: totals.values().map(|(_, bytes)| bytes).sum(),
        totals: totals
            .into_iter()
            .map(|(key, (count, apparent_bytes))| match key {
                Key::Realization(status) => DiskTotal::Realization {
                    status,
                    count,
                    apparent_bytes,
                },
                Key::Table(binding) => DiskTotal::Table {
                    binding,
                    count,
                    apparent_bytes,
                },
                Key::Leftover(status) => DiskTotal::Leftover {
                    status,
                    count,
                    apparent_bytes,
                },
                Key::Cache(kind) => DiskTotal::Cache {
                    kind,
                    count,
                    apparent_bytes,
                },
            })
            .collect(),
    }
}

/// The snake-case spelling a unit enum serializes to.
pub(crate) fn wire<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Binary-prefixed size with one decimal, for terminal output.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub(crate) fn size(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "size unknown".to_owned(), human_bytes)
}

struct Human<'a> {
    directory: &'a Path,
}

impl Human<'_> {
    /// A path relative to the scanned directory when it is inside it.
    fn short(&self, path: &str) -> String {
        Path::new(path).strip_prefix(self.directory).map_or_else(
            |_| path.to_owned(),
            |relative| relative.display().to_string(),
        )
    }

    fn write<W: Write>(&self, writer: &mut W, event: &DiskEvent) -> io::Result<()> {
        match event {
            DiskEvent::Run(run) => {
                match &run.source {
                    Some(source) => writeln!(writer, "disk use of {source} in {}", run.directory)?,
                    None => writeln!(writer, "disk use in {}", run.directory)?,
                }
                if !run.current_classified {
                    writeln!(
                        writer,
                        "  current and superseded are not classified: pass the analysis flags \
                         `corpus build` would get, or --current for its defaults"
                    )?;
                }
                Ok(())
            }
            DiskEvent::Source(source) => {
                let current = match &source.current {
                    SourceCurrent::NotClassified => return Ok(()),
                    SourceCurrent::Present { path } => format!("current {}", self.short(path)),
                    SourceCurrent::Absent { path } => {
                        format!(
                            "no current realization; corpus build would write {}",
                            self.short(path)
                        )
                    }
                    SourceCurrent::SourceMissing => "source missing; nothing is current".to_owned(),
                };
                writeln!(writer, "  source {}: {current}", source.name)
            }
            DiskEvent::Realization(record) => {
                let mut detail = String::new();
                if !record.differences.is_empty() {
                    detail = format!(
                        " (differs in {})",
                        record
                            .differences
                            .iter()
                            .map(|difference| wire(difference).replace('_', " "))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(keep) = record.kept_by_prune {
                    detail.push_str(match keep {
                        keep if keep.deleted_with_include_all_superseded() => {
                            "; prune keeps it unless --include-all-superseded"
                        }
                        _ => "; prune keeps it",
                    });
                }
                writeln!(
                    writer,
                    "  {:<18} {:>10}  {}{detail}",
                    wire(&record.status).replace('_', "-"),
                    size(record.apparent_bytes),
                    self.short(&record.path)
                )
            }
            DiskEvent::Table(record) => writeln!(
                writer,
                "  table {:<12} {:>10}  {}",
                wire(&record.binding),
                size(record.apparent_bytes),
                self.short(&record.path)
            ),
            DiskEvent::Leftover(record) => writeln!(
                writer,
                "  leftover {:<9} {:>10}  {}",
                wire(&record.status),
                size(record.apparent_bytes),
                record
                    .staging
                    .as_deref()
                    .or(record.lock.as_deref())
                    .or(record.pruning.as_deref())
                    .map_or_else(String::new, |path| self.short(path))
            ),
            DiskEvent::Cache(record) => {
                if record.kind == CacheKind::DictionaryCopy {
                    return Ok(());
                }
                writeln!(
                    writer,
                    "  cache {:<12} {:>10}  {} ({} file(s))",
                    wire(&record.kind).replace('_', "-"),
                    size(record.apparent_bytes),
                    record.path,
                    record.files
                )
            }
            DiskEvent::Complete(complete) => {
                writeln!(
                    writer,
                    "total {} (apparent size)",
                    human_bytes(complete.apparent_bytes)
                )?;
                for total in &complete.totals {
                    let (item, status, count, bytes) = match total {
                        DiskTotal::Realization {
                            status,
                            count,
                            apparent_bytes,
                        } => ("realization", wire(status), count, apparent_bytes),
                        DiskTotal::Table {
                            binding,
                            count,
                            apparent_bytes,
                        } => ("table", wire(binding), count, apparent_bytes),
                        DiskTotal::Leftover {
                            status,
                            count,
                            apparent_bytes,
                        } => ("leftover", wire(status), count, apparent_bytes),
                        DiskTotal::Cache {
                            kind,
                            count,
                            apparent_bytes,
                        } => ("cache", wire(kind), count, apparent_bytes),
                    };
                    writeln!(
                        writer,
                        "  {item:<11} {status:<22} {count:>5}  {:>10}",
                        human_bytes(*bytes)
                    )?;
                }
                Ok(())
            }
        }
    }
}
