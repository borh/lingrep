// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::support::{lingrep_without_dictionary, parse_jsonl};

const ROWS: &str = "a 2 0\nb 1 0\nc 1 1\nd 0 3\ne -1 0\nz 0 0\n";

fn write(directory: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn neighbors(table: &Path, word: &str, extra: &[&str]) -> Vec<Value> {
    let output = lingrep_without_dictionary()
        .args(["--json", "neighbors", "--vectors"])
        .arg(table)
        .args(extra)
        .arg(word)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    assert!(
        events
            .iter()
            .all(|event| event["schema"] == "lingrep.neighbors/v1")
    );
    events
}

fn ranked(events: &[Value]) -> Vec<(u64, String, f64)> {
    events
        .iter()
        .filter(|event| event["type"] == "neighbor")
        .map(|event| {
            let data = &event["data"];
            (
                data["rank"].as_u64().unwrap(),
                data["value"].as_str().unwrap().to_owned(),
                (data["similarity"].as_f64().unwrap() * 1000.0).round() / 1000.0,
            )
        })
        .collect()
}

#[test]
fn neighbors_are_ranked_by_cosine_then_value_and_cut_by_threshold_and_count() {
    let directory = tempfile::tempdir().unwrap();
    let with_header = write(directory.path(), "header.vec", &format!("6 2\n{ROWS}"));
    let without_header = write(directory.path(), "bare.vec", ROWS);
    let a = (1, "a".to_owned(), 1.0);
    let b = (2, "b".to_owned(), 1.0);
    let c = (3, "c".to_owned(), 0.707);
    let d = (4, "d".to_owned(), 0.0);
    let cases: [(&[&str], Vec<(u64, String, f64)>); 4] = [
        (
            &["--min-similarity", "0.7"],
            vec![a.clone(), b.clone(), c.clone()],
        ),
        (&["--top", "2"], vec![a.clone(), b.clone()]),
        (
            &["--min-similarity", "0", "--top", "9"],
            vec![a.clone(), b.clone(), c.clone(), d],
        ),
        (&["--min-similarity", "0.7", "--top", "1"], vec![a.clone()]),
    ];
    for table in [&with_header, &without_header] {
        for (extra, expected) in &cases {
            let events = neighbors(table, "a", extra);
            assert_eq!(&ranked(&events), expected, "{extra:?}");
            assert_eq!(events.first().unwrap()["type"], "run");
            let complete = &events.last().unwrap();
            assert_eq!(complete["type"], "complete");
            assert_eq!(complete["data"]["neighbors"], expected.len());
            assert_eq!(complete["data"]["values"], 6);
            assert_eq!(complete["data"]["values_without_vector"], 1);
        }
    }
}

#[test]
fn the_printed_set_is_a_definition_whose_members_are_the_table_values() {
    let values = [
        "plain", "a.b", "x,y", "(p)", "#h", "b\\s", "n|m", "\"q\"", "/re/", "&k", "key]", "{j}",
        ";s", "研究", "2024", "in", "not", "where", "set",
    ];
    let directory = tempfile::tempdir().unwrap();
    let table = write(
        directory.path(),
        "table.vec",
        &values
            .iter()
            .map(|value| format!("{value} 1 0\n"))
            .collect::<String>(),
    );
    let output = lingrep_without_dictionary()
        .args(["neighbors", "--vectors"])
        .arg(&table)
        .args(["--min-similarity", "0.9", "--name", "NEAR", "plain"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let definition = String::from_utf8(output.stdout).unwrap();
    let catalog = write(
        directory.path(),
        "near.lx",
        &format!("{definition}key: [text]\npattern near := [text in @NEAR].\n"),
    );
    let mut conllu = String::new();
    for (index, value) in values.iter().chain(&["other"]).enumerate() {
        let (head, relation) = if index == 0 { (0, "root") } else { (1, "dep") };
        conllu.push_str(&format!(
            "{}\t{value}\t{value}\tX\t_\t_\t{head}\t{relation}\t_\t_\n",
            index + 1
        ));
    }
    conllu.push('\n');
    let corpus = write(directory.path(), "corpus.conllu", &conllu);
    let output = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(&catalog)
        .arg(&corpus)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{definition}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut matched = parse_jsonl(&output.stdout)
        .iter()
        .filter(|event| event["type"] == "summary")
        .map(|event| {
            event["data"]["group"]["fields"][0]["value"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    matched.sort();
    let mut expected = values.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(matched, expected, "{definition}");
}

#[test]
fn unusable_tables_words_and_names_are_reported_without_output() {
    let directory = tempfile::tempdir().unwrap();
    let table = write(directory.path(), "table.vec", ROWS);
    let ragged = write(directory.path(), "ragged.vec", "a 1 0\nb 1\n");
    let cases: [(&Path, &[&str], i32, &str); 6] = [
        (
            &table,
            &["--top", "3", "missing"],
            2,
            "`missing` has no vector",
        ),
        (&table, &["--top", "3", "z"], 2, "`z` has no vector"),
        (&ragged, &["--top", "3", "a"], 1, "line 2"),
        (&table, &["a"], 2, "--min-similarity"),
        (&table, &["--top", "3", "--name", "1x", "a"], 2, "--name"),
        (
            &table,
            &["--min-similarity", "1.5", "a"],
            2,
            "--min-similarity",
        ),
    ];
    for (table, extra, code, message) in cases {
        let output = lingrep_without_dictionary()
            .args(["neighbors", "--vectors"])
            .arg(table)
            .args(extra)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{extra:?}: {stderr}");
        assert!(stderr.contains(message), "{extra:?}: {stderr}");
        assert!(output.stdout.is_empty(), "{extra:?}");
    }
}
