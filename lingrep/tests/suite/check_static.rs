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
//! `ling check` findings that hold for every token sequence.

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::support::{lingrep_without_dictionary, parse_jsonl};

fn check(source: &str, flags: &[&str]) -> Value {
    let directory = TempDir::new().unwrap();
    let catalog = directory.path().join("c.lx");
    std::fs::write(&catalog, source).unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "check", "--patterns"])
        .arg(&catalog)
        .args(flags)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
        .into_iter()
        .find(|event| event["type"] == "check")
        .expect("a check event")["data"]
        .clone()
}

#[test]
fn check_warns_about_a_test_no_token_satisfies() {
    let source = "a := [text=x] [upos=VERB & upos=NOUN].\n";
    let report = check(source, &[]);
    let start = source.find("[upos").unwrap();
    assert_eq!(
        report["warnings"],
        json!([
            {
                "code": "unsatisfiable_test",
                "message": "`a`: no token satisfies [upos=VERB & upos=NOUN]",
                "location": {
                    "source_file": report["warnings"][0]["location"]["source_file"],
                    "source_span": {"start": start, "end": source.len() - 2},
                },
            },
            {
                "code": "unsatisfiable_definition",
                "message": "`a` matches no token sequence",
                "location": {
                    "source_file": report["warnings"][1]["location"]["source_file"],
                    "source_span": report["warnings"][1]["location"]["source_span"],
                },
            },
        ])
    );
    assert_eq!(report.get("containment"), None);
}

#[test]
fn check_lists_contained_definitions_on_request() {
    let report = check(
        concat!(
            "specific := [lemma=こと] [text=も].\n",
            "general := [lemma=こと] [text in {は, も}].\n",
            "same := [lemma=こと] ([text=は] | [text=も]).\n",
            "anchored := ^ [text=も].\n",
        ),
        &["--containment"],
    );
    assert_eq!(
        report["containment"],
        json!({
            "pairs": [
                {"contained": "general", "containing": "same", "equivalent": true},
                {"contained": "specific", "containing": "general", "equivalent": false},
                {"contained": "specific", "containing": "same", "equivalent": false},
            ],
            "not_decided": [
                {"definition": "anchored", "construct": "a position assertion"},
            ],
        })
    );
}
