//! Which corpus a query reads, when the caller did not name one.

// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use crate::support::{assert_success, lingrep};
use lingrep_core::testing::MiniDictionary;
use std::fs;

/// Explicitly cached and default-direct execution answer the same question.
///
/// Records are compared, not counts — the two paths reach
/// publication through different sources, and a difference between them would
/// be a difference in evidence, not in arithmetic.
#[test]
fn explicit_cache_and_default_direct_publish_the_same_records() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.md");
    fs::write(&root, "猫。\n犬。\n猫と犬。\n").unwrap();
    let patterns = directory.path().join("patterns.lx");
    fs::write(&patterns, "pattern animal := [text in {猫, 犬}].\n").unwrap();

    let run = |mode: &[&str]| {
        let output = lingrep()
            .args(["matches", "--json", "--limit", "all", "--patterns"])
            .arg(&patterns)
            .args(["--dict-path"])
            .arg(dictionary.path())
            .args(mode)
            .arg(&root)
            .output()
            .unwrap();
        assert_success(&output);
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.contains("\"type\":\"match\""))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };

    let direct = run(&[]);
    assert!(!direct.is_empty(), "the fixture matches something");
    assert!(
        artifacts_beside(&root).is_empty(),
        "the default neither reads nor publishes an artifact"
    );

    let prepared = run(&["--cache"]);
    assert_eq!(prepared, direct, "preparing does not change the answer");
    let published = artifacts_beside(&root);
    assert_eq!(published.len(), 1, "--cache publishes exactly one artifact");

    let reused = run(&["--cache"]);
    assert_eq!(reused, direct, "reusing does not change the answer");
    assert_eq!(
        artifacts_beside(&root),
        published,
        "reuse publishes nothing further"
    );

    // And `corpus build` recognizes what the query published as its own, which
    // is what makes them one artifact rather than two spellings of one.
    let build = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&build);
    assert_eq!(
        artifacts_beside(&root),
        published,
        "corpus build reuses the artifact a default query published"
    );
}

#[test]
fn tokenize_populates_the_explicit_cache_and_attributes_reuses_it() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus.txt");
    fs::write(&root, "猫。\n犬。\n").unwrap();

    let tokenized = lingrep()
        .args(["tokenize", "--json", "--cache", "--dict-path"])
        .arg(dictionary.path())
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&tokenized);
    let published = artifacts_beside(&root);
    assert_eq!(published.len(), 1, "tokenize populates the shared cache");

    let attributes = lingrep()
        .args([
            "coverage",
            "--json",
            "--cache",
            "--progress",
            "always",
            "--dict-path",
        ])
        .arg(dictionary.path())
        .arg(&root)
        .output()
        .unwrap();
    assert_success(&attributes);
    assert_eq!(artifacts_beside(&root), published);
    assert!(
        String::from_utf8_lossy(&attributes.stderr).contains("reusing the prepared artifact"),
        "attributes should report reuse: {}",
        String::from_utf8_lossy(&attributes.stderr)
    );
}

#[test]
fn cache_rejects_stdin_before_reading_it() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args(["tokenize", "--cache", "--dict-path"])
        .arg(dictionary.path())
        .arg("-")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("stdin cannot be reopened"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn artifacts_beside(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.extension()
                .is_some_and(|suffix| suffix == "lingcorpus")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}
