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
//! `--where` end to end: refusal before work, the clause echo, the four-bucket
//! accounting, and what the denominator covers under prefix coverage.
//!
//! The fixture is four documents and seven sentences, one per accounting
//! bucket under `year >= 2010`: document 0 admitted (2 sentences), document 1
//! false (1), document 2 missing the key (3), document 3 carrying `"47-56"`,
//! a value no ordered comparison can use (1).

use std::process::Output;

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;

use crate::support::{fixture, lingrep, parse_jsonl};

fn run(extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    command
        .arg("--json")
        .arg("matches")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--pattern")
        .arg("[].");
    command
        .args(extra)
        .arg(fixture("scope-fixture.jsonl"))
        .output()
        .unwrap()
}

fn complete(extra: &[&str]) -> Value {
    let output = run(extra);
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]
        .clone()
}

#[test]
fn an_unparseable_clause_is_refused_before_any_input_is_read() {
    let output = run(&["--where", "@document.year >= abc"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("decimal"),
        "the refusal must say what it wanted: {stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "nothing may be published before the clause is accepted"
    );
}

#[test]
fn an_unscoped_run_reports_no_scope() {
    let complete = complete(&[]);

    assert!(complete.get("scope").is_none(), "absent, not null");
    assert_eq!(complete["counters"]["sentences_admitted"], 7);
}

#[test]
fn direct_input_publishes_its_scope_accounting_in_query_complete() {
    // The test that crosses the ownership boundary a unit test cannot.
    // `source_from_events` takes the events by value and the source is moved
    // into `execute`, so these numbers can only arrive through the observation
    // handle. A reader-level test passes whether or not the command can reach
    // them; this one does not.
    let complete = complete(&["--where", "@document.year >= 2010"]);
    let scope = &complete["scope"];

    assert_eq!(scope["clause"], "@document.year >= 2010");
    assert_eq!(scope["basis"], "corpus");
    assert_eq!(scope["documents"]["admitted"], 1);
    assert_eq!(scope["documents"]["excluded_by_predicate"], 1);
    assert_eq!(scope["documents"]["excluded_missing_field"], 1);
    assert_eq!(scope["documents"]["excluded_shape_mismatch"], 1);
    assert_eq!(scope["sentences"]["admitted"], 2);
    assert_eq!(scope["sentences"]["excluded_by_predicate"], 1);
    assert_eq!(scope["sentences"]["excluded_missing_field"], 3);
    assert_eq!(scope["sentences"]["excluded_shape_mismatch"], 1);
    assert_eq!(
        complete["counters"]["sentences_admitted"], 2,
        "at full coverage the scope's admitted count is what the run matched over"
    );
    assert_eq!(complete["coverage"], "full");
}

#[test]
fn the_four_buckets_sum_to_the_whole_corpus_at_both_grains() {
    let scoped = complete(&["--where", "@document.year >= 2010"]);
    let unscoped = complete(&[]);
    let scope = &scoped["scope"];

    let sentences: u64 = [
        "admitted",
        "excluded_by_predicate",
        "excluded_missing_field",
        "excluded_shape_mismatch",
    ]
    .iter()
    .map(|bucket| scope["sentences"][bucket].as_u64().unwrap())
    .sum();

    assert_eq!(
        sentences,
        unscoped["counters"]["sentences_admitted"].as_u64().unwrap(),
        "2 + 1 + 3 + 1 = 7"
    );
}

#[test]
fn two_disjoint_scopes_do_not_partition_the_universe_between_them() {
    // The point of four buckets. `year < 2010` and `year >= 2010` cannot both
    // hold, yet their admitted sets do not sum to the corpus: the shortfall is
    // exactly the two neither-polarity buckets, and a run reporting three
    // buckets could not state it.
    let early = complete(&["--where", "@document.year < 2010"]);
    let late = complete(&["--where", "@document.year >= 2010"]);
    let unscoped = complete(&[]);

    let early_admitted = early["scope"]["sentences"]["admitted"].as_u64().unwrap();
    let late_admitted = late["scope"]["sentences"]["admitted"].as_u64().unwrap();
    let missing = early["scope"]["sentences"]["excluded_missing_field"]
        .as_u64()
        .unwrap();
    let shape = early["scope"]["sentences"]["excluded_shape_mismatch"]
        .as_u64()
        .unwrap();
    let total = unscoped["counters"]["sentences_admitted"].as_u64().unwrap();

    assert_eq!(early_admitted, 1);
    assert_eq!(late_admitted, 2);
    assert!(
        early_admitted + late_admitted < total,
        "complementary-looking scopes are NOT exhaustive"
    );
    assert_eq!(
        early_admitted + late_admitted + missing + shape,
        total,
        "1 + 2 + 3 + 1 = 7"
    );
}

#[test]
fn a_limited_direct_run_reports_an_observed_prefix_not_a_corpus_denominator() {
    // The reader runs ahead of the matcher: `next_batch` crosses document
    // boundaries to fill a batch while the runner can stop inside an earlier
    // partition. So scope counts at prefix coverage describe what the *reader*
    // admitted, which is neither the corpus nor the matched prefix — and the
    // record says so rather than letting a consumer assume.
    let complete = complete(&["--where", "@document.year >= 2010", "--limit", "1"]);
    let scope = &complete["scope"];

    assert_eq!(complete["coverage"], "prefix");
    assert_eq!(
        scope["basis"], "observed_prefix",
        "a stopped direct run must not claim a corpus denominator"
    );
    assert!(
        scope["sentences"]["admitted"].as_u64().unwrap()
            >= complete["counters"]["sentences_admitted"].as_u64().unwrap(),
        "the reader admitted at least what the matcher saw"
    );
    assert_eq!(
        complete["counters"]["records_emitted"], 1,
        "the committed match prefix is unaffected by how far the reader ran"
    );
}

#[test]
fn where_is_rejected_when_given_twice() {
    let output = run(&["--where", "@document.a = 1", "--where", "@document.b = 2"]);

    assert!(!output.status.success());
}

/// BCCWJ's document metadata is queryable, and one clause means the same
/// thing over the live distribution and the prepared artifact — the
/// single-typing-rule invariant (spec D1/D3).
fn bccwj_scope(
    clause: &str,
    root: &std::path::Path,
    artifact: &std::path::Path,
    dictionary: &MiniDictionary,
    prepared: bool,
) -> Value {
    let mut command = lingrep();
    command.args(["--json", "matches", "--pattern", "[]."]);
    if prepared {
        command.arg("--corpus").arg(artifact);
    } else {
        command
            .arg("--dict-path")
            .arg(dictionary.path())
            .args(["--input-format", "bccwj"])
            .arg(root);
    }
    command.args(["--where", clause]);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|record| record["type"] == "complete")
        .expect("a completion record")["data"]["scope"]
        .clone()
}

/// The census the two-zip synthetic distribution has: two sentences in the
/// FIXED `one` entry (subcorpus `書籍`, sampling `fixed`) and one in the
/// VARIABLE `solo` entry (subcorpus `新聞`, sampling `variable`).
#[test]
fn bccwj_metadata_clauses_select_exactly_their_tree_or_subcorpus() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = crate::support::bccwj_distribution(&directory.path().join("dist"));
    let artifact = crate::support::bccwj_build_artifact(&root, &dictionary);

    for clause in ["@document.subcorpus = 書籍", "@document.sampling = fixed"] {
        for prepared in [false, true] {
            let scope = bccwj_scope(clause, &root, &artifact, &dictionary, prepared);
            let (where_over, sentences, documents) = if prepared {
                (
                    "the prepared artifact",
                    &scope["sentences"],
                    &scope["documents"],
                )
            } else {
                (
                    "the live distribution",
                    &scope["sentences"],
                    &scope["documents"],
                )
            };
            assert_eq!(
                sentences["admitted"], 2,
                "`{clause}` over {where_over}: {scope}"
            );
            assert_eq!(
                sentences["excluded_by_predicate"], 1,
                "`{clause}` over {where_over}: {scope}"
            );
            assert_eq!(documents["admitted"], 1, "`{clause}`: {scope}");
            assert_eq!(documents["excluded_by_predicate"], 1, "`{clause}`: {scope}");
        }
    }
}
