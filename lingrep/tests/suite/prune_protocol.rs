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
use crate::support::lingrep;

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
        sibling_with_manifest(&siblings.other_analyzer, "copy", |_| {}),
        &target,
    )
    .unwrap();
    let symlink = directory.join(canonical_name("source.md", generation, 0xa4));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &symlink).unwrap();
    let non_canonical = sibling_with_manifest(
        &siblings.other_analyzer,
        "source.md.mine.lingcorpus",
        |_| {},
    );
    let other_source = sibling_with_manifest(&current, &canonical_name("other.md", 1, 0xa5), |m| {
        m["format_generation"] = 1.into();
    });
    let current_table = directory.join("current.lingtable");
    let superseded_table = directory.join("superseded.lingtable");
    let absent_table = directory.join("absent.lingtable");
    write_table(&current, &current_table);
    write_table(&siblings.other_analyzer, &superseded_table);
    write_table(&target, &absent_table);
    // The copy records the superseded artifact's identity; bind the table to
    // one that no artifact here records.
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

/// A dry run lists the superseded and older-format realizations of the source
/// and the table bound only to a superseded one, with the bytes each would
/// free, and deletes nothing.
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
                path_string(&fixture.siblings.other_reader),
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
/// default-reader artifact is the superseded one.
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
    assert_eq!(removals(&records).len(), 2);
    assert!(fixture.other_reader.exists());
    assert!(!fixture.current.exists() && !fixture.other_analyzer.exists());
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
