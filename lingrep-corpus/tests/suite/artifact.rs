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
    cell::Cell,
    collections::HashMap,
    fs,
    os::unix::fs::symlink,
    path::{Path, PathBuf},
    sync::Arc,
};

use lingrep_core::corpus::{
    AdmittedCorpusEvent, AdmittedSentence, CorpusDiscovery, DiscoveryReport, DocumentId,
    InventoryStats, Metadata, MetadataScalar, MetadataValue, SentenceId, SourceDocument,
    TraversalHighWater, TraversalLimits, conllu,
};
use lingrep_core::execution::{
    EnhancedGraphInput, ExecutionBatch, ExecutionLimits, SentenceBatchSource,
};
use lingrep_core::spill::TempBudget;
use lingrep_core::token_match::{AttributeKey, TokenAttributeIndex, TokenInfo};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisIdentity, AnalysisInputs, BuildConfig,
    CORPUS_ARTIFACT_FORMAT_GENERATION, CorpusArtifact, CorpusBuilder, CorpusError,
    DictionaryFingerprint, IdentityError, IdentityScheme, ReaderIdentity, TrustClass,
    canonical_artifact_path, format_identity, parse_artifact_name,
};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use tempfile::TempDir;

const ENHANCED_FIXTURE: &str = "# text = cannot\n1\tcan\tcan\tAUX\tMD\t_\t0\troot\t0:root\t_\n2\tnot\tnot\tPART\tRB\t_\t1\tadvmod\t1:advmod\t_\n2.1\treally\treally\tADV\tRB\tDegree=Pos\t_\t_\t1:advmod\tSpaceAfter=No\n\n";

fn analysis_inputs() -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::from_bytes([1; 32]),
        AdmissionIdentity::from_bytes([2; 32]),
        &lingrep_corpus::vibrato_fixture_descriptor(),
    )
}

fn build_config() -> BuildConfig {
    BuildConfig {
        analysis: analysis_inputs(),
        analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
        dictionary: DictionaryFingerprint::from_bytes([3; 32]),
        trust: TrustClass::LocallyExecuted,
        temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
        sentence_metadata_keys: Vec::new(),
    }
}

#[tokio::test]
async fn high_cardinality_target_indexes_are_charged_before_scratch_growth() {
    const GROUPS: u64 = 4_096;
    const BYTES_PER_GROUP: u64 = 16;

    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("high-cardinality.lingcorpus");
    let budget = Arc::new(TempBudget::new(GROUPS * BYTES_PER_GROUP));
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            temp_budget: Arc::clone(&budget),
            ..build_config()
        },
        &destination,
    )
    .await
    .unwrap();

    for ordinal in 0..GROUPS {
        builder
            .register_discovered_source(&source_document(
                ordinal,
                &format!("{ordinal:05}.md"),
                ordinal,
                false,
            ))
            .unwrap();
    }
    assert_eq!(budget.used_bytes(), GROUPS * BYTES_PER_GROUP);
    assert_eq!(budget.peak_used_bytes(), GROUPS * BYTES_PER_GROUP);

    let error = builder
        .register_discovered_source(&source_document(GROUPS, "overflow.md", GROUPS, false))
        .unwrap_err();
    assert_eq!(error.code(), "temp_capacity");
    assert_eq!(
        budget.used_bytes(),
        GROUPS * BYTES_PER_GROUP,
        "a rejected group must not leave unreserved scratch bytes"
    );
}

#[tokio::test]
async fn high_cardinality_admitted_targets_use_only_budgeted_indexes() {
    const GROUPS: u64 = 4_096;
    const DISCOVERY_INDEX_BYTES: u64 = 16;
    const ADMITTED_TARGET_BYTES: u64 = 8;
    const DOCUMENT_ROW_BYTES: u64 = 48 + 8;
    const BYTES_PER_GROUP: u64 = DISCOVERY_INDEX_BYTES + ADMITTED_TARGET_BYTES + DOCUMENT_ROW_BYTES;

    let directory = tempfile::tempdir().unwrap();
    let destination = directory
        .path()
        .join("admitted-high-cardinality.lingcorpus");
    let budget = Arc::new(TempBudget::new(GROUPS * BYTES_PER_GROUP));
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            temp_budget: Arc::clone(&budget),
            ..build_config()
        },
        &destination,
    )
    .await
    .unwrap();

    for ordinal in 0..GROUPS {
        let name = format!("{ordinal:05}.md");
        let physical = source_document(ordinal, &name, ordinal, false);
        builder.register_discovered_source(&physical).unwrap();
        builder
            .register_admitted_document_from_source(ordinal, &name, &physical)
            .unwrap();
        builder
            .push_corpus_event(&document_start(ordinal, &name))
            .await
            .unwrap();
        builder
            .push_corpus_event(&document_end(ordinal))
            .await
            .unwrap();
    }

    assert_eq!(budget.used_bytes(), GROUPS * BYTES_PER_GROUP);
    assert_eq!(budget.peak_used_bytes(), GROUPS * BYTES_PER_GROUP);
    let error = builder
        .register_discovered_source(&source_document(GROUPS, "overflow.md", GROUPS, false))
        .unwrap_err();
    assert_eq!(error.code(), "temp_capacity");
}

fn empty_metadata() -> Metadata {
    Box::new([])
}

fn document_start(id: u64, name: &str) -> AdmittedCorpusEvent {
    AdmittedCorpusEvent::DocumentStart {
        id: DocumentId::new(id),
        source_name: name.into(),
        metadata: empty_metadata(),
    }
}

fn paragraph_start(document_id: u64) -> AdmittedCorpusEvent {
    AdmittedCorpusEvent::DivisionStart(std::sync::Arc::new(lingrep_core::corpus::Division {
        source_id: None,
        id: lingrep_core::corpus::DivisionId(document_id),
        document_id: DocumentId::new(document_id),
        kind: "paragraph".into(),
        parent_id: None,
        metadata: empty_metadata(),
    }))
}

fn sentence(sentence: u64, document: u64, local: u64) -> AdmittedCorpusEvent {
    AdmittedCorpusEvent::Sentence(AdmittedSentence {
        structure: Default::default(),
        id: SentenceId::new(sentence),
        document_id: DocumentId::new(document),
        document_sentence_ordinal: local,
        text: format!("sentence {sentence}"),
        words: None,
        enhanced_graph: None,
        active_labels: Box::new([]),
        metadata: Box::new([]),
    })
}

fn paragraph_end(document_id: u64) -> AdmittedCorpusEvent {
    AdmittedCorpusEvent::DivisionEnd {
        id: lingrep_core::corpus::DivisionId(document_id),
    }
}

fn document_end(document_id: u64) -> AdmittedCorpusEvent {
    AdmittedCorpusEvent::DocumentEnd {
        id: DocumentId::new(document_id),
    }
}

fn inventory_digest(names: &[&str]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    for name in names {
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
    }
    *hash.finalize().as_bytes()
}

pub(crate) fn discovery_for(documents: &[SourceDocument]) -> DiscoveryReport {
    let mut groups = HashMap::<u64, u64>::new();
    let mut external_targets = 0;
    for document in documents {
        *groups.entry(document.target_group_ordinal).or_default() += 1;
        external_targets += u64::from(document.external_target);
    }
    let document_count = documents.len() as u64;
    let distinct_targets = groups.len() as u64;
    DiscoveryReport {
        inventory: InventoryStats {
            documents: document_count,
            distinct_targets,
            alias_documents: document_count - distinct_targets,
            max_target_multiplicity: groups.values().copied().max().unwrap_or(0),
            external_targets,
        },
        configured_limits: TraversalLimits::default(),
        observed: TraversalHighWater {
            max_depth: 2,
            examined_entries: document_count + 3,
            max_directory_entries: documents.len(),
            index_bytes: documents.len() * 128,
        },
        ordered_inventory_digest: inventory_digest(
            &documents
                .iter()
                .map(|document| document.logical_name.as_str())
                .collect::<Vec<_>>(),
        ),
    }
}

pub(crate) async fn add_document(
    builder: &mut CorpusBuilder,
    source: &SourceDocument,
    raw: &[u8],
    sentence_ordinal: u64,
) {
    builder.register_source_document(source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&source.logical_name).unwrap();
    observation.update_source_bytes(raw).unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(source.ordinal, &source.logical_name))
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_start(source.ordinal))
        .await
        .unwrap();
    let location = builder
        .push_corpus_event(&sentence(sentence_ordinal, source.ordinal, 0))
        .await
        .unwrap()
        .unwrap();
    builder
        .push_tokenized_sentence(
            &location,
            &[TokenInfo::default()
                .with_orth(format!("token-{sentence_ordinal}"))
                .with_pos("NOUN")],
        )
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_end(source.ordinal))
        .await
        .unwrap();
    builder
        .push_corpus_event(&document_end(source.ordinal))
        .await
        .unwrap();
}

async fn observe_and_add_empty_document(
    builder: &mut CorpusBuilder,
    source: &SourceDocument,
    admitted_ordinal: u64,
) {
    let observation = builder.source_observation();
    observation.begin_source(&source.logical_name).unwrap();
    observation
        .update_source_bytes(source.logical_name.as_bytes())
        .unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(admitted_ordinal, &source.logical_name))
        .await
        .unwrap();
    builder
        .push_corpus_event(&document_end(admitted_ordinal))
        .await
        .unwrap();
}

pub(crate) async fn build_two_document_artifact(parent: &Path) -> PathBuf {
    let destination = parent.join("fixture.lingcorpus");
    let documents = [
        source_document(0, "a.md", 0, false),
        source_document(1, "nested/b.md", 0, true),
    ];
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &documents[0], b"alpha", 0).await;
    add_document(&mut builder, &documents[1], b"beta", 1).await;
    let root = parent.join("fixture.lingcorpus");
    builder.finish(discovery_for(&documents)).await.unwrap();
    root
}

#[tokio::test]
async fn enhanced_graph_component_round_trips_and_stays_lazy_for_morphology() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("enhanced.lingcorpus");
    let descriptor = conllu::analyzer_descriptor();
    let config = BuildConfig {
        analysis: AnalysisInputs::new(
            ReaderIdentity::from_bytes([11; 32]),
            AdmissionIdentity::from_bytes([12; 32]),
            &descriptor,
        ),
        analyzer: lingrep_corpus::AnalyzerRecord::from_descriptor(&descriptor),
        dictionary: DictionaryFingerprint::from_bytes([13; 32]),
        trust: TrustClass::LocallyExecuted,
        temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
        sentence_metadata_keys: vec!["sent_id".into()],
    };
    let schema = conllu::token_schema();
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .unwrap();
    let parsed = conllu::Reader::new(ENHANCED_FIXTURE.as_bytes(), "fixture.conllu")
        .next()
        .unwrap()
        .unwrap();
    let direct = parsed.project(&projection).unwrap();
    let source = source_document(0, "fixture.conllu", 0, false);
    let mut builder = CorpusBuilder::begin_with_schema(config, &destination, schema.clone())
        .await
        .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&source.logical_name).unwrap();
    observation
        .update_source_bytes(ENHANCED_FIXTURE.as_bytes())
        .unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(0, &source.logical_name))
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_start(0))
        .await
        .unwrap();
    for ordinal in 0..2 {
        let event = AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(ordinal),
            document_id: DocumentId::new(0),
            document_sentence_ordinal: ordinal,
            text: parsed.text().to_owned(),
            words: Some(direct.clone()),
            enhanced_graph: Some(if ordinal == 0 {
                parsed.enhanced_graph().clone()
            } else {
                lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable
            }),
            active_labels: Box::new([]),
            metadata: Box::new([]),
        });
        let location = builder.push_corpus_event(&event).await.unwrap().unwrap();
        builder
            .push_bound_sentence(&location, direct.clone(), &projection)
            .await
            .unwrap();
    }
    builder.push_corpus_event(&paragraph_end(0)).await.unwrap();
    builder.push_corpus_event(&document_end(0)).await.unwrap();
    let artifact = builder.finish(discovery_for(&[source])).await.unwrap();

    assert!(artifact.has_enhanced_graphs());
    let text_projection = schema
        .bind_projection([AttributeKey::new("text").unwrap()])
        .unwrap();
    let mut morphology = artifact.bound_sentence_source(&text_projection).unwrap();
    assert_eq!(morphology.enhanced_decoded_block_count(), 0);
    let morphology_batch = morphology
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::BoundTokenized { sentences, .. } = morphology_batch else {
        panic!("morphology projection returned the wrong execution batch");
    };
    assert_eq!(sentences.len(), 2);
    assert_eq!(
        sentences[0].value.enhanced_graph(),
        &EnhancedGraphInput::NotRequested
    );
    assert_eq!(morphology.enhanced_decoded_block_count(), 0);

    let mut graph = artifact.bound_sentence_source(&projection).unwrap();
    let batch = graph
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
        panic!("graph projection returned the wrong execution batch");
    };
    assert_eq!(sentences.len(), 2);
    assert_eq!(sentences[0].value.words(), &direct);
    assert_eq!(
        sentences[0].value.enhanced_graph(),
        &EnhancedGraphInput::Observation(parsed.enhanced_graph().clone())
    );
    assert_eq!(
        sentences[1].value.enhanced_graph(),
        &EnhancedGraphInput::Observation(
            lingrep_core::corpus::conllu::EnhancedGraphObservation::Unavailable
        )
    );
    assert_eq!(graph.enhanced_decoded_block_count(), 1);
    assert_eq!(
        artifact.enhanced_graph_observation(0).unwrap(),
        Some(parsed.enhanced_graph().clone())
    );
    artifact.verify_integrity().await.unwrap();
}

#[tokio::test]
async fn one_observed_jsonl_source_can_back_multiple_admitted_documents() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("jsonl.lingcorpus");
    let physical = source_document(0, "corpus.jsonl", 0, false);
    let admitted = [
        source_document(0, "corpus.jsonl line 1", 0, false),
        source_document(1, "corpus.jsonl line 2", 0, false),
    ];
    let raw = concat!(
        "{\"title\":\"first\",\"sentences\":[\"猫。\"]}\n",
        "{\"title\":\"second\",\"sentences\":[\"犬。\"]}\n",
    );
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&physical.logical_name).unwrap();
    observation.update_source_bytes(raw.as_bytes()).unwrap();
    observation.end_source().unwrap();
    builder.register_discovered_source(&physical).unwrap();

    for (ordinal, document) in admitted.iter().enumerate() {
        builder
            .register_admitted_document_from_source(
                document.ordinal,
                &document.logical_name,
                &physical,
            )
            .unwrap();
        builder
            .push_corpus_event(&document_start(document.ordinal, &document.logical_name))
            .await
            .unwrap();
        builder
            .push_corpus_event(&paragraph_start(document.ordinal))
            .await
            .unwrap();
        let location = builder
            .push_corpus_event(&sentence(ordinal as u64, document.ordinal, 0))
            .await
            .unwrap()
            .unwrap();
        builder
            .push_tokenized_sentence(
                &location,
                &[TokenInfo::default().with_orth(if ordinal == 0 { "猫" } else { "犬" })],
            )
            .await
            .unwrap();
        builder
            .push_corpus_event(&paragraph_end(document.ordinal))
            .await
            .unwrap();
        builder
            .push_corpus_event(&document_end(document.ordinal))
            .await
            .unwrap();
    }

    let artifact = builder.finish(discovery_for(&[physical])).await.unwrap();

    assert_eq!(artifact.inventory_stats().documents, 1);
    assert_eq!(artifact.documents().count(), 2);
    assert_eq!(artifact.sentence_source().unwrap().sentence_count(), 2);
}

#[tokio::test]
async fn expected_analysis_identity_is_checked_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("changed-during-build.lingcorpus");
    let document = source_document(0, "a.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &document, b"alpha", 0).await;

    let error = match builder
        .finish_expected_analysis(
            discovery_for(&[document]),
            AnalysisIdentity::from_bytes([0xff; 32]),
        )
        .await
    {
        Ok(_) => panic!("changed analysis identity was published"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        CorpusError::AnalysisIdentityMismatch {
            component: "manifest.json"
        }
    ));
    assert!(!destination.exists());
}

#[tokio::test]
async fn guarded_finish_cancels_before_the_publication_commit() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("cancelled.lingcorpus");
    let document = source_document(0, "a.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &document, b"alpha", 0).await;

    let result = builder
        .finish_guarded(
            discovery_for(&[document]),
            || false,
            || panic!("a cancelled finalization must not reach the rename claim"),
        )
        .await
        .unwrap();

    assert!(result.is_none());
    assert!(!destination.exists());
    assert!(staging_entries(directory.path()).is_empty());
}

#[tokio::test]
async fn guarded_finish_cancels_during_component_hashing_and_drops_staging() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("cancelled-during-hash.lingcorpus");
    let document = source_document(0, "a.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &document, b"alpha", 0).await;
    let checkpoints = Cell::new(0);

    let result = builder
        .finish_guarded(
            discovery_for(&[document]),
            || {
                let next = checkpoints.get() + 1;
                checkpoints.set(next);
                next < 6
            },
            || panic!("hash cancellation must not reach the rename claim"),
        )
        .await
        .unwrap();

    assert!(result.is_none());
    assert_eq!(checkpoints.get(), 6);
    assert!(!destination.exists());
    assert!(staging_entries(directory.path()).is_empty());
}

fn staging_entries(parent: &Path) -> Vec<PathBuf> {
    fs::read_dir(parent)
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
async fn staging_owner_cleans_dropped_builders_and_failed_or_successful_finish() {
    let directory = tempfile::tempdir().unwrap();
    let sentinel = directory.path().join("unrelated");
    fs::create_dir(&sentinel).unwrap();

    let builder = CorpusBuilder::begin(build_config(), &directory.path().join("drop.lingcorpus"))
        .await
        .unwrap();
    assert_eq!(staging_entries(directory.path()).len(), 1);
    drop(builder);
    assert!(staging_entries(directory.path()).is_empty());

    let document = source_document(0, "unfinished.md", 0, false);
    let mut builder =
        CorpusBuilder::begin(build_config(), &directory.path().join("error.lingcorpus"))
            .await
            .unwrap();
    builder.register_source_document(&document).unwrap();
    builder
        .source_observation()
        .begin_source("unfinished.md")
        .unwrap();
    assert!(builder.finish(discovery_for(&[document])).await.is_err());
    assert!(staging_entries(directory.path()).is_empty());

    let document = source_document(0, "complete.md", 0, false);
    let mut builder = CorpusBuilder::begin(
        build_config(),
        &directory.path().join("complete.lingcorpus"),
    )
    .await
    .unwrap();
    add_document(&mut builder, &document, b"complete", 0).await;
    builder.finish(discovery_for(&[document])).await.unwrap();
    assert!(directory.path().join("complete.lingcorpus").is_dir());
    assert!(staging_entries(directory.path()).is_empty());
    assert!(sentinel.is_dir());
}

#[tokio::test]
async fn tiny_shared_temp_budget_fails_typed_and_cleans_private_staging() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("budget.lingcorpus");
    let temp_budget = Arc::new(TempBudget::new(1));
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            analysis: analysis_inputs(),
            analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
            dictionary: DictionaryFingerprint::from_bytes([3; 32]),
            trust: TrustClass::LocallyExecuted,
            temp_budget: Arc::clone(&temp_budget),
            sentence_metadata_keys: Vec::new(),
        },
        &destination,
    )
    .await
    .unwrap();
    let document = source_document(0, "document-name-needs-scratch.md", 0, false);
    let error = builder.register_source_document(&document).unwrap_err();
    assert_eq!(error.code(), "temp_capacity");
    drop(builder);
    assert!(staging_entries(directory.path()).is_empty());
    assert!(!destination.exists());
    assert_eq!(temp_budget.used_bytes(), 0);
}

#[tokio::test]
async fn manifest_schema_two_fails_before_component_shape_is_decoded() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let mut manifest = read_manifest(&root);
    manifest["schema_version"] = serde_json::json!(2);
    for component in manifest["components"].as_object_mut().unwrap().values_mut() {
        let digest = component.as_object_mut().unwrap().remove("blake3").unwrap();
        component
            .as_object_mut()
            .unwrap()
            .insert("sha256".into(), digest);
    }
    write_manifest(&root, &manifest);

    assert!(matches!(
        CorpusArtifact::open(&root),
        Err(CorpusError::PreparedArtifactRebuildRequired { version: 2 })
    ));
}

#[tokio::test]
async fn document_without_sentences_opens_and_fully_verifies() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("empty-document.lingcorpus");
    let document = source_document(0, "empty.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&document).unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&document.logical_name).unwrap();
    observation.update_source_bytes(b"no sentences").unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(0, &document.logical_name))
        .await
        .unwrap();
    builder.push_corpus_event(&document_end(0)).await.unwrap();
    builder.finish(discovery_for(&[document])).await.unwrap();

    let artifact = CorpusArtifact::open(&destination).unwrap();
    assert_eq!(artifact.documents().count(), 1);
    assert_eq!(
        artifact
            .verify_integrity()
            .await
            .unwrap()
            .compared_location_rows,
        0
    );
}

#[tokio::test]
async fn regular_file_discovery_report_builds_opens_and_verifies_an_artifact() {
    let directory = tempfile::tempdir().unwrap();
    let corpus_path = directory.path().join("single.md");
    let raw = b"one regular-file corpus document";
    fs::write(&corpus_path, raw).unwrap();
    let mut discovery = CorpusDiscovery::new(&corpus_path, TraversalLimits::default()).unwrap();
    let document = discovery.next_document().unwrap().unwrap();
    assert!(discovery.next_document().unwrap().is_none());
    let report = discovery.finish().unwrap();
    assert_eq!(report.inventory.documents, 1);
    assert_eq!(report.observed.examined_entries, 0);

    let destination = directory.path().join("regular-file.lingcorpus");
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &document, raw, 0).await;
    builder.finish(report).await.unwrap();

    let artifact = CorpusArtifact::open(&destination).unwrap();
    assert_eq!(artifact.documents().count(), 1);
    assert_eq!(
        artifact
            .verify_integrity()
            .await
            .unwrap()
            .compared_location_rows,
        1
    );
}

#[tokio::test]
async fn finish_rejects_unfinished_or_divergent_source_inventories() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("unfinished.lingcorpus");
    let document = source_document(0, "a.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&document).unwrap();
    let observed = builder.source_observation();
    observed.begin_source("a.md").unwrap();
    observed.update_source_bytes(b"raw bytes").unwrap();
    assert!(matches!(
        builder.finish(discovery_for(&[document])).await,
        Err(CorpusError::Identity(IdentityError::InvalidState))
    ));

    let destination = directory.path().join("names.lingcorpus");
    let document = source_document(0, "b.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&document).unwrap();
    let observed = builder.source_observation();
    observed.begin_source("a.md").unwrap();
    observed.update_source_bytes(b"raw bytes").unwrap();
    observed.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(0, "b.md"))
        .await
        .unwrap();
    let error = match builder.finish(discovery_for(&[document])).await {
        Ok(_) => panic!("divergent source inventory was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "source_inventory_mismatch");

    let destination = directory.path().join("report.lingcorpus");
    let document = source_document(0, "a.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    add_document(&mut builder, &document, b"raw bytes", 0).await;
    let observed = builder.source_observation();
    let mut report = discovery_for(&[document]);
    report.inventory.documents = 2;
    let error = match builder.finish(report).await {
        Ok(_) => panic!("mismatched discovery report was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "discovery_report_mismatch");
    assert!(matches!(
        observed.update_source_bytes(b"late"),
        Err(IdentityError::InvalidState)
    ));
}

#[tokio::test]
async fn complete_artifact_preserves_alias_inventory_and_can_move() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let before = CorpusArtifact::open(&root).unwrap();
    assert_eq!(
        before.inventory_stats(),
        &InventoryStats {
            documents: 2,
            distinct_targets: 1,
            alias_documents: 1,
            max_target_multiplicity: 2,
            external_targets: 1,
        }
    );
    assert_eq!(
        before.documents().collect::<Result<Vec<_>, _>>().unwrap(),
        vec![
            lingrep_corpus::StoredDocument {
                ordinal: 0,
                logical_name: "a.md".into(),
                target_group_ordinal: 0,
                target_multiplicity: 2,
                external_target: false,
            },
            lingrep_corpus::StoredDocument {
                ordinal: 1,
                logical_name: "nested/b.md".into(),
                target_group_ordinal: 0,
                target_multiplicity: 2,
                external_target: true,
            },
        ]
    );

    let moved = directory.path().join("moved.lingcorpus");
    let source = *before.source_identity();
    let analysis = *before.analysis_identity();
    drop(before);
    fs::rename(&root, &moved).unwrap();
    let after = CorpusArtifact::open(&moved).unwrap();
    assert_eq!(after.source_identity(), &source);
    assert_eq!(after.analysis_identity(), &analysis);
}

#[tokio::test]
async fn convenience_then_mapped_alias_share_one_dense_target_group() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("convenience-then-alias.lingcorpus");
    let documents = [
        source_document(0, "direct.md", 0, false),
        source_document(1, "alias.md", 0, false),
    ];
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();

    builder.register_source_document(&documents[0]).unwrap();
    observe_and_add_empty_document(&mut builder, &documents[0], 0).await;
    builder.register_discovered_source(&documents[1]).unwrap();
    builder
        .register_admitted_document_from_source(1, &documents[1].logical_name, &documents[1])
        .unwrap();
    observe_and_add_empty_document(&mut builder, &documents[1], 1).await;

    let artifact = builder.finish(discovery_for(&documents)).await.unwrap();
    assert_eq!(artifact.inventory_stats().distinct_targets, 1);
    assert_eq!(artifact.inventory_stats().alias_documents, 1);
    assert_eq!(artifact.inventory_stats().max_target_multiplicity, 2);
    let stored = artifact.documents().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|document| (document.target_group_ordinal, document.target_multiplicity))
            .collect::<Vec<_>>(),
        vec![(0, 2), (0, 2)]
    );
}

#[tokio::test]
async fn mapped_alias_then_convenience_share_one_dense_target_group() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("alias-then-convenience.lingcorpus");
    let documents = [
        source_document(0, "alias.md", 0, false),
        source_document(1, "direct.md", 0, false),
    ];
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();

    builder.register_discovered_source(&documents[0]).unwrap();
    builder
        .register_admitted_document_from_source(0, &documents[0].logical_name, &documents[0])
        .unwrap();
    observe_and_add_empty_document(&mut builder, &documents[0], 0).await;
    builder.register_source_document(&documents[1]).unwrap();
    observe_and_add_empty_document(&mut builder, &documents[1], 1).await;

    let artifact = builder.finish(discovery_for(&documents)).await.unwrap();
    assert_eq!(artifact.inventory_stats().distinct_targets, 1);
    assert_eq!(artifact.inventory_stats().alias_documents, 1);
    assert_eq!(artifact.inventory_stats().max_target_multiplicity, 2);
    let stored = artifact.documents().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|document| (document.target_group_ordinal, document.target_multiplicity))
            .collect::<Vec<_>>(),
        vec![(0, 2), (0, 2)]
    );
}

#[tokio::test]
async fn documents_iterates_lazily_and_reports_the_first_corrupt_row_in_place() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let path = root.join("locations.v1");
    let mut bytes = fs::read(&path).unwrap();
    let strings_offset = u64::from_le_bytes(bytes[88..96].try_into().unwrap()) as usize;
    bytes[strings_offset + "a.md".len()] = 0xff;
    fs::write(path, bytes).unwrap();

    let artifact = CorpusArtifact::open(&root).unwrap();
    let mut documents = artifact.documents();
    assert_eq!(documents.next().unwrap().unwrap().logical_name, "a.md");
    assert!(matches!(
        documents.next().unwrap(),
        Err(CorpusError::InvalidFormat {
            component: "locations.v1",
            ..
        })
    ));
}

#[tokio::test]
async fn artifact_composes_complete_tokens_with_authoritative_and_rich_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();

    let mut source = artifact.sentence_source().unwrap();
    let batch = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::Tokenized { sentences, .. } = batch else {
        panic!("prepared source returned text");
    };
    assert_eq!(sentences[0].location.source_name.as_ref(), "a.md");
    assert_eq!(sentences[0].value[0].orth(), Some("token-0"));

    let view = artifact.provenance_view().await.unwrap();
    let provenance = artifact.sentence_provenance(&view, 1).await.unwrap();
    assert_eq!(provenance.location.source_name.as_ref(), "nested/b.md");
    assert_eq!(provenance.location.document_sentence_ordinal, 0);
}

#[tokio::test]
async fn artifact_sentence_source_projects_only_requested_attributes() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();

    let mut source = artifact
        .sentence_source_with_attributes(&[TokenAttributeIndex::Orth])
        .unwrap();
    let batch = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();
    let ExecutionBatch::Tokenized { sentences, .. } = batch else {
        panic!("prepared source returned text");
    };

    assert_eq!(sentences[0].value[0].orth(), Some("token-0"));
    assert_eq!(sentences[0].value[0].pos(), None);
}

#[tokio::test]
async fn artifact_deferred_sentence_source_returns_projected_batches() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();

    let mut source = artifact
        .deferred_sentence_source(&[TokenAttributeIndex::Orth.into()])
        .unwrap();
    let batch = source
        .next_batch(&ExecutionLimits::default())
        .unwrap()
        .unwrap();

    let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
        panic!("deferred source must publish a schema-bound batch");
    };
    assert!(!sentences.is_empty());
    for sentence in &sentences {
        assert_eq!(sentence.value.words().width(), 1);
        assert!(
            sentence
                .value
                .words()
                .token(0)
                .unwrap()
                .get(lingrep_core::token_match::AttributeOrdinal::new(0))
                .is_some()
        );
    }
}

#[tokio::test]
async fn artifact_provenance_page_preserves_positions_duplicates_and_missing_ids() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let view = artifact.provenance_view().await.unwrap();

    let page = artifact
        .sentence_provenance_page(&view, &[1, 0, 1, 99])
        .await
        .unwrap();

    assert_eq!(page.len(), 4);
    assert_eq!(
        page[0].as_ref().unwrap().location.source_name.as_ref(),
        "nested/b.md"
    );
    assert_eq!(
        page[1].as_ref().unwrap().location.source_name.as_ref(),
        "a.md"
    );
    assert_eq!(page[0], page[2]);
    assert!(page[3].is_none());
    assert!(
        artifact
            .sentence_provenance_page(&view, &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn artifact_provenance_page_rejects_more_than_the_shared_bound() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let view = artifact.provenance_view().await.unwrap();
    let ids = vec![0; lingrep_corpus::MAX_PROVENANCE_PAGE_SIZE + 1];

    let error = artifact
        .sentence_provenance_page(&view, &ids)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "provenance_page_capacity");
}

#[tokio::test]
async fn artifact_composition_preserves_genuine_json_numbers_and_opaque_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("numeric-metadata.lingcorpus");
    let source = source_document(0, "paper.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source("paper.md").unwrap();
    observation.update_source_bytes(b"metadata").unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
            id: DocumentId::new(0),
            source_name: "paper.md".into(),
            metadata: vec![
                (
                    "integer".into(),
                    MetadataValue::Scalar(MetadataScalar::Number("2016".into())),
                ),
                (
                    "float".into(),
                    MetadataValue::Scalar(MetadataScalar::Number("2.75".into())),
                ),
                (
                    "opaque".into(),
                    MetadataValue::Opaque(serde_json::json!({"x": 1})),
                ),
            ]
            .into_boxed_slice(),
        })
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_start(0))
        .await
        .unwrap();
    let location = builder
        .push_corpus_event(&sentence(0, 0, 0))
        .await
        .unwrap()
        .unwrap();
    builder
        .push_tokenized_sentence(&location, &[TokenInfo::default()])
        .await
        .unwrap();
    builder.push_corpus_event(&paragraph_end(0)).await.unwrap();
    builder.push_corpus_event(&document_end(0)).await.unwrap();
    let artifact = builder.finish(discovery_for(&[source])).await.unwrap();
    let view = artifact.provenance_view().await.unwrap();
    let sentence = artifact.sentence_provenance(&view, 0).await.unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(sentence.rich.metadata_json.as_deref().unwrap()).unwrap();

    assert_eq!(metadata["integer"], serde_json::json!(2016));
    assert_eq!(metadata["float"], serde_json::json!(2.75));
    assert_eq!(metadata["opaque"], serde_json::json!({"x": 1}));
}

#[tokio::test]
async fn artifact_composition_orders_regions_by_start_then_widest_span() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("ordered-regions.lingcorpus");
    let source = source_document(0, "paper.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source("paper.md").unwrap();
    observation.update_source_bytes(b"three sentences").unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(0, "paper.md"))
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_start(0))
        .await
        .unwrap();
    for (ordinal, labels) in [
        &["z-outer"][..],
        &["a-inner", "z-outer"][..],
        &["z-outer"][..],
    ]
    .into_iter()
    .enumerate()
    {
        let event = AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(ordinal as u64),
            document_id: DocumentId::new(0),
            document_sentence_ordinal: ordinal as u64,
            text: format!("sentence {ordinal}"),
            words: None,
            enhanced_graph: None,
            active_labels: labels.iter().map(|label| (*label).into()).collect(),
            metadata: Box::new([]),
        });
        let location = builder.push_corpus_event(&event).await.unwrap().unwrap();
        builder
            .push_tokenized_sentence(&location, &[TokenInfo::default()])
            .await
            .unwrap();
    }
    builder.push_corpus_event(&paragraph_end(0)).await.unwrap();
    builder.push_corpus_event(&document_end(0)).await.unwrap();
    let artifact = builder.finish(discovery_for(&[source])).await.unwrap();
    let view = artifact.provenance_view().await.unwrap();

    let sentence = artifact.sentence_provenance(&view, 1).await.unwrap();

    assert_eq!(
        sentence
            .active_regions
            .iter()
            .map(|region| (
                region.label.as_str(),
                region.first_sentence,
                region.end_sentence,
            ))
            .collect::<Vec<_>>(),
        [("z-outer", 0, 3), ("a-inner", 1, 2)]
    );
}

#[tokio::test]
async fn source_registration_rejects_a_noncanonical_first_target_group() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("noncanonical.lingcorpus");
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();

    let error = builder
        .register_source_document(&source_document(0, "a.md", 1, false))
        .unwrap_err();

    assert_eq!(error.code(), "source_inventory_mismatch");
}

#[tokio::test]
async fn sentence_provenance_rejects_unbound_and_differently_bound_views() {
    let directory = tempfile::tempdir().unwrap();
    let first_root = build_two_document_artifact(directory.path()).await;
    let first = CorpusArtifact::open(&first_root).unwrap();

    let direct = lingrep_corpus::ProvenanceView::open(
        &first_root.join("provenance.sqlite"),
        first.analysis_identity(),
    )
    .await
    .unwrap();
    assert_eq!(
        first
            .sentence_provenance(&direct, 0)
            .await
            .unwrap_err()
            .code(),
        "artifact_binding_mismatch"
    );

    let other_parent = directory.path().join("other");
    fs::create_dir(&other_parent).unwrap();
    let other_root = build_two_document_artifact(&other_parent).await;
    update_provenance(
        &other_root,
        r#"UPDATE divisions SET metadata_json='{"speaker":"different"}' WHERE division_id=0"#,
    )
    .await;
    let other = CorpusArtifact::open(&other_root).unwrap();
    assert_eq!(first.analysis_identity(), other.analysis_identity());
    let other_view = other.provenance_view().await.unwrap();

    let error = first.sentence_provenance(&other_view, 0).await.unwrap_err();
    assert_eq!(error.code(), "artifact_binding_mismatch");
}

#[tokio::test]
async fn ordinary_open_is_lazy_but_full_verification_scans_all_components_and_rows() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let report = artifact.verify_integrity().await.unwrap();
    assert_eq!(report.compared_location_rows, 2);
    assert_eq!(report.components.len(), 4);
    assert!(
        report
            .components
            .iter()
            .all(|component| component.length_matches && component.blake3_matches)
    );

    let token_path = root.join("tokens.v1.zst");
    let mut bytes = fs::read(&token_path).unwrap();
    let schema_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    bytes[128 + schema_len] ^= 1;
    fs::write(&token_path, bytes).unwrap();
    CorpusArtifact::open(&root).unwrap();
    assert_eq!(
        CorpusArtifact::open(&root)
            .unwrap()
            .verify_integrity()
            .await
            .unwrap_err()
            .code(),
        "component_checksum_mismatch"
    );
}

#[tokio::test]
async fn declared_postings_reject_rehashed_token_directory_before_query() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let path = root.join("tokens.v1.zst");
    let mut bytes = fs::read(&path).unwrap();
    let directory_offset = u64::from_le_bytes(bytes[112..120].try_into().unwrap()) as usize;
    bytes[directory_offset + 48] ^= 1;
    let footer_offset = bytes.len() - 48;
    let checksum = *blake3::hash(&bytes[directory_offset..footer_offset]).as_bytes();
    bytes[footer_offset + 8..footer_offset + 40].copy_from_slice(&checksum);
    fs::write(path, bytes).unwrap();
    rewrite_component_fact(&root, "tokens.v1.zst", true);

    assert!(matches!(
        CorpusArtifact::open(&root),
        Err(CorpusError::InvalidFormat {
            component: "postings.v1",
            ..
        })
    ));
}

#[tokio::test]
async fn ordinary_open_does_not_open_or_hash_provenance_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let path = root.join("provenance.sqlite");
    let mut bytes = fs::read(&path).unwrap();
    bytes[0] ^= 1;
    fs::write(path, bytes).unwrap();

    let artifact = CorpusArtifact::open(&root).unwrap();
    assert_eq!(
        artifact.verify_integrity().await.unwrap_err().code(),
        "component_checksum_mismatch"
    );
}

#[tokio::test]
async fn verify_rejects_rehashed_sqlite_owner_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let sqlite = root.join("provenance.sqlite");
    let options = SqliteConnectOptions::new()
        .filename(&sqlite)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query(
        "UPDATE sentence_provenance
         SET document_id = 0
         WHERE sentence_id = 1",
    )
    .execute(&mut connection)
    .await
    .unwrap();
    connection.close().await.unwrap();
    rewrite_component_fact(&root, "provenance.sqlite", true);

    let artifact = CorpusArtifact::open(&root).unwrap();
    assert!(matches!(
        artifact.verify_integrity().await,
        Err(CorpusError::VerificationMismatch {
            component: "provenance.sqlite",
            ..
        })
    ));
}

#[tokio::test]
async fn verify_rejects_rehashed_valid_alternative_target_partition() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let path = root.join("locations.v1");
    let mut bytes = fs::read(&path).unwrap();
    let documents_offset = u64::from_le_bytes(bytes[80..88].try_into().unwrap()) as usize;

    // The authored partition is [0, 0] with multiplicities [2, 2]. Rewrite it
    // to the independently valid [0, 1] / [1, 1] partition and rehash every
    // mutable integrity layer except the manifest's authored logical contract.
    for document in 0..2 {
        let entry = documents_offset + document * 56;
        bytes[entry + 32..entry + 40].copy_from_slice(&(document as u64).to_le_bytes());
        bytes[entry + 40..entry + 48].copy_from_slice(&1_u64.to_le_bytes());
    }
    let directory_offset = u64::from_le_bytes(bytes[72..80].try_into().unwrap()) as usize;
    let checksum = *blake3::hash(&bytes[directory_offset..]).as_bytes();
    bytes[96..128].copy_from_slice(&checksum);
    fs::write(path, bytes).unwrap();
    rewrite_component_fact(&root, "locations.v1", true);

    let error = CorpusArtifact::open(&root)
        .unwrap()
        .verify_integrity()
        .await
        .unwrap_err();
    assert_eq!(error.code(), "discovery_report_mismatch");
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    SymlinkComponent,
    WrongComponentLength,
    WrongAnalysisIdentity,
    WrongAnalyzerSemanticIdentity,
    WrongManifestInventory,
    InfeasibleManifestMultiplicity,
    ZeroConfiguredLimit,
    ObservedLimitExceeded,
    RehashedDenseTargetGroup,
    RehashedNonCanonicalTargetGroups,
    WrongSentenceCount,
    UnsupportedTokenSchema,
    RetiredTokenSchema,
    UnsupportedProvenanceSchema,
    WrongProvenanceIdentity,
    WrongProvenanceCount,
    MissingProvenanceColumn,
    CorruptLazyTokenBlock,
    CreateSqliteSidecar,
    TruncateLocationDirectory,
}

impl Mutation {
    async fn apply(self, root: &Path) {
        match self {
            Self::SymlinkComponent => {
                let component = root.join("tokens.v1.zst");
                let target = root.join("token-target");
                fs::rename(&component, &target).unwrap();
                symlink(&target, &component).unwrap();
            }
            Self::WrongComponentLength => {
                let mut manifest = read_manifest(root);
                manifest["components"]["locations.v1"]["length"] =
                    (manifest["components"]["locations.v1"]["length"]
                        .as_u64()
                        .unwrap()
                        + 1)
                    .into();
                write_manifest(root, &manifest);
            }
            Self::WrongAnalysisIdentity => {
                mutate_bytes(root.join("tokens.v1.zst"), |bytes| bytes[24] ^= 1);
                rewrite_component_fact(root, "tokens.v1.zst", true);
            }
            // The analyzer's recorded semantics are what the analysis
            // identity is derived from, so tampering with them breaks the
            // manifest's self-consistency. `dictionary_fingerprint` is an
            // inspection fact, not an identity input, so changing it changes a
            // reported value and nothing else.
            Self::WrongAnalyzerSemanticIdentity => {
                let mut manifest = read_manifest(root);
                manifest["analyzer"]["semantic_identity"] = "ff".repeat(32).into();
                write_manifest(root, &manifest);
            }
            Self::WrongManifestInventory => {
                let mut manifest = read_manifest(root);
                manifest["inventory"]["documents"] = 3_u64.into();
                write_manifest(root, &manifest);
            }
            Self::InfeasibleManifestMultiplicity => {
                let mut manifest = read_manifest(root);
                manifest["inventory"]["distinct_targets"] = 2_u64.into();
                manifest["inventory"]["alias_documents"] = 0_u64.into();
                manifest["inventory"]["max_target_multiplicity"] = 2_u64.into();
                write_manifest(root, &manifest);
            }
            Self::ZeroConfiguredLimit => {
                let mut manifest = read_manifest(root);
                manifest["configured_limits"]["max_documents"] = 0_u64.into();
                write_manifest(root, &manifest);
            }
            Self::ObservedLimitExceeded => {
                let mut manifest = read_manifest(root);
                manifest["observed"]["index_bytes"] =
                    (manifest["configured_limits"]["max_index_bytes"]
                        .as_u64()
                        .unwrap()
                        + 1)
                    .into();
                write_manifest(root, &manifest);
            }
            Self::RehashedDenseTargetGroup => {
                let path = root.join("locations.v1");
                let mut bytes = fs::read(&path).unwrap();
                let documents_offset =
                    u64::from_le_bytes(bytes[80..88].try_into().unwrap()) as usize;
                let second_target_group = documents_offset + 56 + 32;
                bytes[second_target_group..second_target_group + 8]
                    .copy_from_slice(&8_u64.to_le_bytes());
                let directory_offset =
                    u64::from_le_bytes(bytes[72..80].try_into().unwrap()) as usize;
                let checksum = *blake3::hash(&bytes[directory_offset..]).as_bytes();
                bytes[96..128].copy_from_slice(&checksum);
                fs::write(path, bytes).unwrap();
                rewrite_component_fact(root, "locations.v1", true);
            }
            Self::RehashedNonCanonicalTargetGroups => {
                let path = root.join("locations.v1");
                let mut bytes = fs::read(&path).unwrap();
                let documents_offset =
                    u64::from_le_bytes(bytes[80..88].try_into().unwrap()) as usize;
                for document in 0..2 {
                    let target_group = documents_offset + document * 56 + 32;
                    bytes[target_group..target_group + 8].copy_from_slice(&1_u64.to_le_bytes());
                }
                let directory_offset =
                    u64::from_le_bytes(bytes[72..80].try_into().unwrap()) as usize;
                let checksum = *blake3::hash(&bytes[directory_offset..]).as_bytes();
                bytes[96..128].copy_from_slice(&checksum);
                fs::write(path, bytes).unwrap();
                rewrite_component_fact(root, "locations.v1", true);
            }
            Self::WrongSentenceCount => {
                mutate_bytes(root.join("locations.v1"), |bytes| {
                    bytes[48..56].copy_from_slice(&3_u64.to_le_bytes());
                });
                rewrite_component_fact(root, "locations.v1", true);
            }
            Self::UnsupportedTokenSchema | Self::RetiredTokenSchema => {
                let version: u32 = if matches!(self, Self::RetiredTokenSchema) {
                    1
                } else {
                    99
                };
                mutate_bytes(root.join("tokens.v1.zst"), |bytes| {
                    bytes[8..12].copy_from_slice(&version.to_le_bytes());
                });
                rewrite_component_fact(root, "tokens.v1.zst", true);
            }
            Self::UnsupportedProvenanceSchema => {
                update_provenance(root, "UPDATE provenance_identity SET schema_version=99").await;
            }
            Self::WrongProvenanceIdentity => {
                update_provenance(
                    root,
                    "UPDATE provenance_identity SET analysis_identity=zeroblob(32)",
                )
                .await;
            }
            Self::WrongProvenanceCount => {
                update_provenance(
                    root,
                    "UPDATE provenance_identity SET sentence_count=sentence_count+1",
                )
                .await;
            }
            Self::MissingProvenanceColumn => {
                update_provenance(
                    root,
                    "ALTER TABLE document_metadata DROP COLUMN metadata_json",
                )
                .await;
            }
            Self::CorruptLazyTokenBlock => {
                mutate_bytes(root.join("tokens.v1.zst"), |bytes| {
                    let schema_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
                    bytes[128 + schema_len] ^= 1;
                });
            }
            Self::CreateSqliteSidecar => {
                fs::write(root.join("provenance.sqlite-wal"), b"hostile").unwrap();
            }
            Self::TruncateLocationDirectory => {
                let path = root.join("locations.v1");
                let mut bytes = fs::read(&path).unwrap();
                bytes.truncate(128);
                fs::write(path, bytes).unwrap();
                rewrite_component_fact(root, "locations.v1", true);
            }
        }
    }
}

#[tokio::test]
async fn open_and_verify_reject_each_adversarial_artifact_state() {
    let cases = [
        (Mutation::SymlinkComponent, "symlink_component"),
        (Mutation::WrongComponentLength, "component_length_mismatch"),
        (
            Mutation::WrongAnalysisIdentity,
            "analysis_identity_mismatch",
        ),
        (
            Mutation::WrongAnalyzerSemanticIdentity,
            "analysis_identity_mismatch",
        ),
        (
            Mutation::WrongManifestInventory,
            "discovery_report_mismatch",
        ),
        (
            Mutation::InfeasibleManifestMultiplicity,
            "discovery_report_mismatch",
        ),
        (Mutation::ZeroConfiguredLimit, "discovery_report_mismatch"),
        (Mutation::ObservedLimitExceeded, "discovery_report_mismatch"),
        (
            Mutation::RehashedDenseTargetGroup,
            "discovery_report_mismatch",
        ),
        (
            Mutation::RehashedNonCanonicalTargetGroups,
            "discovery_report_mismatch",
        ),
        (Mutation::WrongSentenceCount, "sentence_count_mismatch"),
        (Mutation::UnsupportedTokenSchema, "unsupported_schema"),
        (Mutation::RetiredTokenSchema, "unsupported_schema"),
        (Mutation::UnsupportedProvenanceSchema, "unsupported_schema"),
        (
            Mutation::WrongProvenanceIdentity,
            "analysis_identity_mismatch",
        ),
        (Mutation::WrongProvenanceCount, "sentence_count_mismatch"),
        (Mutation::MissingProvenanceColumn, "invalid_format"),
        (
            Mutation::CorruptLazyTokenBlock,
            "component_checksum_mismatch",
        ),
        (Mutation::CreateSqliteSidecar, "sqlite_sidecar"),
        (Mutation::TruncateLocationDirectory, "truncated_component"),
    ];
    for (mutation, expected_code) in cases {
        let fixture = ArtifactFixture::new().await;
        mutation.apply(&fixture.root).await;
        let error = match open_or_verify(&fixture.root).await {
            Err(error) => error,
            Ok(()) => panic!("{mutation:?} was accepted"),
        };
        assert_eq!(error.code(), expected_code, "{mutation:?}");
    }
}

/// The root resolves through a symlink; a component inside it does not.
///
/// This is the shape the NixOS module in `nix/modules/lingrep-web/` produces for
/// every declared corpus — `<state>/corpora/<name>` is a symlink to the
/// operator's path — so a declared artifact reached that way has to open.
/// `is_prepared_artifact` already resolves (it uses `Path::is_dir`), and the
/// two disagreeing meant the gate admitted a path that the open then refused.
///
/// The neighbouring `symlink_component` mutation covers the other half: a
/// symlink *inside* the artifact is a containment escape and stays refused.
#[tokio::test]
async fn an_artifact_opens_through_a_symlinked_root_but_not_a_symlinked_file() {
    let fixture = ArtifactFixture::new().await;
    let staging = tempfile::tempdir().unwrap();

    let link = staging.path().join("declared-corpus");
    symlink(&fixture.root, &link).unwrap();
    assert!(
        lingrep_corpus::is_prepared_artifact(&link),
        "the gate must admit a symlinked root"
    );
    let opened = CorpusArtifact::open(&link).expect("a symlinked root must open");
    let direct = CorpusArtifact::open(&fixture.root).expect("the real root must open");
    assert_eq!(
        opened.analysis_identity().as_bytes(),
        direct.analysis_identity().as_bytes(),
        "reaching an artifact through a symlink must not change what it is"
    );

    // Resolved type, not link type: a symlink to a regular file is still not
    // an artifact directory.
    let to_file = staging.path().join("declared-file");
    symlink(fixture.root.join("manifest.json"), &to_file).unwrap();
    assert!(matches!(
        CorpusArtifact::open(&to_file),
        Err(CorpusError::ExpectedArtifactDirectory { .. })
    ));
}

#[test]
fn every_regular_file_is_rejected_as_not_an_artifact_directory() {
    let directory = tempfile::tempdir().unwrap();
    let columnar = 0x5F50_3031_u64.to_le_bytes();
    for (name, bytes) in [
        ("v4", b"VPLZV004".as_slice()),
        ("columnar", columnar.as_slice()),
        ("ordinary", b"ordinary file".as_slice()),
    ] {
        let path = directory.path().join(name);
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            CorpusArtifact::open(&path),
            Err(CorpusError::ExpectedArtifactDirectory { .. })
        ));
    }
}

struct ArtifactFixture {
    _directory: TempDir,
    root: PathBuf,
}

impl ArtifactFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = build_two_document_artifact(directory.path()).await;
        Self {
            _directory: directory,
            root,
        }
    }
}

#[tokio::test]
async fn prepared_artifact_publishes_the_lingrep_v1_contract() {
    let fixture = ArtifactFixture::new().await;
    let manifest = read_manifest(&fixture.root);

    assert_eq!(lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION, 2);
    assert_eq!(manifest["schema_version"], serde_json::json!(1));
    assert_eq!(manifest["format_generation"], serde_json::json!(2));
    assert_eq!(manifest["identity_scheme"], serde_json::json!("a1"));
    assert_eq!(manifest["provenance_schema_version"], serde_json::json!(4));
    assert_eq!(
        manifest["trust_class"],
        serde_json::json!("locally-executed")
    );
    let artifact = CorpusArtifact::open(&fixture.root).unwrap();
    assert_eq!(artifact.trust_class(), TrustClass::LocallyExecuted);
    assert!(manifest.get("encoding").is_none());
    assert!(manifest["components"]["tokens.v1.zst"].is_object());
    assert_eq!(manifest["components"].as_object().unwrap().len(), 4);

    let bytes = fs::read(fixture.root.join("tokens.v1.zst")).unwrap();
    assert_eq!(&bytes[0..8], b"LINGTOK1");
    assert_eq!(u32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(bytes[12..16].try_into().unwrap()), 128);
    assert!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()) > 0);
    assert_eq!(&bytes[bytes.len() - 8..], b"LGTEND01");

    let provenance = artifact.provenance_view().await.unwrap();
    let analysis = provenance
        .sentence_analysis(SentenceId::new(0))
        .await
        .unwrap();
    assert_eq!(analysis.text, "sentence 0");
    assert!(analysis.surfaces.is_empty());
}

#[tokio::test]
async fn schema_three_artifact_requires_rebuild() {
    let fixture = ArtifactFixture::new().await;
    let mut manifest = read_manifest(&fixture.root);
    manifest["schema_version"] = serde_json::json!(3);
    write_manifest(&fixture.root, &manifest);

    assert!(matches!(
        CorpusArtifact::open(&fixture.root),
        Err(CorpusError::PreparedArtifactRebuildRequired { version: 3 })
    ));
}

/// A manifest field this build does not know is refused, and the error names
/// the field, the version that wrote the artifact, and this version.
#[tokio::test]
async fn an_unknown_manifest_field_is_refused_with_both_versions_named() {
    for (field, insert) in [
        (
            "added_later",
            (|manifest: &mut serde_json::Value| {
                manifest["added_later"] = serde_json::json!("value");
            }) as fn(&mut serde_json::Value),
        ),
        ("nested_later", |manifest: &mut serde_json::Value| {
            manifest["analyzer"]["nested_later"] = serde_json::json!(1);
        }),
    ] {
        let fixture = ArtifactFixture::new().await;
        let mut manifest = read_manifest(&fixture.root);
        manifest["producer_version"] = serde_json::json!("9.9.9");
        insert(&mut manifest);
        write_manifest(&fixture.root, &manifest);

        let error = match CorpusArtifact::open(&fixture.root) {
            Ok(_) => panic!("the manifest accepted the unknown field {field}"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "unknown_manifest_field", "{error}");
        let message = error.to_string();
        assert!(message.contains(&format!("`{field}`")), "{message}");
        assert!(message.contains("ling 9.9.9"), "{message}");
        assert!(
            message.contains(&format!("ling {}", env!("CARGO_PKG_VERSION"))),
            "{message}"
        );
    }
}

async fn open_or_verify(root: &Path) -> Result<(), CorpusError> {
    let artifact = CorpusArtifact::open(root)?;
    artifact.verify_integrity().await?;
    Ok(())
}

#[tokio::test]
async fn bounded_manifest_validation_rejects_impossible_resource_summaries_at_open() {
    for mutation in [
        Mutation::InfeasibleManifestMultiplicity,
        Mutation::ZeroConfiguredLimit,
        Mutation::ObservedLimitExceeded,
    ] {
        let fixture = ArtifactFixture::new().await;
        mutation.apply(&fixture.root).await;
        let error = match CorpusArtifact::open(&fixture.root) {
            Ok(_) => panic!("{mutation:?} was accepted at ordinary open"),
            Err(error) => error,
        };
        assert_eq!(error.code(), "discovery_report_mismatch", "{mutation:?}");
    }
}

fn mutate_bytes(path: PathBuf, mutation: impl FnOnce(&mut Vec<u8>)) {
    let mut bytes = fs::read(&path).unwrap();
    mutation(&mut bytes);
    fs::write(path, bytes).unwrap();
}

fn read_manifest(root: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap()
}

fn write_manifest(root: &Path, manifest: &serde_json::Value) {
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
}

fn rewrite_component_fact(root: &Path, component: &str, update_length: bool) {
    let bytes = fs::read(root.join(component)).unwrap();
    let mut manifest = read_manifest(root);
    manifest["components"][component]["blake3"] = blake3::hash(&bytes).to_hex().to_string().into();
    if update_length {
        manifest["components"][component]["length"] = (bytes.len() as u64).into();
    }
    write_manifest(root, &manifest);
}

async fn update_provenance(root: &Path, statement: &'static str) {
    let path = root.join("provenance.sqlite");
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query(statement)
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    rewrite_component_fact(root, "provenance.sqlite", true);
}

#[allow(dead_code)]
fn assert_analysis_type(_: &AnalysisIdentity) {}

/// Builds a one-document artifact whose sentences carry the given labels.
async fn labelled_artifact(parent: &Path, labels: &[&[&str]]) -> CorpusArtifact {
    let destination = parent.join("labelled.lingcorpus");
    let source = source_document(0, "paper.md", 0, false);
    let mut builder = CorpusBuilder::begin(build_config(), &destination)
        .await
        .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source("paper.md").unwrap();
    observation
        .update_source_bytes(b"labelled sentences")
        .unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&document_start(0, "paper.md"))
        .await
        .unwrap();
    builder
        .push_corpus_event(&paragraph_start(0))
        .await
        .unwrap();
    for (ordinal, active) in labels.iter().enumerate() {
        let event = AdmittedCorpusEvent::Sentence(AdmittedSentence {
            structure: Default::default(),
            id: SentenceId::new(ordinal as u64),
            document_id: DocumentId::new(0),
            document_sentence_ordinal: ordinal as u64,
            text: format!("sentence {ordinal}"),
            words: None,
            enhanced_graph: None,
            active_labels: active.iter().map(|label| (*label).into()).collect(),
            metadata: Box::new([]),
        });
        let location = builder.push_corpus_event(&event).await.unwrap().unwrap();
        builder
            .push_tokenized_sentence(&location, &[TokenInfo::default()])
            .await
            .unwrap();
    }
    builder.push_corpus_event(&paragraph_end(0)).await.unwrap();
    builder.push_corpus_event(&document_end(0)).await.unwrap();
    builder.finish(discovery_for(&[source])).await.unwrap()
}

#[tokio::test]
async fn region_spans_for_reads_only_the_named_labels() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = labelled_artifact(
        directory.path(),
        &[&["methods"], &["methods"], &["results"], &["references"]],
    )
    .await;
    let view = artifact.provenance_view().await.unwrap();

    let spans = view
        .region_spans_for(&std::collections::BTreeSet::from(["methods"]))
        .await
        .unwrap();

    assert_eq!(
        spans,
        vec![lingrep_corpus::RegionSpanRow {
            label: "methods".to_owned(),
            first_sentence: 0,
            end_sentence: 2,
        }],
        "a label the caller did not name must contribute no rows"
    );
}

/// The `ORDER BY span_id` contract, on a corpus that exercises it: `methods`
/// closes and `results` opens, so the two spans are *inserted* in close order
/// and read back in open order.
#[tokio::test]
async fn region_spans_in_span_order_reads_open_order_not_insert_order() {
    use futures::StreamExt as _;

    let directory = tempfile::tempdir().unwrap();
    // `outer` opens first and closes last; `inner` opens later and closes
    // earlier, so it is *inserted* first. Read order and insert order disagree,
    // which is the case an insert-ordered read would get wrong.
    let artifact = labelled_artifact(
        directory.path(),
        &[&["outer"], &["outer", "inner"], &["outer"]],
    )
    .await;
    let view = artifact.provenance_view().await.unwrap();

    let spans = view
        .region_spans_in_span_order()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(
        spans
            .windows(2)
            .all(|pair| pair[0].first_sentence <= pair[1].first_sentence),
        "span_id order must already be first_sentence order: {spans:?}"
    );
    assert_eq!(
        spans
            .iter()
            .map(|span| (span.label.as_str(), span.first_sentence))
            .collect::<Vec<_>>(),
        vec![("outer", 0), ("inner", 1)],
        "read in open order; insert order is the reverse, because inner closes first"
    );
}

#[tokio::test]
async fn region_spans_for_returns_nothing_for_a_label_the_corpus_never_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = labelled_artifact(directory.path(), &[&["methods"]]).await;
    let view = artifact.provenance_view().await.unwrap();

    assert!(
        view.region_spans_for(&std::collections::BTreeSet::from(["absent"]))
            .await
            .unwrap()
            .is_empty()
    );
}

/// The label set a corpus records, which is what a caller checks before
/// deciding a section question is answerable at all. An **empty** result means
/// it is not — whether because the corpus was built from a format that records
/// no sections, or from Markdown with no labelled ones. The two are equally
/// unanswerable and need not be distinguished.
#[tokio::test]
async fn section_coverage_lists_what_the_corpus_records() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = labelled_artifact(
        directory.path(),
        &[&["methods"], &["results"], &["results"]],
    )
    .await;
    let view = artifact.provenance_view().await.unwrap();

    let mut labels = view
        .section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
        .await
        .unwrap()
        .into_iter()
        .map(|coverage| coverage.label)
        .collect::<Vec<_>>();
    labels.sort();
    assert_eq!(labels, vec!["methods".to_owned(), "results".to_owned()]);
}

#[tokio::test]
async fn section_coverage_is_empty_for_a_corpus_that_records_no_labels() {
    let directory = tempfile::tempdir().unwrap();
    let artifact = labelled_artifact(directory.path(), &[&[], &[]]).await;
    let view = artifact.provenance_view().await.unwrap();

    assert!(
        view.section_coverage(lingrep_corpus::MAX_SECTION_LABELS)
            .await
            .unwrap()
            .is_empty()
    );
}

/// Publication refuses to write an artifact whose token schema identity is not
/// this build's, because `finish` validates by opening what it wrote.
#[tokio::test]
async fn publication_refuses_a_different_featurization() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("stale-schema.lingcorpus");
    // The schema identity is no longer separately settable — `AnalysisInputs`
    // derives it from the descriptor — so a mismatch now arrives the one way
    // it still can: a descriptor whose schema is not the storage schema the
    // build writes. `begin` writes the Vibrato morphology schema.
    let stale_descriptor = lingrep_core::analysis::AnalyzerDescriptor::new(
        lingrep_core::analysis::AnalyzerFamily::Vibrato,
        lingrep_core::analysis::ExtractionProfile::Tokenize,
        lingrep_core::analysis::SegmentationMode::Input,
        lingrep_core::analysis::NormalizationPolicy::Nfc,
        "ja",
        "input-unit",
        [0xf1; 32],
        [0xf2; 32],
        [0xf3; 32],
        [0xf4; 32],
        [0xf5; 32],
        [0xf6; 32],
        lingrep_core::token_match::TokenSchema::from_keys(["text"])
            .unwrap()
            .digest(),
    )
    .unwrap();
    let stale = BuildConfig {
        analysis: AnalysisInputs::new(
            ReaderIdentity::from_bytes([1; 32]),
            AdmissionIdentity::from_bytes([2; 32]),
            &stale_descriptor,
        ),
        analyzer: lingrep_corpus::AnalyzerRecord::from_descriptor(&stale_descriptor),
        dictionary: DictionaryFingerprint::from_bytes([3; 32]),
        trust: TrustClass::LocallyExecuted,
        temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
        sentence_metadata_keys: Vec::new(),
    };
    let documents = [source_document(0, "a.md", 0, false)];
    let Err(error) = CorpusBuilder::begin(stale, &destination).await else {
        panic!("construction must refuse a mismatched schema identity");
    };
    let _ = documents;
    assert_eq!(error.code(), "invalid_format");
}

/// The case that matters for artifacts already on disk: one written by an
/// older featurization is internally consistent and passes every other check,
/// so only a comparison against this build's own digest catches it. The
/// V7 had no equivalent, so a featurization fix left every artifact serving
/// the values it existed to correct.
#[tokio::test]
async fn an_artifact_recording_a_different_featurization_is_rejected_on_open() {
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let manifest_path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["token_schema_identity"] = serde_json::Value::String("04".repeat(32));
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let Err(error) = CorpusArtifact::open(&root) else {
        panic!("an artifact from a different featurization must not open");
    };

    assert_eq!(error.code(), "analysis_identity_mismatch");
}

#[test]
fn canonical_artifact_path_is_content_addressed_and_colocated() {
    // V1 inserts trust class into this shape; artifact.rs remains its sole owner.
    // 0x0a is the fixture that catches both ways the encoding can drift: its
    // correct hex is "0a", so uppercase yields "0A" and a lost zero-pad yields
    // "a". A letter-only byte such as 0xab would spell "ab" under {:x} too and
    // so would miss the pad.
    let identity = AnalysisIdentity::from_bytes([0x0a_u8; 32]);
    assert_eq!(format_identity(identity.as_bytes()), "0a".repeat(32));

    let path = canonical_artifact_path(
        Path::new("data-archive/science-corpus.jsonl"),
        &identity,
        TrustClass::LocallyExecuted,
    );
    assert_eq!(
        path,
        Path::new(&format!(
            "data-archive/science-corpus.jsonl.g{CORPUS_ARTIFACT_FORMAT_GENERATION}.a1.{}.locally-executed.lingcorpus",
            "0a".repeat(32)
        ))
    );
}

/// The canonical name reports the recorded generation, scheme and identity.
#[test]
fn artifact_names_parse_back_to_the_scheme_that_wrote_them() {
    let identity = AnalysisIdentity::from_bytes([0x0a_u8; 32]);
    let path = canonical_artifact_path(
        Path::new("/data/corpus.jsonl"),
        &identity,
        TrustClass::ExternallyAttested,
    );
    let name = path.file_name().unwrap().to_str().unwrap();
    let parsed = parse_artifact_name(name).expect("a canonical name parses");
    assert_eq!(parsed.scheme, IdentityScheme::CURRENT);
    assert_eq!(parsed.analysis_identity, identity);
    assert_eq!(parsed.trust, TrustClass::ExternallyAttested);
    assert_eq!(parsed.format_generation, CORPUS_ARTIFACT_FORMAT_GENERATION);
}

/// A name that is not a canonical artifact name is not a candidate, and is not
/// a defect either — a sibling scan has to walk past it silently.
#[test]
fn non_artifact_names_do_not_parse() {
    for name in [
        "corpus.jsonl",
        "notes.lingcorpus",
        "corpus.jsonl.g6.a1.short.locally-executed.lingcorpus",
        "corpus.jsonl.g6.a9.0a.locally-executed.lingcorpus",
        &format!(
            "corpus.jsonl.gX.a1.{}.locally-executed.lingcorpus",
            "0a".repeat(32)
        ),
        &format!(
            "corpus.jsonl.g6.a1.{}.unknown-trust.lingcorpus",
            "0a".repeat(32)
        ),
    ] {
        assert!(
            parse_artifact_name(name).is_none(),
            "{name} must not parse as a canonical artifact name"
        );
    }
}

#[test]
fn canonical_artifact_path_uses_the_directory_name_for_directory_roots() {
    let identity = AnalysisIdentity::from_bytes([0u8; 32]);
    let path = canonical_artifact_path(
        Path::new("/var/corpora/stacks"),
        &identity,
        TrustClass::LocallyExecuted,
    );
    assert_eq!(
        path,
        Path::new(&format!(
            "/var/corpora/stacks.g{CORPUS_ARTIFACT_FORMAT_GENERATION}.a1.{}.locally-executed.lingcorpus",
            "0".repeat(64)
        ))
    );
}

#[test]
fn canonical_artifact_path_falls_back_to_an_empty_stem_for_nameless_roots() {
    let identity = AnalysisIdentity::from_bytes([1u8; 32]);
    let path = canonical_artifact_path(Path::new("/"), &identity, TrustClass::LocallyExecuted);
    let file_name = path.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        file_name,
        format!(
            ".g{CORPUS_ARTIFACT_FORMAT_GENERATION}.a1.{}.locally-executed.lingcorpus",
            "01".repeat(32)
        )
    );
    // Placement inverts for a root with no file name: there is nothing to sit
    // beside, so the empty-stem name nests inside the root instead.
    assert_eq!(path.parent(), Some(Path::new("/")));
}

#[test]
fn canonical_artifact_path_differs_for_a_different_analysis_identity() {
    let root = Path::new("corpus/one.md");
    let first = canonical_artifact_path(
        root,
        &AnalysisIdentity::from_bytes([1u8; 32]),
        TrustClass::LocallyExecuted,
    );
    let second = canonical_artifact_path(
        root,
        &AnalysisIdentity::from_bytes([2u8; 32]),
        TrustClass::LocallyExecuted,
    );
    assert_ne!(first, second);
    assert_eq!(first.parent(), second.parent());
}

#[test]
fn prepared_metadata_source_keeps_the_querys_existing_provenance_reader() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = runtime.block_on(build_two_document_artifact(directory.path()));
    drop(runtime);
    let artifact = CorpusArtifact::open(&root).unwrap();
    let reader = artifact.provenance_reader().unwrap();
    let schema = artifact.token_schema().unwrap();
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .unwrap();
    let metadata =
        lingrep_core::corpus::SourceMetadataProjection::new(vec!["@document.id".to_owned()])
            .unwrap();
    let mut source = artifact
        .bound_sentence_source_with_metadata(&projection, metadata, Arc::clone(&reader))
        .unwrap();
    assert_eq!(Arc::strong_count(&reader), 2);
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_some()
    );
    drop(source);
    assert_eq!(Arc::strong_count(&reader), 1);
}

#[tokio::test]
async fn prepared_admission_checks_declared_index_and_leaves_unvisited_tokens_for_full_verify() {
    use lingrep_core::fsm::{FsmBuilder, PatternSource};
    let directory = tempfile::tempdir().unwrap();
    let root = build_two_document_artifact(directory.path()).await;
    let artifact = CorpusArtifact::open(&root).unwrap();
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "absent.lx",
            "p := [lemma=definitely-absent].",
        ))
        .unwrap();
    let catalog = builder.build().unwrap().matcher;
    let bound = catalog.bind(&artifact.token_schema().unwrap()).unwrap();
    let mut source = artifact
        .filtered_sentence_source(bound.projection(), bound.sentence_filter().unwrap())
        .unwrap();
    let stats = source.filter_stats();
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
    assert_eq!(stats.sentences_proven_absent(), 2);
    drop(source);
    drop(artifact);

    let token_path = root.join("tokens.v1.zst");
    let mut bytes = fs::read(&token_path).unwrap();
    let directory_offset = u64::from_le_bytes(bytes[112..120].try_into().unwrap()) as usize;
    let payload = u64::from_le_bytes(
        bytes[directory_offset + 16..directory_offset + 24]
            .try_into()
            .unwrap(),
    ) as usize;
    bytes[payload] ^= 1;
    fs::write(&token_path, bytes).unwrap();
    let artifact = CorpusArtifact::open(&root).unwrap();
    let mut source = artifact
        .filtered_sentence_source(bound.projection(), bound.sentence_filter().unwrap())
        .unwrap();
    assert!(
        source
            .next_batch(&ExecutionLimits::default())
            .unwrap()
            .is_none()
    );
    assert!(artifact.verify_integrity().await.is_err());
    let mut scan = artifact.bound_sentence_source(bound.projection()).unwrap();
    assert!(scan.next_batch(&ExecutionLimits::default()).is_err());
    drop(scan);
    drop(source);
    drop(artifact);

    let index = root.join("postings.v1");
    let mut bytes = fs::read(&index).unwrap();
    let at = bytes.len() - 49;
    bytes[at] ^= 1;
    fs::write(&index, bytes).unwrap();
    assert!(CorpusArtifact::open(&root).is_err());
    fs::remove_file(&index).unwrap();
    assert!(CorpusArtifact::open(&root).is_err());
    let manifest_path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["components"]
        .as_object_mut()
        .unwrap()
        .remove("postings.v1");
    fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let artifact = CorpusArtifact::open(&root).unwrap();
    let mut source = artifact
        .filtered_sentence_source(bound.projection(), bound.sentence_filter().unwrap())
        .unwrap();
    assert!(source.next_batch(&ExecutionLimits::default()).is_err());
    assert_eq!(source.filter_stats().sentences_proven_absent(), 0);
}
