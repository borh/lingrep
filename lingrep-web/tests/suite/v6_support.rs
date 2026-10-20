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
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use lingrep_core::{
    corpus::{
        AdmittedCorpusEvent, AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats,
        SourceDocument, TraversalHighWater, TraversalLimits,
    },
    spill::TempBudget,
    token_match::TokenInfo,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusArtifact, CorpusBuilder,
    DictionaryFingerprint, ReaderIdentity,
};
use lingrep_web::corpus_runtime::{CorpusIdentity, CorpusRuntime, ReadyCorpus};
use lingrep_web::corpus_set::{CorpusName, CorpusSlot};

pub struct ArtifactFixture {
    pub directory: tempfile::TempDir,
    pub path: PathBuf,
}

pub fn corrupt_first_token_block(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    let schema_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as u64;
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(128 + schema_len)).unwrap();
    file.write_all(&[0xff]).unwrap();
}

pub fn build_artifact(documents: Vec<(&str, u64, bool, Vec<Vec<TokenInfo>>)>) -> ArtifactFixture {
    let documents = documents
        .into_iter()
        .map(|(name, group, external, sentences)| (name.to_owned(), group, external, sentences))
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture.lingcorpus");
    let build_path = path.clone();
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let sources = documents
                    .iter()
                    .enumerate()
                    .map(
                        |(ordinal, (logical_name, target_group_ordinal, external_target, _))| {
                            SourceDocument {
                                ordinal: ordinal as u64,
                                logical_name: logical_name.clone(),
                                path: PathBuf::from(format!("/unused/{logical_name}")),
                                target_group_ordinal: *target_group_ordinal,
                                external_target: *external_target,
                            }
                        },
                    )
                    .collect::<Vec<_>>();
                let mut builder = CorpusBuilder::begin(
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
                    },
                    &build_path,
                )
                .await
                .unwrap();
                let observation = builder.source_observation();
                let mut sentence_ordinal = 0_u64;
                for (source, (_, _, _, sentences)) in sources.iter().zip(&documents) {
                    builder.register_source_document(source).unwrap();
                    observation.begin_source(&source.logical_name).unwrap();
                    observation
                        .update_source_bytes(source.logical_name.as_bytes())
                        .unwrap();
                    observation.end_source().unwrap();
                    builder
                        .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
                            id: DocumentId::new(source.ordinal),
                            source_name: source.logical_name.clone(),
                            metadata: Box::new([]),
                        })
                        .await
                        .unwrap();
                    builder
                        .push_corpus_event(&AdmittedCorpusEvent::DivisionStart(
                            std::sync::Arc::new(lingrep_core::corpus::Division {
                                source_id: None,
                                id: lingrep_core::corpus::DivisionId(source.ordinal),
                                document_id: DocumentId::new(source.ordinal),
                                kind: "paragraph".into(),
                                parent_id: None,
                                metadata: Box::new([]),
                            }),
                        ))
                        .await
                        .unwrap();
                    for (local_ordinal, tokens) in sentences.iter().enumerate() {
                        let location = builder
                            .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
                                structure: Default::default(),
                                id: lingrep_core::corpus::SentenceId::new(sentence_ordinal),
                                document_id: DocumentId::new(source.ordinal),
                                document_sentence_ordinal: local_ordinal as u64,
                                text: String::new(),
                                words: None,
                                enhanced_graph: None,
                                active_labels: Box::new([]),
                                metadata: Box::new([]),
                            }))
                            .await
                            .unwrap()
                            .unwrap();
                        builder
                            .push_tokenized_sentence(&location, tokens)
                            .await
                            .unwrap();
                        sentence_ordinal += 1;
                    }
                    builder
                        .push_corpus_event(&AdmittedCorpusEvent::DivisionEnd {
                            id: lingrep_core::corpus::DivisionId(source.ordinal),
                        })
                        .await
                        .unwrap();
                    builder
                        .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
                            id: DocumentId::new(source.ordinal),
                        })
                        .await
                        .unwrap();
                }
                builder.finish(discovery_report(&sources)).await.unwrap();
            });
    })
    .join()
    .unwrap();
    ArtifactFixture { directory, path }
}

pub fn open_artifact(fixture: &ArtifactFixture) -> CorpusArtifact {
    CorpusArtifact::open(&fixture.path).unwrap()
}

pub fn ready_fixture(
    identity: &str,
    sentences: Vec<Vec<TokenInfo>>,
) -> (ArtifactFixture, ReadyCorpus) {
    let fixture = build_artifact(vec![("fixture.md", 0, false, sentences)]);
    let ready = ReadyCorpus::from_artifact(
        CorpusIdentity::new(identity),
        open_artifact(&fixture),
        Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        ),
    )
    .unwrap();
    (fixture, ready)
}

pub fn runtime_fixture(
    identity: &str,
    sentences: Vec<Vec<TokenInfo>>,
) -> (ArtifactFixture, Arc<CorpusRuntime>) {
    let (fixture, ready) = ready_fixture(identity, sentences);
    let context = Arc::new(lingrep_web::server_context::ServerContext::new("/tmp/dict").unwrap());
    let runtime = Arc::new(CorpusRuntime::from_ready(
        ready.identity,
        Arc::into_inner(ready.artifact).expect("fixture has one artifact owner"),
        context,
    ));
    (fixture, runtime)
}

/// Wrap a test runtime as the default-keyed corpus slot a `CoordinatorRegistry`
/// admission needs. Most test fixtures care about only one corpus, so this
/// gives them a slot without threading a whole `CorpusSet` through.
pub fn default_slot(runtime: Arc<CorpusRuntime>) -> CorpusSlot {
    CorpusSlot::new(CorpusName::default_key(), runtime, "test-corpus".into())
}

fn discovery_report(documents: &[SourceDocument]) -> DiscoveryReport {
    let mut groups = HashMap::<u64, u64>::new();
    let mut external_targets = 0_u64;
    let mut digest = blake3::Hasher::new();
    for document in documents {
        *groups.entry(document.target_group_ordinal).or_default() += 1;
        external_targets += u64::from(document.external_target);
        digest.update(&(document.logical_name.len() as u64).to_le_bytes());
        digest.update(document.logical_name.as_bytes());
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
            examined_entries: document_count,
            max_directory_entries: documents.len(),
            index_bytes: 0,
        },
        ordered_inventory_digest: *digest.finalize().as_bytes(),
    }
}
