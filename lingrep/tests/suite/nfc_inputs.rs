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
//! Text in Unicode normalization form D, as macOS file names and text copied
//! from PDFs carry it: ガ is カ followed by U+3099.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use crate::support::{lingrep, lingrep_without_dictionary, parse_jsonl};

/// ガラス in NFD.
const GLASS_NFD: &str = "\u{30ab}\u{3099}\u{30e9}\u{30b9}";
const GLASS_NFC: &str = "ガラス";

fn write(directory: &Path, variant: &str, name: &str, content: &str) -> PathBuf {
    let directory = directory.join(variant);
    std::fs::create_dir_all(&directory).unwrap();
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

fn matches(catalog: &Path, corpus: &Path) -> Output {
    lingrep()
        .args(["--json", "matches", "--patterns"])
        .arg(catalog)
        .arg(corpus)
        .output()
        .unwrap()
}

fn check(catalog: &Path) -> Value {
    let output = lingrep_without_dictionary()
        .args(["--json", "check", "--patterns"])
        .arg(catalog)
        .output()
        .unwrap();
    events(&output, "check").remove(0)
}

#[test]
fn an_nfd_literal_matches_as_its_nfc_form_and_the_run_names_it() {
    let directory = TempDir::new().unwrap();
    let corpus = write(
        directory.path(),
        "corpus",
        "glass.txt",
        "ガラスが割れた。\n",
    );
    let catalogs: [fn(&str) -> String; 3] = [
        |value| format!("g := [text={value}].\n"),
        |value| format!("g := [text in {{{value}, 窓}}].\n"),
        |value| format!("g := {value}.\n"),
    ];
    for (operator, catalog) in catalogs.iter().enumerate() {
        let nfc = write(directory.path(), "nfc", "g.lx", &catalog(GLASS_NFC));
        let nfd = write(directory.path(), "nfd", "g.lx", &catalog(GLASS_NFD));
        let expected = events(&matches(&nfc, &corpus), "match");
        assert_eq!(expected.len(), 1, "{operator}");
        let output = matches(&nfd, &corpus);
        assert_eq!(events(&output, "match"), expected, "{operator}");
        let notices: Vec<Value> = parse_jsonl(&output.stderr)
            .into_iter()
            .filter(|event| event["data"]["code"] == "literal_normalized")
            .collect();
        assert_eq!(notices.len(), 1, "{operator}: {notices:?}");
        let message = notices[0]["data"]["message"].as_str().unwrap();
        assert!(
            message.contains("`g`") && message.contains(GLASS_NFD) && message.contains(GLASS_NFC),
            "{message}"
        );
        assert!(
            !String::from_utf8_lossy(&matches(&nfc, &corpus).stderr).contains("literal_normalized")
        );
    }
}

#[test]
fn check_reports_the_normalized_literal_and_only_a_non_nfc_catalog_changes_fingerprint() {
    let directory = TempDir::new().unwrap();
    let nfc = write(
        directory.path(),
        "nfc",
        "g.lx",
        &format!("g := [text={GLASS_NFC}].\n"),
    );
    let nfd = write(
        directory.path(),
        "nfd",
        "g.lx",
        &format!("g := [text={GLASS_NFD}].\n"),
    );
    let nfc = check(&nfc);
    let nfd = check(&nfd);
    // The fingerprints ling 0.8.1 reports for these two catalogs.
    assert_eq!(
        nfc["catalog_fingerprint"],
        "a9ec0624de41524315cfe069bbf95907d16ad2d53c527abd14d582d39c71edef"
    );
    assert_ne!(
        nfd["catalog_fingerprint"],
        "fa4ef7d64463b604b4ab19d1f461a1388149607ab2a436bac8dbf39a38e3c3a6"
    );
    assert_ne!(nfd["catalog_fingerprint"], nfc["catalog_fingerprint"]);
    assert_eq!(nfc["warnings"], serde_json::json!([]));
    let warnings = nfd["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["code"], "NormalizedLiteral");
    assert_eq!(
        warnings[0]["location"]["source_span"],
        serde_json::json!({"start": 11, "end": 11 + GLASS_NFD.len()})
    );
}

#[test]
fn an_nfd_regex_is_reported_and_left_as_written() {
    let directory = TempDir::new().unwrap();
    let corpus = write(
        directory.path(),
        "corpus",
        "glass.txt",
        "ガラスが割れた。\n",
    );
    let catalog = write(
        directory.path(),
        "nfd",
        "g.lx",
        &format!("g := [text~/{GLASS_NFD}/].\n"),
    );
    let output = matches(&catalog, &corpus);
    assert_eq!(events(&output, "match"), Vec::<Value>::new());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("regex_not_nfc") && stderr.contains("`g`"),
        "{stderr}"
    );
    let warnings = check(&catalog)["warnings"].clone();
    assert_eq!(warnings[0]["code"], "RegexNotNfc", "{warnings}");
}
