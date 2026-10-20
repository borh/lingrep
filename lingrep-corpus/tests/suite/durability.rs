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
//! Every artifact generation and identity scheme this package claims to read,
//! proved against a stored artifact rather than against prose.
//!
//! A durability promise is worth nothing as documentation. These fixtures are
//! bytes on disk that were published by a real build; the tests open, inspect
//! and query each one. Retiring a fixture is governed by
//! `scripts/check-durability-fixtures.sh`, which requires the same change to
//! add a correspondingly named migration module.
//!
//! Regenerate with:
//!
//! ```text
//! cargo test -p lingrep-corpus --test suite -- durability::regenerate_fixtures \
//!     --ignored --exact
//! ```

use std::{fs, path::Path, path::PathBuf, sync::Arc};

use lingrep_core::{
    analysis::{ExtractionProfile, NormalizationPolicy, SegmentationMode, VibratoPlan},
    execution::{ExecutionLimits, SentenceBatchSource},
    spill::TempBudget,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, AnalyzerRecord, BuildConfig, CorpusArtifact, CorpusBuilder,
    DictionaryFingerprint, IdentityScheme, ReaderIdentity, TrustClass,
};

use crate::artifact::{add_document, discovery_for};
use crate::support::source_document;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/durability")
}

/// Every generation-and-scheme pair the lingrep reader admits.
const RETAINED: &[&str] = &["g2-a1"];

/// The descriptor a durability fixture is built with.
///
/// `VibratoPlan::descriptor_for` is the production construction path — the
/// same function `lingrep-web` and `corpus build` reach. Its only synthetic input is
/// the dictionary digest, which no test can supply honestly anyway.
fn fixture_descriptor() -> lingrep_core::analysis::AnalyzerDescriptor {
    VibratoPlan::descriptor_for(
        *blake3::hash(b"lingrep-corpus/durability-fixture-dictionary/v1").as_bytes(),
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::Nfc,
    )
}

#[test]
fn every_retained_generation_and_scheme_is_present() {
    for name in RETAINED {
        let path = fixture_root().join(name);
        assert!(
            path.is_dir(),
            "durability fixture {name} is missing. \
             Removing one is a durability retirement, not a cleanup: see \
             scripts/check-durability-fixtures.sh"
        );
    }
}

#[tokio::test]
async fn every_retained_fixture_opens_inspects_and_answers_a_query() {
    for name in RETAINED {
        let path = fixture_root().join(name);
        let artifact = CorpusArtifact::open(&path)
            .unwrap_or_else(|error| panic!("durability fixture {name} does not open: {error}"));

        // Inspect: the recorded facts a reader depends on.
        assert_eq!(artifact.identity_scheme(), expected_scheme(name), "{name}");
        assert!(artifact.sentence_count() > 0, "{name}");
        assert!(artifact.document_count() > 0, "{name}");

        // Token format and artifact generation are independent version axes.
        let tokens = fs::read(path.join("tokens.v1.zst")).unwrap();
        assert_eq!(
            &tokens[0..8],
            b"LINGTOK1",
            "{name} does not store the lingrep V1 token format"
        );
        assert_eq!(
            expected_generation(name),
            lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION,
            "{name}"
        );

        // Full component verification, which `open` deliberately does not do.
        let report = artifact
            .verify_integrity()
            .await
            .unwrap_or_else(|error| panic!("durability fixture {name} fails verify: {error}"));
        assert!(
            report
                .components
                .iter()
                .all(|component| component.length_matches && component.blake3_matches),
            "{name}: {report:?}"
        );
        assert_eq!(
            report.compared_location_rows,
            artifact.sentence_count(),
            "{name}"
        );

        // One schema-bound query: the artifact is not merely readable, it
        // answers.
        let schema = artifact.token_schema().unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let mut source = artifact.bound_sentence_source(&projection).unwrap();
        let batch = source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .unwrap_or_else(|| panic!("durability fixture {name} yielded no sentences"));
        // A projection-bound prepared source yields bound token sentences.
        let lingrep_core::execution::ExecutionBatch::BoundTokenized { sentences, .. } = batch
        else {
            panic!("{name}: a bound prepared source yields bound tokenized sentences");
        };
        assert!(!sentences.is_empty(), "{name}");
        assert!(
            sentences
                .iter()
                .any(|sentence| !sentence.value.words().is_empty()),
            "{name}: every sentence decoded empty"
        );
    }
}

/// A fixture must carry a descriptor a production build could emit.
///
/// `AnalyzerRecord::vibrato_fixture()` is hand-assembled from digests no
/// adapter produces. An artifact carrying it proves the storage layout works
/// and proves nothing about the descriptor comparison these fixtures exist to
/// protect — the comparison would be against a value that can never appear on
/// the other side.
#[test]
fn no_retained_fixture_was_built_from_the_storage_fixture_record() {
    let refused = AnalyzerRecord::vibrato_fixture().semantic_identity;
    for name in RETAINED {
        let artifact = CorpusArtifact::open(&fixture_root().join(name)).unwrap();
        let recorded = &artifact.analyzer().semantic_identity;
        assert_ne!(
            recorded, &refused,
            "{name} was built from AnalyzerRecord::vibrato_fixture()"
        );
        assert_ne!(recorded, &"00".repeat(32), "{name} records a null analyzer");
    }
}

#[test]
fn generation_one_requires_a_rebuild() {
    assert!(matches!(
        CorpusArtifact::open(&fixture_root().join("g1-a1")),
        Err(lingrep_corpus::CorpusError::UnsupportedArtifactGeneration { generation: 1 })
    ));
}

fn expected_generation(name: &str) -> u32 {
    name.split('-')
        .next()
        .and_then(|part| part.strip_prefix('g'))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or_else(|| panic!("fixture name {name} does not encode a generation"))
}

fn expected_scheme(name: &str) -> IdentityScheme {
    match name.split_once('-').map(|(_, scheme)| scheme) {
        Some("a1") => IdentityScheme::V1,
        other => panic!("fixture name {name} encodes an unknown scheme: {other:?}"),
    }
}

/// Rebuilds fixtures for the current generation from scratch. Older fixtures
/// are immutable evidence and must never be rewritten by the current writer.
#[tokio::test]
#[ignore = "writes tracked fixtures; run explicitly to regenerate"]
async fn regenerate_fixtures() {
    for name in RETAINED {
        if expected_generation(name) != lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION {
            continue;
        }
        let destination = fixture_root().join(name);
        if destination.exists() {
            fs::remove_dir_all(&destination).unwrap();
        }
        fs::create_dir_all(destination.parent().unwrap()).unwrap();

        let descriptor = fixture_descriptor();
        let documents = [source_document(0, "durable.md", 0, false)];
        let mut builder = CorpusBuilder::begin(
            BuildConfig {
                analysis: AnalysisInputs::new(
                    ReaderIdentity::from_bytes(*blake3::hash(b"durability/reader").as_bytes()),
                    AdmissionIdentity::from_bytes(
                        *blake3::hash(b"durability/admission").as_bytes(),
                    ),
                    &descriptor,
                ),
                analyzer: AnalyzerRecord::from_descriptor(&descriptor),
                dictionary: DictionaryFingerprint::from_bytes(
                    *blake3::hash(b"lingrep-corpus/durability-fixture-dictionary/v1").as_bytes(),
                ),
                trust: TrustClass::LocallyExecuted,
                temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
                sentence_metadata_keys: Vec::new(),
            },
            &destination,
        )
        .await
        .unwrap();
        add_document(&mut builder, &documents[0], "durable text".as_bytes(), 0).await;
        builder.finish(discovery_for(&documents)).await.unwrap();
    }
}
