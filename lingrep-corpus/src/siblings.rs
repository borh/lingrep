//! Discovery of every prepared realization sitting beside a source.
//!
//! `corpus build` reuses matching current artifacts, and `corpus variants`
//! reports the current artifacts present beside a source. Old product formats
//! are excluded from discovery and require rebuilding from source.
//!
//! The scan is bounded. It reads immediate directory entries and each
//! candidate's manifest under the existing manifest size bound; it opens no
//! token component and hashes nothing. Complete component verification stays
//! `corpus inspect --verify`, and a reuse decision does not imply it.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    AnalysisIdentity, AnalysisInputs, AnalyzerRecord, CorpusArtifact, CorpusError,
    DictionaryFingerprint, IdentityScheme, SourceIdentity, TokenSchemaIdentity, TrustClass,
    artifact::parse_artifact_name, manifest::Manifest,
};

const MANIFEST_NAME: &str = "manifest.json";

/// One prepared realization found beside a source.
///
/// Every field is read from the manifest, never recomputed. The recorded
/// [`AnalyzerRecord`] is what makes a reuse decision honest: it is the exact
/// semantic description the build sealed, so comparing it against a request
/// answers "same analysis?" without re-running an analyzer.
#[derive(Clone, Debug)]
pub struct SiblingRealization {
    pub path: PathBuf,
    pub scheme: IdentityScheme,
    pub format_generation: u32,
    pub source_identity: SourceIdentity,
    pub analysis_identity: AnalysisIdentity,
    pub analyzer: AnalyzerRecord,
    pub reader: crate::ReaderIdentity,
    pub admission: crate::AdmissionIdentity,
    pub token_schema: TokenSchemaIdentity,
    pub dictionary: DictionaryFingerprint,
    pub trust: TrustClass,
    pub document_count: u64,
    pub sentence_count: u64,
    pub token_count: u64,
    /// What this artifact truncated, if anything.
    ///
    /// Carried here so a build can decline to reuse a corpus that was shortened
    /// under a different sentence limit. `None` is the ordinary case and the
    /// value every artifact written before truncation existed reports.
    pub sentence_truncation: Option<crate::SentenceTruncation>,
}

impl SiblingRealization {
    /// Whether the path's file name is the canonical artifact name for the
    /// analysis identity and trust class the manifest records.
    #[must_use]
    pub fn is_canonically_named(&self) -> bool {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_artifact_name)
            .is_some_and(|parsed| {
                parsed.analysis_identity == self.analysis_identity && parsed.trust == self.trust
            })
    }

    /// Reads the realization whose manifest is at `path/manifest.json`,
    /// whatever `path` is named, under the same manifest bound as
    /// [`SiblingScan::scan`].
    pub fn read(path: &Path) -> Result<Self, CorpusError> {
        realization(
            path.to_path_buf(),
            &Manifest::open(&path.join(MANIFEST_NAME))?,
        )
    }
}

/// Realizations found, and the candidates that could not be read.
///
/// Successes and failures travel together on purpose. A single truncated or
/// half-written sibling must not blind the scan to every other realization
/// beside it — that would turn one damaged directory entry into a silent
/// rebuild of a corpus that is present and fine.
#[derive(Debug, Default)]
pub struct SiblingReport {
    /// Readable realizations, in bytewise path order.
    pub realizations: Vec<SiblingRealization>,
    /// Candidates whose manifest could not be read, in bytewise path order.
    pub diagnostics: Vec<(PathBuf, CorpusError)>,
}

impl SiblingReport {
    /// Returns the realizations whose recorded source identity matches.
    #[must_use]
    pub fn for_source(&self, source: &SourceIdentity) -> Vec<&SiblingRealization> {
        self.realizations
            .iter()
            .filter(|realization| realization.source_identity == *source)
            .collect()
    }
}

/// Returns the directory a path sits in, as a scannable path.
///
/// [`Path::parent`] returns `Some("")` for a bare relative name such as
/// `corpus.jsonl`, not `None`, so a plain `unwrap_or` fallback never fires and
/// the scan is handed an empty path that cannot be opened. Both callers hit
/// this the first time anyone ran the command from the corpus's own directory.
#[must_use]
pub fn containing_directory(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Bounded, non-recursive scan of one directory for prepared realizations.
pub struct SiblingScan;

impl SiblingScan {
    /// Reads every canonical artifact directory sitting immediately in `dir`.
    ///
    /// A whole-scan failure — an unreadable directory, or a symlinked one — is
    /// an error, because it means the answer is unknown rather than empty. A
    /// per-candidate failure is a diagnostic.
    ///
    /// A directory entry whose name is not a canonical artifact name is
    /// skipped silently and does not appear in either list: it was never a
    /// candidate.
    pub fn scan(dir: &Path) -> Result<SiblingReport, CorpusError> {
        let metadata = fs::symlink_metadata(dir)?;
        if metadata.file_type().is_symlink() {
            return Err(crate::artifact::invalid_artifact(
                "sibling scan refuses a symlinked directory",
            ));
        }
        if !metadata.file_type().is_dir() {
            return Err(CorpusError::ExpectedArtifactDirectory {
                path: dir.to_path_buf(),
            });
        }
        // Collected into an ordered set first: readdir order is a filesystem
        // detail, and a warning or a machine record that changes between runs
        // over unchanged bytes is not reproducible.
        let mut candidates = BTreeSet::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if parse_artifact_name(name).is_none() {
                continue;
            }
            if !entry.file_type()?.is_dir() {
                continue;
            }
            candidates.insert(path);
        }

        let mut report = SiblingReport::default();
        for path in candidates {
            match Manifest::open(&path.join(MANIFEST_NAME)) {
                Ok(manifest) => match realization(path.clone(), &manifest) {
                    Ok(realization) => report.realizations.push(realization),
                    Err(error) => report.diagnostics.push((path, error)),
                },
                Err(error) => report.diagnostics.push((path, error)),
            }
        }
        Ok(report)
    }
}

/// Whether a scan diagnostic records an artifact written in an earlier format
/// than this build reads: an earlier artifact generation, manifest schema or
/// provenance schema.
///
/// Such an artifact can only be rebuilt from source. A newer format, which a
/// newer build reads, and a damaged manifest are not older formats.
#[must_use]
pub fn is_older_format(error: &CorpusError) -> bool {
    match error {
        CorpusError::UnsupportedArtifactGeneration { generation } => {
            *generation < crate::CORPUS_ARTIFACT_FORMAT_GENERATION
        }
        CorpusError::PreparedArtifactRebuildRequired { version } => {
            *version < crate::manifest::MANIFEST_SCHEMA_VERSION
        }
        CorpusError::UnsupportedSchema { component, version } => {
            *component == crate::manifest::PROVENANCE_NAME
                && *version < crate::provenance::SCHEMA_VERSION
        }
        _ => false,
    }
}

/// Whether a scan diagnostic records an artifact written in a later format
/// than this build reads: a later artifact generation, manifest schema or
/// provenance schema, or a manifest field this build does not read.
#[must_use]
pub fn is_newer_format(error: &CorpusError) -> bool {
    match error {
        CorpusError::UnsupportedArtifactGeneration { generation } => {
            *generation > crate::CORPUS_ARTIFACT_FORMAT_GENERATION
        }
        CorpusError::PreparedArtifactRebuildRequired { version } => {
            *version > crate::manifest::MANIFEST_SCHEMA_VERSION
        }
        CorpusError::UnsupportedSchema { component, version } => {
            *component == crate::manifest::PROVENANCE_NAME
                && *version > crate::provenance::SCHEMA_VERSION
        }
        CorpusError::UnknownManifestField { .. } => true,
        _ => false,
    }
}

/// The source identity recorded in the manifest of the artifact at `path`,
/// read without the format checks [`SiblingScan::scan`] applies, or `None`
/// when the manifest records none in the 64-hex-digit form this build writes.
///
/// For an older-format artifact, equality with the current source identity
/// shows that it analyzed the current source content; inequality does not
/// show the opposite, because an earlier build may have derived the identity
/// differently.
#[must_use]
pub fn recorded_source_identity(path: &Path) -> Option<SourceIdentity> {
    let manifest = Manifest::read_untyped(&path.join(MANIFEST_NAME)).ok()?;
    let digest = crate::manifest::decode_digest(manifest["source_identity"].as_str()?, "").ok()?;
    Some(SourceIdentity::from_bytes(digest))
}

/// Total size in bytes of the files under `path`, without following symbolic
/// links.
pub fn artifact_bytes(path: &Path) -> std::io::Result<u64> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        total += artifact_bytes(&entry?.path())?;
    }
    Ok(total)
}

/// Deletes the older-format artifact of `source_name` at `path` and returns
/// its apparent size in bytes.
///
/// Fails without deleting anything unless `path` is a directory rather than a
/// symbolic link, its name is a canonical artifact name for exactly
/// `source_name`, and it holds a regular `manifest.json` that fails to open
/// with an error [`is_older_format`] accepts, and it has no
/// [`foreign_entries`]. The deletion follows
/// [`remove_realization`]'s two steps.
pub fn remove_older_format(path: &Path, source_name: &str) -> Result<u64, CorpusError> {
    let name = path.file_name().and_then(|name| name.to_str());
    if name
        .and_then(parse_artifact_name)
        .is_none_or(|parsed| parsed.source_name != source_name)
    {
        return Err(cannot_delete(
            path,
            "not a canonical artifact name for this source",
        ));
    }
    remove_in_two_steps(path, ARTIFACT_LAYOUT, |at| {
        if !fs::symlink_metadata(at)?.is_dir() {
            return Err(cannot_delete(at, "not a directory"));
        }
        let manifest = at.join(MANIFEST_NAME);
        if !fs::symlink_metadata(&manifest)?.is_file() {
            return Err(cannot_delete(at, "manifest is not a regular file"));
        }
        match Manifest::open(&manifest) {
            Err(error) if is_older_format(&error) => Ok(()),
            _ => Err(cannot_delete(at, "not written in an older format")),
        }
    })
}

/// Deletes the readable realization of `source_name` at `path` and returns
/// its apparent size in bytes.
///
/// Fails without deleting anything unless `path` is a directory rather than a
/// symbolic link, its name is a canonical artifact name for exactly
/// `source_name` and `expected`, it passes [`CorpusArtifact::open`], and it
/// records `expected`, and it has no [`foreign_entries`]. Choosing which
/// realization is superseded is the caller's; this checks only that `path` is
/// still the one it chose.
///
/// A successful rename moves the directory to a hidden sibling before any
/// component is removed. The function rechecks the hidden directory before
/// deleting its components. If removal is interrupted, [`prune_leftovers`]
/// can find the hidden directory and [`finish_prune_leftover`] can remove it.
pub fn remove_realization(
    path: &Path,
    source_name: &str,
    expected: &AnalysisIdentity,
) -> Result<u64, CorpusError> {
    let name = path.file_name().and_then(|name| name.to_str());
    if name.and_then(parse_artifact_name).is_none_or(|parsed| {
        parsed.source_name != source_name || parsed.analysis_identity != *expected
    }) {
        return Err(cannot_delete(
            path,
            "not the canonical artifact name of this source and identity",
        ));
    }
    remove_in_two_steps(path, ARTIFACT_LAYOUT, |at| {
        if !fs::symlink_metadata(at)?.is_dir() {
            return Err(cannot_delete(at, "not a directory"));
        }
        if *CorpusArtifact::open(at)?.analysis_identity() != *expected {
            return Err(cannot_delete(
                at,
                "its manifest records another analysis identity",
            ));
        }
        Ok(())
    })
}

/// The entries of the directory at `path` that are not components of the
/// artifact its `manifest.json` describes, by file name, with a trailing `/`
/// for a directory.
///
/// The components are `manifest.json` and the names the manifest's
/// `components` table lists, whatever its format generation; each must be a
/// regular file. Without a readable manifest, every entry is foreign. On Unix,
/// an entry on another device than `path` is foreign, and so is `path`
/// itself, reported as `.`, when it is on another device than its parent.
pub fn foreign_entries(path: &Path) -> Result<Vec<String>, CorpusError> {
    foreign_entries_in(path, ARTIFACT_LAYOUT)
}

/// The files of one kind of prepared directory.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    /// The file that describes the directory, deleted last.
    pub(crate) descriptor: &'static str,
    /// The names of the other components of the directory at a path, read
    /// from its descriptor, or `None` without a readable descriptor.
    pub(crate) components: fn(&Path) -> Option<BTreeSet<String>>,
}

const ARTIFACT_LAYOUT: Layout = Layout {
    descriptor: MANIFEST_NAME,
    components: listed_components,
};

/// [`foreign_entries`] for the components `layout` lists.
pub(crate) fn foreign_entries_in(path: &Path, layout: Layout) -> Result<Vec<String>, CorpusError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(CorpusError::ExpectedArtifactDirectory {
            path: path.to_path_buf(),
        });
    }
    let mut foreign = Vec::new();
    let parent = fs::symlink_metadata(containing_directory(path))?;
    if !same_device(&parent, &metadata) {
        foreign.push(".".to_owned());
    }
    let listed = (layout.components)(path);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = fs::symlink_metadata(entry.path())?;
        let component = listed
            .as_ref()
            .is_some_and(|listed| name == layout.descriptor || listed.contains(&name));
        if !component || !kind.is_file() || !same_device(&metadata, &kind) {
            foreign.push(if kind.is_dir() {
                format!("{name}/")
            } else {
                name
            });
        }
    }
    foreign.sort();
    Ok(foreign)
}

/// The plain file names the manifest's `components` table lists, or `None`
/// without a readable manifest. A listed name with a separator, `.` or `..`
/// names nothing in the directory and is dropped.
fn listed_components(path: &Path) -> Option<BTreeSet<String>> {
    let manifest = Manifest::read_untyped(&path.join(MANIFEST_NAME)).ok()?;
    manifest["components"].as_object().map(|components| {
        components
            .keys()
            .filter(|name| {
                let mut parts = Path::new(name.as_str()).components();
                matches!(parts.next(), Some(std::path::Component::Normal(part)) if part == name.as_str())
                    && parts.next().is_none()
            })
            .cloned()
            .collect()
    })
}

#[cfg(unix)]
fn same_device(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev()
}

#[cfg(not(unix))]
fn same_device(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

const PRUNE_MARKER: &str = ".pruning-";

/// A directory that a removal renamed for deletion and did not finish
/// deleting.
#[derive(Clone, Debug)]
pub struct PruneLeftover {
    pub path: PathBuf,
    /// The name the directory had before the rename.
    pub published_name: String,
    pub kind: PruneLeftoverKind,
}

/// What a [`PruneLeftover`] was before the rename, read from its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneLeftoverKind {
    /// A prepared artifact: the published name is a canonical artifact name.
    Realization,
    /// A count table: any other published name.
    CountTable,
}

impl PruneLeftover {
    /// The entries that are not components of what it was: for a
    /// [`PruneLeftoverKind::Realization`], [`foreign_entries`]; for a
    /// [`PruneLeftoverKind::CountTable`],
    /// [`crate::CountTable::foreign_entries`].
    pub fn foreign_entries(&self) -> Result<Vec<String>, CorpusError> {
        foreign_entries_in(&self.path, self.kind.layout())
    }
}

impl PruneLeftoverKind {
    fn layout(self) -> Layout {
        match self {
            Self::Realization => ARTIFACT_LAYOUT,
            Self::CountTable => crate::count_table::TABLE_LAYOUT,
        }
    }
}

/// The name a removal renamed a directory from, when `name` is the hidden
/// name `.<published name>.pruning-<suffix>` it gives.
pub(crate) fn pruned_name(name: &str) -> Option<&str> {
    let (published, _) = name.strip_prefix('.')?.rsplit_once(PRUNE_MARKER)?;
    (!published.is_empty() && !published.starts_with('.')).then_some(published)
}

/// Lists the [`PruneLeftover`]s immediately in `dir`, in bytewise path order:
/// directories, not symbolic links, named `.<published name>.pruning-<suffix>`
/// for a published name that does not start with `.`.
pub fn prune_leftovers(dir: &Path) -> std::io::Result<Vec<PruneLeftover>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(published_name) = file_name.to_str().and_then(pruned_name) else {
            continue;
        };
        if entry.file_type()?.is_dir() {
            let kind = if parse_artifact_name(published_name).is_some() {
                PruneLeftoverKind::Realization
            } else {
                PruneLeftoverKind::CountTable
            };
            found.push(PruneLeftover {
                path: entry.path(),
                published_name: published_name.to_owned(),
                kind,
            });
        }
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(found)
}

/// Deletes the [`PruneLeftover`] at `path` and returns its apparent size in bytes.
///
/// Fails without deleting anything unless `path` is a directory rather than a
/// symbolic link with a name [`prune_leftovers`] reports, and it has no
/// [`PruneLeftover::foreign_entries`].
pub fn finish_prune_leftover(path: &Path) -> Result<u64, CorpusError> {
    let leftover = prune_leftovers(containing_directory(path))
        .ok()
        .and_then(|found| found.into_iter().find(|leftover| leftover.path == path));
    let Some(leftover) = leftover else {
        return Err(cannot_delete(path, "not an interrupted prune"));
    };
    delete_renamed(path, leftover.kind.layout())
}

pub(crate) fn cannot_delete(path: &Path, reason: &str) -> CorpusError {
    crate::artifact::invalid_artifact(format!("cannot delete {}: {reason}", path.display()))
}

/// Checks `path` with `verify` and for entries `layout` does not list,
/// renames it to a hidden name beside it, checks it again under that name,
/// and deletes it.
pub(crate) fn remove_in_two_steps(
    path: &Path,
    layout: Layout,
    verify: impl Fn(&Path) -> Result<(), CorpusError>,
) -> Result<u64, CorpusError> {
    let verify = |at: &Path| {
        verify(at)?;
        closed(at, layout)
    };
    verify(path)?;
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(cannot_delete(path, "no file name"));
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let hidden = containing_directory(path).join(format!(
        ".{name}{PRUNE_MARKER}{}-{nanos}",
        std::process::id()
    ));
    crate::fs_capability::rename_noreplace(path, &hidden).map_err(|error| {
        CorpusError::Io(crate::error::io_at(
            format!("rename '{}' to", path.display()),
            &hidden,
            error.kind(),
            error,
        ))
    })?;
    if let Err(error) = verify(&hidden) {
        return Err(
            match crate::fs_capability::rename_noreplace(&hidden, path) {
                Ok(()) => error,
                Err(_) => cannot_delete(
                    &hidden,
                    &format!("it changed after the scan and stays under this name: {error}"),
                ),
            },
        );
    }
    delete_renamed(&hidden, layout)
}

fn closed(path: &Path, layout: Layout) -> Result<(), CorpusError> {
    let foreign = foreign_entries_in(path, layout)?;
    if foreign.is_empty() {
        Ok(())
    } else {
        Err(cannot_delete(
            path,
            &format!(
                "it holds entries that are not artifact components: {}",
                foreign.join(", ")
            ),
        ))
    }
}

/// Deletes the components `layout` lists for the directory at `path`, its
/// descriptor last, and then the directory, which therefore fails to delete
/// if anything else appeared in it.
fn delete_renamed(path: &Path, layout: Layout) -> Result<u64, CorpusError> {
    closed(path, layout)?;
    let listed = (layout.components)(path).unwrap_or_default();
    let bytes = artifact_bytes(path)?;
    let delete = |at: &Path, result: std::io::Result<()>| {
        result.map_err(|error| {
            CorpusError::Io(crate::error::io_at("delete", at, error.kind(), error))
        })
    };
    let descriptor = path.join(layout.descriptor);
    for name in &listed {
        let component = path.join(name);
        if component != descriptor && fs::symlink_metadata(&component).is_ok() {
            delete(&component, fs::remove_file(&component))?;
        }
    }
    if fs::symlink_metadata(&descriptor).is_ok() {
        delete(&descriptor, fs::remove_file(&descriptor))?;
    }
    delete(path, fs::remove_dir(path))?;
    Ok(bytes)
}

fn realization(path: PathBuf, manifest: &Manifest) -> Result<SiblingRealization, CorpusError> {
    let inputs = manifest.analysis_inputs()?;
    Ok(SiblingRealization {
        path,
        scheme: manifest.identity_scheme,
        format_generation: manifest.format_generation,
        source_identity: manifest.source_identity()?,
        analysis_identity: manifest.analysis_identity()?,
        analyzer: manifest.analyzer.clone(),
        reader: inputs.reader(),
        admission: inputs.admission(),
        token_schema: inputs.token_schema(),
        dictionary: manifest.dictionary_fingerprint()?,
        trust: TrustClass::parse(&manifest.trust_class)?,
        document_count: manifest.document_count,
        sentence_count: manifest.sentence_count,
        token_count: manifest.token_count,
        sentence_truncation: manifest.sentence_truncation,
    })
}

/// What a build asks the scan about.
pub struct RealizationRequest<'a> {
    pub source_identity: SourceIdentity,
    pub analysis: &'a AnalysisInputs,
    pub analyzer: &'a AnalyzerRecord,
    pub trust: TrustClass,
    /// The canonical destination this request would publish to.
    pub destination: &'a Path,
}

/// The four closed outcomes of comparing a request against what is present.
#[derive(Debug)]
pub enum BuildOutcome {
    /// A realization already answers this exact request; reuse its path.
    ///
    /// Reuse names what exists rather than what
    /// this package would have written.
    ExactReuse(PathBuf),
    /// Realizations of the same source under a different analysis.
    SiblingOf(Vec<SiblingRealization>),
    /// No reusable artifact or same-source sibling is present.
    Fresh,
    /// The canonical destination exists and records a contradictory identity.
    ContradictoryDestination(PathBuf),
}

/// Classification of a request against a completed scan.
pub struct Realization;

impl Realization {
    /// Decides what a build should do, given everything present beside the
    /// source.
    ///
    /// Reuse requires source, reader, admission, recorded analyzer semantics,
    /// token schema and trust to all match, the candidate's name to be the
    /// canonical name of what its manifest records, **and** the candidate to pass
    /// [`CorpusArtifact::open`] structural admission. A manifest read alone
    /// would hand back an artifact whose token or location component is
    /// missing or truncated: the manifest is the part that parsed, not the
    /// part that has to be there. `open` is run on the single candidate about
    /// to be called reuse, not on every sibling, so a large directory costs one
    /// admission rather than N.
    ///
    /// It does not hash components. "Verifies" here means structurally
    /// admissible, and complete verification stays `corpus inspect --verify`.
    pub fn classify(request: &RealizationRequest<'_>, report: &SiblingReport) -> BuildOutcome {
        let same_source = report.for_source(&request.source_identity);
        let exact = same_source.iter().copied().find(|realization| {
            realization.is_canonically_named()
                && realization.reader == request.analysis.reader()
                && realization.admission == request.analysis.admission()
                && realization.token_schema == request.analysis.token_schema()
                && realization.trust == request.trust
                && realization.analyzer.semantic_identity == request.analyzer.semantic_identity
        });
        if let Some(realization) = exact
            && CorpusArtifact::open(&realization.path).is_ok()
        {
            return BuildOutcome::ExactReuse(realization.path.clone());
        }

        // The destination is only contradictory if something is actually
        // there. A readable realization at that path would have been the exact
        // match above, so reaching here with it present means it records
        // something this request did not ask for.
        if request.destination.exists() {
            return BuildOutcome::ContradictoryDestination(request.destination.to_path_buf());
        }

        if !same_source.is_empty() {
            return BuildOutcome::SiblingOf(same_source.into_iter().cloned().collect());
        }
        BuildOutcome::Fresh
    }
}
