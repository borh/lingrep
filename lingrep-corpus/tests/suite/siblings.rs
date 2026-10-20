// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
//! Contracts for the one scan behind both build reuse and `corpus variants`.
//!
//! Matching current artifacts are reused; differing analyses remain siblings.

use std::{fs, path::Path, path::PathBuf, sync::Arc};

use lingrep_core::{
    analysis::{
        AnalyzerDescriptor, AnalyzerFamily, ExtractionProfile, NormalizationPolicy,
        SegmentationMode,
    },
    spill::TempBudget,
    token_match::TokenSchema,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, AnalyzerRecord, BuildConfig, BuildOutcome, CorpusBuilder,
    DictionaryFingerprint, IdentityScheme, ReaderIdentity, Realization, RealizationRequest,
    SiblingScan, SourceIdentity, TrustClass, canonical_artifact_path, vibrato_fixture_descriptor,
};

use crate::artifact::{add_document, discovery_for};
use crate::support::source_document;

/// A descriptor differing from the fixture only in output configuration.
///
/// Identical resources, different emitted segmentation — the shape of a
/// Sudachi split mode, and the case the old dictionary-fingerprint keying
/// could not see.
fn variant_descriptor() -> AnalyzerDescriptor {
    AnalyzerDescriptor::new(
        AnalyzerFamily::Vibrato,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
        "ja",
        "input-unit",
        [0xf1; 32],
        [0xf2; 32],
        [0xf3; 32],
        [0xf4; 32],
        [0xf5; 32],
        [0xaa; 32],
        TokenSchema::unidic_morphology().digest(),
    )
    .unwrap()
}

fn inputs(descriptor: &AnalyzerDescriptor) -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::from_bytes([1; 32]),
        AdmissionIdentity::from_bytes([2; 32]),
        descriptor,
    )
}

struct Built {
    path: PathBuf,
    source: SourceIdentity,
}

/// Builds one artifact and moves it to the canonical name for `scheme`.
///
/// The name is derived from the identity the build recorded, so the fixture
/// exercises real addressing rather than a hand-placed directory. A current
/// fixture is produced by editing the published manifest: nothing in this
/// package derives a current identity, deliberately, so a stored current shape
/// is the only way one exists.
async fn build_realization(
    directory: &Path,
    root: &Path,
    descriptor: &AnalyzerDescriptor,
    content: &[u8],
) -> Built {
    let staging = directory.join(format!("staging-{}.lingcorpus", content.len()));
    let staging = unique(staging);
    let documents = [source_document(0, "a.md", 0, false)];
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            analysis: inputs(descriptor),
            analyzer: AnalyzerRecord::from_descriptor(descriptor),
            dictionary: DictionaryFingerprint::from_bytes([3; 32]),
            trust: TrustClass::LocallyExecuted,
            temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
            sentence_metadata_keys: Vec::new(),
        },
        &staging,
    )
    .await
    .unwrap();
    add_document(&mut builder, &documents[0], content, 0).await;
    let artifact = builder.finish(discovery_for(&documents)).await.unwrap();
    let source = *artifact.source_identity();
    let path = canonical_artifact_path(
        root,
        artifact.analysis_identity(),
        TrustClass::LocallyExecuted,
    );
    drop(artifact);
    fs::rename(&staging, &path).unwrap();

    Built { path, source }
}

fn unique(path: PathBuf) -> PathBuf {
    let mut candidate = path.clone();
    let mut index = 0;
    while candidate.exists() {
        index += 1;
        candidate = path.with_file_name(format!("staging-{index}.lingcorpus"));
    }
    candidate
}

fn request<'a>(
    source: SourceIdentity,
    analysis: &'a AnalysisInputs,
    analyzer: &'a AnalyzerRecord,
    destination: &'a Path,
) -> RealizationRequest<'a> {
    RealizationRequest {
        source_identity: source,
        analysis,
        analyzer,
        trust: TrustClass::LocallyExecuted,
        destination,
    }
}

/// `Path::parent` returns `Some("")` for a bare relative name, not `None`, so
/// an `unwrap_or(".")` fallback never fires and the scan is handed a path that
/// cannot be opened. Both callers hit this the first time anyone ran a command
/// from the corpus's own directory.
#[test]
fn a_bare_relative_name_resolves_to_the_current_directory() {
    use lingrep_corpus::containing_directory;
    assert_eq!(
        containing_directory(Path::new("corpus.jsonl")),
        Path::new(".")
    );
    assert_eq!(containing_directory(Path::new("corpus")), Path::new("."));
    assert_eq!(
        containing_directory(Path::new("data/corpus.jsonl")),
        Path::new("data")
    );
    assert_eq!(
        containing_directory(Path::new("/data/corpus.jsonl")),
        Path::new("/data")
    );
    assert_eq!(containing_directory(Path::new("/")), Path::new("."));
}

/// A canonical destination this request would publish to but which does not
/// exist, so classification is decided by what the scan found.
fn fresh_destination(directory: &Path) -> PathBuf {
    directory.join("absent.g6.a1.aa.locally-executed.lingcorpus")
}

/// The property the whole stream exists for: a current artifact whose analyzer
/// is unchanged is reused, at its own current path.
#[tokio::test]
async fn an_exact_current_realization_is_reused_at_its_current_path() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    let built = build_realization(directory.path(), &root, &descriptor, b"alpha").await;

    let report = SiblingScan::scan(directory.path()).unwrap();
    assert_eq!(report.realizations.len(), 1);
    assert_eq!(report.realizations[0].scheme, IdentityScheme::V1);
    assert!(report.diagnostics.is_empty());

    let analysis = inputs(&descriptor);
    let analyzer = AnalyzerRecord::from_descriptor(&descriptor);
    let destination = fresh_destination(directory.path());
    match Realization::classify(
        &request(built.source, &analysis, &analyzer, &destination),
        &report,
    ) {
        BuildOutcome::ExactReuse(path) => assert_eq!(path, built.path),
        other => panic!("expected reuse of the current artifact, got {other:?}"),
    }
}

/// A current artifact whose recorded analyzer differs is a sibling, never reuse.
#[tokio::test]
async fn a_current_realization_with_a_different_analyzer_is_never_reused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let stored = variant_descriptor();
    let built = build_realization(directory.path(), &root, &stored, b"alpha").await;

    let report = SiblingScan::scan(directory.path()).unwrap();
    let requested = vibrato_fixture_descriptor();
    let analysis = inputs(&requested);
    let analyzer = AnalyzerRecord::from_descriptor(&requested);
    let destination = fresh_destination(directory.path());
    match Realization::classify(
        &request(built.source, &analysis, &analyzer, &destination),
        &report,
    ) {
        BuildOutcome::SiblingOf(siblings) => {
            assert_eq!(siblings.len(), 1);
            assert_eq!(siblings[0].path, built.path);
            assert_ne!(
                siblings[0].analyzer.semantic_identity,
                analyzer.semantic_identity
            );
        }
        other => panic!("expected a sibling report, got {other:?}"),
    }
}

/// Different source content at the same local name is an older snapshot, not a
/// sibling of this analysis.
#[tokio::test]
async fn changed_source_content_requires_a_fresh_realization() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    build_realization(directory.path(), &root, &descriptor, b"alpha").await;
    // A second observation of different bytes: the identity a build over the
    // edited source would carry.
    let elsewhere = tempfile::tempdir().unwrap();
    let other = build_realization(
        elsewhere.path(),
        &elsewhere.path().join("corpus.jsonl"),
        &descriptor,
        b"changed content",
    )
    .await;

    let report = SiblingScan::scan(directory.path()).unwrap();
    let analysis = inputs(&descriptor);
    let analyzer = AnalyzerRecord::from_descriptor(&descriptor);
    let destination = fresh_destination(directory.path());
    match Realization::classify(
        &request(other.source, &analysis, &analyzer, &destination),
        &report,
    ) {
        BuildOutcome::Fresh => assert_eq!(report.realizations.len(), 1),
        other => panic!("expected a fresh realization, got {other:?}"),
    }
}

/// One malformed sibling must not blind the scan to every realization beside
/// it. A single truncated directory would otherwise turn into a silent rebuild
/// of a corpus that is present and fine.
#[tokio::test]
async fn a_damaged_sibling_is_a_diagnostic_and_the_rest_still_appear() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    let good = build_realization(directory.path(), &root, &descriptor, b"alpha").await;
    let damaged = build_realization(
        directory.path(),
        &directory.path().join("other.jsonl"),
        &variant_descriptor(),
        b"alpha",
    )
    .await;
    fs::write(damaged.path.join("manifest.json"), b"{ truncated").unwrap();

    let report = SiblingScan::scan(directory.path()).unwrap();
    assert_eq!(
        report.realizations.len(),
        1,
        "the readable sibling survives"
    );
    assert_eq!(report.realizations[0].path, good.path);
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].0, damaged.path);

    // And the good one is still reusable despite its damaged neighbour.
    let analysis = inputs(&descriptor);
    let analyzer = AnalyzerRecord::from_descriptor(&descriptor);
    let destination = fresh_destination(directory.path());
    assert!(matches!(
        Realization::classify(
            &request(good.source, &analysis, &analyzer, &destination),
            &report
        ),
        BuildOutcome::ExactReuse(_)
    ));
}

/// A manifest that parses is not an artifact that opens. Reuse runs structural
/// admission on the one candidate it is about to hand back, so a missing or
/// truncated token component is not reuse.
#[tokio::test]
async fn a_sibling_whose_component_is_truncated_is_not_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    let built = build_realization(directory.path(), &root, &descriptor, b"alpha").await;
    let tokens = built.path.join("tokens.v1.zst");
    let mut bytes = fs::read(&tokens).unwrap();
    bytes.truncate(bytes.len() - 16);
    fs::write(&tokens, bytes).unwrap();

    let report = SiblingScan::scan(directory.path()).unwrap();
    assert_eq!(
        report.realizations.len(),
        1,
        "its manifest still parses, which is exactly the trap"
    );

    let analysis = inputs(&descriptor);
    let analyzer = AnalyzerRecord::from_descriptor(&descriptor);
    let destination = fresh_destination(directory.path());
    assert!(
        !matches!(
            Realization::classify(
                &request(built.source, &analysis, &analyzer, &destination),
                &report
            ),
            BuildOutcome::ExactReuse(_)
        ),
        "a candidate that cannot open is not reuse"
    );
}

/// Reuse decides on manifests and one structural admission. It does not hash
/// components; that stays `corpus inspect --verify`.
#[tokio::test]
async fn reuse_does_not_hash_components() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    let built = build_realization(directory.path(), &root, &descriptor, b"alpha").await;

    // Corrupt a lazily-decoded token block while preserving the component's
    // recorded length and its fixed headers. A hashing reuse check would
    // reject this; a structural one admits it, and `corpus inspect --verify`
    // is what catches it. This is the same mutation the adversarial-state
    // suite files under `component_checksum_mismatch` — there it is run
    // through open *and* verify, and only verify rejects it.
    let tokens = built.path.join("tokens.v1.zst");
    let mut bytes = fs::read(&tokens).unwrap();
    let schema_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    bytes[128 + schema_len] ^= 1;
    fs::write(&tokens, bytes).unwrap();

    let report = SiblingScan::scan(directory.path()).unwrap();
    let analysis = inputs(&descriptor);
    let analyzer = AnalyzerRecord::from_descriptor(&descriptor);
    let destination = fresh_destination(directory.path());
    assert!(matches!(
        Realization::classify(
            &request(built.source, &analysis, &analyzer, &destination),
            &report
        ),
        BuildOutcome::ExactReuse(_)
    ));
}

/// A canonical destination that exists while nothing matches the request is a
/// contradiction, and a build must fail rather than overwrite.
#[tokio::test]
async fn an_occupied_destination_with_a_contradictory_identity_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let stored = variant_descriptor();
    let built = build_realization(directory.path(), &root, &stored, b"alpha").await;

    let report = SiblingScan::scan(directory.path()).unwrap();
    let requested = vibrato_fixture_descriptor();
    let analysis = inputs(&requested);
    let analyzer = AnalyzerRecord::from_descriptor(&requested);
    match Realization::classify(
        &request(built.source, &analysis, &analyzer, &built.path),
        &report,
    ) {
        BuildOutcome::ContradictoryDestination(path) => assert_eq!(path, built.path),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The scan reads immediate entries only, skips non-artifact names silently,
/// and refuses a symlinked root outright — an unknown answer is not an empty
/// one.
#[tokio::test]
async fn the_scan_is_bounded_non_recursive_and_refuses_a_symlinked_root() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.jsonl");
    let descriptor = vibrato_fixture_descriptor();
    build_realization(directory.path(), &root, &descriptor, b"alpha").await;

    fs::write(directory.path().join("notes.txt"), b"unrelated").unwrap();
    fs::create_dir(directory.path().join("plain-directory")).unwrap();
    fs::write(
        directory.path().join("decoy.lingcorpus"),
        b"a file, not a directory",
    )
    .unwrap();
    let nested = directory.path().join("nested");
    fs::create_dir(&nested).unwrap();
    build_realization(&nested, &nested.join("inner.jsonl"), &descriptor, b"alpha").await;

    let report = SiblingScan::scan(directory.path()).unwrap();
    assert_eq!(
        report.realizations.len(),
        1,
        "the nested realization is not this directory's"
    );
    assert!(
        report.diagnostics.is_empty(),
        "skipped names are not defects"
    );

    let link = directory.path().join("link");
    std::os::unix::fs::symlink(directory.path(), &link).unwrap();
    assert!(SiblingScan::scan(&link).is_err());
}
