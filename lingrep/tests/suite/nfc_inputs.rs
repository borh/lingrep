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
//! Canonically equivalent Unicode spellings in patterns and source names.

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

/// ガラス.txt with its name in NFC and in NFD.
const NAME_NFC: &str = "ガラス.txt";
const NAME_NFD: &str = "\u{30ab}\u{3099}\u{30e9}\u{30b9}.txt";

/// The match records and the `complete` record's identities of a run of
/// `w := [].` over `corpus`.
fn token_run(corpus: &Path, format: &str) -> (Vec<Value>, Value, Value) {
    let output = lingrep()
        .args([
            "--json",
            "matches",
            "--input-format",
            format,
            "--pattern",
            "w := [].",
        ])
        .arg(corpus)
        .output()
        .unwrap();
    let complete = events(&output, "complete").remove(0);
    (
        events(&output, "match"),
        complete["source_identity"].clone(),
        complete["analysis_identity"].clone(),
    )
}

fn soranoha(path: &Path, member: &str) {
    use std::io::Write;
    let catalog = crate::support::SORANOHA_CATALOG.replace("kumo.txt", NAME_NFC);
    let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (entry, content) in [
        ("catalog.csv", catalog.as_str()),
        (member, "ガラスが割れた。\n"),
        ("hari.txt", "犬が走る。\n"),
    ] {
        writer
            .start_file(entry, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

#[test]
fn a_source_named_in_nfd_has_the_identity_and_records_of_its_nfc_name() {
    let directory = TempDir::new().unwrap();
    let text = "ガラスが割れた。\n";
    for (format, nfc, nfd) in [
        (
            "plain",
            write(directory.path(), "plain-nfc", NAME_NFC, text)
                .parent()
                .unwrap()
                .to_owned(),
            write(directory.path(), "plain-nfd", NAME_NFD, text)
                .parent()
                .unwrap()
                .to_owned(),
        ),
        (
            "markdown",
            write(
                directory.path(),
                "md-nfc",
                &NAME_NFC.replace("txt", "md"),
                text,
            )
            .parent()
            .unwrap()
            .to_owned(),
            write(
                directory.path(),
                "md-nfd",
                &NAME_NFD.replace("txt", "md"),
                text,
            )
            .parent()
            .unwrap()
            .to_owned(),
        ),
        (
            "jsonl",
            write(
                directory.path(),
                "jsonl-nfc",
                &NAME_NFC.replace("txt", "jsonl"),
                "{\"text\":\"ガラスが割れた。\"}\n",
            ),
            write(
                directory.path(),
                "jsonl-nfd",
                &NAME_NFD.replace("txt", "jsonl"),
                "{\"text\":\"ガラスが割れた。\"}\n",
            ),
        ),
        (
            "soranoha",
            directory.path().join("nfc.zip"),
            directory.path().join("nfd.zip"),
        ),
    ] {
        if format == "soranoha" {
            soranoha(&nfc, NAME_NFC);
            soranoha(&nfd, NAME_NFD);
        }
        let expected = token_run(&nfc, format);
        assert!(!expected.0.is_empty(), "{format}");
        let observed = token_run(&nfd, format);
        if format == "soranoha" {
            // An archive's source identity is the digest of its bytes, and
            // the two archives differ in the bytes of a member name.
            assert_eq!(observed.0, expected.0, "{format}");
        } else {
            assert_eq!(observed, expected, "{format}");
        }
    }
}

#[test]
fn source_names_that_normalize_to_the_same_name_are_both_reported() {
    let directory = TempDir::new().unwrap();
    let corpus = write(directory.path(), "both", NAME_NFC, "ガラスが割れた。\n");
    write(directory.path(), "both", NAME_NFD, "ガラスが割れた。\n");
    let corpus = corpus.parent().unwrap();
    // APFS and HFS+ store canonically equivalent names as one entry.
    if std::fs::read_dir(corpus).unwrap().count() == 2 {
        let output = lingrep()
            .args([
                "matches",
                "--input-format",
                "plain",
                "--pattern",
                "w := [].",
            ])
            .arg(corpus)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(NAME_NFC) && stderr.contains(NAME_NFD) && stderr.contains("NFC"),
            "{stderr}"
        );
    }

    let archive = directory.path().join("both.zip");
    let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    for entry in ["catalog.csv", NAME_NFC, NAME_NFD] {
        writer
            .start_file(entry, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut writer, crate::support::SORANOHA_CATALOG.as_bytes())
            .unwrap();
    }
    writer.finish().unwrap();
    let output = lingrep()
        .args([
            "matches",
            "--input-format",
            "soranoha",
            "--pattern",
            "w := [].",
        ])
        .arg(&archive)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(NAME_NFC) && stderr.contains(NAME_NFD) && stderr.contains("NFC"),
        "{stderr}"
    );
}
