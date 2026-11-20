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
//! `text` literals the key fold changes, over a corpus whose `text` is the
//! folded key and over one whose `text` is not.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use crate::support::{lingrep, lingrep_without_dictionary, parse_jsonl};

/// Each written spelling, the plain spelling it folds to, and the sentence
/// Vibrato analyzes it in as one token.
const SPELLINGS: [(&str, &str); 3] = [("ｶﾞﾗｽ", "ガラス"), ("ＮＨＫ", "NHK"), ("長～い", "長〜い")];
const TEXT: &str = "ｶﾞﾗｽが割れた。ＮＨＫを見た。長～い道だ。\n";

fn write(directory: &Path, name: &str, content: &str) -> PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

fn events(output: &Output, kind: &str) -> Vec<Value> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|event| event["type"] == kind)
        .map(|event| event["data"].clone())
        .collect()
}

fn notices(output: &Output, code: &str) -> Vec<String> {
    parse_jsonl(&output.stderr)
        .into_iter()
        .filter(|event| event["data"]["code"] == code)
        .map(|event| event["data"]["message"].as_str().unwrap().to_owned())
        .collect()
}

fn run(command: &str, catalog: &Path, corpus: &Path, format: &str) -> Output {
    lingrep()
        .args(["--json", command, "--input-format", format, "--patterns"])
        .arg(catalog)
        .arg(corpus)
        .output()
        .unwrap()
}

#[test]
fn a_folded_text_literal_finds_the_plain_spellings_records_and_the_run_names_it() {
    let directory = TempDir::new().unwrap();
    let corpus = write(directory.path(), "corpus.txt", TEXT);
    for (written, plain) in SPELLINGS {
        let catalog = |name: &str, value: &str| {
            write(
                directory.path(),
                name,
                &format!("g := (?<w> [text={value}]).\n"),
            )
        };
        let plain_catalog = catalog("plain.lx", plain);
        let written_catalog = catalog("written.lx", written);
        let expected = events(&run("matches", &plain_catalog, &corpus, "plain"), "match");
        assert_eq!(expected.len(), 1, "{plain}");
        let output = run("matches", &written_catalog, &corpus, "plain");
        assert_eq!(events(&output, "match"), expected, "{written}");
        let messages = notices(&output, "literal_folded");
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("`g`")
                && messages[0].contains(written)
                && messages[0].contains(plain),
            "{}",
            messages[0]
        );
        assert!(
            notices(
                &run("matches", &plain_catalog, &corpus, "plain"),
                "literal_folded"
            )
            .is_empty()
        );

        let summary = run("summarize", &written_catalog, &corpus, "plain");
        let expected = events(
            &run("summarize", &plain_catalog, &corpus, "plain"),
            "summary",
        );
        assert_eq!(expected.len(), 1, "{plain}");
        assert_eq!(events(&summary, "summary"), expected, "{written}");
        assert_eq!(notices(&summary, "literal_folded").len(), 1, "{written}");
    }
}

/// CoNLL-U publishes FORM as `surface` and its folded key as `text`, so a
/// folded literal finds both spellings and the run reports no unfolded `text`.
#[test]
fn over_conllu_a_folded_text_literal_finds_every_spelling_of_form() {
    let directory = TempDir::new().unwrap();
    let corpus = write(
        directory.path(),
        "corpus.conllu",
        "# text = ＮＨＫとNHK\n1\tＮＨＫ\tＮＨＫ\tPROPN\t_\t_\t0\troot\t_\t_\n\
         2\tと\tと\tADP\t_\t_\t1\tcase\t_\t_\n3\tNHK\tNHK\tPROPN\t_\t_\t1\tconj\t_\t_\n\n",
    );
    let written = write(
        directory.path(),
        "written.lx",
        "g := (?<w> [text=ＮＨＫ]).\n",
    );
    let output = run("matches", &written, &corpus, "conllu");
    let spans: Vec<Value> = events(&output, "match")
        .into_iter()
        .map(|record| record["span"].clone())
        .collect();
    assert_eq!(
        spans,
        [
            serde_json::json!({"start": 0, "end": 1}),
            serde_json::json!({"start": 2, "end": 3})
        ]
    );
    assert!(notices(&output, "text_unfolded").is_empty());

    let exact = write(directory.path(), "exact.lx", "g := [surface=ＮＨＫ].\n");
    let spans: Vec<Value> = events(&run("matches", &exact, &corpus, "conllu"), "match")
        .into_iter()
        .map(|record| record["span"].clone())
        .collect();
    assert_eq!(spans, [serde_json::json!({"start": 0, "end": 1})]);

    let every = write(directory.path(), "every.lx", "t := (?<w> []).\n");
    let tokens = lingrep()
        .args([
            "--json",
            "matches",
            "--input-format",
            "conllu",
            "--publish-attribute",
            "surface",
            "--patterns",
        ])
        .arg(&every)
        .arg(&corpus)
        .output()
        .unwrap();
    let rows: Vec<Value> = events(&tokens, "match")
        .into_iter()
        .map(|record| record["tokens"][0]["attributes"].clone())
        .collect();
    assert_eq!(
        rows,
        [("ＮＨＫ", "NHK"), ("と", "と"), ("NHK", "NHK")]
            .map(|(surface, text)| serde_json::json!({"surface": surface, "text": text}))
    );
}

/// Sudachi reads the folded view of the source, publishes the exact source
/// as `surface`, and its `text` is the folded key the literals compile to.
#[test]
#[ignore = "requires the fixed Sudachi core dictionary"]
fn over_sudachi_a_folded_text_literal_finds_the_written_spelling() {
    let directory = TempDir::new().unwrap();
    let corpus = write(
        directory.path(),
        "corpus.txt",
        "ｶﾞﾗｽが割れた。ＮＨＫを見た。\n",
    );
    let sudachi = |command: &str, catalog: &Path| {
        lingrep()
            .args(["--json", command, "--analyzer", "sudachi", "--sudachi-dict"])
            .arg(std::env::var_os("LINGREP_SUDACHI_DICT_CORE").unwrap())
            .arg("--patterns")
            .arg(catalog)
            .arg(&corpus)
            .output()
            .unwrap()
    };
    for (written, plain, surface, char_span) in [
        ("ｶﾞﾗｽ", "ガラス", "ｶﾞﾗｽ", [0, 4]),
        ("ＮＨＫ", "NHK", "ＮＨＫ", [0, 3]),
    ] {
        let catalog = |name: &str, value: &str| {
            write(directory.path(), name, &format!("g := [text={value}].\n"))
        };
        let output = sudachi("matches", &catalog("written.lx", written));
        let records = events(&output, "match");
        assert_eq!(
            events(&sudachi("matches", &catalog("plain.lx", plain)), "match"),
            records,
            "{written}"
        );
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0]["surface"], surface, "{:?}", records[0]);
        assert_eq!(
            records[0]["char_span"],
            serde_json::json!({"start": char_span[0], "end": char_span[1]}),
            "{:?}",
            records[0]
        );
        assert!(notices(&output, "text_unfolded").is_empty());
    }
}

#[test]
fn a_text_regex_the_fold_changes_is_used_as_written_and_named() {
    let directory = TempDir::new().unwrap();
    let corpus = write(directory.path(), "corpus.txt", TEXT);
    let catalog = write(directory.path(), "g.lx", "g := [text~/ＮＨＫ/].\n");
    let output = run("matches", &catalog, &corpus, "plain");
    assert_eq!(events(&output, "match"), Vec::<Value>::new());
    let messages = notices(&output, "regex_not_folded");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("`g`") && messages[0].contains("/NHK/"),
        "{}",
        messages[0]
    );
}

#[test]
fn check_names_the_fold_and_keeps_every_catalog_fingerprint() {
    let directory = TempDir::new().unwrap();
    let check = |source: &str| {
        let catalog = write(directory.path(), "g.lx", source);
        let output = lingrep_without_dictionary()
            .args(["--json", "check", "--patterns"])
            .arg(&catalog)
            .output()
            .unwrap();
        events(&output, "check").remove(0)
    };
    // The fingerprints ling 0.8.2 reports for these catalogs. A stored count
    // for the folded one is invalidated by `MATCHER_SEMANTICS_IDENTITY`.
    for (source, fingerprint, warning) in [
        (
            "g := [text=ＮＨＫ].\n",
            "e02f900339e3236fb05956285abd7d97e35f25208cbfb14ea5d9495f0a5e72da",
            Some("FoldedLiteral"),
        ),
        (
            "g := [text=NHK].\n",
            "b1f8e56d1f8a97b60a40a8a435d5affdfd0c4101b9aa9046bd3073457731d6c2",
            None,
        ),
    ] {
        let report = check(source);
        assert_eq!(report["catalog_fingerprint"], fingerprint, "{source}");
        let codes: Vec<&Value> = report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|warning| &warning["code"])
            .collect();
        assert_eq!(
            codes,
            warning.map(Value::from).iter().collect::<Vec<_>>(),
            "{source}"
        );
    }
    assert_eq!(
        check("g := [text~/[ｶﾞ]/].\n")["warnings"][0]["code"],
        "RegexNotFolded"
    );
}
