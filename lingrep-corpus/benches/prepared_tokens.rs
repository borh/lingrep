// Exempt benchmark code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::{path::Path, sync::Arc, time::Instant};

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use lingrep_core::{
    analysis::{
        AnalyzerPlan, AnalyzerSession, InputUnitId, NormalizationPolicy, TextUnit, VibratoPlan,
    },
    corpus::{
        AdmittedCorpusEvent, AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats,
        SentenceId, SourceDocument, TraversalHighWater, TraversalLimits,
    },
    execution::{ExecutionBatch, ExecutionLimits, SentenceBatchSource},
    fsm::{CompileLimits, PatternSource, compile_catalog},
    spill::TempBudget,
    token_match::AttributeKey,
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusArtifact, CorpusBuilder,
    DictionaryFingerprint, ReaderIdentity,
};
use tempfile::TempDir;

const SENTENCES: usize = 4_000;
const TOKENS: u64 = SENTENCES as u64;

/// The build benchmark writes its own corpus rather than reusing the
/// single-token one the read benchmarks measure against. Block encoding is
/// charged per token *and* per attribute, and its string table amortizes over
/// the block, so a corpus of 4,000 identical one-token sentences understates
/// both: the table holds one entry and every lookup hits it. This sentence
/// tokenizes into a realistic row count with values that recur at a realistic
/// rate.
const BUILD_SENTENCE: &str = "この研究では、大学院の学生が行った調査の結果について詳しく報告する。";
const BUILD_SENTENCES: usize = 2_000;

fn analysis_inputs() -> AnalysisInputs {
    AnalysisInputs::new(
        ReaderIdentity::from_bytes([1; 32]),
        AdmissionIdentity::from_bytes([2; 32]),
        &lingrep_corpus::vibrato_fixture_descriptor(),
    )
}

fn matcher() -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("noun.lx", "noun := [upos=NOUN]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn inventory_digest(name: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(&(name.len() as u64).to_le_bytes());
    hash.update(name.as_bytes());
    *hash.finalize().as_bytes()
}

async fn build_artifact(path: &Path) {
    build_corpus(path, "研究", SENTENCES).await;
}

async fn build_corpus(path: &Path, text: &str, sentences: usize) {
    let source = SourceDocument {
        ordinal: 0,
        logical_name: "benchmark.md".into(),
        path: "/unopened/benchmark.md".into(),
        target_group_ordinal: 0,
        external_target: false,
    };
    let mut builder = CorpusBuilder::begin(
        BuildConfig {
            analysis: analysis_inputs(),
            analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
            dictionary: DictionaryFingerprint::from_bytes([3; 32]),
            trust: lingrep_corpus::TrustClass::LocallyExecuted,
            temp_budget: Arc::new(TempBudget::new(64 * 1024 * 1024)),
            sentence_metadata_keys: Vec::new(),
        },
        path,
    )
    .await
    .unwrap();
    builder.register_source_document(&source).unwrap();
    let observation = builder.source_observation();
    observation.begin_source(&source.logical_name).unwrap();
    observation
        .update_source_bytes(b"benchmark corpus")
        .unwrap();
    observation.end_source().unwrap();
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
            id: DocumentId::new(0),
            source_name: source.logical_name.clone(),
            metadata: Box::new([]),
        })
        .await
        .unwrap();
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DivisionStart(std::sync::Arc::new(
            lingrep_core::corpus::Division {
                source_id: None,
                id: lingrep_core::corpus::DivisionId(0),
                document_id: DocumentId::new(0),
                kind: "paragraph".into(),
                parent_id: None,
                metadata: Box::new([]),
            },
        )))
        .await
        .unwrap();
    let plan = VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let projection = plan
        .schema()
        .bind_projection(plan.schema().keys().iter().cloned())
        .unwrap();
    let mut session = plan.create_session();
    for index in 0..sentences {
        let location = builder
            .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
                structure: Default::default(),
                id: SentenceId::new(index as u64),
                document_id: DocumentId::new(0),
                document_sentence_ordinal: index as u64,
                text: text.into(),
                words: None,
                enhanced_graph: None,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            }))
            .await
            .unwrap()
            .unwrap();
        let input = TextUnit::sentence(
            InputUnitId::new(index as u64),
            text,
            NormalizationPolicy::Nfc,
        )
        .unwrap();
        let mut analyzed = session
            .analyze_bounded_batch(vec![input], &projection)
            .unwrap()
            .into_inputs()
            .into_vec();
        builder
            .push_analyzed_sentence(&location, analyzed.pop().unwrap(), &projection)
            .await
            .unwrap();
    }
    builder
        .push_corpus_event(&AdmittedCorpusEvent::DivisionEnd {
            id: lingrep_core::corpus::DivisionId(0),
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
        .finish(DiscoveryReport {
            inventory: InventoryStats {
                documents: 1,
                distinct_targets: 1,
                alias_documents: 0,
                max_target_multiplicity: 1,
                external_targets: 0,
            },
            configured_limits: TraversalLimits::default(),
            observed: TraversalHighWater {
                max_depth: 1,
                examined_entries: 1,
                max_directory_entries: 1,
                index_bytes: 128,
            },
            ordered_inventory_digest: inventory_digest(&source.logical_name),
        })
        .await
        .unwrap();
}

fn projected_decode(artifact: &CorpusArtifact) -> usize {
    let projection = artifact
        .token_schema()
        .unwrap()
        .bind_projection([
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("upos").unwrap(),
        ])
        .unwrap();
    let mut source = artifact.bound_sentence_source(&projection).unwrap();
    let limits = ExecutionLimits::default();
    let mut tokens = 0;
    while let Some(batch) = source.next_batch(&limits).unwrap() {
        let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
            panic!("prepared source returned non-bound data");
        };
        tokens += sentences
            .iter()
            .map(|sentence| sentence.value.words().len())
            .sum::<usize>();
    }
    tokens
}

fn match_projected(
    artifact: &CorpusArtifact,
    matcher: &lingrep_core::fsm::CompiledCatalog,
) -> usize {
    let schema = artifact.token_schema().unwrap();
    let matcher = matcher.bind(&schema).unwrap();
    let mut source = artifact
        .bound_sentence_source(matcher.projection())
        .unwrap();
    let limits = ExecutionLimits::default();
    let mut matches = 0;
    while let Some(batch) = source.next_batch(&limits).unwrap() {
        let ExecutionBatch::BoundTokenized { sentences, .. } = batch else {
            panic!("prepared source returned non-bound data");
        };
        matches += sentences
            .iter()
            .map(|sentence| matcher.find_matches(sentence.value.words()).unwrap().len())
            .sum::<usize>();
    }
    matches
}

fn prepared_tokens(c: &mut Criterion) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("benchmark.lingcorpus");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(build_artifact(&path));
    let artifact = CorpusArtifact::open(&path).unwrap();
    let matcher = matcher();
    let bytes = std::fs::metadata(path.join("tokens.v1.zst")).unwrap().len();

    let started = Instant::now();
    let measured_open = CorpusArtifact::open(&path).unwrap();
    let open_latency = started.elapsed();
    let started = Instant::now();
    runtime.block_on(measured_open.verify_integrity()).unwrap();
    let verification_latency = started.elapsed();
    let started = Instant::now();
    let matches = match_projected(&measured_open, &matcher);
    let matching_latency = started.elapsed();
    assert_eq!(matches, SENTENCES);
    eprintln!(
        "PREPARED_METRICS bytes={bytes} tokens={TOKENS} bytes_per_token={:.3} \
         open_latency_ns={} verification_latency_ns={} matching_tokens_per_sec={:.0}",
        bytes as f64 / TOKENS as f64,
        open_latency.as_nanos(),
        verification_latency.as_nanos(),
        TOKENS as f64 / matching_latency.as_secs_f64(),
    );

    let mut group = c.benchmark_group("prepared_tokens");
    group.throughput(Throughput::Elements(TOKENS));
    group.bench_function("normal_open", |b| {
        b.iter(|| black_box(CorpusArtifact::open(&path).unwrap()));
    });
    group.bench_function("full_verification", |b| {
        b.iter(|| {
            black_box(runtime.block_on(artifact.verify_integrity()).unwrap());
        });
    });
    group.bench_function("projected_decode", |b| {
        b.iter(|| {
            assert_eq!(black_box(projected_decode(&artifact)), SENTENCES);
        });
    });
    group.bench_function("matching_throughput", |b| {
        b.iter(|| {
            assert_eq!(black_box(match_projected(&artifact, &matcher)), SENTENCES);
        });
    });
    group.finish();

    // Each iteration publishes into a directory that does not yet exist, as
    // `CorpusBuilder` requires, and the timing loop is manual so that creating
    // and removing it is not charged to the measurement.
    let mut group = c.benchmark_group("prepared_build");
    group.sample_size(20);
    group.bench_function("build_artifact", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = std::time::Duration::ZERO;
            for iteration in 0..iterations {
                let target = directory
                    .path()
                    .join(format!("build-{iteration}.lingcorpus"));
                let started = Instant::now();
                runtime.block_on(build_corpus(&target, BUILD_SENTENCE, BUILD_SENTENCES));
                elapsed += started.elapsed();
                std::fs::remove_dir_all(&target).unwrap();
            }
            elapsed
        });
    });
    group.finish();
}

criterion_group!(benches, prepared_tokens);
criterion_main!(benches);
