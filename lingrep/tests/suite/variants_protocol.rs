// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Contracts for `corpus variants`.
//!
//! Current artifacts report their analyzer differences and bounded totals.

use std::fs;
use std::path::{Path, PathBuf};

use lingrep_core::testing::MiniDictionary;
use lingrep_corpus::{CorpusArtifact, TrustClass, canonical_artifact_path};

use crate::support::{lingrep, lingrep_without_dictionary, parse_jsonl};

fn artifact_directories(parent: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lingcorpus"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn build(dictionary: &MiniDictionary, source: &Path) -> PathBuf {
    let output = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    artifact_directories(source.parent().unwrap())
        .into_iter()
        .next()
        .unwrap()
}

/// Copies one current realization to another canonical address.
fn duplicate_realization(source: &Path, current: &Path) -> PathBuf {
    let artifact = CorpusArtifact::open(current).unwrap();
    let copy = canonical_artifact_path(
        &source.with_file_name("other.md"),
        artifact.analysis_identity(),
        TrustClass::LocallyExecuted,
    );
    copy_tree(current, &copy);
    copy
}

fn variants_json(target: &Path) -> Vec<serde_json::Value> {
    let output = lingrep_without_dictionary()
        .args(["corpus", "variants", "--json"])
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    parse_jsonl(&output.stdout)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Two artifacts over one directory tree, read under different readers, are a
/// **reader** difference with no analyzer difference.
///
/// This is the case `--input-format` creates and the BCCWJ reader will make
/// routine: one tree read two ways is two correct artifacts. Without reader in
/// the dimension list `variants` would show two siblings and be unable to say
/// what distinguished them — the reader is not in the analyzer record, so it
/// would report nothing at all.
#[test]
fn two_readers_over_one_tree_are_a_reader_difference_with_no_analyzer_difference() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "猫。\n").unwrap();

    for sections in ["references", "references,appendix"] {
        let output = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--md-exclude-sections", sections])
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let artifacts = artifact_directories(directory.path());
    assert_eq!(
        artifacts.len(),
        2,
        "two readers over one tree are two artifacts"
    );

    let records = variants_json(&artifacts[0]);
    let other = records
        .iter()
        .filter(|record| record["type"] == "variant")
        .find(|record| record["data"]["path"] != artifacts[0].display().to_string())
        .expect("the sibling is reported");
    assert_eq!(
        other["data"]["differences"].as_array().unwrap(),
        &vec![serde_json::json!("reader")],
        "reader is the only dimension that differs"
    );
    assert_eq!(other["data"]["same_source"], true);
}

/// Given an artifact, differences are relative to it, and the reference itself
/// reports no differences.
#[test]
fn an_artifact_argument_makes_every_difference_relative_to_it() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    let artifact = build(&dictionary, &source);

    let records = variants_json(&artifact);
    let run = records.iter().find(|r| r["type"] == "run").unwrap();
    assert_eq!(run["data"]["reference"]["kind"], "artifact");
    assert_eq!(
        run["data"]["reference"]["path"],
        artifact.display().to_string()
    );

    let reference = records.iter().find(|r| r["type"] == "variant").unwrap();
    assert!(
        reference["data"]["differences"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the reference differs from itself in nothing"
    );
}

/// Given a source, there is no reference; realizations are reported on their
/// own terms.
#[test]
fn a_source_argument_reports_realizations_without_a_reference() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    build(&dictionary, &source);

    let records = variants_json(&source);
    let run = records.iter().find(|r| r["type"] == "run").unwrap();
    assert_eq!(run["data"]["reference"]["kind"], "source");
    let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
    assert_eq!(complete["data"]["realizations"], 1);
    assert_eq!(complete["data"]["reported"], 1);
}

/// A **directory** source root reports its realizations, which sit beside it.
///
/// `canonical_artifact_path` names every realization with `with_file_name`, so
/// a directory root's artifacts land in the root's *parent* — never inside it.
/// The scan reads the parent, not the target itself, which would answer zero
/// for every directory root — the same answer "none exist" gives.
///
/// This is not a corner. A BCCWJ distribution is a directory, so is every
/// Markdown corpus, and every source-argument test above happens to use a file
/// — which is how the gap survived. The assertion that the root stays empty is
/// half the test: it pins *why* scanning the root was wrong, not just that a
/// realization is now found.
#[test]
fn a_directory_source_root_reports_the_realizations_beside_it() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("a.md"), "猫。\n").unwrap();

    // Two readers over one tree: two correct realizations of one directory,
    // which is what `--input-format` and the BCCWJ reader make routine.
    for sections in ["references", "references,appendix"] {
        let output = lingrep()
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .args(["--md-exclude-sections", sections])
            .arg(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert_eq!(
        artifact_directories(directory.path()).len(),
        2,
        "a directory root's realizations sit beside it"
    );
    assert!(
        artifact_directories(&root).is_empty(),
        "and never inside it, which is what the old scan read"
    );

    let records = variants_json(&root);
    let run = records.iter().find(|r| r["type"] == "run").unwrap();
    assert_eq!(run["data"]["reference"]["kind"], "source");
    assert_eq!(
        run["data"]["directory"],
        directory.path().display().to_string(),
        "the scanned directory is the root's parent"
    );

    let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
    assert_eq!(complete["data"]["realizations"], 2);
    assert_eq!(complete["data"]["reported"], 2);
}

/// A capped run reports the complete count. Showing three of forty and saying
/// "three" would be a different, wrong answer.
#[test]
fn a_limit_caps_detail_and_never_the_reported_total() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    let first = build(&dictionary, &source);
    duplicate_realization(&source, &first);

    let output = lingrep_without_dictionary()
        .args(["corpus", "variants", "--json", "--limit", "1"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success());
    let records = parse_jsonl(&output.stdout);
    assert_eq!(
        records.iter().filter(|r| r["type"] == "variant").count(),
        1,
        "detail is capped"
    );
    let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
    assert_eq!(complete["data"]["realizations"], 2, "the count is not");
    assert_eq!(complete["data"]["reported"], 1);

    let exhaustive = lingrep_without_dictionary()
        .args(["corpus", "variants", "--json", "--limit", "all"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(exhaustive.status.success());
    assert_eq!(
        parse_jsonl(&exhaustive.stdout)
            .iter()
            .filter(|r| r["type"] == "variant")
            .count(),
        2
    );
}

/// A damaged sibling is a diagnostic, and the realizations beside it are still
/// reported. Anything else turns one bad directory into an unanswerable
/// question.
#[test]
fn an_unreadable_realization_is_a_diagnostic_and_does_not_stop_the_report() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    let good = build(&dictionary, &source);

    let damaged = directory.path().join(format!(
        "other.md.g{}.a1.{}.locally-executed.lingcorpus",
        lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION,
        "ab".repeat(32)
    ));
    copy_tree(&good, &damaged);
    fs::write(damaged.join("manifest.json"), b"{ truncated").unwrap();

    let records = variants_json(&source);
    assert_eq!(
        records.iter().filter(|r| r["type"] == "variant").count(),
        1,
        "the readable realization survives"
    );
    let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
    assert_eq!(complete["data"]["unreadable"], 1);
    assert_eq!(
        complete["data"]["diagnostics"][0]["path"],
        damaged.display().to_string()
    );
}

/// Run from the corpus's own directory, with the root spelled as a bare
/// relative name. `Path::parent` answers `Some("")` there, and an
/// `unwrap_or(".")` fallback does not fire — the command failed outright with
/// "cannot scan  for prepared realizations".
#[test]
fn a_bare_relative_target_scans_the_current_directory() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    build(&dictionary, &source);

    for target in ["source.md", "."] {
        let output = lingrep_without_dictionary()
            .current_dir(directory.path())
            .args(["corpus", "variants", "--json"])
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "target {target:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let records = parse_jsonl(&output.stdout);
        let complete = records.iter().find(|r| r["type"] == "complete").unwrap();
        assert_eq!(complete["data"]["realizations"], 1, "target {target:?}");
    }
}

/// The same for `corpus build`: its destination resolution scans the same way.
#[test]
fn a_bare_relative_root_builds_and_then_reuses() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("source.md"), "猫。\n").unwrap();

    for pass in ["first", "second"] {
        let output = lingrep()
            .current_dir(directory.path())
            .args(["corpus", "build", "--dict-path"])
            .arg(dictionary.path())
            .arg("source.md")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{pass} pass: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        artifact_directories(directory.path()).len(),
        1,
        "the second pass must reuse, not publish a duplicate"
    );
}

/// `variants` absorbed what an earlier draft split into a second command.
/// There is no `corpus compare` subcommand, alias, or help string.
#[test]
fn no_corpus_compare_surface_exists() {
    let help = lingrep_without_dictionary()
        .args(["corpus", "--help"])
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&help.stdout).into_owned() + &String::from_utf8_lossy(&help.stderr);
    assert!(text.contains("variants"), "variants is advertised: {text}");
    assert!(
        !text.contains("compare"),
        "no compare surface may be advertised: {text}"
    );

    let attempted = lingrep_without_dictionary()
        .args(["corpus", "compare", "."])
        .output()
        .unwrap();
    assert!(
        !attempted.status.success(),
        "`corpus compare` must not be a command"
    );
}

/// The contract is discoverable, which is what makes it usable without reading
/// the source.
#[test]
fn the_variants_contract_appears_in_schema_discovery() {
    let index = lingrep_without_dictionary()
        .args(["contracts", "--json"])
        .output()
        .unwrap();
    assert!(index.status.success());
    let records = parse_jsonl(&index.stdout);
    let contracts = records[0]["data"]["contracts"].as_array().unwrap();
    assert!(
        contracts
            .iter()
            .any(|contract| contract["id"] == "lingrep.variants/v1"),
        "the contract index must carry lingrep.variants/v1"
    );

    let detail = lingrep_without_dictionary()
        .args(["contracts", "lingrep.variants/v1", "--json"])
        .output()
        .unwrap();
    assert!(
        detail.status.success(),
        "{}",
        String::from_utf8_lossy(&detail.stderr)
    );
}
