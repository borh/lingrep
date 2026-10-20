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
use crate::support::source_document;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lingrep_core::{
    corpus::{
        AdmittedCorpusEvent, DiscoveryReport, DocumentId, InventoryStats, TraversalHighWater,
        TraversalLimits,
    },
    spill::TempBudget,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusArtifact, CorpusBuilder, CorpusError,
    DictionaryFingerprint, ReaderIdentity,
};

fn build_config() -> BuildConfig {
    BuildConfig {
        analysis: AnalysisInputs::new(
            ReaderIdentity::from_bytes([1; 32]),
            AdmissionIdentity::from_bytes([2; 32]),
            &lingrep_corpus::vibrato_fixture_descriptor(),
        ),
        analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
        dictionary: DictionaryFingerprint::from_bytes([3; 32]),
        trust: lingrep_corpus::TrustClass::LocallyExecuted,
        temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
        sentence_metadata_keys: Vec::new(),
    }
}

fn discovery_report() -> DiscoveryReport {
    let mut digest = blake3::Hasher::new();
    digest.update(&("science.md".len() as u64).to_le_bytes());
    digest.update(b"science.md");
    DiscoveryReport {
        inventory: InventoryStats {
            documents: 1,
            distinct_targets: 1,
            alias_documents: 0,
            max_target_multiplicity: 1,
            external_targets: 0,
        },
        configured_limits: TraversalLimits::default(),
        observed: TraversalHighWater {
            max_depth: 0,
            examined_entries: 1,
            max_directory_entries: 1,
            index_bytes: 128,
        },
        ordered_inventory_digest: *digest.finalize().as_bytes(),
    }
}

async fn complete_builder(destination: &Path) -> CorpusBuilder {
    let source = source_document(0, "science.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), destination)
        .await
        .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&source.logical_name).unwrap();
    observation.update_source_bytes(b"source bytes").unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
            id: DocumentId::new(0),
            source_name: source.logical_name,
            metadata: Box::new([]),
        })
        .await
        .unwrap();
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
            id: DocumentId::new(0),
        })
        .await
        .unwrap();
    builder
}

fn staging_entries(parent: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(".staging-"))
        })
        .collect()
}

#[tokio::test]
async fn finish_publishes_one_complete_immutable_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("science.lingcorpus");
    let builder = complete_builder(&destination).await;

    let artifact = builder.finish(discovery_report()).await.unwrap();

    assert_eq!(artifact.documents().count(), 1);
    assert_eq!(
        artifact
            .verify_integrity()
            .await
            .unwrap()
            .compared_location_rows,
        0
    );
    assert!(destination.join("manifest.json").is_file());
    assert!(staging_entries(directory.path()).is_empty());
}

#[tokio::test]
async fn no_replace_preserves_preexisting_empty_and_nonempty_directories() {
    for with_marker in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        std::fs::create_dir(&destination).unwrap();
        let marker = destination.join("owned-by-another-writer");
        if with_marker {
            std::fs::write(&marker, b"keep me").unwrap();
        }
        let builder = complete_builder(&destination).await;

        let error = match builder.finish(discovery_report()).await {
            Ok(_) => panic!("publication replaced an existing destination"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            CorpusError::DestinationExists { ref path } if path == &destination
        ));
        assert!(destination.is_dir());
        assert_eq!(marker.exists(), with_marker);
        if with_marker {
            assert_eq!(std::fs::read(&marker).unwrap(), b"keep me");
        } else {
            assert!(std::fs::read_dir(&destination).unwrap().next().is_none());
        }
        assert!(staging_entries(directory.path()).is_empty());
    }
}

#[tokio::test]
async fn two_builders_for_one_absent_destination_publish_at_most_one() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("science.lingcorpus");
    let first = complete_builder(&destination).await;
    let second = complete_builder(&destination).await;

    let (first, second) = tokio::join!(
        first.finish(discovery_report()),
        second.finish(discovery_report())
    );

    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let loser = if let Err(error) = first {
        error
    } else if let Err(error) = second {
        error
    } else {
        panic!("both builders published the same destination")
    };
    assert!(matches!(
        loser,
        CorpusError::DestinationExists { ref path } if path == &destination
    ));
    CorpusArtifact::open(&destination)
        .unwrap()
        .verify_integrity()
        .await
        .unwrap();
    assert!(staging_entries(directory.path()).is_empty());
}
