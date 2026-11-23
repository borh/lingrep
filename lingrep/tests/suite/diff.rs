// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

//! `ling diff` over real `ling --json matches` streams.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::support::{assert_success, fixture, lingrep_without_dictionary, parse_jsonl};

const BASE: &str = "p := (?<n> [upos=NOUN]) [upos=ADP] (?<v> [upos=VERB]).";
/// `BASE` with the same spans and a wider `v` capture.
const WIDER_CAPTURE: &str = "p := (?<n> [upos=NOUN]) (?<v> [upos=ADP] [upos=VERB]).";
const SUBJECTS: &str =
    "p := (?<n> [upos=NOUN]) [upos=ADP] (?<v> [upos=VERB]) where $v -[nsubj]-> $n.";

fn matches(directory: &Path, name: &str, pattern: &str, extra: &[&str]) -> PathBuf {
    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "matches",
            "--evidence",
            "--input-format",
            "conllu",
        ])
        .args(["--pattern", pattern])
        .args(extra)
        .arg(fixture("psycholinguistics.conllu"))
        .output()
        .unwrap();
    assert_success(&output);
    let path = directory.join(name);
    std::fs::write(&path, &output.stdout).unwrap();
    path
}

fn diff(a: &Path, b: &Path, extra: &[&str]) -> Vec<Value> {
    let output = lingrep_without_dictionary()
        .args(["--json", "diff"])
        .args([a, b])
        .args(extra)
        .output()
        .unwrap();
    assert_success(&output);
    let events = parse_jsonl(&output.stdout);
    assert!(
        events
            .iter()
            .all(|event| event["schema"] == "lingrep.diff/v1")
    );
    events
}

fn of<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|event| event["type"] == kind)
        .map(|event| &event["data"])
        .collect()
}

#[test]
fn records_that_differ_only_in_captures_are_shared_spans_and_distinct_records() {
    let directory = tempfile::tempdir().unwrap();
    let a = matches(directory.path(), "a.jsonl", BASE, &[]);
    let b = matches(directory.path(), "b.jsonl", WIDER_CAPTURE, &[]);
    let published = |path: &Path| {
        parse_jsonl(&std::fs::read(path).unwrap())
            .into_iter()
            .filter(|event| event["type"] == "match")
            .map(|event| event["data"].clone())
            .collect::<Vec<_>>()
    };
    let (left, right) = (published(&a), published(&b));
    assert_eq!(left.len(), 5);
    // The two runs publish the same spans and different captures.
    for (left, right) in left.iter().zip(&right) {
        assert_eq!(left["span"], right["span"]);
        assert_eq!(left["captures"][0], right["captures"][0]);
        assert_ne!(left["captures"][1]["span"], right["captures"][1]["span"]);
    }

    let span = diff(&a, &b, &[]);
    assert_eq!(of(&span, "run")[0]["level"], "span");
    assert_eq!(
        of(&span, "run")[0]["coverage"],
        json!({"kind": "full", "sentences_scanned": 6})
    );
    assert_eq!(
        of(&span, "definition")[0],
        &json!({"a": "p", "b": "p", "shared": 5, "a_only": 0, "b_only": 0})
    );
    assert!(of(&span, "occurrence").is_empty());
    assert_eq!(of(&span, "complete")[0]["differences_found"], false);

    for level in ["record", "roles"] {
        let events = diff(&a, &b, &["--level", level, "--examples", "2"]);
        assert_eq!(of(&events, "run")[0]["level"], level);
        let definition = of(&events, "definition")[0];
        assert_eq!(
            (
                &definition["shared"],
                &definition["a_only"],
                &definition["b_only"]
            ),
            (&json!(0), &json!(5), &json!(5)),
            "{level}"
        );
        let occurrences = of(&events, "occurrence");
        assert_eq!(
            occurrences
                .iter()
                .map(|occurrence| occurrence["status"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["a_only", "a_only", "b_only", "b_only"]
        );
        assert_eq!(occurrences[0]["sentence"], "新聞を子どもが読む");
        assert_eq!(occurrences[0]["corpus_sentence_ordinal"], 0);
        assert_eq!(of(&events, "complete")[0]["differences_found"], true);
    }
}

#[test]
fn a_run_stopped_at_a_limit_is_compared_over_the_sentences_it_completed() {
    let directory = tempfile::tempdir().unwrap();
    // A direct run stopped at --limit records no corpus identity; a prepared
    // corpus states it before the first sentence.
    let corpus = directory.path().join("fixture.lingcorpus");
    let built = lingrep_without_dictionary()
        .args(["corpus", "build", "--input-format", "conllu", "--output"])
        .arg(&corpus)
        .arg(fixture("psycholinguistics.conllu"))
        .output()
        .unwrap();
    assert_success(&built);
    let prepared = |name: &str, pattern: &str, extra: &[&str]| {
        let output = lingrep_without_dictionary()
            .args(["--json", "matches", "--evidence", "--pattern", pattern])
            .args(extra)
            .arg("--corpus")
            .arg(&corpus)
            .output()
            .unwrap();
        assert_success(&output);
        let path = directory.path().join(name);
        std::fs::write(&path, &output.stdout).unwrap();
        path
    };
    let a = prepared("a.jsonl", BASE, &[]);
    let full = prepared("full.jsonl", SUBJECTS, &[]);
    let limited = prepared("limited.jsonl", SUBJECTS, &["--limit", "2"]);

    let direct = matches(
        directory.path(),
        "direct.jsonl",
        SUBJECTS,
        &["--limit", "2"],
    );
    let refused = lingrep_without_dictionary()
        .arg("diff")
        .args([&direct, &direct])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("prepared corpus"));

    let events = diff(&a, &full, &[]);
    assert_eq!(
        of(&events, "definition")[0],
        &json!({"a": "p", "b": "p", "shared": 2, "a_only": 3, "b_only": 0})
    );

    let events = diff(&a, &limited, &[]);
    assert_eq!(
        of(&events, "run")[0]["coverage"],
        json!({"kind": "prefix", "before_corpus_sentence": 2})
    );
    assert_eq!(
        of(&events, "definition")[0],
        &json!({"a": "p", "b": "p", "shared": 1, "a_only": 1, "b_only": 0})
    );

    let text = lingrep_without_dictionary()
        .arg("diff")
        .args([&limited, &limited])
        .output()
        .unwrap();
    assert_success(&text);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.ends_with(
            "no difference in the sentences before corpus sentence 2, where a run stopped at --limit\n"
        ),
        "{text}"
    );
    assert!(!text.contains("equivalent") && !text.contains("identical"));
}

#[test]
fn definitions_are_paired_by_name_or_named_and_incomparable_runs_exit_2() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = directory.path().join("two.lx");
    std::fs::write(
        &catalog,
        format!("{BASE}\n{}\n", SUBJECTS.replacen("p :=", "subjects :=", 1)),
    )
    .unwrap();
    let both = lingrep_without_dictionary()
        .args([
            "--json",
            "matches",
            "--input-format",
            "conllu",
            "--patterns",
        ])
        .arg(&catalog)
        .arg(fixture("psycholinguistics.conllu"))
        .output()
        .unwrap();
    assert_success(&both);
    let both_path = directory.path().join("both.jsonl");
    std::fs::write(&both_path, &both.stdout).unwrap();
    let a = matches(directory.path(), "a.jsonl", BASE, &[]);

    // Two definitions of one run.
    let events = diff(
        &both_path,
        &both_path,
        &["--a-pattern", "p", "--b-pattern", "subjects"],
    );
    assert_eq!(
        of(&events, "definition")[0],
        &json!({"a": "p", "b": "subjects", "shared": 2, "a_only": 3, "b_only": 0})
    );

    // Two catalog revisions: `subjects` exists only in the second.
    let events = diff(&a, &both_path, &[]);
    assert_eq!(of(&events, "definition").len(), 1);
    let complete = of(&events, "complete")[0];
    assert_eq!(
        complete["b_only_definitions"],
        json!([{"name": "subjects", "records": 2}])
    );
    assert_eq!(complete["differences_found"], true);

    let retained = matches(directory.path(), "retained.jsonl", BASE, &["--retain", "2"]);
    let output = lingrep_without_dictionary()
        .arg("diff")
        .args([&a, &retained])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--retain"));
}

#[test]
fn definitions_are_compared_under_their_published_names() {
    let directory = tempfile::tempdir().unwrap();
    let run = |name: &str, body: &str| {
        let catalog = directory.path().join(format!("{name}.lx"));
        std::fs::write(
            &catalog,
            format!("output_name: \"x\"\npattern p := {body}.\n"),
        )
        .unwrap();
        let output = lingrep_without_dictionary()
            .args(["--json", "matches", "--input-format", "conllu"])
            .args([
                "--source-metadata",
                if name == "a" {
                    "@sentence.sent_id"
                } else {
                    "@sentence.text"
                },
            ])
            .arg("--patterns")
            .arg(&catalog)
            .arg(fixture("psycholinguistics.conllu"))
            .output()
            .unwrap();
        assert_success(&output);
        let path = directory.path().join(format!("{name}.jsonl"));
        std::fs::write(&path, &output.stdout).unwrap();
        path
    };
    let a = run("a", "[upos=NOUN]");
    let b = run("b", "[upos=VERB]");

    let events = diff(&a, &b, &[]);
    let definition = of(&events, "definition")[0];
    assert_eq!(definition["a"], "x");
    assert_eq!(definition["shared"], 0);
    assert_ne!(definition["a_only"], 0);
    assert_ne!(definition["b_only"], 0);

    // Requested source metadata is not part of the published record.
    let a_again = run("b", "[upos=NOUN]");
    let events = diff(&a, &a_again, &["--level", "record"]);
    assert_eq!(of(&events, "complete")[0]["differences_found"], false);
}

#[test]
fn human_output_shows_terminal_controls_in_stream_text_as_visible_text() {
    let directory = tempfile::tempdir().unwrap();
    let a = matches(directory.path(), "a.jsonl", BASE, &[]);
    let hostile = directory.path().join("hostile.jsonl");
    let stream = std::fs::read_to_string(&a).unwrap();
    let mut seen = false;
    let lines = stream
        .lines()
        .filter(|line| {
            // Drop one match so the other stream reports it as an occurrence.
            let is_match = line.contains("\"type\":\"match\"");
            let drop = is_match && !seen;
            seen |= is_match;
            !drop
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&hostile, lines).unwrap();
    let escaped = stream.replace("\"surface\":\"", "\"surface\":\"\\u001b[2J");
    assert_ne!(escaped, stream);
    std::fs::write(&a, escaped).unwrap();

    let output = lingrep_without_dictionary()
        .arg("diff")
        .args([&a, &hostile])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("a_only"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
}
