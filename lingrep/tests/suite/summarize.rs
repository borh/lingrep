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

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use lingrep::direct_source::matched_surface;
use lingrep::protocol::validate_query_jsonl;
use lingrep::summary::{AggregationGroup, AggregationInput, SummaryAggregator, SummaryRow};
use lingrep_core::analysis::{InputUnitId, NormalizationPolicy, TextUnit};
use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::{
    CancellationFlag, ExecutionBatch, ExecutionError, ExecutionLimits, ExecutionSentence,
    MatchSink, RunOutcome, SentenceAdmission, SentenceBatchSource, SentenceLocation, SinkControl,
    TokenizedBatchObservation, execute,
};
use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, Match, PatternSource, ProjectedKeyField, compile_catalog,
};
use lingrep_core::testing::MiniDictionary;
use lingrep_core::token_match::{TokenInfo, TokenSequence};
use lingrep_corpus::CorpusArtifact;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{
    assert_success, build_single_sentence_artifact, lingrep, lingrep_std,
    lingrep_without_dictionary, missing_attribute_location, parse_jsonl,
};

fn row(pattern_ordinal: u64, rank: u64, surface: &'static str, count: u64) -> SummaryRow {
    SummaryRow {
        pattern_ordinal,
        rank,
        group: AggregationGroup::Surface(surface.to_owned()),
        count,
    }
}

fn summarize_fixture(
    input: &[(u64, &str)],
    resident_key_limit: usize,
    max_temp_bytes: u64,
    top: Option<u64>,
) -> std::io::Result<Vec<SummaryRow>> {
    let mut summary = SummaryAggregator::with_limits(resident_key_limit, 2, max_temp_bytes, 2)?;
    for &(pattern_ordinal, surface) in input {
        summary.add(
            pattern_ordinal,
            AggregationInput::Surface(surface.to_owned()),
        )?;
    }
    let mut finalized = summary.finalize()?;
    let mut rows = Vec::new();
    finalized.visit(top, |actual| {
        rows.push(actual.clone());
        Ok(())
    })?;
    Ok(rows)
}

/// Tests author the published shape; the aggregator takes the borrowed one.
fn aggregation_input(group: &AggregationGroup) -> AggregationInput<'_> {
    match group {
        AggregationGroup::Surface(value) => AggregationInput::Surface(value.clone()),
        AggregationGroup::Unit(value) => AggregationInput::Unit(value),
        AggregationGroup::Term(value) => AggregationInput::Term(value),
        AggregationGroup::Key(fields) => AggregationInput::Key(fields),
        AggregationGroup::LexicalKey(fields) => AggregationInput::LexicalKey(fields),
    }
}

fn finalize_groups(groups: &[AggregationGroup]) -> Vec<SummaryRow> {
    let mut summary = SummaryAggregator::with_limits(1, 2, u64::MAX, 2).unwrap();
    for group in groups {
        summary.add(0, aggregation_input(group)).unwrap();
    }
    let mut finalized = summary.finalize().unwrap();
    let mut rows = Vec::new();
    finalized
        .visit(None, |row| {
            rows.push(row.clone());
            Ok(())
        })
        .unwrap();
    rows
}

fn projected_group(names: &[&str]) -> AggregationGroup {
    AggregationGroup::Key(
        names
            .iter()
            .map(|name| ProjectedKeyField {
                name: (*name).into(),
                value: "same".into(),
            })
            .collect(),
    )
}

#[test]
fn spill_merge_matches_an_independent_oracle_and_orders_utf8_bytes() {
    let input = [
        (1, "猫"),
        (0, "犬"),
        (0, "猫"),
        (0, "犬"),
        (1, "α"),
        (1, "猫"),
        (1, "あ"),
    ];

    let actual = summarize_fixture(&input, 2, u64::MAX, None).unwrap();

    assert_eq!(
        actual,
        [
            row(0, 1, "犬", 2),
            row(0, 2, "猫", 1),
            row(1, 1, "猫", 2),
            row(1, 2, "α", 1),
            row(1, 3, "あ", 1),
        ]
    );
}

#[test]
fn top_is_applied_per_authored_pattern_after_exact_counting() {
    let input = [
        (1, "猫"),
        (0, "犬"),
        (0, "猫"),
        (0, "犬"),
        (1, "α"),
        (1, "猫"),
        (1, "あ"),
    ];

    let actual = summarize_fixture(&input, 1, u64::MAX, Some(1)).unwrap();

    assert_eq!(actual, [row(0, 1, "犬", 2), row(1, 1, "猫", 2)]);
}

#[test]
fn equal_count_key_schema_order_is_independent_of_encounter_order() {
    let authored = projected_group(&["pos1", "pos2"]);
    let reversed = projected_group(&["pos2", "pos1"]);
    let expected = vec![authored.clone(), reversed.clone()];

    for groups in [
        vec![authored.clone(), reversed.clone()],
        vec![reversed, authored],
    ] {
        let rows = finalize_groups(&groups);
        assert_eq!(
            rows.iter().map(|row| row.group.clone()).collect::<Vec<_>>(),
            expected
        );
        assert!(rows.iter().all(|row| row.count == 1));
    }
}

#[test]
fn equal_count_group_kinds_have_a_canonical_order() {
    let expected = vec![
        AggregationGroup::Surface("surface".into()),
        AggregationGroup::Unit("unit".into()),
        projected_group(&["pos1"]),
    ];

    for groups in [expected.clone(), expected.iter().cloned().rev().collect()] {
        let rows = finalize_groups(&groups);
        assert_eq!(
            rows.iter().map(|row| row.group.clone()).collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn shipped_pos_distributions_publish_exact_typed_groups_and_equal_isolation() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.txt");
    fs::write(&corpus, "猫。犬。猫。\n").unwrap();
    let catalog = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patterns/distributions.lx");

    let run = |patterns: Option<&std::path::Path>, inline: Option<&str>| {
        let mut command = lingrep();
        command.arg("--json").arg("summarize");
        if let Some(patterns) = patterns {
            command.arg("--patterns").arg(patterns);
        }
        if let Some(inline) = inline {
            command.arg("--pattern").arg(inline);
        }
        let output = command
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg(&corpus)
            .output()
            .unwrap();
        assert_success(&output);
        summary_events(&parse_jsonl(&output.stdout))
            .into_iter()
            .map(|event| event["data"].clone())
            .collect::<Vec<_>>()
    };

    let combined = run(Some(&catalog), None);
    assert_eq!(combined.len(), 4);
    let noun = combined
        .iter()
        .find(|row| {
            row["pattern"] == "unidic_pos_distribution"
                && row["group"]["fields"][0]["value"] == "名詞"
        })
        .unwrap();
    assert_eq!(noun["count"], 3);
    assert_eq!(noun["group"]["kind"], "key");
    assert_eq!(
        noun["group"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["value"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["名詞", "普通名詞", "一般", "*"]
    );

    let mut isolated = run(None, Some("key: [upos]\npattern pos_distribution := []."));
    isolated.extend(run(
        None,
        Some("key: [unidic.pos1, unidic.pos2, unidic.pos3, unidic.pos4]\npattern unidic_pos_distribution := []."),
    ));
    let canonical = |mut rows: Vec<Value>| {
        rows.sort_by_key(|row| serde_json::to_string(row).unwrap());
        rows
    };
    assert_eq!(canonical(combined), canonical(isolated));
}

#[test]
fn shipped_ngram_catalog_counts_overlapping_windows_without_crossing_sentences() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("ngrams.txt");
    fs::write(&corpus, "猫と犬と猫\n猫と犬と猫\n").unwrap();
    let catalog = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patterns/ngrams.lx");

    let output = lingrep()
        .args(["--json", "summarize", "--patterns"])
        .arg(catalog)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(corpus)
        .output()
        .unwrap();
    assert_success(&output);

    let events = parse_jsonl(&output.stdout);
    let mut actual = summary_events(&events)
        .into_iter()
        .map(|event| {
            let data = &event["data"];
            let values = data["group"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|field| field["value"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            (
                data["pattern"].as_str().unwrap().to_owned(),
                values,
                data["count"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    actual.sort();

    let keyed = |tokens: &[&str]| {
        tokens
            .iter()
            .flat_map(|token| {
                [
                    (*token).to_owned(),
                    if *token == "と" { "ADP" } else { "NOUN" }.to_owned(),
                ]
            })
            .collect::<Vec<_>>()
    };
    let mut expected = vec![
        ("unigram".to_owned(), keyed(&["猫"]), 4),
        ("unigram".to_owned(), keyed(&["と"]), 4),
        ("unigram".to_owned(), keyed(&["犬"]), 2),
        ("bigram".to_owned(), keyed(&["猫", "と"]), 2),
        ("bigram".to_owned(), keyed(&["と", "犬"]), 2),
        ("bigram".to_owned(), keyed(&["犬", "と"]), 2),
        ("bigram".to_owned(), keyed(&["と", "猫"]), 2),
        ("trigram".to_owned(), keyed(&["猫", "と", "犬"]), 2),
        ("trigram".to_owned(), keyed(&["と", "犬", "と"]), 2),
        ("trigram".to_owned(), keyed(&["犬", "と", "猫"]), 2),
        ("fourgram".to_owned(), keyed(&["猫", "と", "犬", "と"]), 2),
        ("fourgram".to_owned(), keyed(&["と", "犬", "と", "猫"]), 2),
        (
            "fivegram".to_owned(),
            keyed(&["猫", "と", "犬", "と", "猫"]),
            2,
        ),
    ];
    expected.sort();
    assert_eq!(actual, expected, "events={events:#?}");
}

#[test]
fn cross_variant_groups_merge_into_one_row_across_shards() {
    use lingrep_core::fsm::{LexicalProjectedKeyField, LexicalProjectedKeyValue};
    // An ordinary key and an all-scalar lexical key with the same field names
    // and values are one group. Equality, the routing hash, and shard-local
    // interning must all agree on that, or the count splits.
    let ordinary = AggregationGroup::Key(vec![ProjectedKeyField {
        name: "surface".into(),
        value: "犬".into(),
    }]);
    let lexical = AggregationGroup::LexicalKey(vec![LexicalProjectedKeyField {
        name: "surface".into(),
        value: LexicalProjectedKeyValue::Scalar("犬".into()),
    }]);
    for shards in [1, 2, 3] {
        // Engagement at one key forces the sharded path immediately.
        let mut summary = SummaryAggregator::with_limits(1, 2, u64::MAX, shards).unwrap();
        for group in [&ordinary, &lexical, &ordinary, &lexical, &lexical] {
            summary.add(7, aggregation_input(group)).unwrap();
        }
        let mut finalized = summary.finalize().unwrap();
        let mut rows = Vec::new();
        finalized
            .visit(None, |row| {
                rows.push((row.pattern_ordinal, row.count));
                Ok(())
            })
            .unwrap();
        assert_eq!(rows, [(7, 5)], "shards={shards}");
    }
}

#[test]
fn shard_count_does_not_change_published_rows() {
    // Shard-local interners assign schedule-dependent ids; the canonical
    // global ranks the snapshot handshake computes are what publication
    // orders by, so any shard count publishes identical rows.
    let groups = [
        AggregationGroup::Surface("ん".into()),
        AggregationGroup::Surface("あ".into()),
        AggregationGroup::Unit("あ ん".into()),
        AggregationGroup::Surface("あ".into()),
        projected_group(&["a", "b"]),
        projected_group(&["a"]),
        AggregationGroup::Surface("ん".into()),
        projected_group(&["a", "b"]),
    ];
    let rows_with = |shards: usize| {
        let mut summary = SummaryAggregator::with_limits(1, 2, u64::MAX, shards).unwrap();
        for (ordinal, group) in groups.iter().enumerate() {
            summary
                .add((ordinal % 3) as u64, aggregation_input(group))
                .unwrap();
        }
        let mut finalized = summary.finalize().unwrap();
        let mut rows = Vec::new();
        finalized
            .visit(None, |row| {
                rows.push(row.clone());
                Ok(())
            })
            .unwrap();
        rows
    };
    let single = rows_with(1);
    assert_eq!(single, rows_with(2));
    assert_eq!(single, rows_with(5));
}

#[test]
fn temp_exhaustion_is_exact_and_publishes_no_rows() {
    let mut summary = SummaryAggregator::with_limits(1, 2, 1, 2).unwrap();
    let mut published = Vec::<SummaryRow>::new();

    let result = (|| {
        for surface in ["一", "二", "三", "四"] {
            summary.add(0, AggregationInput::Surface(surface.to_owned()))?;
        }
        let mut finalized = summary.finalize()?;
        finalized.visit(None, |row| {
            published.push(row.clone());
            Ok(())
        })
    })();

    assert!(result.is_err());
    assert!(published.is_empty());
}

#[test]
fn direct_filesystem_and_prepared_summaries_have_identical_semantics_and_identity() {
    let fixture = QueryFixture::new();
    let direct = fixture.direct_jsonl();
    let prepared = fixture.prepared_jsonl();
    assert_success(&direct);
    assert_success(&prepared);
    validate_query_jsonl(&direct.stdout).unwrap();
    validate_query_jsonl(&prepared.stdout).unwrap();

    let direct_events = parse_jsonl(&direct.stdout);
    let prepared_events = parse_jsonl(&prepared.stdout);
    assert_eq!(
        semantic_summary_events(&direct_events),
        semantic_summary_events(&prepared_events)
    );

    let direct_run = event(&direct_events, "run");
    let prepared_run = event(&prepared_events, "run");
    let direct_complete = event(&direct_events, "complete");
    let prepared_complete = event(&prepared_events, "complete");
    assert_eq!(direct_run["data"]["mode"], "summarize");
    assert_eq!(prepared_run["data"]["mode"], "summarize");
    assert_eq!(direct_run["data"]["inventory"], Value::Null);
    assert_eq!(
        prepared_run["data"]["inventory"],
        prepared_complete["data"]["inventory"]
    );
    for field in [
        "catalog_fingerprint",
        "definition_count",
        "dictionary_identity",
        "reader_identity",
        "token_schema_identity",
        "ordering",
        "span_convention",
        "execution_limits",
        "temporary_storage_limits",
        "traversal_limits",
    ] {
        assert_eq!(
            direct_run["data"][field], prepared_run["data"][field],
            "{field}"
        );
    }
    for field in ["source_identity", "analysis_identity", "inventory"] {
        assert_eq!(
            direct_complete["data"][field], prepared_complete["data"][field],
            "{field}"
        );
    }
    assert_eq!(direct_complete["data"]["coverage"], "full");
    assert_eq!(prepared_complete["data"]["coverage"], "full");
    assert_eq!(direct_complete["data"]["termination"], "eof");
    assert_eq!(prepared_complete["data"]["termination"], "eof");
    assert_eq!(direct_complete["data"]["inventory"]["documents"], 4);
    assert_eq!(direct_complete["data"]["inventory"]["distinct_targets"], 3);
    assert_eq!(direct_complete["data"]["inventory"]["alias_documents"], 1);
    assert_eq!(direct_complete["data"]["inventory"]["external_targets"], 1);
}

/// A JSONL source file carries many documents, while discovery counts source
/// files. `summarize` compared the admitted-document count against the
/// discovered-file count and so rejected every JSONL input holding more than
/// one document — `matches` accepted the same input, because it compares
/// quantities in the same unit.
///
/// Every other fixture here writes one document per file, which makes the two
/// counts coincide and hides the confusion. This one does not.
#[test]
fn a_jsonl_file_holding_several_documents_is_summarized() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"猫。\"]}\n{\"sentences\":[\"犬。\"]}\n{\"sentences\":[\"猫。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern animal := [text in {猫, 犬}].\n").unwrap();

    let output = lingrep()
        .arg("--json")
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    let events = parse_jsonl(&output.stdout);
    let complete = event(&events, "complete");
    assert_eq!(complete["data"]["counters"]["documents_admitted"], 3);
    assert_eq!(complete["data"]["inventory"]["documents"], 1);
    assert_eq!(complete["data"]["coverage"], "full");

    let tsv = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();
    assert_success(&tsv);
    assert_eq!(
        String::from_utf8(tsv.stdout).unwrap(),
        "animal\t1\t猫\t2\nanimal\t2\t犬\t1\n"
    );
}

#[test]
fn top_one_publishes_one_row_for_each_pattern_and_keeps_exact_counts() {
    let dictionary = MiniDictionary::new();
    let fixture = QueryFixture::new();
    let output = lingrep()
        .arg("--json")
        .args(["summarize", "--patterns"])
        .arg(&fixture.patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--top", "1"])
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert_success(&output);
    validate_query_jsonl(&output.stdout).unwrap();

    let events = parse_jsonl(&output.stdout);
    let summaries = summary_events(&events);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0]["data"]["pattern"], "z_animal");
    assert_eq!(summaries[0]["data"]["rank"], 1);
    assert_eq!(summaries[0]["data"]["group"]["kind"], "surface");
    assert_eq!(summaries[0]["data"]["group"]["value"], "犬");
    assert_eq!(summaries[0]["data"]["count"], 4);
    assert_eq!(summaries[1]["data"]["pattern"], "a_punctuation");
    assert_eq!(summaries[1]["data"]["rank"], 1);
    assert_eq!(summaries[1]["data"]["group"]["kind"], "surface");
    assert_eq!(summaries[1]["data"]["group"]["value"], "。");
    assert_eq!(summaries[1]["data"]["count"], 7);
    assert_eq!(event(&events, "run")["data"]["requested_top"], 1);
    assert_eq!(
        event(&events, "complete")["data"]["counters"]["matches_found"],
        14
    );
    assert_eq!(
        event(&events, "complete")["data"]["counters"]["records_emitted"],
        2
    );
    let complete = event(&events, "complete");
    assert!(
        complete["data"]["resources"]["temporary_storage_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(
        complete["data"]["resources"]["temporary_storage_bytes"]
            .as_u64()
            .unwrap()
            <= complete["data"]["resources"]["temporary_storage_limits"]["max_temp_bytes"]
                .as_u64()
                .unwrap()
    );
}

#[test]
fn stdin_summary_uses_stream_identity_and_exact_counts() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();
    let output = lingrep()
        .arg("--json")
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg("-")
        .write_stdin("猫。\n犬。\n猫。\n")
        .output()
        .unwrap();
    assert_success(&output);

    let events = parse_jsonl(&output.stdout);
    assert_eq!(event(&events, "run")["data"]["corpus_kind"], "stdin");
    assert_eq!(event(&events, "complete")["data"]["corpus_kind"], "stdin");
    assert!(
        event(&events, "complete")["data"]["observed_content_digest"]
            .as_str()
            .is_some()
    );
    let actual = summary_events(&events)
        .into_iter()
        .map(|record| {
            (
                record["data"]["group"]["value"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                record["data"]["count"].as_u64().unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        actual,
        BTreeMap::from([
            ("。".to_owned(), 3),
            ("犬".to_owned(), 1),
            ("猫".to_owned(), 2)
        ])
    );
}

#[test]
fn temp_exhaustion_after_run_publishes_no_summary_or_complete() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();
    let output = lingrep()
        .args(["--json", "summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--max-temp-bytes", "1"])
        .arg("-")
        .write_stdin("猫。\n犬。\n")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let events = parse_jsonl(&output.stdout);
    assert_eq!(
        events
            .iter()
            .map(|record| record["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["run", "definition"]
    );
    let diagnostics = parse_jsonl(&output.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["data"]["stage"], "aggregation");
    assert_eq!(diagnostics[0]["data"]["code"], "temp_budget_exceeded");
}

#[test]
fn prepared_summary_never_substitutes_text_for_missing_orth() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("missing-orth.lingcorpus");
    let missing = TokenInfo::default().with_text("fallback-must-not-appear");
    runtime.block_on(build_single_sentence_artifact(
        &artifact,
        "surface",
        std::slice::from_ref(&missing),
    ));
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(&patterns)
        .args(["--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        parse_jsonl(&output.stdout)
            .iter()
            .map(|record| record["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
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
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fallback-must-not-appear"));

    let location = SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: 0,
        document_id: DocumentId::new(0),
        document_ordinal: 0,
        document_sentence_ordinal: 0,
        source_name: "a.md".into(),
    };
    let matcher = compile_matcher("pattern any := [].\n");
    let direct_error = execute_surface(
        OneBatchSource(Some(ExecutionBatch::Text(vec![ExecutionSentence {
            value: TextUnit::sentence(
                InputUnitId::new(location.corpus_sentence_ordinal),
                "raw",
                NormalizationPolicy::None,
            )
            .unwrap(),
            location,
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
    assert_eq!(
        diagnostic[0]["data"]["location"]["source_name"],
        missing_attribute_location(&direct_error).0
    );
    assert_eq!(
        diagnostic[0]["data"]["location"]["corpus_sentence_ordinal"],
        missing_attribute_location(&direct_error).1
    );
    assert_eq!(
        diagnostic[0]["data"]["location"]["token_index"],
        missing_attribute_location(&direct_error).2
    );
}

#[test]
fn alias_warning_precedes_tsv_rows_and_documents_multiplicity_semantics() {
    let dictionary = MiniDictionary::new();
    let fixture = QueryFixture::new();
    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&fixture.patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg("--tsv")
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert_success(&output);

    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "z_animal\t1\t犬\t4\nz_animal\t2\t猫\t3\na_punctuation\t1\t。\t7\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("warning[aliased_source_documents]"));
    assert!(stderr.contains("alias_documents=1"));
    assert!(stderr.contains("max_target_multiplicity=2"));

    let help = lingrep().args(["summarize", "--help"]).output().unwrap();
    assert_success(&help);
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("each alias contributes its own counts")
    );

    let json_warning = lingrep()
        .args(["--json", "summarize", "--patterns"])
        .arg(&fixture.patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(&fixture.root)
        .output()
        .unwrap();
    assert_success(&json_warning);
    let diagnostics = parse_jsonl(&json_warning.stderr);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["data"]["code"], "aliased_source_documents");
    assert_eq!(diagnostics[0]["data"]["severity"], "warning");
    assert_eq!(diagnostics[0]["data"]["stage"], "aggregation");
}

#[cfg(unix)]
#[test]
fn cancellation_before_finalize_publishes_no_summary_or_complete() {
    let dictionary = MiniDictionary::new();
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let directory = tempfile::tempdir().unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern any := [].\n").unwrap();
    let mut child = lingrep_std()
        .args(["--json", "summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut prefix = String::new();
    stdout.read_line(&mut prefix).unwrap();
    stdout.read_line(&mut prefix).unwrap();
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    stdout.read_to_string(&mut prefix).unwrap();

    assert_eq!(status.code(), Some(130));
    let events = parse_jsonl(prefix.as_bytes());
    assert_eq!(
        events
            .iter()
            .map(|record| record["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["run", "definition"]
    );
}

fn summary_events(events: &[Value]) -> Vec<&Value> {
    events
        .iter()
        .filter(|record| record["type"] == "summary")
        .collect()
}

fn semantic_summary_events(events: &[Value]) -> Vec<Value> {
    events
        .iter()
        .filter(|record| matches!(record["type"].as_str(), Some("pattern" | "summary")))
        .cloned()
        .collect()
}

fn event<'a>(events: &'a [Value], kind: &str) -> &'a Value {
    events.iter().find(|record| record["type"] == kind).unwrap()
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

struct RequiredSurfaceSink;

impl MatchSink for RequiredSurfaceSink {
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

fn compile_matcher(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("summary-differential.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
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
    let mut sink = RequiredSurfaceSink;
    let mut progress = |_, _: &RequiredSurfaceSink| {};
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

struct QueryFixture {
    _directory: TempDir,
    root: PathBuf,
    artifact: PathBuf,
    patterns: PathBuf,
    dictionary: MiniDictionary,
}

impl QueryFixture {
    fn new() -> Self {
        let dictionary = MiniDictionary::new();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("corpus");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("a.md"), "猫。\n犬。\n").unwrap();
        fs::hard_link(root.join("a.md"), root.join("alias.md")).unwrap();
        fs::write(root.join("nested/b.md"), "猫。\n犬。\n").unwrap();
        let external = directory.path().join("external.md");
        fs::write(&external, "犬。\n").unwrap();
        std::os::unix::fs::symlink(&external, root.join("nested/external.md")).unwrap();
        let patterns = directory.path().join("patterns.lx");
        fs::write(
            &patterns,
            "pattern z_animal := [text in {猫, 犬}].\n\
             pattern a_punctuation := [text=。].\n",
        )
        .unwrap();
        let artifact = directory.path().join("fixture.lingcorpus");
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
            patterns,
            dictionary,
        }
    }

    fn direct_jsonl(&self) -> std::process::Output {
        lingrep()
            .arg("--json")
            .arg("summarize")
            .arg("--patterns")
            .arg(&self.patterns)
            .args(["--dict-path"])
            .arg(self.dictionary.path())
            .arg(&self.root)
            .output()
            .unwrap()
    }

    fn prepared_jsonl(&self) -> std::process::Output {
        lingrep()
            .arg("--json")
            .args(["summarize", "--patterns"])
            .arg(&self.patterns)
            .args(["--corpus"])
            .arg(&self.artifact)
            .output()
            .unwrap()
    }
}

/// A catalog authored for one dictionary and run against another is reported,
/// and the run still succeeds.
///
/// The notice exists because this failure is otherwise invisible: measured in
/// `docs/results/2026-08-04-catalog-tagset-binding/`, one catalog over one set
/// of sentences reports the same match count under `unidic-cwj-202512` and
/// `unidic-kinsei-edo-202512` while 37 token analyses differ underneath. A
/// count that stays equal is indistinguishable from a correct one.
#[test]
fn a_catalog_bound_to_another_dictionary_warns_without_failing() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"猫。\"]}\n").unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "analyzer: { family: \"vibrato\", resource: \"unidic-kinsei-edo\" }\npattern animal := [text=猫].\n",
    )
    .unwrap();

    let output = lingrep()
        .arg("summarize")
        .arg("--patterns")
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "animal\t1\t猫\t1\n",
        "the notice must not disturb the result stream"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("warning[dictionary_binding_mismatch]"),
        "expected a binding notice; got {stderr}"
    );
    assert!(
        stderr.contains("resource `mini`") && stderr.contains("loaded as `mini`"),
        "the notice must name the loaded analyzer resource; got {stderr}"
    );
}

/// The common case must stay quiet, or the notice trains people to ignore it.
#[test]
fn a_catalog_bound_to_the_loaded_dictionary_is_silent() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"猫。\"]}\n").unwrap();
    let loaded = dictionary.path();
    let declared = lingrep::dictionary_binding::dictionary_name_from_path(
        loaded.to_str().expect("dictionary path is UTF-8"),
    )
    .expect("the test dictionary path names a dictionary");
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        format!(
            "analyzer: {{ family: \"vibrato\", resource: \"{}\" }}\npattern animal := [text=猫].\n",
            lingrep::dictionary_binding::strip_release_version(declared)
        ),
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(loaded)
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains("dictionary_binding_mismatch"),
        "a matching binding must be silent; got {stderr}"
    );
}

/// Two surface realizations of one collocation become one summary row.
///
/// Measured motivation in `docs/results/2026-08-03-sketch-grammar-catalog/`:
/// over 500 documents, surface keying yields 91,562 rows where capture keying
/// yields 71,360 pairs, and 実験を行う alone is published as seven rows ranked
/// 2 through 11,182.
#[test]
fn a_declared_key_groups_two_inflections_of_one_collocation_into_one_row() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"実験を行った。\",\"実験を行う。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "key: [$noun.lemma, $verb.lemma]\npattern wo := (?<noun> [unidic.pos1=名詞]) [text=を] (?<verb> [unidic.pos1=動詞]).\n",
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    // The compact JSON object contains quotes, so `write_tsv_field` applies
    // RFC-4180 quoting. That keeps the summary TSV at its fixed four columns;
    // the authored field names make the tuple self-describing without changing
    // the grouping this test protects.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "wo\t1\t\"{\"\"$noun.lemma\"\":\"\"実験\"\",\"\"$verb.lemma\"\":\"\"行う\"\"}\"\t2\n",
        "行っ and 行う are one (noun, verb) pair and must be one row with count 2"
    );
}

/// A prepared summary groups on `lemma`, which no predicate in the catalog
/// mentions.
///
/// A prepared run reconstructs only the token attributes it asked the artifact
/// for. Everything the *matcher* needs is derivable from the automaton, but a
/// `key:` is read by publication and is invisible to it — so a projection
/// narrowed to the automaton's requirements would leave `lemma` unset, and this
/// summary would group 行っ and 行う as two rows instead of one, silently and
/// only on the prepared path.
///
/// The direct path is the control: it always has every attribute, so the two
/// outputs being byte-identical is the property worth asserting.
#[test]
fn a_prepared_summary_keyed_on_a_non_matcher_attribute_matches_the_direct_one() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.md"), "実験を行った。\n実験を行う。\n").unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "key: [$noun.lemma, $verb.lemma]\npattern wo := (?<noun> [unidic.pos1=名詞]) [text=を] (?<verb> [unidic.pos1=動詞]).\n",
    )
    .unwrap();
    let artifact = directory.path().join("keyed.lingcorpus");
    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&build);

    let direct = lingrep()
        .arg("summarize")
        .arg("--patterns")
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg("--tsv")
        .arg(&root)
        .output()
        .unwrap();
    let prepared = lingrep_without_dictionary()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--tsv", "--corpus"])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&direct);
    assert_success(&prepared);

    assert_eq!(
        String::from_utf8(prepared.stdout).unwrap(),
        String::from_utf8(direct.stdout).unwrap(),
        "the prepared summary lost an attribute the key needs"
    );
}

/// A keyed row names its fields and preserves their authored order. That order
/// is part of the tuple's grouping identity rather than a load-bearing external
/// column convention.
#[test]
fn a_keyed_row_names_its_fields_and_preserves_authored_identity_order() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(&corpus, "{\"sentences\":[\"実験を行った。\"]}\n").unwrap();
    let patterns = directory.path().join("patterns.lx");
    // Authored verb-then-noun; publication must retain that identity order.
    fs::write(
        &patterns,
        "key: [$verb.lemma, $noun.lemma]\npattern wo := (?<noun> [unidic.pos1=名詞]) [text=を] (?<verb> [unidic.pos1=動詞]).\n",
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "wo\t1\t\"{\"\"$verb.lemma\"\":\"\"行う\"\",\"\"$noun.lemma\"\":\"\"実験\"\"}\"\t1\n",
        "authored projection order is the published tuple order"
    );
}

/// Without the key the same corpus produces two rows. This is the control: it
/// pins that the difference above comes from the declaration, not from the
/// fixture.
#[test]
fn without_a_key_the_same_corpus_produces_two_surface_rows() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"実験を行った。\",\"実験を行う。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "pattern wo := (?<noun> [unidic.pos1=名詞]) [text=を] (?<verb> [unidic.pos1=動詞]).\n",
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "wo\t1\t実験を行う\t1\nwo\t2\t実験を行っ\t1\n"
    );
}

/// A record whose key capture did not participate is dropped from keyed output
/// and counted, never folded in under a partial key.
#[test]
fn an_unkeyable_record_is_dropped_from_keyed_output_and_counted() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"実験を行った。\",\"調査を行った。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "key: [$noun.lemma]\npattern wo := ((?<noun> [text=実験]) | [text=調査]) [text=を] [unidic.pos1=動詞].\n",
    )
    .unwrap();

    let output = lingrep()
        .arg("--json")
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    let events = parse_jsonl(&output.stdout);
    let complete = event(&events, "complete");
    assert_eq!(complete["data"]["counters"]["matches_found"], 2);
    assert_eq!(
        complete["data"]["counters"]["records_unkeyed"], 1,
        "the 調査 branch carries no $noun and must be counted, not silently kept"
    );
    let rows: Vec<&Value> = events
        .iter()
        .filter(|record| record["type"] == "summary")
        .collect();
    assert_eq!(rows.len(), 1, "only the keyable record is summarized");
    assert_eq!(rows[0]["data"]["group"]["kind"], "key");
    assert_eq!(rows[0]["data"]["group"]["fields"][0]["name"], "$noun.lemma");
    assert_eq!(rows[0]["data"]["group"]["fields"][0]["value"], "実験");
}

/// A `join` transform makes the *unit* the grouping value, not the span it was
/// built from.
///
/// 行った and 行う are one construction because the transform says so. Grouping
/// on the span would count the two
/// realizations separately even though the catalog declares them equal, which
/// is the same defect capture keying fixed for collocations.
#[test]
fn a_join_transform_groups_by_the_rendered_unit_rather_than_the_span() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"実験を行った。\",\"実験を行う。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "transform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[unidic.pos1=助動詞]\"}, {op: \"substitute\", component: \"last\", attr: \"lemma\"}, {op: \"join\"}]\npattern wo := [unidic.pos1=名詞] [text=を] [unidic.pos1=動詞] [unidic.pos1=助動詞]*.\n",
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "wo\t1\t実験を行う\t2\n",
        "the unit 実験を行う is one row; the spans 実験を行っ and 実験を行う are not"
    );
}

/// A pattern that renames itself still finds its own policy.
///
/// `output_name` runs last, so a published record carries a name the
/// policy map does not contain. Looking the key up by the published name finds
/// nothing and unkeys every record of exactly the definitions that renamed
/// themselves — a failure that is invisible unless both keys are declared
/// together.
#[test]
fn a_key_is_found_on_a_pattern_that_also_declares_an_output_name() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    fs::write(
        &corpus,
        "{\"sentences\":[\"実験を行った。\",\"実験を行う。\"]}\n",
    )
    .unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(
        &patterns,
        "key: [$noun.lemma, $verb.lemma]\noutput_name: \"collocation\"\npattern wo := (?<noun> [unidic.pos1=名詞]) [text=を] (?<verb> [unidic.pos1=動詞]).\n",
    )
    .unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--tsv"])
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "collocation\t1\t\"{\"\"$noun.lemma\"\":\"\"実験\"\",\"\"$verb.lemma\"\":\"\"行う\"\"}\"\t2\n",
        "the renamed pattern must still be keyed by its own declaration"
    );
}

fn summarize_shared_output_name(pattern_source: &str, sentences: &[&str]) -> String {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus.jsonl");
    let document = serde_json::json!({ "sentences": sentences });
    fs::write(&corpus, format!("{document}\n")).unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, pattern_source).unwrap();

    let output = lingrep()
        .args(["summarize", "--patterns"])
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg("--tsv")
        .arg(&corpus)
        .output()
        .unwrap();

    assert_success(&output);
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn shared_output_name_key_schema_order_is_independent_of_corpus_order() {
    let patterns = r#"key: [text]
output_name: "shared"
pattern cat := [text=猫].

key: [norm]
output_name: "shared"
pattern dog := [text=犬].
"#;
    // Ranked by key schema, and `norm` precedes `text` where it followed
    // `form`. The claim under test is that the order does not depend on
    // corpus order, which is unchanged.
    let expected = concat!(
        "shared\t1\t\"{\"\"norm\"\":\"\"犬\"\"}\"\t1\n",
        "shared\t2\t\"{\"\"text\"\":\"\"猫\"\"}\"\t1\n",
    );

    assert_eq!(
        summarize_shared_output_name(patterns, &["猫。", "犬。"]),
        expected
    );
    assert_eq!(
        summarize_shared_output_name(patterns, &["犬。", "猫。"]),
        expected
    );
}

#[test]
fn shared_output_name_aggregates_symmetric_definitions_without_downstream_summing() {
    let patterns = r#"key: [text]
output_name: "shared"
pattern left := [text=猫].

key: [text]
output_name: "shared"
pattern right := [text=猫].
"#;

    assert_eq!(
        summarize_shared_output_name(patterns, &["猫。"]),
        "shared\t1\t\"{\"\"text\"\":\"\"猫\"\"}\"\t2\n"
    );
}

#[test]
fn shared_output_name_mixed_group_kind_order_is_independent_of_corpus_order() {
    let patterns = r#"key: [text]
output_name: "shared"
pattern cat := [text=猫].

output_name: "shared"
pattern dog := [text=犬].
"#;
    let expected = concat!(
        "shared\t1\t犬\t1\n",
        "shared\t2\t\"{\"\"text\"\":\"\"猫\"\"}\"\t1\n",
    );

    assert_eq!(
        summarize_shared_output_name(patterns, &["猫。", "犬。"]),
        expected
    );
    assert_eq!(
        summarize_shared_output_name(patterns, &["犬。", "猫。"]),
        expected
    );
}

#[test]
fn published_order_follows_value_text_rather_than_first_encounter() {
    // Values are interned in first-encounter order, so the ids these five
    // surfaces carry through the scan run backwards against their text. What
    // is published must still be the order of the text, which is what the
    // lexicographic rank rewrite between the two phases exists to guarantee.
    // A resident limit of one sends every key through the run codec and the
    // merge, so the assertion covers the spilled representation and not just
    // an in-memory sort.
    let input = [(0, "な"), (0, "た"), (0, "さ"), (0, "か"), (0, "あ")];
    let rows = summarize_fixture(&input, 1, u64::MAX, None).unwrap();

    let surfaces = rows.iter().map(|row| row.group.clone()).collect::<Vec<_>>();
    assert_eq!(
        surfaces,
        ["あ", "か", "さ", "た", "な"].map(|value| AggregationGroup::Surface(value.into()))
    );
    assert!(rows.iter().all(|row| row.count == 1));
}

#[test]
fn a_projected_key_orders_element_wise_on_value_text() {
    // Every element of a key carries its own value identity, so a rank rewrite
    // that reached only the first one would order these three by their second
    // field's first-encounter id instead of its text.
    let group = |first: &str, second: &str| {
        AggregationGroup::Key(vec![
            ProjectedKeyField {
                name: "first".into(),
                value: first.to_owned(),
            },
            ProjectedKeyField {
                name: "second".into(),
                value: second.to_owned(),
            },
        ])
    };
    let rows = finalize_groups(&[group("む", "あ"), group("あ", "む"), group("あ", "あ")]);

    let published = rows.iter().map(|row| row.group.clone()).collect::<Vec<_>>();
    assert_eq!(
        published,
        [group("あ", "あ"), group("あ", "む"), group("む", "あ")]
    );
}
