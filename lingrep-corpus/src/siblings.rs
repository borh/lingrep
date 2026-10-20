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
    /// token schema and trust to all match **and** the candidate to pass
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
            realization.reader == request.analysis.reader()
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
