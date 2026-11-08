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
use std::sync::Arc;

use lingrep::direct_source::matched_surface;
use lingrep::protocol::query::{MetadataValueData, PatternCount, SectionData};
use lingrep::protocol::validate_query_jsonl;
use lingrep::summary::{
    AggregationGroup, AggregationInput, MetadataValueTally, SectionTally, SummaryAggregator,
    SummaryPartition, SummaryRow,
};
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
use lingrep_core::spill::TempBudget;
use lingrep_core::testing::MiniDictionary;
use lingrep_core::token_match::{TokenInfo, TokenSequence};
use lingrep_corpus::CorpusArtifact;
use roaring::RoaringTreemap;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{
    assert_success, build_single_sentence_artifact, lingrep, lingrep_std,
    lingrep_without_dictionary, missing_attribute_location, parse_jsonl,
};

fn row(pattern_ordinal: u64, rank: u64, surface: &'static str, count: u64) -> SummaryRow {
    SummaryRow {
        partition: SummaryPartition::pattern(pattern_ordinal),
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
            SummaryPartition::pattern(pattern_ordinal),
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
        summary
            .add(SummaryPartition::pattern(0), aggregation_input(group))
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
            summary
                .add(SummaryPartition::pattern(7), aggregation_input(group))
                .unwrap();
        }
        let mut finalized = summary.finalize().unwrap();
        let mut rows = Vec::new();
        finalized
            .visit(None, |row| {
                rows.push((row.partition.pattern_ordinal, row.count));
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
                .add(
                    SummaryPartition::pattern((ordinal % 3) as u64),
                    aggregation_input(group),
                )
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
            summary.add(
                SummaryPartition::pattern(0),
                AggregationInput::Surface(surface.to_owned()),
            )?;
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
        lookaround_divisions: None,
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

    let help = lingrep()
        .args(["summarize", "--help-all"])
        .output()
        .unwrap();
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

#[test]
fn by_document_publishes_every_document_with_an_admitted_sentence() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("documents.jsonl");
    fs::write(
        &input,
        "{\"text\":\"猫。猫。\",\"metadata\":{\"id\":\"first\"}}\n\
         {\"text\":\"犬。\",\"metadata\":{\"id\":\"second\"}}\n\
         {\"text\":\"\",\"metadata\":{\"id\":\"empty\"}}\n\
         {\"text\":\"猫。犬。\",\"metadata\":{\"id\":\"third\"}}\n",
    )
    .unwrap();
    let artifact = directory.path().join("documents.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&built);

    let mut published = Vec::new();
    for prepared in [false, true] {
        let mut command = lingrep();
        command.args([
            "--json",
            "summarize",
            "--pattern",
            "cat := [text=猫].",
            "--by-document",
            "--source-metadata",
            "@document.id",
        ]);
        if prepared {
            command.arg("--corpus").arg(&artifact);
        } else {
            command
                .arg("--dict-path")
                .arg(dictionary.path())
                .arg(&input);
        }
        let output = command.output().unwrap();
        assert_success(&output);
        validate_query_jsonl(&output.stdout).unwrap();
        let events = parse_jsonl(&output.stdout);
        let documents = events
            .iter()
            .filter(|record| record["type"] == "document")
            .map(|record| {
                let data = &record["data"];
                (
                    data["document"].as_u64().unwrap(),
                    data["sentences_admitted"].as_u64().unwrap(),
                    data["tokens_admitted"].as_u64().unwrap(),
                    data["source_metadata"]["document"]["id"].clone(),
                )
            })
            .collect::<Vec<_>>();
        let ids = documents
            .iter()
            .map(|document| document.3.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["first", "second", "third"], "prepared={prepared}");
        assert_eq!(
            documents.iter().map(|document| document.1).sum::<u64>(),
            5,
            "prepared={prepared}"
        );
        let rows = summary_events(&events)
            .into_iter()
            .map(|record| {
                (
                    record["data"]["document"].as_u64().unwrap(),
                    record["data"]["count"].as_u64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [(documents[0].0, 2), (documents[2].0, 1)],
            "prepared={prepared}"
        );
        assert_eq!(
            event(&events, "complete")["data"]["counters"]["documents_admitted"],
            3,
            "prepared={prepared}"
        );
        published.push((documents, rows));
    }
    assert_eq!(published[0], published[1]);
}

/// `counters.documents_admitted` counts the documents with at least one
/// admitted sentence, which are the documents `--by-document` publishes.
///
/// `empty.md` has no sentence and `discussion.md` has none in `methods`, so a
/// count of documents that pass the metadata predicate differs from it.
/// `old.md` and the `zoo` documents contain no 猫, so the unscoped `summarize`
/// without `--by-document` and `matches` skip all their sentences as proven
/// absent. The `zoo` documents are long enough for the posting index to be
/// used, one run of skipped sentences spans them and the empty `zoo2.md`, and
/// `zoo5.md` starts after the last sentence the source returns.
#[test]
fn documents_admitted_counts_the_documents_by_document_publishes() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir_all(&root).unwrap();
    for (name, text) in [
        (
            "recent.md",
            "---\nyear: 2020\n---\n\n# Methods {.methods}\n\n猫が鳥を見る。\n猫も犬を見る。\n\n\
             # Results {.results}\n\n猫は犬である。\n",
        ),
        (
            "old.md",
            "---\nyear: 2005\n---\n\n# Methods {.methods}\n\n犬が鳥を見る。\n",
        ),
        (
            "discussion.md",
            "---\nyear: 2020\n---\n\n# Discussion {.discussion}\n\n猫が鳥を見る。\n",
        ),
        ("empty.md", "---\nyear: 2020\n---\n"),
    ] {
        fs::write(root.join(name), text).unwrap();
    }
    for name in ["zoo1.md", "zoo3.md", "zoo4.md", "zoo5.md"] {
        fs::write(
            root.join(name),
            format!(
                "---\nyear: 2005\n---\n\n{}",
                "犬が鳥を見る。\n".repeat(1000)
            ),
        )
        .unwrap();
    }
    fs::write(root.join("zoo2.md"), "").unwrap();
    let artifact = directory.path().join("documents.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&built);

    let run = |prepared: bool, arguments: &[&str], clause: Option<&str>| {
        let mut command = lingrep();
        command
            .arg("--json")
            .args(arguments)
            .args(["--pattern", "cat := [lemma=猫]."]);
        if let Some(clause) = clause {
            command.args(["--where", clause]);
        }
        if prepared {
            command.arg("--corpus").arg(&artifact);
        } else {
            command.arg("--dict-path").arg(dictionary.path()).arg(&root);
        }
        let output = command.output().unwrap();
        assert_success(&output);
        validate_query_jsonl(&output.stdout).unwrap();
        parse_jsonl(&output.stdout)
    };
    let counters = |events: &[Value]| event(events, "complete")["data"]["counters"].clone();

    for (clause, published) in [
        (
            None,
            &[
                "discussion.md",
                "old.md",
                "recent.md",
                "zoo1.md",
                "zoo3.md",
                "zoo4.md",
                "zoo5.md",
            ][..],
        ),
        (
            Some("@document.year >= 2010"),
            &["discussion.md", "recent.md"],
        ),
        (Some("@section = methods"), &["old.md", "recent.md"]),
        (
            Some("@section = methods & @document.year >= 2010"),
            &["recent.md"],
        ),
    ] {
        let mut by_document_counters = Vec::new();
        for prepared in [false, true] {
            let case = format!("clause={clause:?} prepared={prepared}");
            let events = run(prepared, &["summarize", "--by-document"], clause);
            let mut names = events
                .iter()
                .filter(|record| record["type"] == "document")
                .map(|record| {
                    let name = record["data"]["source_name"].as_str().unwrap();
                    PathBuf::from(name)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, published, "{case}");
            let by_document = counters(&events);
            assert_eq!(by_document["documents_admitted"], published.len(), "{case}");
            for arguments in [&["summarize"][..], &["matches", "--limit", "all"]] {
                let other = counters(&run(prepared, arguments, clause));
                for field in ["documents_admitted", "sentences_admitted"] {
                    assert_eq!(
                        other[field], by_document[field],
                        "{case} {arguments:?} {field}"
                    );
                }
            }
            by_document_counters.push(by_document);
        }
        assert_eq!(
            by_document_counters[0], by_document_counters[1],
            "clause={clause:?}: direct and prepared counters differ"
        );
    }
}

#[test]
fn by_document_rejects_output_and_metadata_it_cannot_publish() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("documents.jsonl");
    fs::write(&input, "{\"text\":\"猫。\"}\n").unwrap();
    for (case, json, extra) in [
        ("tsv output", false, &["--by-document"][..]),
        (
            "sentence metadata",
            true,
            &["--by-document", "--source-metadata", "@sentence.id"][..],
        ),
        (
            "metadata without --by-document",
            true,
            &["--source-metadata", "@document.id"][..],
        ),
    ] {
        let mut command = lingrep();
        if json {
            command.arg("--json");
        }
        let output = command
            .args(["summarize", "--pattern", "cat := [text=猫]."])
            .args(extra)
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg(&input)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{case}");
        assert_eq!(output.status.code(), Some(2), "{case}");
    }
}

fn sectioned_artifact(directory: &TempDir, dictionary: &MiniDictionary) -> PathBuf {
    let root = directory.path().join("corpus");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("paper.md"),
        "猫と犬。\n\n\
         # Methods {.methods}\n\n猫が鳥を見る。\n犬も鳥を見る。\n\n\
         # Results {.results}\n\n猫は犬である。\n\n\
         ::: {.discussion}\n猫が猫を見る。\n:::\n",
    )
    .unwrap();
    let artifact = directory.path().join("sections.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&built);
    artifact
}

#[test]
fn by_section_counts_matches_and_matched_sentences_under_every_active_label() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = sectioned_artifact(&directory, &dictionary);
    let catalog = directory.path().join("catalog.lx");
    fs::write(
        &catalog,
        "pattern cat := [text=猫].\npattern bird := [text=鳥].\n",
    )
    .unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "summarize", "--by-section", "--corpus"])
        .arg(&artifact)
        .arg("--patterns")
        .arg(&catalog)
        .output()
        .unwrap();
    assert_success(&output);
    validate_query_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);
    let sections = events
        .iter()
        .filter(|record| record["type"] == "section")
        .map(|record| record["data"].clone())
        .collect::<Vec<_>>();
    let pattern = |name: &str, matches: u64, sentences: u64| serde_json::json!({"pattern": name, "matches": matches, "sentences_matched": sentences});
    let tokens = |label: Option<&str>| {
        sections
            .iter()
            .find(|section| section["label"].as_str() == label)
            .unwrap()["tokens_admitted"]
            .clone()
    };
    assert_eq!(
        sections,
        [
            serde_json::json!({
                "label": "methods", "sentences_admitted": 2, "tokens_admitted": tokens(Some("methods")),
                "patterns": [pattern("cat", 1, 1), pattern("bird", 2, 2)],
            }),
            serde_json::json!({
                "label": "results", "sentences_admitted": 2, "tokens_admitted": tokens(Some("results")),
                "patterns": [pattern("cat", 3, 2), pattern("bird", 0, 0)],
            }),
            serde_json::json!({
                "label": "discussion", "sentences_admitted": 1, "tokens_admitted": tokens(Some("discussion")),
                "patterns": [pattern("cat", 2, 1), pattern("bird", 0, 0)],
            }),
            serde_json::json!({
                "label": null, "sentences_admitted": 1, "tokens_admitted": tokens(None),
                "patterns": [pattern("cat", 1, 1), pattern("bird", 0, 0)],
            }),
        ]
    );
    for section in &sections {
        assert!(
            section["tokens_admitted"].as_u64().unwrap() > 0,
            "{section}"
        );
    }
    let complete = &event(&events, "complete")["data"]["counters"];
    assert_eq!(complete["sentences_admitted"], 5);
    assert_eq!(complete["matches_found"], 7);
}

/// Every sentence carries at most one section label: one unlabelled sentence,
/// then two under `methods` and two under `results`. `猫` occurs in three
/// sentences, `鳥` in two, and the last sentence has neither.
fn partitioned_artifact(directory: &TempDir, dictionary: &MiniDictionary) -> PathBuf {
    let root = directory.path().join("partitioned");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("paper.md"),
        "猫と犬。\n\n\
         # Methods {.methods}\n\n猫が鳥を見る。\n犬も鳥を見る。\n\n\
         # Results {.results}\n\n猫は犬である。\n犬。\n",
    )
    .unwrap();
    let artifact = directory.path().join("partitioned.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&built);
    artifact
}

/// The complete record's `sentences_matched` is the number of distinct
/// sentences with a match record: for several patterns the union of their
/// sentences, not the sum of each pattern's count.
#[test]
fn complete_sentences_matched_counts_the_sentences_with_a_match_record() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = partitioned_artifact(&directory, &dictionary);
    for (catalog, matches, sentences) in [
        ("pattern cat := [text=猫].\n", 3, 3),
        // `cat` matches three sentences and `bird` two; one sentence holds both.
        (
            "pattern cat := [text=猫].\npattern bird := [text=鳥].\n",
            5,
            4,
        ),
    ] {
        let path = directory.path().join("catalog.lx");
        fs::write(&path, catalog).unwrap();
        let output = lingrep_without_dictionary()
            .args(["--json", "matches", "--limit", "all", "--corpus"])
            .arg(&artifact)
            .arg("--patterns")
            .arg(&path)
            .output()
            .unwrap();
        assert_success(&output);
        validate_query_jsonl(&output.stdout).unwrap();
        let events = parse_jsonl(&output.stdout);
        let ordinals = events
            .iter()
            .filter(|record| record["type"] == "match")
            .map(|record| record["data"]["corpus_sentence_ordinal"].as_u64().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ordinals.len(), sentences, "{catalog}");
        let counters = &event(&events, "complete")["data"]["counters"];
        let mut expected = serde_json::json!({
            "documents_admitted": 1,
            "sentences_admitted": 5,
            "sentences_matched": sentences,
            "tokens_scanned": counters["tokens_scanned"],
            "matches_found": matches,
            "records_emitted": matches,
        });
        if let Some(absent) = counters.get("sentences_proven_absent") {
            expected["sentences_proven_absent"] = absent.clone();
        }
        assert_eq!(counters, &expected, "{catalog}");
    }
}

/// With sections that partition the corpus, the complete record's
/// `sentences_matched` for one pattern is the sum over the section records.
#[test]
fn complete_sentences_matched_is_the_sum_over_partitioning_section_records() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = partitioned_artifact(&directory, &dictionary);
    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "summarize",
            "--by-section",
            "--pattern",
            "cat := [text=猫].",
            "--corpus",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert_success(&output);
    validate_query_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);
    let sections = events
        .iter()
        .filter(|record| record["type"] == "section")
        .map(|record| serde_json::from_value::<SectionData>(record["data"].clone()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        sections
            .iter()
            .map(|section| (
                section.label.as_deref(),
                section.sentences_admitted,
                section.patterns[0].sentences_matched
            ))
            .collect::<Vec<_>>(),
        [
            (Some("methods"), 2, 1),
            (Some("results"), 2, 1),
            (None, 1, 1)
        ]
    );
    let counters = &event(&events, "complete")["data"]["counters"];
    assert_eq!(counters["sentences_admitted"], 5);
    assert_eq!(
        counters["sentences_matched"],
        sections
            .iter()
            .map(|section| section.patterns[0].sentences_matched)
            .sum::<u64>()
    );
}

#[test]
fn by_section_rejects_tsv_output_direct_input_and_by_document() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = sectioned_artifact(&directory, &dictionary);
    let direct = directory.path().join("corpus");
    for (case, json, prepared, extra) in [
        ("tsv output", false, true, &["--by-section"][..]),
        ("direct input", true, false, &["--by-section"][..]),
        (
            "with --by-document",
            true,
            true,
            &["--by-section", "--by-document"][..],
        ),
    ] {
        let mut command = lingrep();
        if json {
            command.arg("--json");
        }
        command
            .args(["summarize", "--pattern", "cat := [text=猫]."])
            .args(extra);
        if prepared {
            command.arg("--corpus").arg(&artifact);
        } else {
            command
                .arg("--dict-path")
                .arg(dictionary.path())
                .arg(&direct);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
    }
}

#[test]
fn spilled_section_counts_publish_the_same_records_as_resident_counts() {
    let labels = || {
        let mut methods = RoaringTreemap::new();
        methods.insert_range(1..3);
        let mut results = RoaringTreemap::new();
        results.insert_range(2..5);
        vec![
            ("methods".to_owned(), methods),
            ("results".to_owned(), results),
            ("empty".to_owned(), RoaringTreemap::new()),
        ]
    };
    // (sentence, tokens, pattern ordinal per match). Sentence 2 carries both
    // labels, sentences 0 and 5 carry none, and pattern 3 is not published.
    let sentences: [(u64, usize, &[u64]); 6] = [
        (0, 3, &[0]),
        (1, 4, &[1, 1, 2]),
        (2, 5, &[0, 2, 0]),
        (3, 2, &[]),
        (4, 6, &[3, 1]),
        (5, 1, &[2]),
    ];
    let names = ["cat", "bird", "dog", "hidden"];
    let run = |resident_key_limit: Option<usize>| {
        let budget = Arc::new(TempBudget::new(u64::MAX));
        let mut tally = match resident_key_limit {
            Some(limit) => SectionTally::with_limits(labels(), limit, 2, Arc::clone(&budget)),
            None => SectionTally::new(labels(), Arc::clone(&budget)),
        }
        .unwrap();
        for (sentence, tokens, patterns) in sentences {
            tally
                .observe(sentence, 0, tokens, patterns.iter().copied())
                .unwrap();
        }
        let mut records = tally.into_records(&[2, 0, 1], &names).unwrap();
        let mut published = Vec::new();
        while let Some(record) = records.next_record().unwrap() {
            published.push(record);
        }
        (published, budget.peak_used_bytes())
    };
    let (resident, resident_temp_bytes) = run(None);
    let (spilled, spilled_temp_bytes) = run(Some(1));
    assert_eq!(resident_temp_bytes, 0);
    assert!(spilled_temp_bytes > 0, "a one-key buffer writes runs");
    assert_eq!(spilled, resident);
    let section = |label: Option<&str>, sentences, tokens, counts: [(u64, u64); 3]| SectionData {
        label: label.map(str::to_owned),
        sentences_admitted: sentences,
        tokens_admitted: tokens,
        patterns: ["cat", "bird", "dog"]
            .into_iter()
            .zip(counts)
            .map(|(pattern, (matches, sentences_matched))| PatternCount {
                pattern: pattern.to_owned(),
                matches,
                sentences_matched,
            })
            .collect(),
    };
    assert_eq!(
        resident,
        [
            section(Some("methods"), 2, 9, [(2, 1), (2, 1), (2, 2)]),
            section(Some("results"), 3, 13, [(2, 1), (1, 1), (1, 1)]),
            section(None, 2, 4, [(1, 1), (0, 0), (1, 1)]),
        ]
    );
}

fn tagged_artifact(directory: &TempDir, dictionary: &MiniDictionary) -> PathBuf {
    let input = directory.path().join("tagged.jsonl");
    fs::write(
        &input,
        "{\"text\":\"猫が猫を見る。犬。\",\"metadata\":{\"tags\":[\"x\",\"y\",\"x\"],\"year\":\"2001\"}}\n\
         {\"text\":\"猫。\",\"metadata\":{\"tags\":\"x\",\"year\":\"2002\"}}\n\
         {\"text\":\"犬が鳥を見る。\",\"metadata\":{\"year\":\"2001\"}}\n\
         {\"text\":\"猫と犬。\",\"metadata\":{\"tags\":[],\"year\":\"2002\"}}\n\
         {\"text\":\"鳥。\",\"metadata\":{\"tags\":1,\"year\":\"2003\"}}\n\
         {\"text\":\"\",\"metadata\":{\"tags\":\"x\",\"year\":\"2003\"}}\n\
         {\"text\":\"猫。猫。\",\"metadata\":{\"tags\":\"1\",\"year\":\"2003\"}}\n",
    )
    .unwrap();
    let artifact = directory.path().join("tagged.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&built);
    artifact
}

fn summarize_events(artifact: &std::path::Path, extra: &[&str]) -> Vec<Value> {
    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "summarize",
            "--pattern",
            "cat := [text=猫].",
            "--corpus",
        ])
        .arg(artifact)
        .args(extra)
        .output()
        .unwrap();
    assert_success(&output);
    validate_query_jsonl(&output.stdout).unwrap();
    parse_jsonl(&output.stdout)
}

/// Each `metadata_value` record equals the counters of the run whose `--where`
/// clause adds that value, as `=` for a scalar and `contains` for a list.
#[test]
fn by_metadata_value_records_equal_the_where_run_for_each_value() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    for (scope, values) in [
        (None, &[Some("x"), Some("y"), Some("1"), None][..]),
        (
            Some("@document.year >= 2002"),
            &[Some("x"), Some("1"), None][..],
        ),
    ] {
        let mut extra = vec!["--by-metadata", "@document.tags"];
        if let Some(scope) = scope {
            extra.extend(["--where", scope]);
        }
        let events = summarize_events(&artifact, &extra);
        let records = events
            .iter()
            .filter(|record| record["type"] == "metadata_value")
            .map(|record| {
                serde_json::from_value::<MetadataValueData>(record["data"].clone()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            records
                .iter()
                .map(|record| record.value.as_deref())
                .collect::<Vec<_>>(),
            values,
            "{scope:?}"
        );
        for record in &records {
            assert_eq!(record.key, "@document.tags");
            let Some(value) = record.value.as_deref() else {
                // Documents 3 and 4: no key, and an empty list.
                let expected = if scope.is_some() {
                    (1, 1, 1, 1)
                } else {
                    (2, 2, 1, 1)
                };
                let cat = &record.patterns[0];
                assert_eq!(
                    (
                        record.documents_admitted,
                        record.sentences_admitted,
                        cat.matches,
                        cat.sentences_matched
                    ),
                    expected,
                    "{scope:?}"
                );
                continue;
            };
            let selected =
                format!("(@document.tags = \"{value}\" | @document.tags contains \"{value}\")");
            let clause =
                scope.map_or_else(|| selected.clone(), |scope| format!("{scope} & {selected}"));
            let oracle = summarize_events(&artifact, &["--where", &clause]);
            let counters = &event(&oracle, "complete")["data"]["counters"];
            let matched = lingrep_without_dictionary()
                .args([
                    "--json",
                    "matches",
                    "--pattern",
                    "cat := [text=猫].",
                    "--corpus",
                ])
                .arg(&artifact)
                .args(["--where", &clause])
                .output()
                .unwrap();
            assert_success(&matched);
            let matched = parse_jsonl(&matched.stdout)
                .into_iter()
                .filter(|record| record["type"] == "match")
                .map(|record| record["data"]["corpus_sentence_ordinal"].as_u64().unwrap())
                .collect::<Vec<_>>();
            let matched_sentences = matched
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            assert_eq!(counters["sentences_matched"], matched_sentences, "{clause}");
            assert_eq!(
                serde_json::to_value(record).unwrap(),
                serde_json::json!({
                    "key": "@document.tags",
                    "value": value,
                    "documents_admitted": counters["documents_admitted"],
                    "sentences_admitted": counters["sentences_admitted"],
                    "tokens_admitted": counters["tokens_scanned"],
                    "patterns": [{
                        "pattern": "cat",
                        "matches": matched.len(),
                        "sentences_matched": matched_sentences,
                    }],
                }),
                "{clause}"
            );
        }
        let complete = &event(&events, "complete")["data"]["counters"];
        let plain = summarize_events(&artifact, &extra[2..]);
        assert_eq!(complete, &event(&plain, "complete")["data"]["counters"]);
        assert_eq!(summary_events(&events), summary_events(&plain));
    }
}

#[test]
fn by_metadata_rejects_other_owners_tsv_output_direct_input_and_other_groupings() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let direct = directory.path().join("tagged.jsonl");
    for (case, json, prepared, extra) in [
        (
            "@sentence key",
            true,
            true,
            &["--by-metadata", "@sentence.tags"][..],
        ),
        ("bare key", true, true, &["--by-metadata", "tags"][..]),
        (
            "empty key",
            true,
            true,
            &["--by-metadata", "@document."][..],
        ),
        (
            "tsv output",
            false,
            true,
            &["--by-metadata", "@document.tags"][..],
        ),
        (
            "direct input",
            true,
            false,
            &["--by-metadata", "@document.tags"][..],
        ),
        (
            "with --by-document",
            true,
            true,
            &["--by-metadata", "@document.tags", "--by-document"][..],
        ),
        (
            "with --by-section",
            true,
            true,
            &["--by-metadata", "@document.tags", "--by-section"][..],
        ),
    ] {
        let mut command = lingrep();
        if json {
            command.arg("--json");
        }
        command
            .args(["summarize", "--pattern", "cat := [text=猫]."])
            .args(extra);
        if prepared {
            command.arg("--corpus").arg(&artifact);
        } else {
            command
                .arg("--dict-path")
                .arg(dictionary.path())
                .arg(&direct);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
    }
}

#[test]
fn spilled_metadata_value_counts_publish_the_same_records_as_resident_counts() {
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
    // (sentence range, values). The empty document lists a value that no
    // sentence can count, and "a" is listed twice by the first document.
    let documents = || {
        vec![
            (0..2, strings(&["a", "b", "a"])),
            (2..2, strings(&["unseen"])),
            (2..4, strings(&["b"])),
            (4..5, strings(&[])),
            (5..7, strings(&["a"])),
        ]
    };
    // (sentence, tokens, pattern ordinal per match); pattern 3 is not
    // published, and sentence 3 is not admitted.
    let sentences: [(u64, usize, &[u64]); 6] = [
        (0, 3, &[0]),
        (1, 4, &[1, 1, 2]),
        (2, 5, &[0, 2, 0]),
        (4, 6, &[3, 1]),
        (5, 1, &[2]),
        (6, 2, &[]),
    ];
    let names = ["cat", "bird", "dog", "hidden"];
    let run = |resident_key_limit: Option<usize>| {
        let budget = Arc::new(TempBudget::new(u64::MAX));
        let key = "@document.tags".to_owned();
        let mut tally = match resident_key_limit {
            Some(limit) => {
                MetadataValueTally::with_limits(key, documents(), limit, 2, Arc::clone(&budget))
            }
            None => MetadataValueTally::new(key, documents(), Arc::clone(&budget)),
        }
        .unwrap();
        for (sentence, tokens, patterns) in sentences {
            tally
                .observe(sentence, tokens, patterns.iter().copied())
                .unwrap();
        }
        let mut records = tally.into_records(&[2, 0, 1], &names).unwrap();
        let mut published = Vec::new();
        while let Some(record) = records.next_record().unwrap() {
            published.push(record);
        }
        (published, budget.peak_used_bytes())
    };
    let (resident, resident_temp_bytes) = run(None);
    let (spilled, spilled_temp_bytes) = run(Some(1));
    assert_eq!(resident_temp_bytes, 0);
    assert!(spilled_temp_bytes > 0, "a one-key buffer writes runs");
    assert_eq!(spilled, resident);
    let value = |value: Option<&str>, documents, sentences, tokens, counts: [(u64, u64); 3]| {
        MetadataValueData {
            key: "@document.tags".to_owned(),
            value: value.map(str::to_owned),
            documents_admitted: documents,
            sentences_admitted: sentences,
            tokens_admitted: tokens,
            patterns: ["cat", "bird", "dog"]
                .into_iter()
                .zip(counts)
                .map(|(pattern, (matches, sentences_matched))| PatternCount {
                    pattern: pattern.to_owned(),
                    matches,
                    sentences_matched,
                })
                .collect(),
        }
    };
    assert_eq!(
        resident,
        [
            value(Some("a"), 2, 4, 10, [(1, 1), (2, 1), (2, 2)]),
            value(Some("b"), 2, 3, 12, [(3, 2), (2, 1), (2, 2)]),
            value(None, 1, 1, 6, [(0, 0), (1, 1), (0, 0)]),
        ]
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
/// The notice exists because this failure is otherwise invisible: one catalog over one set
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

    let json = lingrep()
        .arg("summarize")
        .arg("--patterns")
        .arg(&patterns)
        .args(["--dict-path"])
        .arg(dictionary.path())
        .args(["--json"])
        .arg(&corpus)
        .output()
        .unwrap();
    assert_success(&json);
    let diagnostics = crate::support::parse_jsonl(&json.stderr);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0]["data"]["code"],
        "dictionary_binding_mismatch"
    );
    assert_eq!(diagnostics[0]["data"]["severity"], "warning");
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
/// Over 500 documents, surface keying yields 91,562 rows where capture keying
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

const TABLE_CATALOG: &str = "cat := [text=猫].\n\
    key: [text]\n\
    pattern word := [].\n\
    pattern pair := [] [].\n";

fn table_events(
    table: &std::path::Path,
    catalog: &std::path::Path,
    artifact: &std::path::Path,
    extra: &[&str],
) -> std::process::Output {
    lingrep_without_dictionary()
        .args(["--json", "table"])
        .arg(table)
        .arg("--patterns")
        .arg(catalog)
        .arg("--corpus")
        .arg(artifact)
        .args(extra)
        .output()
        .unwrap()
}

fn write_table(
    artifact: &std::path::Path,
    catalog: &std::path::Path,
    table: &std::path::Path,
) -> Vec<Value> {
    let output = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(catalog)
        .arg("--corpus")
        .arg(artifact)
        .arg("--write-table")
        .arg(table)
        .output()
        .unwrap();
    assert_success(&output);
    validate_query_jsonl(&output.stdout).unwrap();
    parse_jsonl(&output.stdout)
}

/// For each clause, the table's rows are the summary records of `summarize
/// --where` with the same rank, and each row's works is the number of
/// `--by-document` records of its group; the admitted documents and tokens
/// are the sums over its `document` records.
#[test]
fn table_rows_equal_summarize_where_and_by_document_works() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("tagged.table.lingtable");
    let events = write_table(&artifact, &catalog, &table);
    assert!(summary_events(&events).is_empty());
    assert_eq!(
        event(&events, "complete")["data"]["counters"]["records_emitted"],
        0
    );

    let summarize = |extra: &[&str]| {
        let output = lingrep_without_dictionary()
            .args(["--json", "summarize", "--patterns"])
            .arg(&catalog)
            .arg("--corpus")
            .arg(&artifact)
            .args(extra)
            .output()
            .unwrap();
        assert_success(&output);
        parse_jsonl(&output.stdout)
    };
    for clause in [
        None,
        Some("@document.year = 2001"),
        Some("@document.year >= 2002"),
        Some("@document.tags contains x & @document.year != 2003"),
        Some("@document.year = 1999"),
    ] {
        for top in [None, Some("1")] {
            let mut extra = Vec::new();
            if let Some(clause) = clause {
                extra.extend(["--where", clause]);
            }
            if let Some(top) = top {
                extra.extend(["--top", top]);
            }
            let output = table_events(&table, &catalog, &artifact, &extra);
            assert_success(&output);
            let records = parse_jsonl(&output.stdout);
            for record in &records {
                serde_json::from_value::<lingrep::protocol::table::TableEnvelope>(record.clone())
                    .unwrap();
            }
            let rows = records
                .iter()
                .filter(|record| record["type"] == "row")
                .map(|record| &record["data"])
                .collect::<Vec<_>>();
            let expected = summarize(&extra);
            let expected = summary_events(&expected);
            assert_eq!(
                rows.iter()
                    .map(|row| (&row["pattern"], &row["group"], &row["count"], &row["rank"]))
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|record| {
                        let data = &record["data"];
                        (
                            &data["pattern"],
                            &data["group"],
                            &data["count"],
                            &data["rank"],
                        )
                    })
                    .collect::<Vec<_>>(),
                "{clause:?} top {top:?}"
            );

            let mut by_document = vec!["--by-document"];
            by_document.extend(clause.iter().flat_map(|clause| ["--where", *clause]));
            let by_document = summarize(&by_document);
            let mut works = BTreeMap::<(String, String), u64>::new();
            for record in summary_events(&by_document) {
                let data = &record["data"];
                *works
                    .entry((data["pattern"].to_string(), data["group"].to_string()))
                    .or_default() += 1;
            }
            for row in &rows {
                assert_eq!(
                    row["works"],
                    works[&(row["pattern"].to_string(), row["group"].to_string())],
                    "{clause:?}"
                );
            }

            // Every pattern's totals precede the rows, in catalog order, and
            // count every group under the clause whatever --top keeps.
            let mut all = vec!["--top", "all"];
            all.extend(clause.iter().flat_map(|clause| ["--where", *clause]));
            let all = summarize(&all);
            let expected_patterns = ["cat", "word", "pair"]
                .map(|pattern| {
                    let occurrences = summary_events(&all)
                        .iter()
                        .filter(|record| record["data"]["pattern"] == pattern)
                        .map(|record| record["data"]["count"].as_u64().unwrap())
                        .sum::<u64>();
                    let works = summary_events(&by_document)
                        .iter()
                        .filter(|record| record["data"]["pattern"] == pattern)
                        .map(|record| record["data"]["document"].as_u64().unwrap())
                        .collect::<std::collections::BTreeSet<_>>()
                        .len();
                    serde_json::json!({
                        "schema": "lingrep.table/v1",
                        "type": "pattern",
                        "data": {"pattern": pattern, "occurrences": occurrences, "works": works},
                    })
                })
                .to_vec();
            assert_eq!(records[1..=3], expected_patterns, "{clause:?} top {top:?}");
            if clause == Some("@document.year = 1999") {
                assert!(
                    expected_patterns
                        .iter()
                        .all(|record| record["data"]["occurrences"] == 0),
                    "the clause admits nothing"
                );
            } else {
                assert!(
                    expected_patterns
                        .iter()
                        .any(|record| record["data"]["occurrences"] != 0),
                    "{clause:?} admits a match"
                );
            }
            let documents = by_document
                .iter()
                .filter(|record| record["type"] == "document")
                .map(|record| &record["data"])
                .collect::<Vec<_>>();
            let complete = &event(&records, "complete")["data"];
            assert_eq!(
                complete["documents_admitted"],
                documents.len(),
                "{clause:?}"
            );
            for (field, document_field) in [
                ("tokens_admitted", "tokens_admitted"),
                ("sentences_admitted", "sentences_admitted"),
            ] {
                assert_eq!(
                    complete[field],
                    documents
                        .iter()
                        .map(|document| document[document_field].as_u64().unwrap())
                        .sum::<u64>(),
                    "{clause:?} {field}"
                );
            }
        }
    }
}

/// The text output states the same pattern totals as the `pattern` records,
/// one line per pattern before the rows.
#[test]
fn table_text_output_begins_with_each_pattern_total() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("tagged.table.lingtable");
    write_table(&artifact, &catalog, &table);
    let clause = ["--where", "@document.year = 2001", "--top", "1"];

    let json = table_events(&table, &catalog, &artifact, &clause);
    assert_success(&json);
    let expected = parse_jsonl(&json.stdout)
        .iter()
        .filter(|record| record["type"] == "pattern")
        .map(|record| {
            let data = &record["data"];
            format!(
                "pattern {}: {} occurrences in {} documents",
                data["pattern"].as_str().unwrap(),
                data["occurrences"],
                data["works"]
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 3);

    let text = lingrep_without_dictionary()
        .arg("table")
        .arg(&table)
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .args(clause)
        .output()
        .unwrap();
    assert_success(&text);
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert_eq!(
        stdout.lines().take(3).collect::<Vec<_>>(),
        expected,
        "{stdout}"
    );
}

/// The `--by-metadata` values a document record's `--source-metadata` field
/// stands for: each distinct listed value, or none.
fn listed_values(value: Option<&Value>) -> Vec<String> {
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut values = match value {
        None => Vec::new(),
        Some(Value::Array(list)) => list.iter().map(text).collect(),
        Some(scalar) => vec![text(scalar)],
    };
    values.sort();
    values.dedup();
    values
}

/// Documents whose `tags` order values over every document: `w` first
/// appears in a 2001 document and again after `x`; `v` is only on a document
/// without sentences, which the artifact stores no metadata for; `x` is
/// listed twice; `1` is a string and a number; and two documents have no tag.
fn profile_artifact(directory: &TempDir, dictionary: &MiniDictionary) -> PathBuf {
    let input = directory.path().join("profile.jsonl");
    fs::write(
        &input,
        "{\"text\":\"猫が猫を見る。\",\"metadata\":{\"tags\":[\"w\"],\"year\":\"2001\"}}\n\
         {\"text\":\"\",\"metadata\":{\"tags\":\"v\",\"year\":\"2002\"}}\n\
         {\"text\":\"猫。犬。\",\"metadata\":{\"tags\":[\"x\",\"x\"],\"year\":\"2002\"}}\n\
         {\"text\":\"犬が鳥を見る。\",\"metadata\":{\"year\":\"2003\"}}\n\
         {\"text\":\"猫と犬。\",\"metadata\":{\"tags\":[],\"year\":\"2002\"}}\n\
         {\"text\":\"猫。猫。\",\"metadata\":{\"tags\":[\"w\",\"1\"],\"year\":\"2003\"}}\n\
         {\"text\":\"鳥。\",\"metadata\":{\"tags\":1,\"year\":\"2003\"}}\n",
    )
    .unwrap();
    let artifact = directory.path().join("profile.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert_success(&built);
    artifact
}

/// For one entry, `ling table --entry --by-metadata` publishes the
/// `metadata_value` records of `summarize --by-metadata` over a catalog whose
/// pattern matches only that entry: the same values in the same order with
/// the same admitted documents, sentences and tokens, and the pattern's
/// matches as the entry's count. Its works per value is the number of
/// `--by-document` documents with a match that carry the value.
#[test]
fn table_entry_by_metadata_equals_summarize_by_metadata_of_a_catalog_of_that_entry() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = profile_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("profile.table.lingtable");
    write_table(&artifact, &catalog, &table);

    // Each entry is selected by its human row text, that text with JSON
    // whitespace, and the `group` of its JSON row.
    for (pattern, keys, restricted) in [
        (
            "word",
            &[
                r#"{"text":"猫"}"#,
                r#"{ "text" : "猫" }"#,
                r#"{"kind":"key","fields":[{"kind":"scalar","name":"text","value":"猫"}]}"#,
            ][..],
            "key: [text]\npattern word := [text=猫].\n",
        ),
        (
            "pair",
            &["猫。", r#"{"kind":"surface","value":"猫。"}"#][..],
            "pattern pair := [text=猫] [text=。].\n",
        ),
    ] {
        let key = keys[0];
        let restricted_catalog = directory.path().join(format!("{pattern}.lx"));
        fs::write(&restricted_catalog, restricted).unwrap();
        let summarize = |extra: &[&str]| {
            let output = lingrep_without_dictionary()
                .args(["--json", "summarize", "--patterns"])
                .arg(&restricted_catalog)
                .arg("--corpus")
                .arg(&artifact)
                .args(extra)
                .output()
                .unwrap();
            assert_success(&output);
            parse_jsonl(&output.stdout)
        };
        for by in ["@document.tags", "@document.year"] {
            for clause in [
                None,
                Some("@document.year >= 2002"),
                Some("@document.year = 1999"),
            ] {
                let scope = clause
                    .iter()
                    .flat_map(|clause| ["--where", *clause])
                    .collect::<Vec<_>>();
                let entry = format!("{pattern}={key}");
                let mut extra = vec!["--entry", entry.as_str(), "--by-metadata", by];
                extra.extend(&scope);
                let output = table_events(&table, &catalog, &artifact, &extra);
                assert_success(&output);
                let records = parse_jsonl(&output.stdout);
                for record in &records {
                    serde_json::from_value::<lingrep::protocol::table::TableEnvelope>(
                        record.clone(),
                    )
                    .unwrap();
                }
                assert!(
                    records
                        .iter()
                        .all(|record| record["type"] != "pattern" && record["type"] != "row"),
                    "{pattern} {by} {clause:?}"
                );
                let published = records
                    .iter()
                    .filter(|record| record["type"] == "metadata_value")
                    .map(|record| record["data"].clone())
                    .collect::<Vec<_>>();
                for other in &keys[1..] {
                    let entry = format!("{pattern}={other}");
                    let mut extra = vec!["--entry", entry.as_str(), "--by-metadata", by];
                    extra.extend(&scope);
                    let output = table_events(&table, &catalog, &artifact, &extra);
                    assert_success(&output);
                    let selected = parse_jsonl(&output.stdout)
                        .into_iter()
                        .filter(|record| record["type"] == "metadata_value")
                        .map(|record| record["data"].clone())
                        .collect::<Vec<_>>();
                    assert_eq!(selected, published, "{entry} {by} {clause:?}");
                }

                let mut by_document = vec!["--by-document", "--source-metadata", by];
                by_document.extend(&scope);
                let by_document = summarize(&by_document);
                let field = by.strip_prefix("@document.").unwrap();
                let mut works = BTreeMap::<Option<String>, u64>::new();
                for record in by_document
                    .iter()
                    .filter(|record| record["type"] == "document")
                {
                    let data = &record["data"];
                    let matched = summary_events(&by_document)
                        .iter()
                        .any(|summary| summary["data"]["document"] == data["document"]);
                    if !matched {
                        continue;
                    }
                    let values = listed_values(data["source_metadata"]["document"].get(field));
                    if values.is_empty() {
                        *works.entry(None).or_default() += 1;
                    }
                    for value in values {
                        *works.entry(Some(value)).or_default() += 1;
                    }
                }

                let mut by_metadata = vec!["--by-metadata", by];
                by_metadata.extend(&scope);
                let expected = summarize(&by_metadata)
                    .into_iter()
                    .filter(|record| record["type"] == "metadata_value")
                    .map(|record| {
                        let data =
                            serde_json::from_value::<MetadataValueData>(record["data"].clone())
                                .unwrap();
                        let [count] = &data.patterns[..] else {
                            panic!("one pattern: {data:?}");
                        };
                        assert_eq!(count.pattern, pattern);
                        let group = if key.starts_with('{') {
                            serde_json::json!({
                                "kind": "key",
                                "fields": [{"kind": "scalar", "name": "text", "value": "猫"}],
                            })
                        } else {
                            serde_json::json!({"kind": "surface", "value": key})
                        };
                        serde_json::json!({
                            "key": data.key,
                            "value": data.value,
                            "documents_admitted": data.documents_admitted,
                            "sentences_admitted": data.sentences_admitted,
                            "tokens_admitted": data.tokens_admitted,
                            "entry": {
                                "pattern": pattern,
                                "group": group,
                                "count": count.matches,
                                "works": works.get(&data.value).copied().unwrap_or(0),
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(published, expected, "{pattern} {by} {clause:?}");
                if clause == Some("@document.year = 1999") {
                    assert!(expected.is_empty());
                } else {
                    assert!(
                        expected
                            .iter()
                            .any(|record| record["entry"]["count"].as_u64() > Some(0)),
                        "{pattern} {by} {clause:?} counts a match"
                    );
                }
                if by == "@document.tags" && clause != Some("@document.year = 1999") {
                    // Values are numbered over every document, including one the
                    // clause rejects; `v` has no record; documents without a tag
                    // share the null bucket.
                    let values = expected
                        .iter()
                        .map(|record| record["value"].as_str())
                        .collect::<Vec<_>>();
                    assert_eq!(
                        values,
                        [Some("w"), Some("x"), Some("1"), None],
                        "{clause:?}"
                    );
                }
            }
        }
    }
}

/// `--entry` and `--by-metadata` are given together, as JSON, without
/// `--top`; the entry names a table pattern and one of its keys as `ling
/// table` prints it.
#[test]
fn table_entry_rejects_an_unknown_pattern_or_key_and_incomplete_requests() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("tagged.table.lingtable");
    write_table(&artifact, &catalog, &table);
    let by = ["--by-metadata", "@document.year"];
    for (case, extra, diagnostic) in [
        (
            "unknown pattern",
            &["--entry", "nope=猫", by[0], by[1]][..],
            "nope",
        ),
        (
            "unknown key",
            &["--entry", r#"word={"text":"象"}"#, by[0], by[1]][..],
            "象",
        ),
        (
            "key not printed as ling table prints it",
            &["--entry", "word=猫", by[0], by[1]][..],
            "猫",
        ),
        (
            "a JSON object with kind that is not a group",
            &["--entry", r#"word={"kind":"nope"}"#, by[0], by[1]][..],
            "group",
        ),
        (
            "no key",
            &["--entry", "word", by[0], by[1]][..],
            "PATTERN=KEY",
        ),
        (
            "no --by-metadata",
            &["--entry", "cat=猫"][..],
            "--by-metadata",
        ),
        ("no --entry", &by[..], "--entry"),
        (
            "not a document key",
            &["--entry", "cat=猫", "--by-metadata", "@sentence.year"][..],
            "@document.NAME",
        ),
        (
            "with --top",
            &["--entry", "cat=猫", by[0], by[1], "--top", "1"][..],
            "--entry",
        ),
    ] {
        let output = table_events(&table, &catalog, &artifact, extra);
        assert_eq!(output.status.code(), Some(2), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{case}: {stderr}");
    }
    let text = lingrep_without_dictionary()
        .arg("table")
        .arg(&table)
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .args(["--entry", "cat=猫", by[0], by[1]])
        .output()
        .unwrap();
    assert_eq!(text.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&text.stderr).contains("--json"));
}

/// Two definitions published under one name can print the same row text, a
/// surface and a lexicon term; the text then exits 2 and the `group` of the
/// JSON row selects either.
#[test]
fn table_entry_selects_by_group_where_the_row_text_is_ambiguous() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("shared.lx");
    fs::write(
        &catalog,
        "output_name: \"shared\"\npattern cat := [text=猫].\n\n\
         output_name: \"shared\"\nlexicon cats by [text] := {\"猫\"}.\n",
    )
    .unwrap();
    let table = directory.path().join("shared.lingtable");
    write_table(&artifact, &catalog, &table);
    let by = ["--by-metadata", "@document.year"];

    let text = table_events(
        &table,
        &catalog,
        &artifact,
        &["--entry", "shared=猫", by[0], by[1]],
    );
    assert_eq!(text.status.code(), Some(2));
    assert!(text.stdout.is_empty());
    assert!(String::from_utf8_lossy(&text.stderr).contains("more than one entry"));

    for kind in ["surface", "term"] {
        let entry = format!(r#"shared={{"kind":"{kind}","value":"猫"}}"#);
        let output = table_events(
            &table,
            &catalog,
            &artifact,
            &["--entry", &entry, by[0], by[1]],
        );
        assert_success(&output);
        let records = parse_jsonl(&output.stdout);
        let groups = records
            .iter()
            .filter(|record| record["type"] == "metadata_value")
            .map(|record| record["data"]["entry"]["group"].clone())
            .collect::<Vec<_>>();
        assert!(!groups.is_empty(), "{kind}");
        assert!(
            groups
                .iter()
                .all(|group| *group == serde_json::json!({"kind": kind, "value": "猫"})),
            "{kind}: {groups:?}"
        );
    }
}

#[test]
fn table_rejects_a_table_for_another_catalog_or_artifact() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("tagged.table.lingtable");
    write_table(&artifact, &catalog, &table);

    let other_catalog = directory.path().join("other.lx");
    fs::write(&other_catalog, "cat := [text=猫].\n").unwrap();
    let other_input = directory.path().join("other.jsonl");
    fs::write(&other_input, "{\"text\":\"猫。\"}\n").unwrap();
    let other_artifact = directory.path().join("other.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&other_artifact)
        .arg(&other_input)
        .output()
        .unwrap();
    assert_success(&built);
    for (field, catalog, artifact) in [
        ("catalog fingerprint", &other_catalog, &artifact),
        ("analysis identity", &catalog, &other_artifact),
    ] {
        let output = table_events(&table, catalog, artifact, &[]);
        assert!(!output.status.success(), "{field}");
        assert!(output.stdout.is_empty(), "{field}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(field), "{field}: {stderr}");
    }
}

#[test]
fn write_table_and_table_reject_what_a_per_document_table_cannot_answer() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let table = directory.path().join("t.lingtable");
    for (case, extra) in [
        ("with --where", &["--where", "@document.year = 2001"][..]),
        ("with --sample", &["--sample", "0.5"][..]),
        ("with --by-document", &["--by-document"][..]),
    ] {
        let output = lingrep_without_dictionary()
            .args([
                "--json",
                "summarize",
                "--pattern",
                "cat := [text=猫].",
                "--corpus",
            ])
            .arg(&artifact)
            .arg("--write-table")
            .arg(&table)
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{case}");
        assert!(!table.exists(), "{case}");
    }
    let catalog = directory.path().join("cat.lx");
    fs::write(&catalog, "cat := [text=猫].\n").unwrap();
    write_table(&artifact, &catalog, &table);
    let again = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .arg("--write-table")
        .arg(&table)
        .output()
        .unwrap();
    assert!(!again.status.success(), "an existing table is replaced");
    let output = table_events(
        &table,
        &catalog,
        &artifact,
        &["--where", "@section = methods"],
    );
    assert_eq!(output.status.code(), Some(2));
}

/// A signal delivered while the table is staged ends the run as a cancelled
/// `summarize` ends: exit 130 or 143, no diagnostic, no `complete` record,
/// and neither the table nor its staging directory left behind.
#[cfg(all(unix, debug_assertions))]
#[test]
fn a_signal_while_a_table_is_written_publishes_nothing() {
    use std::process::Stdio;

    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let tables = directory.path().join("tables");
    fs::create_dir(&tables).unwrap();
    let table = tables.join("t.lingtable");
    let entries = || fs::read_dir(&tables).unwrap().count();
    for (phase, signal, code) in [
        ("ranking", libc::SIGINT, 130),
        ("ranking", libc::SIGTERM, 143),
        ("transpose", libc::SIGINT, 130),
        ("transpose", libc::SIGTERM, 143),
    ] {
        let case = format!("signal {signal} during {phase}");
        let child = lingrep_std()
            .args(["--json", "summarize", "--patterns"])
            .arg(&catalog)
            .arg("--corpus")
            .arg(&artifact)
            .arg("--write-table")
            .arg(&table)
            .env("LINGREP_TEST_PAUSE_COUNT_TABLE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while entries() == 0 {
            assert!(std::time::Instant::now() < deadline, "{case}: no staging");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        unsafe {
            libc::kill(child.id() as libc::pid_t, signal);
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{case}");
        assert!(
            output.stderr.is_empty(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            parse_jsonl(&output.stdout)
                .iter()
                .all(|record| record["type"] != "complete"),
            "{case}"
        );
        assert_eq!(entries(), 0, "{case}: a table or staging directory remains");
    }
}

#[test]
fn table_human_output_precedes_a_later_row_decode_error() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let artifact = tagged_artifact(&directory, &dictionary);
    let catalog = directory.path().join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let table = directory.path().join("tagged.table.lingtable");
    write_table(&artifact, &catalog, &table);
    let ranked = fs::read(table.join("ranked_entries.u32")).unwrap();
    let last = u32::from_le_bytes(ranked[ranked.len() - 4..].try_into().unwrap()) as usize;
    let offsets = fs::read(table.join("entry_key_offsets.u64")).unwrap();
    let start = u64::from_le_bytes(offsets[last * 8..last * 8 + 8].try_into().unwrap()) as usize;
    let mut keys = fs::read(table.join("entry_keys.u32")).unwrap();
    keys[start * 4..start * 4 + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fs::write(table.join("entry_keys.u32"), keys).unwrap();
    let output = lingrep_without_dictionary()
        .arg("table")
        .arg(&table)
        .arg("--patterns")
        .arg(&catalog)
        .arg("--corpus")
        .arg(&artifact)
        .args(["--top", "all"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("entry key schema out of range"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| !line.starts_with("pattern ") && !line.trim().is_empty()),
        "no rows emitted before error: {stdout}"
    );
    assert!(!stdout.contains("rows in"));
}
