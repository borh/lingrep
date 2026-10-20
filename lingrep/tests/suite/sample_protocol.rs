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
//! `--sample` end to end: refusal before work, what the `run` record declares,
//! and the property that makes a sampled result quotable — one seed selects one
//! sample, whatever else the invocation says.
//!
//! The fixture is generated rather than checked in, because the interesting
//! assertions are about proportions and a handful of sentences cannot carry
//! one.

use std::process::Output;

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{lingrep, parse_jsonl};

/// One thousand single-token sentences, ten to a document.
fn corpus() -> (TempDir, std::path::PathBuf) {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("sample-fixture.jsonl");
    let documents = (0..100)
        .map(|document| {
            let sentences = (0..10)
                .map(|index| format!("\"猫が鳥{}\"", document * 10 + index))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{\"metadata\": {{}}, \"sentences\": [{sentences}]}}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, documents + "\n").unwrap();
    (directory, path)
}

fn run(root: &std::path::Path, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    command
        .arg("--json")
        .arg("summarize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[text=猫].");
    command.args(extra).arg(root).output().unwrap()
}

fn records(root: &std::path::Path, extra: &[&str]) -> Vec<Value> {
    let output = run(root, extra);
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
}

fn run_record(records: &[Value]) -> Value {
    records
        .iter()
        .find(|record| record["type"] == "run")
        .expect("a run record")["data"]
        .clone()
}

/// Sentences the run actually looked at, which is what a sample changes.
fn admitted(records: &[Value]) -> u64 {
    records
        .iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]["counters"]["sentences_admitted"]
        .as_u64()
        .expect("an admitted-sentence count")
}

/// An unsampled run must stay byte-identical under `lingrep.query/v2`: the
/// field is optional and skipped, so no existing consumer sees a new key.
#[test]
fn an_unsampled_run_declares_no_sample() {
    let (_directory, root) = corpus();
    let run = run_record(&records(&root, &[]));
    assert!(
        run.get("sample").is_none(),
        "an unsampled run must not carry the key at all: {run}"
    );
}

#[test]
fn a_sampled_run_declares_its_rate_seed_and_unit() {
    let (_directory, root) = corpus();
    let run = run_record(&records(&root, &["--sample", "0.25", "--sample-seed", "7"]));
    let sample = &run["sample"];
    assert_eq!(sample["rate_per_million"], 250_000);
    assert_eq!(sample["seed"], 7);
    assert_eq!(
        sample["unit"], "sentence",
        "the rate is per sentence, and the record has to say which unit"
    );
}

/// Loose bounds on purpose: this pins that the rate reaches the run at all,
/// not that a thousand draws pass a uniformity test.
#[test]
fn a_sampled_run_looks_at_about_the_requested_fraction() {
    let (_directory, root) = corpus();
    let whole = admitted(&records(&root, &[]));
    assert_eq!(whole, 1_000, "the fixture is a thousand sentences");
    let sampled = admitted(&records(&root, &["--sample", "0.25"]));
    assert!(
        (150..350).contains(&sampled),
        "a quarter of 1,000 should land near 250, got {sampled}"
    );
}

/// The property the whole design rests on. `--limit` changes how much of the
/// run is published and `--top` changes how much is reported; neither may
/// change which sentences the sample selected.
#[test]
fn one_seed_selects_one_sample_whatever_else_the_invocation_says() {
    let (_directory, root) = corpus();
    let plain = admitted(&records(&root, &["--sample", "0.25", "--sample-seed", "3"]));
    let with_top = admitted(&records(
        &root,
        &["--sample", "0.25", "--sample-seed", "3", "--top", "1"],
    ));
    assert_eq!(plain, with_top);

    let other_seed = admitted(&records(&root, &["--sample", "0.25", "--sample-seed", "4"]));
    assert_ne!(
        plain, other_seed,
        "two seeds must not agree on 1,000 sentences by coincidence"
    );
}

#[test]
fn a_full_rate_is_a_valid_no_op_rather_than_an_error() {
    let (_directory, root) = corpus();
    let records = records(&root, &["--sample", "1.0"]);
    assert_eq!(admitted(&records), 1_000);
    assert_eq!(
        run_record(&records)["sample"]["rate_per_million"],
        1_000_000,
        "the run still declares that it was sampled, at a rate of one"
    );
}

#[test]
fn a_rate_outside_the_unit_interval_is_refused_before_any_input_is_read() {
    let (_directory, root) = corpus();
    for rate in ["--sample=0", "--sample=1.5", "--sample=-0.2"] {
        let output = run(&root, &[rate]);
        assert!(!output.status.success(), "`{rate}` must be refused");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("greater than 0 and at most 1"),
            "the refusal must name the interval it wanted: {stderr}"
        );
        assert!(output.stdout.is_empty(), "nothing may be published");
    }
}

/// An artifact built from a sample would wear the identity of the whole
/// corpus, and every later run against it would be silently sampled.
#[test]
fn sampling_a_cached_run_is_refused() {
    let (_directory, root) = corpus();
    let output = run(&root, &["--sample", "0.25", "--cache"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--cache"),
        "the refusal must name the flag it conflicts with: {stderr}"
    );
}

/// The TSV contracts are rows and nothing else, so a sampled count would
/// travel with nothing saying it is an estimate.
#[test]
fn sampling_a_tsv_run_is_refused_because_it_has_no_run_record() {
    let (_directory, root) = corpus();
    let dictionary = MiniDictionary::new();
    for subcommand in ["summarize", "matches"] {
        let output = lingrep()
            .arg(subcommand)
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg("--pattern")
            .arg("[text=猫].")
            .arg("--sample")
            .arg("0.25")
            .arg("--tsv")
            .arg(&root)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{subcommand} --tsv must refuse");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("run record"),
            "the refusal must say why: {stderr}"
        );
    }
}

/// `--sample-seed` alone would look like it sampled and would not have.
#[test]
fn a_seed_without_a_rate_is_refused() {
    let (_directory, root) = corpus();
    let output = run(&root, &["--sample-seed", "7"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--sample"),
        "the refusal must name the flag it requires: {stderr}"
    );
}

#[test]
fn prepared_block_samples_publish_the_exact_full_stream_subset() {
    use lingrep_core::execution::CorpusSample;
    let (directory, root) = corpus();
    let dictionary = MiniDictionary::new();
    let artifact = directory.path().join("sample.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let query = |command: &str, extra: &[&str]| {
        let mut cmd = crate::support::lingrep_without_dictionary();
        cmd.args(["--json", command, "--corpus"])
            .arg(&artifact)
            .args(["--pattern", "p := ^ (?<whole> []+) $."]);
        if command == "matches" && !extra.contains(&"--limit") {
            cmd.args(["--limit", "all"]);
        }
        let output = cmd.args(extra).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)
    };
    let matches = |records: &[Value]| {
        records
            .iter()
            .filter(|r| r["type"] == "match")
            .map(|r| r["data"].clone())
            .collect::<Vec<_>>()
    };
    let full = matches(&query("matches", &[]));
    assert_eq!(full.len(), 1000);
    let sample = CorpusSample::new(500_000, 7).unwrap();
    let mut expected = full
        .iter()
        .filter(|r| {
            let ordinal = r["corpus_sentence_ordinal"].as_u64().unwrap();
            sample.admits(ordinal / 256 * 256)
        })
        .cloned()
        .collect::<Vec<_>>();
    for (sequence, record) in expected.iter_mut().enumerate() {
        record["sequence"] = (sequence as u64).into();
    }
    assert!(!expected.is_empty() && expected.len() < full.len());
    for workers in ["1", "4"] {
        let rows = query(
            "matches",
            &[
                "--sample",
                "0.5",
                "--sample-blocks",
                "--sample-seed",
                "7",
                "--workers",
                workers,
            ],
        );
        assert_eq!(matches(&rows), expected);
        assert_eq!(
            run_record(&rows)["sample"],
            serde_json::json!({"rate_per_million":500000,"seed":7,"unit":"token_block",
                "token_component_blake3": serde_json::from_slice::<Value>(&std::fs::read(artifact.join("manifest.json")).unwrap()).unwrap()["components"]["tokens.v1.zst"]["blake3"]})
        );
        assert_eq!(admitted(&rows), expected.len() as u64);
    }
    let summary = query(
        "summarize",
        &["--sample", "0.5", "--sample-blocks", "--sample-seed", "7"],
    );
    assert_eq!(admitted(&summary), expected.len() as u64);
    assert_eq!(run_record(&summary)["sample"]["unit"], "token_block");
    let prefix = query(
        "matches",
        &[
            "--sample",
            "0.5",
            "--sample-blocks",
            "--sample-seed",
            "7",
            "--limit",
            "3",
        ],
    );
    assert_eq!(matches(&prefix), expected[..3]);
    let all = query("matches", &["--sample", "1", "--sample-blocks"]);
    assert_eq!(matches(&all), full);
    let none = query(
        "matches",
        &[
            "--sample",
            "0.000001",
            "--sample-blocks",
            "--sample-seed",
            "7",
        ],
    );
    assert!(matches(&none).is_empty());
    assert_eq!(admitted(&none), 0);
}

#[test]
fn block_sampling_requires_a_declared_rate_and_prepared_corpus() {
    for args in [
        vec!["--sample-blocks"],
        vec!["--sample", "0.5", "--sample-blocks"],
    ] {
        for command in ["matches", "summarize"] {
            let output = lingrep().arg(command).args(&args).output().unwrap();
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("--corpus"));
            assert!(parse_jsonl(&output.stdout).is_empty());
        }
    }
}

#[test]
fn block_sampling_requires_json_before_opening_the_artifact() {
    for command in ["matches", "summarize"] {
        let output = lingrep()
            .args([
                command,
                "--pattern",
                "p := [].",
                "--sample",
                "0.5",
                "--sample-blocks",
                "--corpus",
                "missing.lingcorpus",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--json"));
        assert!(parse_jsonl(&output.stdout).is_empty());
    }
}

#[test]
fn retained_matches_preserve_exact_counts_and_full_record_evidence() {
    let (_directory, root) = corpus();
    let dictionary = MiniDictionary::new();
    let query = |extra: &[&str]| {
        let output = lingrep()
            .args([
                "--json",
                "matches",
                "--pattern",
                "p := (?<n> [text=猫]).",
                "--dict-path",
            ])
            .arg(dictionary.path())
            .args(extra)
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)
    };
    let full = query(&[]);
    let evidence = |events: &[Value]| {
        events
            .iter()
            .filter(|v| v["type"] == "match")
            .cloned()
            .collect::<Vec<_>>()
    };
    let complete =
        |events: &[Value]| events.iter().find(|v| v["type"] == "complete").unwrap()["data"].clone();
    for capacity in ["1", "20", "2000"] {
        let draw = query(&["--retain", capacity, "--retain-seed", "7"]);
        let again = query(&["--retain", capacity, "--retain-seed", "7", "--workers", "4"]);
        let rows = evidence(&draw);
        assert_eq!(rows, evidence(&again));
        let end = complete(&draw);
        assert_eq!(end["coverage"], "full");
        assert_eq!(
            end["counters"]["matches_found"],
            complete(&full)["counters"]["matches_found"]
        );
        assert_eq!(end["retained_matches"]["seen"], evidence(&full).len());
        assert_eq!(end["retained_matches"]["retained"], rows.len());
        assert_eq!(end["counters"]["records_emitted"], rows.len());
        assert_eq!(rows.len(), capacity.parse::<usize>().unwrap().min(1000));
        for row in &rows {
            assert!(evidence(&full).contains(row));
        }
        if capacity == "20" {
            assert!(
                rows.iter()
                    .any(|v| v["data"]["corpus_sentence_ordinal"].as_u64().unwrap() > 750)
            );
        }
    }
    let sampled = query(&["--retain", "20", "--sample", "0.1"]);
    assert_eq!(run_record(&sampled)["sample"]["unit"], "sentence");
    assert!(
        complete(&sampled)["retained_matches"]["seen"]
            .as_u64()
            .unwrap()
            < 1000
    );
}

#[test]
fn retention_refuses_prefix_and_human_output_and_handles_no_hits() {
    let (_directory, root) = corpus();
    let dictionary = MiniDictionary::new();
    for flags in [
        vec!["--retain", "2"],
        vec!["--json", "--retain", "2", "--limit", "1"],
    ] {
        let output = lingrep()
            .args(["matches", "--pattern", "[text=猫].", "--dict-path"])
            .arg(dictionary.path())
            .args(flags)
            .arg(&root)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let output = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            "[text=不存在].",
            "--dict-path",
        ])
        .arg(dictionary.path())
        .args(["--retain", "20"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert!(!events.iter().any(|v| v["type"] == "match"));
    assert_eq!(
        events.last().unwrap()["data"]["retained_matches"]["seen"],
        0
    );
}

#[test]
fn a_failed_retention_scan_publishes_no_sample_or_completion() {
    let (directory, root) = corpus();
    let mut source = std::fs::read_to_string(&root).unwrap();
    source.push_str("this is not json\n");
    std::fs::write(&root, source).unwrap();
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "--json",
            "matches",
            "--pattern",
            "[text=猫].",
            "--dict-path",
        ])
        .arg(dictionary.path())
        .args(["--retain", "20"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let events = parse_jsonl(&output.stdout);
    assert!(
        !events
            .iter()
            .any(|v| v["type"] == "match" || v["type"] == "complete")
    );
}

#[test]
fn selective_prepared_queries_preserve_records_and_report_skipped_matcher_work() {
    let directory = TempDir::new().unwrap();
    let root = directory.path().join("selective.jsonl");
    let sentences = (0..2048)
        .map(|ordinal| if ordinal == 1025 { "犬" } else { "猫" })
        .collect::<Vec<_>>();
    std::fs::write(
        &root,
        serde_json::to_vec(
            &serde_json::json!({"metadata": {"channel":"keep"}, "sentences": sentences}),
        )
        .unwrap(),
    )
    .unwrap();
    let dictionary = MiniDictionary::new();
    let artifact = directory.path().join("selective.lingcorpus");
    let built = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let query = |command: &str, predicate: &str, extra: &[&str]| {
        let mut cmd = crate::support::lingrep_without_dictionary();
        cmd.args(["--json", command, "--corpus"])
            .arg(&artifact)
            .args(["--pattern", &format!("p := (?<word> [{predicate}]).")]);
        if command == "matches" && !extra.contains(&"--limit") {
            cmd.args(["--limit", "all"]);
        }
        let output = cmd.args(extra).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)
    };
    let data = |records: &[Value], kind: &str| {
        records
            .iter()
            .filter(|row| row["type"] == kind)
            .map(|row| row["data"].clone())
            .collect::<Vec<_>>()
    };
    for command in ["matches", "summarize"] {
        let indexed = query(command, "lemma=犬", &[]);
        let scanned = query(command, "lemma ~ /犬/", &[]);
        let event = if command == "matches" {
            "match"
        } else {
            "summary"
        };
        assert_eq!(data(&indexed, event), data(&scanned, event));
        let narrow = query(command, "lemma=犬", &["--max-tokenized-bytes", "64"]);
        assert_eq!(data(&narrow, event), data(&scanned, event));
        let actual = data(&indexed, "complete").pop().unwrap();
        let reference = data(&scanned, "complete").pop().unwrap();
        assert_eq!(actual["corpus"], reference["corpus"]);
        assert_eq!(actual["inventory"], reference["inventory"]);
        let counters = &actual["counters"];
        assert_eq!(counters["sentences_admitted"], 2048);
        assert_eq!(counters["sentences_proven_absent"], 2047);
        assert_eq!(counters["sentences_matched"], 1);
        assert_eq!(counters["tokens_matched"], 1);
        for key in ["documents_admitted", "matches_found", "records_emitted"] {
            assert_eq!(counters[key], reference["counters"][key], "{key}");
        }
        assert!(
            reference["counters"]
                .get("sentences_proven_absent")
                .is_none()
        );
        for extra in [
            vec!["--sample", "1"],
            vec!["--where", "@document.channel = keep"],
        ] {
            let fallback = query(command, "lemma=犬", &extra);
            assert_eq!(data(&fallback, event), data(&indexed, event));
            let complete = data(&fallback, "complete").pop().unwrap();
            assert_eq!(complete["counters"]["sentences_matched"], 2048);
            assert!(
                complete["counters"]
                    .get("sentences_proven_absent")
                    .is_none()
            );
        }
    }
    for command in ["matches", "summarize"] {
        for predicate in ["lemma=不在", "lemma ~ /不在/"] {
            let mut cmd = crate::support::lingrep_without_dictionary();
            cmd.args(["--json", command, "--corpus"])
                .arg(&artifact)
                .args([
                    "--pattern",
                    &format!("p := [{predicate}]."),
                    "--max-tokenized-bytes",
                    "1",
                ]);
            if command == "matches" {
                cmd.args(["--limit", "all"]);
            }
            let output = cmd.output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{command}: {predicate}");
            let diagnostics = parse_jsonl(&output.stderr);
            assert_eq!(
                diagnostics[0]["data"]["code"],
                "oversized_sentences_skipped",
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !parse_jsonl(&output.stdout)
                    .iter()
                    .any(|row| row["type"] == "complete")
            );
        }
    }
    let limited = query("matches", "lemma=犬", &["--limit", "1"]);
    let complete = data(&limited, "complete").pop().unwrap();
    assert!(
        complete["counters"]
            .get("sentences_proven_absent")
            .is_none()
    );
}
