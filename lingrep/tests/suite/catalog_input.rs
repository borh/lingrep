// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::unwrap_used)]

use crate::support::{fixture, lingrep_without_dictionary, parse_jsonl};

fn successful_check(args: &[&str]) -> serde_json::Value {
    let output = lingrep_without_dictionary().args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout).remove(0)
}

#[test]
fn file_and_inline_catalogs_keep_source_labels_fingerprints_and_authored_order() {
    let pattern_path = fixture("one.lx");
    let first = lingrep_without_dictionary()
        .args(["check", "--json", "--patterns"])
        .arg(&pattern_path)
        .output()
        .unwrap();
    let second = lingrep_without_dictionary()
        .args(["check", "--json", "--patterns"])
        .arg(&pattern_path)
        .output()
        .unwrap();
    assert!(first.status.success());
    assert!(second.status.success());
    let first_records = parse_jsonl(&first.stdout);
    let second_records = parse_jsonl(&second.stdout);
    let file = &first_records[0];
    let file_again = &second_records[0];
    assert_eq!(file["schema"], "lingrep.check/v1");
    assert_eq!(file["data"]["definitions"][0]["name"], "one");
    assert_eq!(file["data"]["definitions"][0]["catalog_ordinal"], 0);
    assert!(
        file["data"]["definitions"][0]["source_file"]
            .as_str()
            .unwrap()
            .ends_with("tests/fixtures/one.lx")
    );
    assert_eq!(
        file["data"]["catalog_fingerprint"],
        file_again["data"]["catalog_fingerprint"]
    );

    let inline = successful_check(&["check", "--json", "--pattern", "[]."]);
    assert_eq!(inline["data"]["definitions"][0]["name"], "inline");
    assert_eq!(inline["data"]["definitions"][0]["source_file"], "inline.lx");
    assert_eq!(inline["data"]["definitions"][0]["catalog_ordinal"], 0);
}

#[test]
fn compiler_error_keeps_catalog_stage_code_and_source_location() {
    let output = lingrep_without_dictionary()
        .args(["check", "--json", "--pattern", "[upos=]."])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let records = parse_jsonl(&output.stderr);
    let diagnostic = &records[0];
    assert_eq!(diagnostic["schema"], "lingrep.diagnostic/v1");
    assert_eq!(diagnostic["data"]["stage"], "catalog");
    assert_eq!(diagnostic["data"]["code"], "Syntax");
    assert!(diagnostic["data"]["location"]["source_span"].is_object());
}

#[test]
fn explicit_example_verification_reports_claims_identity_and_policy_failures() {
    use lingrep_core::testing::MiniDictionary;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("claims.lx");
    let dictionary = MiniDictionary::new();
    for (label, catalog, checked, failures, post_failures) in [
        (
            "passing pair",
            "examples: [\"猫\"]\ncounterexamples: [\"鳥\"]\np := [text=猫].",
            2,
            0,
            0,
        ),
        (
            "positive miss",
            "examples: [\"鳥\"]\np := [text=猫].",
            1,
            1,
            0,
        ),
        (
            "negative hit",
            "counterexamples: [\"猫\"]\np := [text=猫].",
            1,
            1,
            0,
        ),
        ("no claims", "p := [text=猫].", 0, 0, 0),
        ("malformed", "examples: [7]\np := [text=猫].", 0, 1, 0),
        (
            "policy suppresses",
            "examples: [\"猫鳥\"]\np := [text=猫].\npolicy\n admit @p terminal: {exact: true, anchor: sentence_end}.\nend",
            1,
            0,
            1,
        ),
    ] {
        std::fs::write(&path, catalog).unwrap();
        let output = lingrep_without_dictionary()
            .args(["--json", "check", "--verify-examples", "--dict-path"])
            .arg(dictionary.path())
            .arg("--patterns")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(failures + post_failures != 0)),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = &parse_jsonl(&output.stdout)[0]["data"]["examples"];
        assert_eq!(report["checked"], checked, "{label}");
        assert_eq!(
            report["failures"].as_array().unwrap().len(),
            failures,
            "{label}"
        );
        assert_eq!(
            report["post_policy_failures"].as_array().unwrap().len(),
            post_failures,
            "{label}"
        );
        assert_eq!(report["analyzer"]["family"], "vibrato");
        assert_eq!(report["analyzer"]["trust"], "locally-executed");
        assert_eq!(report["resource_identity"].as_str().unwrap().len(), 64);
        assert_eq!(
            report["analyzer"]["semantic_identity"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    }
}

#[test]
fn unavailable_analysis_or_analysis_failure_cannot_pass_a_counterexample() {
    use lingrep_core::testing::MiniDictionary;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("claims.lx");
    let dictionary = MiniDictionary::new();
    std::fs::write(
        &path,
        "counterexamples: [\"猫\"]\np := [unknown.attribute=x].",
    )
    .unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "check", "--verify-examples", "--dict-path"])
        .arg(dictionary.path())
        .arg("--patterns")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    std::fs::write(&path, "counterexamples: [\"猫鳥\"]\np := [text=猫].").unwrap();
    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "check",
            "--verify-examples",
            "--max-sentence-bytes",
            "3",
            "--dict-path",
        ])
        .arg(dictionary.path())
        .arg("--patterns")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let plain = lingrep_without_dictionary()
        .args(["--json", "check", "--patterns"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(
        parse_jsonl(&plain.stdout)[0]["data"]
            .get("examples")
            .is_none()
    );
}

#[test]
fn catalog_comparison_ignores_spelling_and_reports_transitive_and_one_sided_definitions() {
    let directory = tempfile::tempdir().unwrap();
    let before = directory.path().join("before.lx");
    let after = directory.path().join("after.lx");
    std::fs::write(&before, "fragment f := [text in {猫, 鳥}].\np := @f.\nstable := [text=猫 & lemma=猫].\nremoved := [].").unwrap();
    std::fs::write(&after, "# reordering is not meaning\nstable := [lemma=猫 & text=猫].\nfragment f := [text in {鳥, 犬}].\np := @f.\nadded := [].").unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "check", "--patterns"])
        .arg(&before)
        .arg("--compare-with")
        .arg(&after)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = parse_jsonl(&output.stdout).remove(0);
    let definitions = report["data"]["comparison"].as_array().unwrap();
    let definition = |name: &str| definitions.iter().find(|d| d["name"] == name).unwrap();
    let p = &definition("p")["revisions"];
    assert_ne!(p[0]["body_group"], p[1]["body_group"]);
    assert!(!p[0]["expanded"].as_str().unwrap().contains("@f"));
    let stable = &definition("stable")["revisions"];
    assert_eq!(stable[0]["body_group"], stable[1]["body_group"]);
    assert_eq!(
        definition("added")["revisions"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        definition("removed")["revisions"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn catalog_diff_compares_published_assignments_over_both_catalogs_examples() {
    let directory = tempfile::tempdir().unwrap();
    let before = directory.path().join("before.lx");
    let after = directory.path().join("after.lx");
    std::fs::write(
        &before,
        "examples: [\"猫\", \"鳥\"]\np := (?<n> [text=猫]).",
    )
    .unwrap();
    std::fs::write(
        &after,
        "examples: [\"犬\"]\np := (?<n> [text in {猫, 鳥}]).",
    )
    .unwrap();
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let output = std::process::Command::new("python3")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/catalog_diff.py"))
        .arg(&before)
        .arg(&after)
        .arg("--ling")
        .arg(crate::support::lingrep_binary_path())
        .arg("--examples")
        .arg("--dict-path")
        .arg(dictionary.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["definitions"][0]["status"], "changed");
    assert_eq!(report["example_sentences"], 3);
    assert_eq!(report["behavior"]["only_before"], serde_json::json!({}));
    assert_eq!(report["behavior"]["only_after"]["p"], 1);
    for scan in report["behavior"]["runs"].as_array().unwrap() {
        assert_eq!(scan["complete"]["coverage"], "full");
        assert_eq!(scan["complete"]["counters"]["sentences_admitted"], 3);
    }
}

#[test]
fn catalog_diff_reports_prepared_corpus_differences_and_refuses_failed_runs() {
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
    let directory = tempfile::tempdir().unwrap();
    let before = directory.path().join("before.lx");
    let after = directory.path().join("after.lx");
    let artifact = directory.path().join("prepared.lingcorpus");
    std::fs::write(&before, "p := (?<old> [text=猫]).").unwrap();
    std::fs::write(&after, "p := (?<new> [text=猫]).").unwrap();
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, "猫".into());
    tokio::runtime::Runtime::new().unwrap().block_on(
        crate::support::build_single_sentence_artifact(&artifact, "猫", &[token]),
    );
    let run = |source: &std::path::Path| {
        std::process::Command::new("python3")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/catalog_diff.py"),
            )
            .arg(&before)
            .arg(&after)
            .arg("--ling")
            .arg(crate::support::lingrep_binary_path())
            .arg("--corpus")
            .arg(source)
            .output()
            .unwrap()
    };
    let output = run(&artifact);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["behavior"]["only_before"]["p"], 1);
    assert_eq!(report["behavior"]["only_after"]["p"], 1);
    let output = run(&directory.path().join("missing.lingcorpus"));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
