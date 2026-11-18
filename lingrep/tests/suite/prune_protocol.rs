// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Contracts for `corpus prune`.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::disk_protocol::{
    Siblings, canonical_name, of_type, run_json, sibling_with_manifest, siblings, write_table,
};
use crate::support::{lingrep, lingrep_without_dictionary};

/// The source's realizations plus every kind of entry prune must keep.
struct Fixture {
    siblings: Siblings,
    older: PathBuf,
    newer: PathBuf,
    misnamed: PathBuf,
    symlink: PathBuf,
    non_canonical: PathBuf,
    other_source: PathBuf,
    current_table: PathBuf,
    superseded_table: PathBuf,
    absent_table: PathBuf,
    leftover: PathBuf,
    _outside: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let siblings = siblings();
    let directory = siblings.directory.path().to_path_buf();
    let generation = lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION;
    let current = siblings.current.clone();
    let older = sibling_with_manifest(&current, &canonical_name("source.md", 1, 0xa1), |m| {
        m["format_generation"] = 1.into();
    });
    let newer = sibling_with_manifest(
        &current,
        &canonical_name("source.md", generation, 0xa2),
        |m| m["schema_version"] = 99.into(),
    );
    let misnamed = sibling_with_manifest(
        &current,
        &canonical_name("source.md", generation, 0xa3),
        |_| {},
    );
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.lingcorpus");
    fs::rename(
        sibling_with_manifest(&siblings.other_reader, "copy", |_| {}),
        &target,
    )
    .unwrap();
    let symlink = directory.join(canonical_name("source.md", generation, 0xa4));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &symlink).unwrap();
    let non_canonical =
        sibling_with_manifest(&siblings.other_reader, "source.md.mine.lingcorpus", |_| {});
    let other_source = sibling_with_manifest(&current, &canonical_name("other.md", 1, 0xa5), |m| {
        m["format_generation"] = 1.into();
    });
    let current_table = directory.join("current.lingtable");
    let superseded_table = directory.join("superseded.lingtable");
    let absent_table = directory.join("absent.lingtable");
    write_table(&current, &current_table);
    write_table(&siblings.other_analyzer, &superseded_table);
    write_table(&target, &absent_table);
    // The copy records the reader variant's identity; bind the table to one
    // that no artifact here records.
    let header_path = absent_table.join("header.json");
    let mut header: Value = serde_json::from_slice(&fs::read(&header_path).unwrap()).unwrap();
    header["key"]["analysis_identity"] = "ab".repeat(32).into();
    fs::write(&header_path, serde_json::to_vec(&header).unwrap()).unwrap();
    let name = current.file_name().unwrap().to_str().unwrap();
    let leftover = directory.join(format!(".{name}.staging-1-0"));
    fs::create_dir(&leftover).unwrap();
    Fixture {
        siblings,
        older,
        newer,
        misnamed,
        symlink,
        non_canonical,
        other_source,
        current_table,
        superseded_table,
        absent_table,
        leftover,
        _outside: outside,
    }
}

fn prune(fixture: &Fixture, extra: &[&OsStr]) -> Vec<Value> {
    let mut args: Vec<&OsStr> = vec![
        "corpus".as_ref(),
        "prune".as_ref(),
        "--dict-path".as_ref(),
        fixture.siblings.dictionary.path().as_os_str(),
    ];
    args.extend_from_slice(extra);
    run_json(&args)
}

fn removals(records: &[Value]) -> BTreeMap<String, u64> {
    of_type(records, "removal")
        .iter()
        .map(|record| {
            (
                record["path"].as_str().unwrap().to_owned(),
                record["bytes"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

/// A dry run lists the realizations of the source superseded in an analysis
/// dimension, the older-format ones, and the table bound only to a superseded
/// one, with the bytes each would free, and deletes nothing. A reader-only
/// variant is listed as kept.
#[cfg(unix)]
#[test]
fn a_dry_run_lists_what_would_be_freed_and_deletes_nothing() {
    let fixture = fixture();
    let records = prune(
        &fixture,
        &["--source".as_ref(), fixture.siblings.source.as_os_str()],
    );
    let planned = removals(&records);
    assert_eq!(
        planned.keys().cloned().collect::<Vec<_>>(),
        {
            let mut expected = vec![
                path_string(&fixture.siblings.other_analyzer),
                path_string(&fixture.older),
                path_string(&fixture.superseded_table),
            ];
            expected.sort();
            expected
        },
        "{records:?}"
    );
    assert!(planned.values().all(|bytes| *bytes > 0));
    assert!(
        of_type(&records, "removal")
            .iter()
            .all(|record| record["deleted"] == false)
    );
    let kept = of_type(&records, "kept");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["path"], path_string(&fixture.siblings.other_reader));
    assert_eq!(kept[0]["reason"], "reader_variant");
    assert_eq!(kept[0]["differences"], serde_json::json!(["reader"]));
    let complete = of_type(&records, "complete")[0];
    assert_eq!(complete["executed"], false);
    assert_eq!(
        complete["expected_freed_bytes"],
        planned.values().sum::<u64>()
    );
    assert_eq!(complete["freed_bytes"], 0);
    for path in planned.keys() {
        assert!(Path::new(path).exists(), "a dry run deletes nothing");
    }
}

/// `--execute` deletes exactly the set the dry run reported, freeing the
/// sizes it reported, and keeps everything else.
#[cfg(unix)]
#[test]
fn execute_deletes_exactly_the_reported_set() {
    let fixture = fixture();
    let planned = removals(&prune(
        &fixture,
        &["--source".as_ref(), fixture.siblings.source.as_os_str()],
    ));
    let records = prune(
        &fixture,
        &[
            "--execute".as_ref(),
            "--source".as_ref(),
            fixture.siblings.source.as_os_str(),
        ],
    );
    assert_eq!(removals(&records), planned, "the deletions are the plan");
    assert!(
        of_type(&records, "removal")
            .iter()
            .all(|record| record["deleted"] == true)
    );
    let complete = of_type(&records, "complete")[0];
    assert_eq!(complete["executed"], true);
    assert_eq!(complete["freed_bytes"], planned.values().sum::<u64>());
    assert_eq!(complete["freed_bytes"], complete["expected_freed_bytes"]);
    for path in planned.keys() {
        assert!(!Path::new(path).exists(), "{path} is deleted");
    }
    for kept in [
        &fixture.siblings.source,
        &fixture.siblings.current,
        &fixture.siblings.other_reader,
        &fixture.newer,
        &fixture.misnamed,
        &fixture.symlink,
        &fixture.non_canonical,
        &fixture.other_source,
        &fixture.current_table,
        &fixture.absent_table,
        &fixture.leftover,
    ] {
        assert!(
            fs::symlink_metadata(kept).is_ok(),
            "{} is kept",
            kept.display()
        );
    }
    assert!(
        fs::symlink_metadata(&fixture.symlink)
            .unwrap()
            .file_type()
            .is_symlink()
            && fixture.symlink.join("manifest.json").exists(),
        "the symbolic link and its target are kept"
    );
}

/// Without a current realization prune deletes nothing unless `--keep` names
/// what to keep; with `--keep` it keeps that artifact and its table.
#[test]
fn a_missing_current_realization_requires_keep() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let table = directory.join("analyzer.lingtable");
    write_table(&fixture.other_analyzer, &table);
    fs::remove_dir_all(&fixture.current).unwrap();

    let refused = lingrep()
        .args(["--json", "corpus", "prune", "--execute", "--dict-path"])
        .arg(fixture.dictionary.path())
        .arg(directory)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("--keep"), "{stderr}");
    assert!(fixture.other_analyzer.exists() && fixture.other_reader.exists());
    assert!(table.exists());

    let records = run_json(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--keep".as_ref(),
        fixture.other_analyzer.as_os_str(),
        "--include-all-superseded".as_ref(),
        directory.as_os_str(),
    ]);
    assert_eq!(
        removals(&records).keys().cloned().collect::<Vec<_>>(),
        [path_string(&fixture.other_reader)]
    );
    assert!(fixture.other_analyzer.exists() && table.exists());
    assert!(!fixture.other_reader.exists());
}

/// "Current" follows the analysis inputs: under another reader policy the
/// default-reader artifact is superseded, and kept by default because it
/// differs only in the reader.
#[test]
fn the_inputs_decide_which_realization_is_kept() {
    let fixture = siblings();
    let records = run_json(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--md-exclude-sections".as_ref(),
        "references,appendix".as_ref(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert_eq!(
        removals(&records).keys().cloned().collect::<Vec<_>>(),
        [path_string(&fixture.other_analyzer)]
    );
    assert!(fixture.other_reader.exists() && fixture.current.exists());
    assert!(!fixture.other_analyzer.exists());
}

/// `--include-all-superseded` adds the reader-only variant to the plan, and
/// executing deletes exactly that plan.
#[test]
fn reader_variants_are_deleted_only_with_include_all_superseded() {
    let fixture = siblings();
    let args = |execute: bool| {
        let mut args: Vec<&OsStr> = vec![
            "corpus".as_ref(),
            "prune".as_ref(),
            "--dict-path".as_ref(),
            fixture.dictionary.path().as_os_str(),
            "--include-all-superseded".as_ref(),
        ];
        if execute {
            args.push("--execute".as_ref());
        }
        args.push(fixture.directory.path().as_os_str());
        args
    };
    let planned = removals(&run_json(&args(false)));
    let mut expected = vec![
        path_string(&fixture.other_analyzer),
        path_string(&fixture.other_reader),
    ];
    expected.sort();
    assert_eq!(planned.keys().cloned().collect::<Vec<_>>(), expected);
    let records = run_json(&args(true));
    assert_eq!(of_type(&records, "run")[0]["include_all_superseded"], true);
    assert_eq!(removals(&records), planned, "the deletions are the plan");
    assert!(of_type(&records, "kept").is_empty());
    assert!(!fixture.other_reader.exists() && !fixture.other_analyzer.exists());
    assert!(fixture.current.exists());
}

/// A sibling that differs in the analyzer as well as the reader is analysis
/// churn, so the default prune deletes it.
#[test]
fn an_analysis_difference_outweighs_a_reader_difference() {
    let fixture = siblings();
    let both = crate::disk_protocol::build(
        &fixture.dictionary,
        &fixture.source,
        &[
            "--profile",
            "text",
            "--md-exclude-sections",
            "references,appendix",
        ],
    );
    let records = run_json(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let removal = of_type(&records, "removal")
        .into_iter()
        .find(|record| record["path"] == path_string(&both))
        .unwrap_or_else(|| panic!("the analyzer-and-reader sibling is deleted: {records:?}"));
    assert_eq!(
        removal["differences"],
        serde_json::json!(["reader", "analyzer", "token_schema"])
    );
    assert!(!both.exists());
    assert!(fixture.other_reader.exists());
}

/// `--keep` must name a readable realization in the scanned directory.
#[test]
fn keep_rejects_a_path_that_is_not_a_scanned_realization() {
    let fixture = siblings();
    let output = lingrep()
        .args(["corpus", "prune", "--dict-path"])
        .arg(fixture.dictionary.path())
        .arg("--keep")
        .arg(&fixture.source)
        .arg(fixture.directory.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

/// The removal primitives re-check what the scan classified and delete
/// nothing that differs from it.
#[cfg(unix)]
#[test]
fn the_removal_primitives_reject_entries_that_differ_from_the_classification() {
    use lingrep_corpus::{AnalysisIdentity, CorpusArtifact, CountTable, remove_realization};

    let fixture = fixture();
    let current = &fixture.siblings.current;
    let identity = *CorpusArtifact::open(current).unwrap().analysis_identity();
    let other = AnalysisIdentity::from_bytes([0xa4; 32]);
    assert!(remove_realization(&fixture.misnamed, "source.md", &identity).is_err());
    assert!(remove_realization(&fixture.symlink, "source.md", &other).is_err());
    assert!(remove_realization(current, "other.md", &identity).is_err());
    assert!(remove_realization(current, "source.md", &other).is_err());
    assert!(CountTable::remove(&fixture.current_table, &"ab".repeat(32)).is_err());
    for kept in [current, &fixture.misnamed, &fixture.current_table] {
        assert!(kept.exists(), "{}", kept.display());
    }
    assert!(fixture.symlink.join("manifest.json").exists());
}

/// A realization of edited source content under the same analysis is kept by
/// default, since a server reading the old content may still serve it, and
/// deleted with `--include-all-superseded`.
#[test]
fn a_source_revision_is_kept_by_default() {
    let fixture = siblings();
    fs::write(&fixture.source, "犬。\n").unwrap();
    let rebuilt = crate::disk_protocol::build(&fixture.dictionary, &fixture.source, &[]);
    let args = |all: bool| {
        let mut args: Vec<&OsStr> = vec![
            "corpus".as_ref(),
            "prune".as_ref(),
            "--execute".as_ref(),
            "--dict-path".as_ref(),
            fixture.dictionary.path().as_os_str(),
        ];
        if all {
            args.push("--include-all-superseded".as_ref());
        }
        args.push("--source".as_ref());
        args.push(fixture.source.as_os_str());
        args
    };
    let records = run_json(&args(false));
    let kept = of_type(&records, "kept")
        .into_iter()
        .find(|record| record["path"] == path_string(&fixture.current))
        .unwrap_or_else(|| panic!("the old content is kept: {records:?}"));
    assert_eq!(kept["reason"], "source_revision");
    assert!(fixture.current.exists());

    let records = run_json(&args(true));
    assert!(removals(&records).contains_key(&path_string(&fixture.current)));
    assert!(!fixture.current.exists() && rebuilt.exists());
}

/// Copies `from` to the canonical name of an externally attested realization
/// of the same analysis, with the trust class its manifest records.
fn attested_copy(fixture: &crate::disk_protocol::Siblings, from: &Path) -> PathBuf {
    use lingrep_corpus::{CorpusArtifact, TrustClass, canonical_artifact_path};
    let identity = *CorpusArtifact::open(from).unwrap().analysis_identity();
    let name = canonical_artifact_path(&fixture.source, &identity, TrustClass::ExternallyAttested);
    let attested = sibling_with_manifest(from, name.file_name().unwrap().to_str().unwrap(), |m| {
        m["trust_class"] = "externally-attested".into();
    });
    CorpusArtifact::open(&attested).unwrap();
    attested
}

/// An externally attested realization is kept as `other_trust` in both prune
/// modes when the source is missing and `--keep` anchors a locally executed
/// realization.
#[test]
fn another_trust_class_is_kept_when_the_source_is_missing() {
    let fixture = siblings();
    let attested = attested_copy(&fixture, &fixture.other_analyzer);
    fs::remove_file(&fixture.source).unwrap();
    for all in [false, true] {
        let mut args: Vec<&OsStr> = vec![
            "corpus".as_ref(),
            "prune".as_ref(),
            "--execute".as_ref(),
            "--dict-path".as_ref(),
            fixture.dictionary.path().as_os_str(),
            "--keep".as_ref(),
            fixture.current.as_os_str(),
        ];
        if all {
            args.push("--include-all-superseded".as_ref());
        }
        args.push("--source".as_ref());
        args.push(fixture.source.as_os_str());
        let records = run_json(&args);
        let kept = of_type(&records, "kept")
            .into_iter()
            .find(|record| record["path"] == path_string(&attested))
            .unwrap_or_else(|| panic!("the attested realization is kept: {records:?}"));
        assert_eq!(kept["reason"], "other_trust");
        assert!(!removals(&records).contains_key(&path_string(&attested)));
        assert!(attested.exists(), "include_all_superseded={all}");
    }
    assert!(fixture.current.exists());
}

/// A superseded realization that fails the structural validation
/// `corpus inspect` runs is kept as `damaged` with the validation failure,
/// and cannot be named by `--keep`.
#[test]
fn a_damaged_realization_is_kept_and_cannot_anchor_keep() {
    let fixture = siblings();
    fs::remove_file(fixture.other_analyzer.join("tokens.v1.zst")).unwrap();
    let inspect = lingrep_without_dictionary()
        .args(["corpus", "inspect"])
        .arg(&fixture.other_analyzer)
        .output()
        .unwrap();
    assert!(!inspect.status.success(), "inspect rejects the realization");

    let keep = lingrep()
        .args(["corpus", "prune", "--dict-path"])
        .arg(fixture.dictionary.path())
        .arg("--keep")
        .arg(&fixture.other_analyzer)
        .arg("--source")
        .arg(&fixture.source)
        .output()
        .unwrap();
    assert_eq!(keep.status.code(), Some(2), "{keep:?}");

    let records = run_json(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert!(fixture.other_analyzer.exists(), "{records:?}");
    assert!(!removals(&records).contains_key(&path_string(&fixture.other_analyzer)));
    let kept = of_type(&records, "kept")
        .into_iter()
        .find(|record| record["path"] == path_string(&fixture.other_analyzer))
        .unwrap_or_else(|| panic!("the damaged realization is reported: {records:?}"));
    assert_eq!(kept["reason"], "damaged");
    assert!(
        kept["message"]
            .as_str()
            .is_some_and(|message| message.contains("tokens.v1.zst")),
        "{kept:?}"
    );
}

/// Reads `table` against `corpus` with the catalog `write_table` wrote.
fn read_table(table: &Path, corpus: &Path) -> std::process::Output {
    lingrep_without_dictionary()
        .args(["table", "--patterns"])
        .arg(table.with_extension("lx"))
        .arg("--corpus")
        .arg(corpus)
        .arg(table)
        .output()
        .unwrap()
}

/// A count table whose identity a retained explicit-output corpus records is
/// kept, while the superseded canonical realization of the same identity is
/// deleted.
#[test]
fn a_table_bound_to_a_retained_explicit_output_is_kept() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let explicit = sibling_with_manifest(&fixture.other_analyzer, "explicit-copy", |_| {});
    let table = directory.join("explicit.lingtable");
    write_table(&explicit, &table);
    crate::support::assert_success(&read_table(&table, &explicit));

    let records = run_json(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert!(
        removals(&records).contains_key(&path_string(&fixture.other_analyzer)),
        "{records:?}"
    );
    assert!(!removals(&records).contains_key(&path_string(&table)));
    assert!(explicit.exists() && table.exists(), "{records:?}");
    crate::support::assert_success(&read_table(&table, &explicit));
}

/// A prune stopped by a deletion error leaves a state that a retry finishes:
/// each table bound only to a planned removal is deleted, and a table whose
/// binding was already absent is kept.
#[cfg(unix)]
#[test]
fn a_retry_after_a_failed_deletion_finishes_the_table_cleanup() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = siblings();
    let directory = fixture.directory.path();
    let failing = directory.join("a.lingtable");
    let table = directory.join("b.lingtable");
    write_table(&fixture.other_analyzer, &failing);
    write_table(&fixture.other_analyzer, &table);
    let elsewhere = tempfile::tempdir().unwrap();
    let moved = elsewhere.path().join("moved.lingcorpus");
    fs::rename(&fixture.other_reader, &moved).unwrap();
    let absent = directory.join("absent.lingtable");
    write_table(&moved, &absent);
    // A file in a read-only directory cannot be unlinked, so deleting the
    // table that holds it fails.
    let locked = failing.join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("file"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

    let prune = || {
        lingrep()
            .args(["--json", "corpus", "prune", "--execute", "--dict-path"])
            .arg(fixture.dictionary.path())
            .arg("--source")
            .arg(&fixture.source)
            .output()
            .unwrap()
    };
    let failed = prune();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!failed.status.success(), "{failed:?}");

    crate::support::assert_success(&prune());
    assert!(!fixture.other_analyzer.exists());
    assert!(!table.exists(), "the retry deletes the planned table");
    assert!(absent.exists(), "a table bound to nothing here is kept");
    assert!(fixture.current.exists());
}
