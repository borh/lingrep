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

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use lingrep::direct_source::matched_surface;
use lingrep_core::analysis::{InputUnitId, NormalizationPolicy, TextUnit};
use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::{
    CancellationFlag, ExecutionBatch, ExecutionError, ExecutionLimits, ExecutionSentence,
    MatchSink, RunOutcome, SentenceAdmission, SentenceBatchSource, SentenceLocation, SinkControl,
    TokenizedBatchObservation, execute,
};
use lingrep_core::fsm::{CompileLimits, CompiledCatalog, Match, PatternSource, compile_catalog};
use lingrep_core::testing::MiniDictionary;
use lingrep_core::token_match::{TokenInfo, TokenSequence};
use lingrep_corpus::{
    CORPUS_ARTIFACT_FORMAT_GENERATION, CorpusArtifact, TrustClass, canonical_artifact_path,
};
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{
    build_single_sentence_artifact, lingrep, lingrep_without_dictionary,
    missing_attribute_location, parse_jsonl,
};

#[test]
fn corpus_build_rejects_retired_encoding_option() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    let artifact = directory.path().join("corpus.lingcorpus");
    fs::write(&source, "猫。\n").unwrap();

    let output = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .args(["--encoding", "uncompressed"])
        .arg(&source)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--encoding"));
    assert!(!artifact.exists());
}

struct PreparedFixture {
    _directory: TempDir,
    root: PathBuf,
    artifact: PathBuf,
    patterns: PathBuf,
    dictionary: MiniDictionary,
}

struct JsonlEvidenceFixture {
    _directory: TempDir,
    root: PathBuf,
    artifact: PathBuf,
    matches: PathBuf,
    no_matches: PathBuf,
    dictionary: MiniDictionary,
}

struct OneBatchSource(Option<ExecutionBatch>);

impl SentenceBatchSource for OneBatchSource {
    fn wants_lookahead(&self) -> bool {
        false
    }

    fn next_batch(
        &mut self,
        _limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        Ok(self.0.take())
    }
}

#[derive(Default)]
struct RecordingSink {
    matches: Vec<Match>,
}

impl MatchSink for RecordingSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.matches.extend_from_slice(matches);
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

#[derive(Default)]
struct SurfaceSink;

impl MatchSink for SurfaceSink {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        for matched in matches {
            matched_surface(sentence, matched.start..matched.end, location)?;
        }
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }
}

impl PreparedFixture {
    fn new() -> Self {
        let dictionary = MiniDictionary::new();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("corpus");
        fs::create_dir_all(root.join("shard")).unwrap();
        fs::write(root.join("a.md"), "猫。\n").unwrap();
        fs::hard_link(root.join("a.md"), root.join("alias.md")).unwrap();
        let external = directory.path().join("external.md");
        fs::write(&external, "犬。\n").unwrap();
        std::os::unix::fs::symlink(&external, root.join("shard/external.md")).unwrap();
        let patterns = directory.path().join("patterns.lx");
        fs::write(
            &patterns,
            "pattern captured := (?<unit> [text=猫]).\n\
             pattern punctuation := [text=。].\n\
             pattern tag := [xpos=名詞-普通名詞-一般].\n\
             pattern norm := [norm=猫].\n\
             pattern lextype := [unidic.lextype=system].\n",
        )
        .unwrap();
        let artifact = directory.path().join("corpus.lingcorpus");
        let build = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--output"])
            .arg(&artifact)
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        assert!(String::from_utf8_lossy(&build.stdout).contains("corpus inventory (recorded)"));
        Self {
            _directory: directory,
            root,
            artifact,
            patterns,
            dictionary,
        }
    }

    fn matches_direct(&self) -> Output {
        lingrep()
            .arg("--json")
            .arg("matches")
            .arg("--patterns")
            .arg(&self.patterns)
            .args(["--dict-path"])
            .arg(self.dictionary.path())
            .args(["--context", "1"])
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn matches_prepared(&self) -> Output {
        lingrep()
            .arg("--json")
            .args(["matches", "--patterns"])
            .arg(&self.patterns)
            .args(["--context", "1", "--corpus"])
            .arg(&self.artifact)
            .output()
            .unwrap()
    }

    fn tokenize_direct(&self) -> Output {
        lingrep()
            .arg("--json")
            .arg("tokenize")
            .arg("--dict-path")
            .arg(self.dictionary.path())
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn tokenize_prepared(&self) -> Output {
        lingrep()
            .arg("--json")
            .args(["tokenize", "--corpus"])
            .arg(&self.artifact)
            .output()
            .unwrap()
    }

    fn attributes_direct(&self, extra: &[&str]) -> Output {
        lingrep()
            .arg("coverage")
            .arg("--dict-path")
            .arg(self.dictionary.path())
            .args(extra)
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn attributes_prepared(&self, extra: &[&str]) -> Output {
        lingrep()
            .args(["coverage", "--corpus"])
            .arg(&self.artifact)
            .args(extra)
            .output()
            .unwrap()
    }
}

/// The `(attribute, present, unspecified, absent)` rows of a coverage stream./// The `(attribute, present, unspecified, absent)` rows of a coverage stream./// The `(attribute, present, unspecified, absent)` rows of a coverage stream.
fn coverage_rows(output: &Output) -> Vec<(String, u64, u64, u64)> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == "coverage")
        .map(|event| {
            let data = &event["data"];
            (
                data["attribute"].as_str().unwrap().to_owned(),
                data["present"].as_u64().unwrap(),
                data["unspecified"].as_u64().unwrap(),
                data["absent"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// A prepared run must reconstruct every column. Projecting
/// `required_attributes` -- right for `summarize` -- would report every
/// unprojected attribute absent, which is the wrong answer in the alarming
/// direction. Compared attribute-for-attribute over the *same* corpus, because
/// "at least one column survived" proves nothing and two different corpora
/// prove less.
#[test]
fn a_prepared_run_matches_the_direct_run_attribute_for_attribute() {
    let fixture = PreparedFixture::new();
    let direct = coverage_rows(&fixture.attributes_direct(&["--json"]));
    let prepared = coverage_rows(&fixture.attributes_prepared(&["--json"]));

    assert_eq!(direct.len(), 31);
    assert_eq!(prepared, direct);
}

impl JsonlEvidenceFixture {
    fn new() -> Self {
        let dictionary = MiniDictionary::new();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("evidence.txt");
        fs::write(&root, "猫と犬と猫\n").unwrap();
        let matches = directory.path().join("matches.lx");
        fs::write(
            &matches,
            "pattern left := (?<pair> [text=猫] [text=と] [text=犬]).\n\
             pattern right := [text=犬] [text=と] [text=猫].\n",
        )
        .unwrap();
        let no_matches = directory.path().join("no-matches.lx");
        fs::write(&no_matches, "pattern absent := [text=鳥].\n").unwrap();
        let artifact = directory.path().join("evidence.lingcorpus");
        let build = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--output"])
            .arg(&artifact)
            .arg(&root)
            .output()
            .unwrap();
        assert_success(&build);

        Self {
            _directory: directory,
            root,
            artifact,
            matches,
            no_matches,
            dictionary,
        }
    }

    fn query_direct(&self, catalog: &Path, extra: &[&str]) -> Output {
        lingrep()
            .arg("--json")
            .arg("matches")
            .arg("--patterns")
            .arg(catalog)
            .args(extra)
            .args(["--dict-path"])
            .arg(self.dictionary.path())
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn query_prepared(&self, artifact: &Path, catalog: &Path, extra: &[&str]) -> Output {
        lingrep()
            .arg("--json")
            .args(["matches", "--patterns"])
            .arg(catalog)
            .args(extra)
            .args(["--corpus"])
            .arg(artifact)
            .output()
            .unwrap()
    }
}

#[test]
fn prepared_query_ignores_inherited_dictionary_environment() {
    let fixture = PreparedFixture::new();
    let output = lingrep_without_dictionary()
        .env(
            "LINGREP_DICTIONARY_PATH",
            fixture._directory.path().join("missing-env-dictionary"),
        )
        .args(["matches", "--patterns"])
        .arg(&fixture.patterns)
        .args(["--corpus"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn event_type(event: &Value) -> &str {
    event["type"].as_str().unwrap()
}

fn semantic_match_events(output: &Output) -> Vec<Value> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| matches!(event_type(event), "definition" | "match"))
        .collect()
}

fn semantic_result_jsonl(output: &Output) -> Vec<u8> {
    semantic_match_events(output)
        .into_iter()
        .flat_map(|event| {
            let mut line = serde_json::to_vec(&event).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

fn assert_jsonl_evidence_parity(direct: &Output, prepared: &Output, records_emitted: u64) {
    assert_success(direct);
    assert_success(prepared);
    let direct_semantic = semantic_result_jsonl(direct);
    let prepared_semantic = semantic_result_jsonl(prepared);
    assert_eq!(direct_semantic, prepared_semantic);
    assert_eq!(
        blake3::hash(&direct_semantic),
        blake3::hash(&prepared_semantic)
    );

    let direct_events = parse_jsonl(&direct.stdout);
    let prepared_events = parse_jsonl(&prepared.stdout);
    assert_eq!(direct_events.len(), prepared_events.len());
    assert_eq!(
        direct_events.iter().map(event_type).collect::<Vec<_>>(),
        prepared_events.iter().map(event_type).collect::<Vec<_>>()
    );
    for events in [&direct_events, &prepared_events] {
        let match_count = events
            .iter()
            .filter(|event| event_type(event) == "match")
            .count() as u64;
        let complete = events
            .iter()
            .find(|event| event_type(event) == "complete")
            .unwrap();
        assert_eq!(match_count, records_emitted);
        assert_eq!(
            complete["data"]["counters"]["records_emitted"],
            records_emitted
        );
    }
    if direct_events
        .iter()
        .find(|event| event_type(event) == "complete")
        .unwrap()["data"]["coverage"]
        == "full"
    {
        assert_direct_prepared_lifecycle(direct, prepared);
    }
}

fn assert_direct_prepared_lifecycle(direct: &Output, prepared: &Output) {
    let direct = parse_jsonl(&direct.stdout);
    let prepared = parse_jsonl(&prepared.stdout);
    let direct_run = direct[0]["data"].as_object().unwrap();
    let prepared_run = prepared[0]["data"].as_object().unwrap();
    let direct_complete = direct
        .iter()
        .find(|event| event_type(event) == "complete")
        .unwrap()["data"]
        .as_object()
        .unwrap();
    let prepared_complete = prepared
        .iter()
        .find(|event| event_type(event) == "complete")
        .unwrap()["data"]
        .as_object()
        .unwrap();

    assert_eq!(direct_run["corpus_kind"], "direct");
    assert_eq!(direct_run["state"], "pending");
    assert!(!direct_run.contains_key("source_identity"));
    assert!(!direct_run.contains_key("analysis_identity"));
    assert!(direct_run["inventory"].is_null());
    assert_eq!(prepared_run["corpus_kind"], "prepared");
    assert!(!prepared_run.contains_key("state"));
    assert_eq!(
        prepared_run["source_identity"],
        prepared_complete["source_identity"]
    );
    assert_eq!(
        prepared_run["analysis_identity"],
        prepared_complete["analysis_identity"]
    );
    assert_eq!(prepared_run["inventory"], prepared_complete["inventory"]);

    assert_eq!(direct_complete["corpus_kind"], "direct");
    assert_eq!(direct_complete["state"], "known");
    assert_eq!(prepared_complete["corpus_kind"], "prepared");
    assert!(!prepared_complete.contains_key("state"));
    for field in ["source_identity", "analysis_identity", "inventory"] {
        assert_eq!(direct_complete[field], prepared_complete[field], "{field}");
    }

    for field in [
        "mode",
        "catalog_fingerprint",
        "definition_count",
        "dictionary_identity",
        "reader_identity",
        "token_schema_identity",
        "ordering",
        "span_convention",
        "requested_context",
        "requested_limit",
        "requested_top",
        "execution_limits",
        "temporary_storage_limits",
        "traversal_limits",
    ] {
        assert_eq!(direct_run[field], prepared_run[field], "run {field}");
    }
    for field in [
        "coverage",
        "counters",
        "committed_prefix",
        "inventory",
        "termination",
    ] {
        assert_eq!(
            direct_complete[field], prepared_complete[field],
            "complete {field}"
        );
    }
    assert_eq!(
        direct_complete["resources"]["execution_limits"],
        prepared_complete["resources"]["execution_limits"]
    );
    assert_eq!(
        direct_complete["resources"]["temporary_storage_limits"],
        prepared_complete["resources"]["temporary_storage_limits"]
    );
}

fn complete_identity(output: &Output) -> (String, String) {
    let events = parse_jsonl(&output.stdout);
    let complete = events
        .iter()
        .find(|event| event_type(event) == "complete")
        .unwrap();
    (
        complete["data"]["source_identity"]
            .as_str()
            .unwrap()
            .to_owned(),
        complete["data"]["analysis_identity"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn sentence_events(output: &Output) -> Vec<Value> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event_type(event) == "sentence")
        .collect()
}

fn compile_matcher(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("prepared-differential.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn execute_recorded<S, F>(
    source: S,
    tokenize: F,
    matcher: &CompiledCatalog,
) -> Result<Vec<Match>, ExecutionError>
where
    S: SentenceBatchSource + Send,
    F: Fn(&str) -> Vec<TokenInfo> + Sync,
{
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut sink = RecordingSink::default();
    let mut progress = |_, _: &RecordingSink| {};
    let mut observer = |_: TokenizedBatchObservation| Ok(());
    let outcome = execute(
        source,
        tokenize,
        matcher,
        &pool,
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )?;
    assert!(matches!(outcome, RunOutcome::Completed(_)));
    Ok(sink.matches)
}

fn execute_surface<S, F>(
    source: S,
    tokenize: F,
    matcher: &CompiledCatalog,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    F: Fn(&str) -> Vec<TokenInfo> + Sync,
{
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut sink = SurfaceSink;
    let mut progress = |_, _: &SurfaceSink| {};
    let mut observer = |_: TokenizedBatchObservation| Ok(());
    execute(
        source,
        tokenize,
        matcher,
        &pool,
        &ExecutionLimits::default(),
        1,
        &CancellationFlag::new(),
        &mut sink,
        &mut progress,
        &mut observer,
        lingrep_core::execution::PolicySelection::Apply,
        &SentenceAdmission::All,
    )
}

#[test]
fn multi_record_jsonl_builds_one_admitted_document_per_record() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("documents.jsonl");
    fs::write(
        &root,
        "{\"title\":\"cat\",\"sentences\":[\"猫。\"]}\n\
         {\"title\":\"dog\",\"sentences\":[\"犬。\"]}\n",
    )
    .unwrap();
    let artifact = directory.path().join("documents.lingcorpus");
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "animal := [text in {猫, 犬}].\n").unwrap();

    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .args(["--output"])
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&build);

    let documents = lingrep()
        .args(["--json", "corpus", "inspect", "--documents"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&documents);
    let documents = parse_jsonl(&documents.stdout);
    assert_eq!(
        documents[0]["data"]["aggregate_state"]["inventory"]["documents"], 1,
        "the discovery inventory remains the one physical JSONL source"
    );
    let logical = documents
        .iter()
        .filter(|event| event_type(event) == "source")
        .map(|event| &event["data"])
        .collect::<Vec<_>>();
    assert_eq!(
        logical
            .iter()
            .map(|document| document["logical_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["documents.jsonl line 1", "documents.jsonl line 2"]
    );
    assert_eq!(
        logical
            .iter()
            .map(|document| document["target_group_ordinal"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 0]
    );
    assert_eq!(
        logical
            .iter()
            .map(|document| document["target_multiplicity"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [2, 2]
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(artifact.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["inventory"]["documents"], 1);
    assert_eq!(manifest["admitted_inventory"]["documents"], 2);
    assert_eq!(manifest["admitted_inventory"]["distinct_targets"], 1);

    let verify = lingrep()
        .args(["--json", "corpus", "inspect", "--verify"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&verify);

    let direct = lingrep()
        .arg("--json")
        .args(["matches", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&direct);
    let prepared = lingrep()
        .arg("--json")
        .args(["matches", "--patterns"])
        .arg(&patterns)
        .args(["--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&prepared);
    assert_direct_prepared_lifecycle(&direct, &prepared);
    assert_eq!(
        semantic_result_jsonl(&direct),
        semantic_result_jsonl(&prepared)
    );
}

#[test]
fn compressed_build_inspect_verify_and_documents_use_the_stored_projection() {
    let fixture = PreparedFixture::new();

    let live = lingrep()
        .args(["--json", "corpus", "inventory"])
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert_success(&live);

    let inspect = lingrep()
        .args(["--json", "corpus", "inspect"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_success(&inspect);
    let events = parse_jsonl(&inspect.stdout);
    assert_eq!(
        events.iter().map(event_type).collect::<Vec<_>>(),
        ["run", "complete"]
    );
    assert_eq!(events[0]["data"]["source_kind"], "artifact");
    assert_eq!(events[0]["data"]["aggregate_state"]["state"], "known");
    assert_eq!(
        events[0]["data"]["aggregate_state"]["inventory"]["documents"],
        3
    );

    let verify = lingrep()
        .args(["--json", "corpus", "inspect", "--verify"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_success(&verify);
    let events = parse_jsonl(&verify.stdout);
    let complete = events.last().unwrap();
    assert_eq!(complete["type"], "complete");
    assert_eq!(
        complete["data"]["verification"]["compared_location_rows"],
        3
    );
    assert_eq!(
        complete["data"]["verification"]["components"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    fs::remove_dir_all(&fixture.root).unwrap();
    fs::remove_file(fixture._directory.path().join("external.md")).unwrap();
    let documents = lingrep()
        .args(["--json", "corpus", "inspect", "--documents"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_success(&documents);
    let events = parse_jsonl(&documents.stdout);
    assert_eq!(
        events.iter().map(event_type).collect::<Vec<_>>(),
        ["run", "source", "source", "source", "complete"]
    );
    let sources = events
        .iter()
        .filter(|event| event_type(event) == "source")
        .map(|event| &event["data"])
        .collect::<Vec<_>>();
    assert_eq!(
        sources
            .iter()
            .map(|source| source["logical_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["a.md", "alias.md", "shard/external.md"]
    );
    assert_eq!(sources[0]["target_group_ordinal"], 0);
    assert_eq!(sources[1]["target_group_ordinal"], 0);
    assert_eq!(sources[0]["target_multiplicity"], 2);
    assert_eq!(sources[1]["target_multiplicity"], 2);
    assert_eq!(sources[2]["external_target"], true);

    let live = parse_jsonl(&live.stdout);
    let live_sources = live
        .iter()
        .filter(|event| event_type(event) == "source")
        .map(|event| {
            (
                event["data"]["ordinal"].clone(),
                event["data"]["logical_name"].clone(),
                event["data"]["target_group_ordinal"].clone(),
                event["data"]["external_target"].clone(),
            )
        })
        .collect::<Vec<_>>();
    let stored_sources = sources
        .iter()
        .map(|source| {
            (
                source["ordinal"].clone(),
                source["logical_name"].clone(),
                source["target_group_ordinal"].clone(),
                source["external_target"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(live_sources, stored_sources);
    assert_eq!(
        live.last().unwrap()["data"]["inventory"],
        events.last().unwrap()["data"]["inventory"]
    );
}

#[test]
fn direct_and_compressed_prepared_matches_have_equal_complete_evidence_and_identity() {
    let fixture = PreparedFixture::new();
    let direct = fixture.matches_direct();
    let prepared = fixture.matches_prepared();
    assert_success(&direct);
    assert_success(&prepared);

    assert_eq!(
        semantic_result_jsonl(&direct),
        semantic_result_jsonl(&prepared)
    );
    assert_eq!(complete_identity(&direct), complete_identity(&prepared));
    assert_direct_prepared_lifecycle(&direct, &prepared);

    let matches = semantic_match_events(&prepared)
        .into_iter()
        .filter(|event| event_type(event) == "match")
        .collect::<Vec<_>>();
    assert!(!matches.is_empty());
    let captured = matches
        .iter()
        .find(|event| event["data"]["pattern"] == "captured")
        .unwrap();
    assert_eq!(captured["data"]["source_name"], "a.md");
    assert_eq!(captured["data"]["document_ordinal"], 0);
    assert_eq!(captured["data"]["document_sentence_ordinal"], 0);
    assert_eq!(captured["data"]["surface"], "猫");
    assert_eq!(captured["data"]["captures"][0]["name"], "unit");
    assert_eq!(
        captured["data"]["captures"][0]["hygienic_path"],
        "captured/unit"
    );
    assert_eq!(
        captured["data"]["right_context"][0]["attributes"]["text"],
        "。"
    );

    let prepared_run = parse_jsonl(&prepared.stdout);
    let prepared_run = &prepared_run[0]["data"];
    assert_eq!(prepared_run["corpus_kind"], "prepared");
    assert!(prepared_run["source_identity"].as_str().is_some());
    assert!(prepared_run["analysis_identity"].as_str().is_some());
    assert_eq!(prepared_run["inventory"]["alias_documents"], 1);
    assert_eq!(prepared_run["inventory"]["external_targets"], 1);
}

#[test]
fn prepared_jsonl_late_materialization_preserves_evidence() {
    let fixture = JsonlEvidenceFixture::new();
    let direct_context_0 = fixture.query_direct(&fixture.matches, &["--context", "0"]);
    let direct_context_2 = fixture.query_direct(&fixture.matches, &["--context", "2"]);
    let direct_no_matches = fixture.query_direct(&fixture.no_matches, &[]);
    let direct_limit = fixture.query_direct(&fixture.matches, &["--context", "2", "--limit", "1"]);

    {
        let artifact = &fixture.artifact;
        for (context, direct) in [("0", &direct_context_0), ("2", &direct_context_2)] {
            let extra = ["--context", context];
            let prepared = fixture.query_prepared(artifact, &fixture.matches, &extra);
            assert_jsonl_evidence_parity(direct, &prepared, 2);

            let matches = semantic_match_events(&prepared)
                .into_iter()
                .filter(|event| event_type(event) == "match")
                .collect::<Vec<_>>();
            assert_eq!(matches[0]["data"]["pattern"], "left");
            assert_eq!(
                matches[0]["data"]["span"],
                serde_json::json!({"start": 0, "end": 3})
            );
            assert_eq!(matches[0]["data"]["captures"][0]["name"], "pair");
            assert_eq!(
                matches[0]["data"]["captures"][0]["span"],
                matches[0]["data"]["span"]
            );
            assert_eq!(matches[1]["data"]["pattern"], "right");
            assert_eq!(
                matches[1]["data"]["span"],
                serde_json::json!({"start": 2, "end": 5})
            );
        }

        let prepared_no_matches = fixture.query_prepared(artifact, &fixture.no_matches, &[]);
        assert_jsonl_evidence_parity(&direct_no_matches, &prepared_no_matches, 0);

        let prepared_limit = fixture.query_prepared(
            artifact,
            &fixture.matches,
            &["--context", "2", "--limit", "1"],
        );
        assert_jsonl_evidence_parity(&direct_limit, &prepared_limit, 1);
        for output in [&direct_limit, &prepared_limit] {
            let events = parse_jsonl(&output.stdout);
            let complete = events
                .iter()
                .find(|event| event_type(event) == "complete")
                .unwrap();
            assert_eq!(complete["data"]["coverage"], "prefix");
            assert_eq!(complete["data"]["termination"], "result_limit");
            assert_eq!(complete["data"]["committed_prefix"]["sequence"], 0);
        }
    }
}

#[test]
fn direct_and_prepared_surface_formats_are_byte_identical() {
    let fixture = PreparedFixture::new();
    for format in ["tsv", "aligned"] {
        let direct = lingrep()
            .args(["matches", "--patterns"])
            .arg(&fixture.patterns)
            .args(["--dict-path"])
            .arg(fixture.dictionary.path())
            .args(["--context", "1"])
            .args((format == "tsv").then_some("--tsv"))
            .arg(&fixture.root)
            .output()
            .unwrap();
        let prepared = lingrep()
            .args(["matches", "--patterns"])
            .arg(&fixture.patterns)
            .args(["--context", "1"])
            .args((format == "tsv").then_some("--tsv"))
            .arg("--corpus")
            .arg(&fixture.artifact)
            .output()
            .unwrap();

        assert_success(&direct);
        assert_success(&prepared);
        assert_eq!(direct.stdout, prepared.stdout, "format: {format}");
    }
}

#[test]
fn prepared_matches_and_tokenize_equal_the_direct_stream() {
    let fixture = PreparedFixture::new();
    let direct_matches = fixture.matches_direct();
    let prepared_matches = fixture.matches_prepared();
    assert_success(&direct_matches);
    assert_success(&prepared_matches);
    assert_eq!(
        semantic_result_jsonl(&direct_matches),
        semantic_result_jsonl(&prepared_matches)
    );
    assert_eq!(
        complete_identity(&direct_matches),
        complete_identity(&prepared_matches)
    );
    assert_direct_prepared_lifecycle(&direct_matches, &prepared_matches);

    let direct_tokens = fixture.tokenize_direct();
    let prepared_tokens = fixture.tokenize_prepared();
    assert_success(&direct_tokens);
    assert_success(&prepared_tokens);
    assert_eq!(
        sentence_events(&direct_tokens),
        sentence_events(&prepared_tokens)
    );
    assert_eq!(
        complete_identity(&direct_tokens),
        complete_identity(&prepared_tokens)
    );
    let run = parse_jsonl(&prepared_tokens.stdout);
    assert_eq!(run[0]["data"]["corpus_kind"], "prepared");
}

#[test]
fn normal_inspect_is_lazy_but_verify_rejects_a_corrupt_token_payload() {
    let fixture = PreparedFixture::new();
    let token_path = fixture.artifact.join("tokens.v1.zst");
    let mut bytes = fs::read(&token_path).unwrap();
    let schema_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    bytes[128 + schema_len + 8] ^= 0x55;
    fs::write(&token_path, bytes).unwrap();

    let normal = lingrep()
        .args(["--json", "corpus", "inspect"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_success(&normal);

    let verified = lingrep()
        .args(["--json", "corpus", "inspect", "--verify"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(1));
    assert!(verified.stdout.is_empty());
    let diagnostic = parse_jsonl(&verified.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "component_checksum_mismatch");
}

/// `matches` and `summarize` must answer a poisoned artifact the same way.
///
/// `3d031a49` gave the prepared path a recovery — omit the oversized
/// sentence, count it, exit 1 with `oversized_sentences_skipped` and no
/// `complete` record — because the hard failure it replaced "made `--corpus`
/// on a poisoned artifact permanently unusable". It wired that into `matches`
/// and not into `summarize`, so over `markdown-corpus` g5 one command scanned
/// the artifact and the other stopped at sentence 338066 and published
/// nothing. Same artifact, same limit, opposite answers.
///
/// The narrow *direct*-markdown recovery is deliberately `matches`-only and
/// stays that way; this is about the prepared path alone.
#[test]
fn matches_and_summarize_recover_from_a_poisoned_artifact_the_same_way() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("poisoned.lingcorpus");
    let tokens: Vec<TokenInfo> = (0..64)
        .map(|index| TokenInfo::default().with_text(format!("t{index}")))
        .collect();
    runtime.block_on(build_single_sentence_artifact(
        &artifact, "surface", &tokens,
    ));
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();

    // Small enough that the artifact's one sentence cannot be admitted. The
    // artifact is built at the default bound and queried under a lower one,
    // which is exactly the shape a corpus acquires when a limit is tightened
    // after the fact.
    let run = |command: &str| {
        lingrep_without_dictionary()
            .args(["--json", command, "--patterns"])
            .arg(&patterns)
            .args(["--corpus"])
            .arg(&artifact)
            .args(["--max-tokenized-bytes", "512"])
            .output()
            .unwrap()
    };

    for command in ["matches", "summarize"] {
        let output = run(command);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{command}: a degraded run is not a success",
        );
        let diagnostic = parse_jsonl(&output.stderr);
        assert_eq!(
            diagnostic[0]["data"]["code"],
            "oversized_sentences_skipped",
            "{command}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            diagnostic[0]["data"]["message"]
                .as_str()
                .unwrap()
                .contains("count=1"),
            "{command}: the count is what stopping at the first cannot report",
        );
        assert!(
            !parse_jsonl(&output.stdout)
                .iter()
                .any(|event| event["type"] == "complete"),
            "{command}: a degraded run publishes no successful completion",
        );
    }
}

#[test]
fn prepared_match_never_substitutes_text_for_missing_orth() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("missing-orth.lingcorpus");
    let missing = TokenInfo::default().with_text("surface");
    runtime.block_on(build_single_sentence_artifact(
        &artifact,
        "surface",
        std::slice::from_ref(&missing),
    ));
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();

    let output = lingrep_without_dictionary()
        .args(["--json", "matches", "--patterns"])
        .arg(&patterns)
        .args(["--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let events = parse_jsonl(&output.stdout);
    assert_eq!(
        events.iter().map(event_type).collect::<Vec<_>>(),
        ["run", "definition"]
    );
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "missing_required_attribute");
    assert_eq!(
        diagnostic[0]["data"]["location"],
        serde_json::json!({
            "source_name": "a.md",
            "source_span": null,
            "corpus_sentence_ordinal": 0,
            "token_index": 0
        })
    );

    let tokens = [TokenInfo::default().with_text("surface")];
    let location = SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 0,
        document_id: DocumentId::new(0),
        document_ordinal: 0,
        document_sentence_ordinal: 0,
        source_name: "a.md".into(),
    };
    let direct = matched_surface(&tokens, 0..1, &location).unwrap_err();
    assert_eq!(
        diagnostic[0]["data"]["location"]["source_name"],
        direct.source_name
    );
    assert_eq!(
        diagnostic[0]["data"]["location"]["corpus_sentence_ordinal"],
        direct.corpus_sentence_ordinal
    );
    assert_eq!(
        diagnostic[0]["data"]["location"]["token_index"],
        direct.token_index
    );

    let matcher = compile_matcher("pattern any := [].\n");
    let direct_error = execute_surface(
        OneBatchSource(Some(ExecutionBatch::Text(vec![ExecutionSentence {
            location: location.clone(),
            value: TextUnit::sentence(
                InputUnitId::new(location.corpus_sentence_ordinal),
                "raw",
                NormalizationPolicy::None,
            )
            .unwrap(),
        }]))),
        move |_| vec![missing.clone()],
        &matcher,
    )
    .unwrap_err();
    let prepared_error = execute_surface(
        CorpusArtifact::open(&artifact)
            .unwrap()
            .sentence_source()
            .unwrap(),
        |_| panic!("prepared execution must not tokenize raw text"),
        &matcher,
    )
    .unwrap_err();
    assert_eq!(
        missing_attribute_location(&direct_error),
        missing_attribute_location(&prepared_error)
    );
}

#[test]
fn direct_and_v6_queries_keep_distinct_tag_norm_and_lextype_columns() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("distinct-attributes.lingcorpus");
    let token = TokenInfo::default()
        .with_orth("ORTH")
        .with_tag("TAG")
        .with_norm("NORM")
        .with_text("TEXT")
        .with_lex_type("LEX");
    runtime.block_on(build_single_sentence_artifact(
        &artifact,
        "surface",
        std::slice::from_ref(&token),
    ));
    let matcher = compile_matcher(
        "pattern orth := [text=ORTH].\n\
         pattern tag := [xpos=TAG].\n\
         pattern norm := [norm=NORM].\n\
         pattern lextype := [unidic.lextype=LEX].\n",
    );
    let location = SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 0,
        document_id: DocumentId::new(0),
        document_ordinal: 0,
        document_sentence_ordinal: 0,
        source_name: "a.md".into(),
    };
    let direct = execute_recorded(
        OneBatchSource(Some(ExecutionBatch::Text(vec![ExecutionSentence {
            value: TextUnit::sentence(
                InputUnitId::new(location.corpus_sentence_ordinal),
                "raw",
                NormalizationPolicy::None,
            )
            .unwrap(),
            location,
        }]))),
        move |_| vec![token.clone()],
        &matcher,
    )
    .unwrap();
    let prepared = execute_recorded(
        CorpusArtifact::open(&artifact)
            .unwrap()
            .sentence_source()
            .unwrap(),
        |_| panic!("prepared execution must not tokenize raw text"),
        &matcher,
    )
    .unwrap();

    assert_eq!(direct, prepared);
    assert_eq!(
        direct
            .iter()
            .map(|matched| matched.pattern.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["lextype", "norm", "orth", "tag"])
    );
}

#[test]
fn build_never_replaces_an_existing_root() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "猫。\n").unwrap();
    for populated in [false, true] {
        let destination = directory
            .path()
            .join(format!("existing-{populated}.lingcorpus"));
        fs::create_dir(&destination).unwrap();
        if populated {
            fs::write(destination.join("sentinel"), "keep").unwrap();
        }
        let output = lingrep()
            .args(["--json", "corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--output"])
            .arg(&destination)
            .arg(&root)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let diagnostics = parse_jsonl(&output.stderr);
        assert_eq!(diagnostics[0]["data"]["code"], "destination_exists");
        if populated {
            assert_eq!(
                fs::read_to_string(destination.join("sentinel")).unwrap(),
                "keep"
            );
        } else {
            assert!(fs::read_dir(destination).unwrap().next().is_none());
        }
    }
}

/// Exceeding the residency limit terminates with an error; exceeding the sentence character limit truncates.
///
/// The two look symmetric and are not. An oversized *sentence* is invalid input,
/// and a build truncates it and reports the truncation rather than losing the corpus around it
/// — see `an_oversized_sentence_is_truncated_and_named_rather_than_refused`.
/// An oversized *tokenized* sentence is a resource bound on this process, and
/// there is nothing to truncate: the sentence already fits its own limit, and
/// what exceeded is what analyzing it costs. Rejecting the build remains correct there.
#[test]
fn build_enforces_tokenized_residency_before_publication() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "猫。\n").unwrap();

    let artifact = directory.path().join("tokenized.lingcorpus");
    let output = lingrep()
        .args(["--json", "corpus", "build", "--max-tokenized-bytes", "1"])
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--output"])
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(!artifact.exists());
    assert_eq!(
        parse_jsonl(&output.stderr)[0]["data"]["code"],
        "tokenized_sentence_too_large"
    );
}

/// A build under a sentence limit nothing can satisfy still publishes, with
/// every sentence truncated to it. The limit bounds what is stored; it is no
/// longer a way to reject a corpus.
#[test]
fn build_truncates_rather_than_refusing_an_oversized_sentence() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "猫。\n").unwrap();

    let artifact = directory.path().join("truncated.lingcorpus");
    let output = lingrep()
        .args(["corpus", "build", "--max-sentence-bytes", "1"])
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--output"])
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();

    assert_success(&output);
    assert!(artifact.exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("warning[sentence_truncated]"),
        "the loss must be reported: {stderr}"
    );
}

#[test]
fn direct_oversize_gap_preserves_prepared_ordinals() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("sentences.md");
    std::fs::write(&source, "猫。\n犬犬犬。\n鳥。\n").unwrap();
    let artifact = directory.path().join("sentences.lingcorpus");
    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&source)
        .output()
        .unwrap();
    assert_success(&build);

    let direct = lingrep()
        .args(["--json", "matches", "--dict-path"])
        .arg(dictionary.path())
        .args(["--pattern", "[text=鳥].", "--max-sentence-bytes", "9"])
        .arg(&source)
        .output()
        .unwrap();
    let prepared = lingrep()
        .args(["--json", "matches", "--pattern", "[text=鳥].", "--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();

    let direct_bird = semantic_match_events(&direct)
        .into_iter()
        .find(|event| event["data"]["surface"] == "鳥")
        .unwrap();
    let prepared_bird = semantic_match_events(&prepared)
        .into_iter()
        .find(|event| event["data"]["surface"] == "鳥")
        .unwrap();
    assert_eq!(direct.status.code(), Some(1));
    assert!(prepared.status.success());
    assert_eq!(direct_bird["data"]["corpus_sentence_ordinal"], 2);
    assert_eq!(
        direct_bird["data"]["corpus_sentence_ordinal"],
        prepared_bird["data"]["corpus_sentence_ordinal"]
    );
    assert_eq!(
        direct_bird["data"]["document_sentence_ordinal"],
        prepared_bird["data"]["document_sentence_ordinal"]
    );
}

#[test]
fn corpus_build_rejects_omitted_and_dash_roots_without_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = MiniDictionary::new();
    for (name, root) in [("omitted", None), ("dash", Some("-"))] {
        let artifact = directory.path().join(format!("{name}.lingcorpus"));
        let mut command = lingrep();
        command
            .args(["--json", "corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--output"])
            .arg(&artifact);
        if let Some(root) = root {
            command.arg(root);
        }
        let output = command.write_stdin("猫。\n").output().unwrap();

        assert_eq!(output.status.code(), Some(2), "{name}");
        assert!(output.stdout.is_empty(), "{name}");
        assert!(!artifact.exists(), "{name}");
        assert_eq!(
            parse_jsonl(&output.stderr)[0]["data"]["code"],
            "invalid_invocation",
            "{name}"
        );
    }
}

/// An artifact this build produced must not be reported as built by another.
///
/// The notice compares the artifact's recorded token schema identity against
/// the one this binary computes, and those agree only if `corpus build` and
/// the query derive it the same way. If they ever diverge, every prepared run
/// in the workspace grows a warning that is always wrong — and no existing
/// test would say so, because the prepared assertions here read stderr only to
/// print it on failure.
///
/// The mismatch direction is a unit test in `commands::matches`: it can force
/// a differing digest, which no integration run can do without a second binary
/// built under a different structural token schema.
#[test]
fn a_freshly_built_artifact_is_not_reported_as_token_schema_stale() {
    let fixture = PreparedFixture::new();
    for output in [fixture.matches_prepared(), fixture.tokenize_prepared()] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("featurization_policy_mismatch"),
            "a round-tripped artifact must not warn, got {stderr}"
        );
    }
}

/// A published token carries exactly what the run declared, and nothing more.
///
/// This is the projection change: publication used to emit the whole attribute
/// schema whatever the catalog asked about.
#[test]
fn a_published_token_carries_exactly_the_declared_projection() {
    let fixture = PreparedFixture::new();
    let prepared = fixture.matches_prepared();
    assert_success(&prepared);
    let events = parse_jsonl(&prepared.stdout);

    let declared: Vec<&str> = events[0]["data"]["published_attributes"]
        .as_array()
        .expect("the run record declares the projection")
        .iter()
        .map(|value| value.as_str().expect("attribute name"))
        .collect();
    assert!(declared.contains(&"text"), "text is always published");
    let mut sorted = declared.clone();
    sorted.sort_unstable();
    assert_eq!(declared, sorted, "declared in schema key order");

    let matched = events
        .iter()
        .find(|event| event["type"] == "match")
        .expect("at least one match");
    let published: Vec<&str> = matched["data"]["tokens"][0]["attributes"]
        .as_object()
        .expect("attributes object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(
        !published.is_empty(),
        "a token still carries the attributes it was matched on: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    for key in &published {
        assert!(
            declared.contains(key),
            "published {key} was not declared: {declared:?}"
        );
    }
    assert!(
        published.len() < lingrep_core::token_match::TokenAttributeIndex::COUNT,
        "the whole schema is no longer published: {published:?}"
    );
}

/// The identity is a property of the run, not of each of its 12.2M tokens.
#[test]
fn a_published_token_does_not_repeat_the_schema_identity() {
    let fixture = PreparedFixture::new();
    let prepared = fixture.matches_prepared();
    assert_success(&prepared);
    let events = parse_jsonl(&prepared.stdout);

    assert!(
        events[0]["data"]["token_schema_identity"].is_string(),
        "the run record still states it once"
    );
    let matched = events
        .iter()
        .find(|event| event["type"] == "match")
        .expect("at least one match");
    assert!(
        matched["data"]["tokens"][0]["token_schema_identity"].is_null(),
        "and no token repeats it"
    );
}

/// `cached_run_attributes` force-adds `orth`, which is what keeps `surface` and
/// context rendering working for a catalog that never names it. The guarantee
/// lives in a different crate from the code that depends on it.
#[test]
fn surface_survives_a_catalog_that_never_mentions_orth() {
    let fixture = PreparedFixture::new();
    let prepared = fixture.matches_prepared();
    assert_success(&prepared);
    let matched = parse_jsonl(&prepared.stdout)
        .into_iter()
        .find(|event| event["type"] == "match")
        .expect("at least one match");
    assert!(
        !matched["data"]["surface"].as_str().unwrap().is_empty(),
        "surface must not come back empty"
    );
}

/// `--progress always` names the projection while the researcher can still act.
#[test]
fn progress_always_names_the_published_projection() {
    let fixture = PreparedFixture::new();
    let output = lingrep()
        .arg("--json")
        .args(["matches", "--patterns"])
        .arg(&fixture.patterns)
        .args(["--context", "1", "--progress", "always", "--corpus"])
        .arg(&fixture.artifact)
        .output()
        .unwrap();
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("publishing token attributes:"),
        "stderr should name the projection, got: {stderr}"
    );
    assert!(stderr.contains("text"), "and list them: {stderr}");
}

/// Prepared artifact directories directly under `parent`, sorted.
fn artifact_directories(parent: &Path) -> Vec<PathBuf> {
    let mut paths = std::fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lingcorpus"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

/// Every entry directly inside `directory`, by name, with the bytes of each
/// regular file. `None` marks an entry that is not a regular file, so an added
/// subdirectory shows up as a changed snapshot rather than a missing read.
fn directory_snapshot(directory: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut entries = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            (
                path.file_name().unwrap().to_str().unwrap().to_owned(),
                path.is_file().then(|| fs::read(&path).unwrap()),
            )
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

/// Human inventory output with the wall-clock tail of the `complete:` line
/// normalized away. Two runs over one artifact report the same counts; they do
/// not report the same milliseconds, and under CPU contention they differ.
fn without_elapsed(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| match line.strip_prefix("complete: ") {
            Some(rest) => match rest.rsplit_once(", ") {
                Some((counts, _millis)) => format!("complete: {counts}, <elapsed>"),
                None => line.to_owned(),
            },
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The test binary cannot recompute the analysis identity, so it pins the
/// canonical *shape*:
/// `<stem>.g<generation>.a<scheme>.<64 lowercase hex>.<trust>.lingcorpus`.
///
/// The scheme component is checked separately from the generation because they
/// are separate axes: an identity re-keying moves `a` and leaves `g` alone, and
/// a name that merged them would hide exactly that.
fn assert_canonical_name(path: &Path, stem: &str) {
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    let marker = format!(".g{CORPUS_ARTIFACT_FORMAT_GENERATION}.a1.");
    let Some(rest) = name
        .strip_prefix(stem)
        .and_then(|rest| rest.strip_prefix(marker.as_str()))
    else {
        panic!("artifact {name:?} is not canonically named for stem {stem:?}");
    };
    let Some(hex) = rest.strip_suffix(".locally-executed.lingcorpus") else {
        panic!("artifact {name:?} does not end in .lingcorpus");
    };
    assert_eq!(hex.len(), 64, "analysis-identity segment: {name:?}");
    assert!(
        hex.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{name:?}"
    );
}

/// The canonical default rests on the commit-time no-replace invariant, so
/// pin that invariant directly rather than only through the default.
#[test]
fn corpus_build_with_explicit_output_never_replaces_an_existing_artifact() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    let artifact = directory.path().join("explicit.lingcorpus");
    fs::write(&source, "猫。\n").unwrap();

    let first = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let manifest = fs::read(artifact.join("manifest.json")).unwrap();

    let second = lingrep()
        .args(["--json", "corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&source)
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(second.stdout.is_empty());
    assert_eq!(
        parse_jsonl(&second.stderr)[0]["data"]["code"],
        "destination_exists"
    );
    assert_eq!(
        fs::read(artifact.join("manifest.json")).unwrap(),
        manifest,
        "the existing artifact must be untouched"
    );
}

/// `.` has no file name, so an unresolved root would nest the artifact inside
/// the very directory being scanned, as a hidden dotfile.
#[test]
fn corpus_build_without_output_resolves_a_nameless_root_before_naming() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus");
    fs::create_dir(&corpus).unwrap();
    fs::write(corpus.join("one.md"), "猫。\n").unwrap();

    let output = lingrep()
        .current_dir(&corpus)
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(".")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        artifact_directories(&corpus).is_empty(),
        "the artifact must not nest inside the scanned root"
    );
    let artifacts = artifact_directories(directory.path());
    assert_eq!(artifacts.len(), 1, "the artifact belongs beside the root");
    assert_canonical_name(&artifacts[0], "corpus");
}

/// A named root is passed through as spelled, symlink included: the artifact
/// sits beside the path the caller wrote. Resolving here would place it beside
/// the link target instead, which is a different artifact for the same
/// invocation.
#[test]
fn corpus_build_without_output_names_a_symlinked_root_as_spelled() {
    use std::os::unix::fs::symlink;

    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let real = directory.path().join("real");
    let link_parent = directory.path().join("view");
    fs::create_dir(&real).unwrap();
    fs::create_dir(&link_parent).unwrap();
    fs::write(real.join("one.md"), "猫。\n").unwrap();
    let link = link_parent.join("corpus");
    symlink(&real, &link).unwrap();

    let output = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&link)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        artifact_directories(directory.path()).is_empty(),
        "the artifact must not land beside the link target's parent"
    );
    let artifacts = artifact_directories(&link_parent);
    assert_eq!(artifacts.len(), 1, "the artifact belongs beside the link");
    assert_canonical_name(&artifacts[0], "corpus");

    // Exact here, unlike the nameless-root case: no `canonicalize` stands
    // between the spelled root and the published destination.
    let artifact = CorpusArtifact::open(&artifacts[0]).unwrap();
    assert_eq!(
        artifacts[0],
        canonical_artifact_path(
            &link,
            artifact.analysis_identity(),
            TrustClass::LocallyExecuted,
        ),
        "a symlinked root must be named from the link, not its target"
    );
}

/// The shape assertions elsewhere would accept any 64-hex name. This pins the
/// exact path: the CLI's destination is what `lingrep_corpus::canonical_artifact_path`
/// computes for the same root and the artifact's own recorded identity. That is
/// agreement with the shared naming function, which is all it claims — whether
/// lingrep-web derives the *same* identity for the same root is a reader-identity
/// question this proves nothing about, and for Markdown the answer is no.
#[test]
fn corpus_build_without_output_publishes_where_lingrep_corpus_names_it() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();

    let output = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let artifacts = artifact_directories(directory.path());
    assert_eq!(artifacts.len(), 1, "exactly one artifact beside the source");
    assert_canonical_name(&artifacts[0], "source.md");

    let artifact = CorpusArtifact::open(&artifacts[0]).unwrap();
    assert_eq!(
        artifacts[0],
        canonical_artifact_path(
            &source,
            artifact.analysis_identity(),
            TrustClass::LocallyExecuted,
        ),
        "the CLI must publish exactly where lingrep-corpus names the artifact"
    );
}

/// A second canonical build on unchanged input must not rebuild: the
/// destination already holds an artifact of exactly this identity, so the run
/// reports it and exits, leaving the published bytes alone.
#[test]
fn corpus_build_without_output_reuses_an_existing_matching_artifact() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();

    let first = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let artifacts = artifact_directories(directory.path());
    assert_eq!(artifacts.len(), 1);
    let manifest = fs::read(artifacts[0].join("manifest.json")).unwrap();

    let second = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "reuse run failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let after = artifact_directories(directory.path());
    assert_eq!(after.len(), 1, "reuse must not create a second artifact");
    assert_eq!(
        fs::read(after[0].join("manifest.json")).unwrap(),
        manifest,
        "reuse must not rewrite the manifest"
    );
    assert_eq!(
        without_elapsed(&second.stdout),
        without_elapsed(&first.stdout),
        "a reuse reports the artifact exactly as the build that published it"
    );
    assert_eq!(
        without_elapsed(&second.stdout),
        "corpus inventory (recorded)\ncomplete: 1 document(s), 1 distinct target(s), <elapsed>",
        "and what it reports is the recorded inventory"
    );
}

/// A changed source has a different analysis identity and therefore a
/// different canonical name, so it publishes beside the old artifact rather
/// than contesting it.
#[test]
fn corpus_build_without_output_keeps_the_old_artifact_when_the_source_changes() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();

    let first = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before = artifact_directories(directory.path());
    assert_eq!(before.len(), 1);
    let first_manifest = fs::read(before[0].join("manifest.json")).unwrap();

    fs::write(&source, "犬。\n").unwrap();
    let second = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "rebuild after a source change failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let after = artifact_directories(directory.path());
    assert_eq!(
        after.len(),
        2,
        "a changed source publishes a second canonical artifact"
    );
    assert!(after.contains(&before[0]));
    assert_eq!(
        fs::read(before[0].join("manifest.json")).unwrap(),
        first_manifest,
        "the pre-existing artifact must never be overwritten"
    );
}

/// A *readable* artifact at the canonical path whose recorded identity
/// disagrees is refused just as firmly. It is reachable in one run: the naming
/// pass and the build pass read the source separately, so a source edited
/// between them lands an artifact named from the older content while recording
/// the newer identity. Staged here deterministically instead, by moving one
/// build's artifact onto the name another build's content earns.
#[test]
fn corpus_build_without_output_refuses_a_canonical_artifact_of_another_identity() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");

    let build = || {
        let output = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        artifact_directories(directory.path()).remove(0)
    };

    fs::write(&source, "犬。\n").unwrap();
    let other = build();
    fs::rename(&other, directory.path().join("held.elsewhere")).unwrap();
    let held = directory.path().join("held.elsewhere");

    fs::write(&source, "猫。\n").unwrap();
    let canonical = build();
    // The name `猫。` earns, now holding the artifact `犬。` produced.
    fs::remove_dir_all(&canonical).unwrap();
    fs::rename(&held, &canonical).unwrap();
    let planted = directory_snapshot(&canonical);

    let refused = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "an artifact of another identity must not be reused or replaced"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();
    assert!(
        stderr.contains(&format!(
            "the artifact at {} records a different analysis identity",
            canonical.display()
        )),
        "the refusal must name the path and the identity that disagrees: {stderr}"
    );
    assert!(
        stderr.contains("remove it or pass an explicit --output"),
        "and must name both recoveries: {stderr}"
    );
    assert_eq!(
        directory_snapshot(&canonical),
        planted,
        "the artifact found there must be left exactly as found"
    );
    assert_eq!(
        artifact_directories(directory.path()),
        vec![canonical],
        "and no artifact may be published beside it"
    );
}

/// A directory at the canonical path that is not a readable artifact is a
/// hard, early error — never an overwrite and never a silent rebuild.
#[test]
fn corpus_build_without_output_refuses_a_corrupt_canonical_artifact() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();

    let first = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let artifact = artifact_directories(directory.path()).remove(0);

    fs::write(artifact.join("manifest.json"), b"not a manifest").unwrap();
    let tampered = directory_snapshot(&artifact);

    let second = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "a corrupt canonical artifact must not be silently rebuilt or replaced"
    );
    assert_eq!(
        directory_snapshot(&artifact),
        tampered,
        "the tampered directory must be left exactly as found"
    );
    assert_eq!(
        artifact_directories(directory.path()),
        vec![artifact.clone()],
        "and no artifact may be published beside it"
    );
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(
        stderr.contains(&artifact.display().to_string()),
        "the error must name the canonical path: {stderr}"
    );
    // Separates the early diagnosis from the late commit-time refusal: reaching
    // `destination_exists` would mean the whole tokenizing build ran first and
    // the corruption was never diagnosed, only collided with.
    assert!(
        !stderr.contains("destination already exists"),
        "the corruption must be diagnosed before the build, not by the no-replace seam: {stderr}"
    );
}

/// Japanese corpora are routinely not in NFC — `か` followed by U+3099 is the
/// ordinary decomposed spelling of `が` — and `--normalization nfc` is the
/// default precisely so that such a corpus analyzes as the composed text.
///
/// The provenance writer's equality check compares the analyzer's text with
/// the admitted text after normalization; comparing a pre-NFC string with a
/// post-NFC one would abort with "analyzed text differs from admitted sentence
/// bytes".
///
/// The contract this pins is the one `--normalization`'s own help states:
/// normalization happens once, before analysis, so a decomposed source and its
/// composed twin analyze to the same tokens. It compares *decoded* sentences
/// rather than stored bytes, because an artifact's token header carries the
/// analysis identity, which binds the source digest and so differs for two
/// files that are not byte-identical.
#[test]
fn corpus_build_admits_text_that_normalization_recomposes() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();

    // Identically named inside distinct directories, so a decoded sentence's
    // recorded source name is the same for both and only the bytes differ.
    let write = |name: &str, sentence: &str| {
        let root = directory.path().join(name);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("corpus.jsonl");
        fs::write(&source, format!("{{\"sentences\":[\"{sentence}\"]}}\n")).unwrap();
        source
    };
    let decomposed_source = write("decomposed", "か\u{3099}まん。");
    let composed_source = write("composed", "がまん。");

    let build = |source: &Path, artifact: &Path, normalization: &str| {
        lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--normalization", normalization])
            .arg("--output")
            .arg(artifact)
            .arg(source)
            .output()
            .unwrap()
    };
    let sentences = |artifact: &Path| {
        let output = lingrep()
            .args(["tokenize", "--json", "--corpus"])
            .arg(artifact)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        parse_jsonl(&output.stdout)
            .into_iter()
            .filter(|record| record["type"] == "sentence")
            .map(|record| record["data"].clone())
            .collect::<Vec<_>>()
    };

    let normalized_decomposed = directory.path().join("nfc-decomposed.lingcorpus");
    let normalized_composed = directory.path().join("nfc-composed.lingcorpus");
    let output = build(&decomposed_source, &normalized_decomposed, "nfc");
    assert_eq!(
        output.status.code(),
        Some(0),
        "a decomposed source must prepare under the default policy: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        build(&composed_source, &normalized_composed, "nfc")
            .status
            .code(),
        Some(0)
    );
    let recomposed = sentences(&normalized_decomposed);
    assert!(!recomposed.is_empty(), "the artifact records a sentence");
    assert_eq!(
        recomposed,
        sentences(&normalized_composed),
        "under nfc the two spellings analyze to one corpus"
    );

    // And the policy is still a policy: `none` analyzes what was written.
    let raw_decomposed = directory.path().join("none-decomposed.lingcorpus");
    let raw_composed = directory.path().join("none-composed.lingcorpus");
    assert_eq!(
        build(&decomposed_source, &raw_decomposed, "none")
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        build(&composed_source, &raw_composed, "none").status.code(),
        Some(0)
    );
    assert_ne!(
        sentences(&raw_decomposed),
        sentences(&raw_composed),
        "under none the two spellings stay two corpora"
    );
}

/// A prepared BCCWJ artifact is reusable, and its canonical path is a claim
/// about content:
/// the DOC context tables and the zip bytes are identity inputs, and the
/// observation order is byte-sorted rather than readdir order.
///
/// The two-zip synthetic distribution (support::bccwj_distribution) carries
/// three sentences: two in the FIXED `one` entry (subcorpus 書籍, sampling
/// fixed) and one in the VARIABLE `solo` entry (subcorpus 新聞, sampling
/// variable).
#[test]
fn bccwj_build_reuses_the_canonical_artifact_and_renames_on_identity_change() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));

    // Build 1 publishes; build 2 is a cache hit (same canonical path, the
    // published bytes untouched, no duplicate sibling).
    let first = crate::support::bccwj_build_artifact(&root, &dictionary);
    let planted = directory_snapshot(&first);
    let second = crate::support::bccwj_build_artifact(&root, &dictionary);
    assert_eq!(
        first, second,
        "an unchanged distribution must resolve to the same canonical path"
    );
    assert_eq!(
        directory_snapshot(&first),
        planted,
        "a cache hit must leave the published bytes untouched"
    );
    assert_eq!(
        artifact_directories(directory.path()),
        vec![first.clone()],
        "no duplicate artifact may be published beside the canonical one"
    );

    // Changing a DOC context table moves the source identity and canonical
    // path. The renamed build publishes beside, never over, the first artifact.
    let copyright = root.join("DOC/CopyRight_Annotation.txt");
    fs::write(&copyright, "a\tb\tnote\tn\n").unwrap();
    crate::support::bccwj_build_artifact(&root, &dictionary);
    let both = artifact_directories(directory.path());
    assert_eq!(
        both.len(),
        2,
        "the renamed build publishes a second artifact"
    );
    let renamed = both
        .into_iter()
        .find(|path| path != &first)
        .expect("the renamed artifact is not the first");
    assert_ne!(
        first, renamed,
        "a changed context table must move the canonical path"
    );
}

/// A zip content change renames the artifact, while rewriting a context file
/// with identical content does not. Identity observes name and content in
/// byte-sorted order, not directory iteration order.
#[test]
fn bccwj_rename_tracks_zip_bytes_not_content_identity() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let first = crate::support::bccwj_build_artifact(&root, &dictionary);

    // Rewriting a context file with identical bytes does not rename.
    let copyright = root.join("DOC/CopyRight_Annotation.txt");
    let content = fs::read(&copyright).unwrap();
    fs::write(&copyright, &content).unwrap();
    let rewritten = crate::support::bccwj_build_artifact(&root, &dictionary);
    assert_eq!(
        first, rewritten,
        "rewriting a context file with identical content must not rename"
    );

    // A zip byte change renames: rewrite the FIXED entry with one extra
    // byte in its XML payload.
    let zip = root.join("C-XML/FIXED/LB/LB.zip");
    let entry = "LBa/one.xml";
    let original = {
        let mut archive = zip::ZipArchive::new(fs::File::open(&zip).unwrap()).unwrap();
        let mut file = archive.by_name(entry).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
        bytes
    };
    let mut changed = original;
    changed.extend(b"\n");
    {
        let file = fs::File::create(&zip).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(entry.to_owned(), zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write as _;
        writer.write_all(&changed).unwrap();
        writer.finish().unwrap();
    }
    crate::support::bccwj_build_artifact(&root, &dictionary);
    let both = artifact_directories(directory.path());
    assert_eq!(both.len(), 2, "the zip change publishes a second artifact");
    let renamed = both
        .into_iter()
        .find(|path| path != &first)
        .expect("the renamed artifact is not the first");
    assert_ne!(first, renamed, "a changed zip must move the canonical path");
}

/// A prepared artifact is independent of its source distribution. After the
/// build, the test renames the source root and runs prepared commands against
/// the artifact alone. This proves later runs
/// never re-parse the XML or rebuild the metadata table from DOC/; a
/// lazily-held zip handle or an on-demand metadata rebuild would fail here.
#[test]
fn bccwj_prepared_artifact_needs_no_source_root() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let artifact = crate::support::bccwj_build_artifact(&root, &dictionary);
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern captured := (?<unit> [text=猫]).\n").unwrap();

    // Rename the source root away. The artifact must stand alone.
    let gone = directory.path().join("dist-renamed");
    fs::rename(&root, &gone).unwrap();
    assert!(!root.exists());

    let mut ran = 0usize;
    let checks: Vec<(&str, std::process::Output)> = vec![
        (
            "matches",
            lingrep()
                .arg("--json")
                .args(["matches", "--patterns"])
                .arg(&patterns)
                .args(["--context", "1", "--corpus"])
                .arg(&artifact)
                .output()
                .unwrap(),
        ),
        (
            "summarize",
            lingrep()
                .arg("--json")
                .args(["summarize", "--patterns"])
                .arg(&patterns)
                .args(["--corpus"])
                .arg(&artifact)
                .output()
                .unwrap(),
        ),
        (
            "sentence-final",
            lingrep()
                .args(["sentence-final", "--tsv", "--patterns"])
                .arg(&patterns)
                .args(["--corpus"])
                .arg(&artifact)
                .output()
                .unwrap(),
        ),
        (
            "doctor",
            lingrep()
                .args(["doctor", "--json", "--dict-path"])
                .arg(dictionary.path())
                .args(["--corpus"])
                .arg(&artifact)
                .output()
                .unwrap(),
        ),
    ];
    for (name, output) in &checks {
        assert!(
            output.status.success(),
            "{name} against the rootless artifact failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        ran += 1;
    }
    assert_eq!(ran, 4);

    // The scoped query selected exactly the FIXED tree's two sentences and
    // excluded the VARIABLE tree's one — `sampling` made the overlap
    // filterable through the prepared artifact, with no source root.
    let scoped = lingrep()
        .arg("--json")
        .args([
            "matches",
            "--pattern",
            "[].",
            "--where",
            "@document.sampling = fixed",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        scoped.status.success(),
        "{}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let scope = parse_jsonl(&scoped.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]["scope"]
        .clone();
    assert_eq!(
        (
            scope["sentences"]["admitted"].as_u64(),
            scope["sentences"]["excluded_by_predicate"].as_u64()
        ),
        (Some(2), Some(1)),
        "sampling = fixed admits the FIXED entry's two sentences and excludes the VARIABLE one"
    );
    assert_eq!(
        (
            scope["documents"]["admitted"].as_u64(),
            scope["documents"]["excluded_by_predicate"].as_u64()
        ),
        (Some(1), Some(1)),
    );
}

/// `matches` over the live distribution and prepared artifact publish
/// identical records — same spans,
/// captures, ordinals, surfaces, and context — and the same completion
/// counters and identities. The prepared run reads only the artifact; the
/// live run re-parses the XML. Their agreement is the correctness proof that
/// the artifact carries everything a later run needs.
#[test]
fn bccwj_prepared_matches_agree_with_the_live_run() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let artifact = crate::support::bccwj_build_artifact(&root, &dictionary);
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern captured := (?<unit> [text=猫]).\n").unwrap();

    let live = lingrep()
        .arg("--json")
        .args(["matches", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--input-format", "bccwj", "--context", "1"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        live.status.success(),
        "{}",
        String::from_utf8_lossy(&live.stderr)
    );
    let prepared = lingrep()
        .arg("--json")
        .args(["matches", "--patterns"])
        .arg(&patterns)
        .args(["--context", "1", "--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );

    let live_matches: Vec<Value> = parse_jsonl(&live.stdout)
        .into_iter()
        .filter(|record| record["type"] == "match")
        .collect();
    let prepared_matches: Vec<Value> = parse_jsonl(&prepared.stdout)
        .into_iter()
        .filter(|record| record["type"] == "match")
        .collect();
    assert_eq!(
        live_matches, prepared_matches,
        "the match records must agree"
    );
    assert_eq!(live_matches.len(), 2, "two 猫 sentences, one per entry");

    let live_complete = parse_jsonl(&live.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a live completion");
    let prepared_complete = parse_jsonl(&prepared.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a prepared completion");
    for field in ["documents_admitted", "sentences_admitted", "matches_found"] {
        assert_eq!(
            live_complete["data"]["counters"][field], prepared_complete["data"]["counters"][field],
            "the {field} counter must agree"
        );
    }
    assert_eq!(
        live_complete["data"]["source_identity"], prepared_complete["data"]["source_identity"],
        "the source identity must agree"
    );
    assert_eq!(
        live_complete["data"]["analysis_identity"], prepared_complete["data"]["analysis_identity"],
        "the analysis identity must agree"
    );

    // summarize agrees too: the per-definition counts are read from the same
    // tokenized stream in both channels, so they must be identical.
    let summarize = |input: &Path, prepared: bool| {
        let mut cmd = lingrep();
        cmd.arg("--json")
            .args(["summarize", "--patterns"])
            .arg(&patterns);
        if prepared {
            cmd.args(["--corpus"]).arg(input);
        } else {
            cmd.args(["--dict-path"])
                .arg(dictionary.path())
                .args(["--input-format", "bccwj"]);
            cmd.arg(input);
        }
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)
            .into_iter()
            .filter(|record| record["type"] == "summary")
            .collect::<Vec<_>>()
    };
    let live_summary = summarize(&root, false);
    let prepared_summary = summarize(&artifact, true);
    assert_eq!(
        live_summary, prepared_summary,
        "the summarize records must agree"
    );
}

/// A multi-zip distribution has no aliases: its zips are distinct files
/// carrying distinct content, so neither the live run nor the prepared one
/// warns about aliased source documents, and both report one target per zip.
///
/// Filing every zip in target group 0 made `alias_documents` equal
/// `zips - 1`, and `summarize` prints a warning whenever that is non-zero —
/// so every real BCCWJ run claimed 40 of its 41 zips were duplicates of one
/// file.
#[test]
fn bccwj_zips_are_distinct_targets_and_draw_no_alias_warning() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let artifact = crate::support::bccwj_build_artifact(&root, &dictionary);

    for prepared in [false, true] {
        let mut command = lingrep();
        command.args(["--json", "summarize", "--pattern", "[]."]);
        if prepared {
            command.arg("--corpus").arg(&artifact);
        } else {
            command
                .arg("--dict-path")
                .arg(dictionary.path())
                .args(["--input-format", "bccwj"])
                .arg(&root);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("aliased_source_documents"),
            "prepared={prepared}: {stderr}"
        );

        let complete = parse_jsonl(&output.stdout)
            .into_iter()
            .find(|record| record["type"] == "complete")
            .expect("a completion record");
        let inventory = &complete["data"]["inventory"];
        // Two zips in the fixture, one entry each.
        assert_eq!(inventory["documents"], 2, "prepared={prepared}");
        assert_eq!(inventory["distinct_targets"], 2, "prepared={prepared}");
        assert_eq!(inventory["alias_documents"], 0, "prepared={prepared}");
        assert_eq!(
            inventory["max_target_multiplicity"], 1,
            "prepared={prepared}"
        );
    }
}
