// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::process::{Command, Output, Stdio};

use lingrep_core::testing::MiniDictionary;
use serde_json::Value;

use crate::support::{lingrep, parse_jsonl};

fn script() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("examples/gravity.py")
}

fn gravity(input: &[u8], args: &[&str]) -> Output {
    let mut child = Command::new("python3")
        .arg(script())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input).unwrap();
    child.wait_with_output().unwrap()
}

fn synthetic_matches(sentences: &[&[&str]]) -> Vec<u8> {
    let schema = "lingrep.query/v2";
    let mut events = vec![serde_json::json!({
        "schema": schema,
        "type": "run",
        "data": {"mode": "matches"}
    })];
    for (ordinal, sentence) in sentences.iter().enumerate() {
        let tokens = sentence
            .iter()
            .enumerate()
            .map(|(index, text)| serde_json::json!({"index": index, "attributes": {"text": text}}))
            .collect::<Vec<_>>();
        events.push(serde_json::json!({
            "schema": schema,
            "type": "match",
            "data": {"corpus_sentence_ordinal": ordinal, "tokens": tokens}
        }));
    }
    events.push(serde_json::json!({
        "schema": schema,
        "type": "complete",
        "data": {"coverage": "full", "termination": "eof"}
    }));
    events
        .into_iter()
        .flat_map(|event| format!("{event}\n").into_bytes())
        .collect()
}

fn score(row: &Value) -> f64 {
    row["score"].as_f64().unwrap()
}

#[test]
fn pattern_match_spans_feed_adjacent_pair_gravity() {
    let dictionary = MiniDictionary::new();
    let matches = lingrep()
        .args(["--json", "matches", "--dict-path"])
        .arg(dictionary.path())
        .args(["--pattern", "[text=猫] | [text=犬].", "--context", "0", "-"])
        .write_stdin("猫と犬。\n猫と犬。\n")
        .output()
        .unwrap();
    assert!(
        matches.status.success(),
        "{}",
        String::from_utf8_lossy(&matches.stderr)
    );

    let output = gravity(&matches.stdout, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = parse_jsonl(&output.stdout);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["left"], "猫");
    assert_eq!(rows[0]["right"], "犬");
    assert_eq!(rows[0]["count"], 2);
    assert!((score(&rows[0]) - 4.0f64.ln()).abs() < f64::EPSILON);
}

#[test]
fn gravity_uses_the_natural_log_formula_and_lexical_pair_order() {
    let output = gravity(&synthetic_matches(&[&["a", "b", "c"], &["a", "b"]]), &[]);
    assert!(output.status.success());
    let rows = parse_jsonl(&output.stdout);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0]["left"].as_str(), rows[0]["right"].as_str()),
        (Some("a"), Some("b"))
    );
    assert_eq!(rows[0]["count"], 2);
    assert!((score(&rows[0]) - (8.0f64 / 3.0).ln()).abs() < f64::EPSILON);
    assert_eq!(
        (rows[1]["left"].as_str(), rows[1]["right"].as_str()),
        (Some("b"), Some("c"))
    );
    assert_eq!(rows[1]["count"], 1);
    assert!((score(&rows[1]) - (1.0f64 / 3.0).ln()).abs() < f64::EPSILON);
}

#[test]
fn gravity_rejects_an_incomplete_upstream_run_without_output() {
    let output = gravity(
        b"{\"schema\":\"lingrep.query/v2\",\"type\":\"run\",\"data\":{\"mode\":\"matches\"}}\n",
        &[],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("complete event"));
    assert!(output.stdout.is_empty());
}

#[test]
fn gravity_rejects_tokenize_output() {
    let output = gravity(
        b"{\"schema\":\"lingrep.tokenize/v1\",\"type\":\"run\",\"data\":{}}\n",
        &[],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("query/v2"));
    assert!(output.stdout.is_empty());
}

#[test]
fn gravity_rejects_retired_analysis_options() {
    let output = gravity(&synthetic_matches(&[]), &["--threshold", "5.5"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no options"));
    assert!(output.stdout.is_empty());
}
