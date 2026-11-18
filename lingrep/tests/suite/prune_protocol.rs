// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Contracts for `corpus prune`.

use std::collections::{BTreeMap, BTreeSet};
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
                record["apparent_bytes"].as_u64().unwrap(),
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
            .all(|record| record.get("failure").is_none() && record.get("deleted").is_none())
    );
    let kept = of_type(&records, "kept");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0]["path"], path_string(&fixture.siblings.other_reader));
    assert_eq!(kept[0]["reason"], "reader_variant");
    assert_eq!(kept[0]["differences"], serde_json::json!(["reader"]));
    let complete = of_type(&records, "complete")[0];
    assert_eq!(complete["executed"], false);
    assert_eq!(
        complete["planned_apparent_bytes"],
        planned.values().sum::<u64>()
    );
    assert_eq!(complete["deleted"], 0);
    assert_eq!(complete["failed"], 0);
    assert_eq!(complete["deleted_apparent_bytes"], 0);
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
            .all(|record| record.get("failure").is_none())
    );
    let complete = of_type(&records, "complete")[0];
    assert_eq!(complete["executed"], true);
    assert_eq!(complete["deleted"], planned.len());
    assert_eq!(complete["failed"], 0);
    assert_eq!(
        complete["deleted_apparent_bytes"],
        planned.values().sum::<u64>()
    );
    assert_eq!(
        complete["deleted_apparent_bytes"],
        complete["planned_apparent_bytes"]
    );
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

/// A table whose own directory cannot be written is renamed to a hidden name
/// before its deletion fails, and the run stops before the realization it
/// binds. `corpus disk` reports the hidden directory as an interrupted prune,
/// and once it is writable a retry deletes it, the other table bound only to
/// the planned removal, and the realization, and keeps a table whose binding
/// was already absent.
#[cfg(unix)]
#[test]
fn a_read_only_table_leaves_an_interrupted_prune_that_a_retry_finishes() {
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
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o555)).unwrap();

    let failed = prune_command(&fixture);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let leftovers = pruning_leftovers(directory);
    assert_eq!(leftovers.len(), 1, "{failed:?}");
    let leftover = leftovers[0].clone();
    let records = crate::support::parse_jsonl(&failed.stdout);
    let attempted = of_type(&records, "removal");
    assert_eq!(
        attempted.len(),
        1,
        "a failed deletion ends the removals: {records:?}"
    );
    assert_eq!(attempted[0]["path"], path_string(&failing));
    assert!(
        attempted[0]["failure"]
            .as_str()
            .is_some_and(|failure| failure.contains(leftover.to_str().unwrap())),
        "the failure names the hidden directory: {records:?}"
    );
    let complete = of_type(&records, "complete");
    assert_eq!(complete.len(), 1, "the stream completes: {records:?}");
    assert_eq!(complete[0]["removals"], 3);
    assert_eq!(complete[0]["deleted"], 0);
    assert_eq!(complete[0]["failed"], 1);
    assert!(!failing.exists(), "the table's name is free");
    assert!(fixture.other_analyzer.exists() && table.exists());

    let again = prune_command(&fixture);
    assert_eq!(again.status.code(), Some(1), "{again:?}");
    assert_eq!(
        pruning_leftovers(directory),
        std::slice::from_ref(&leftover)
    );
    assert!(fixture.other_analyzer.exists() && table.exists());

    let disk = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let reported = of_type(&disk, "leftover");
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0]["status"], "interrupted_prune");
    assert_eq!(reported[0]["published_name"], "a.lingtable");
    assert_eq!(reported[0]["pruning"], path_string(&leftover));

    fs::set_permissions(&leftover, fs::Permissions::from_mode(0o755)).unwrap();
    let records = prune_source(&fixture, &[]);
    assert_eq!(
        removal_reasons(&records),
        BTreeSet::from([
            (path_string(&leftover), "interrupted_prune".to_owned()),
            (path_string(&table), "table_of_removed".to_owned()),
            (
                path_string(&fixture.other_analyzer),
                "superseded".to_owned()
            ),
        ])
    );
    assert!(pruning_leftovers(directory).is_empty());
    assert!(absent.exists(), "a table bound to nothing here is kept");
    assert!(fixture.current.exists());
}

/// Each `removal` record's path and reason.
fn removal_reasons(records: &[Value]) -> BTreeSet<(String, String)> {
    of_type(records, "removal")
        .iter()
        .map(|record| {
            (
                record["path"].as_str().unwrap().to_owned(),
                record["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// Each `kept` record's path and reason.
fn kept(records: &[Value]) -> BTreeMap<String, String> {
    of_type(records, "kept")
        .iter()
        .map(|record| {
            (
                record["path"].as_str().unwrap().to_owned(),
                record["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// `corpus prune --execute` over `source` with `extra` flags.
fn prune_source(fixture: &crate::disk_protocol::Siblings, extra: &[&str]) -> Vec<Value> {
    let mut args: Vec<&OsStr> = vec![
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
    ];
    args.extend(extra.iter().map(OsStr::new));
    args.push("--source".as_ref());
    args.push(fixture.source.as_os_str());
    run_json(&args)
}

/// After an edit and a dictionary change, every realization of the old text is
/// a source revision, whatever else differs, so the default prune keeps it and
/// `--include-all-superseded` deletes it.
#[test]
fn a_source_revision_is_kept_whatever_else_differs() {
    let fixture = siblings();
    fs::write(&fixture.source, "犬。\n").unwrap();
    let rebuilt =
        crate::disk_protocol::build(&fixture.dictionary, &fixture.source, &["--profile", "text"]);

    let records = prune_source(&fixture, &["--profile", "text"]);
    assert_eq!(removals(&records), BTreeMap::new(), "{records:?}");
    assert_eq!(
        kept(&records),
        BTreeMap::from([
            (path_string(&fixture.current), "source_revision".to_owned()),
            (
                path_string(&fixture.other_analyzer),
                "source_revision".to_owned()
            ),
            (
                path_string(&fixture.other_reader),
                "source_revision".to_owned()
            ),
        ])
    );
    let differences = of_type(&records, "kept")
        .into_iter()
        .find(|record| record["path"] == path_string(&fixture.current))
        .unwrap()["differences"]
        .clone();
    assert_eq!(
        differences,
        serde_json::json!(["source", "analyzer", "token_schema"])
    );

    let records = prune_source(&fixture, &["--profile", "text", "--include-all-superseded"]);
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            path_string(&fixture.current),
            path_string(&fixture.other_analyzer),
            path_string(&fixture.other_reader),
        ])
    );
    assert!(kept(&records).is_empty(), "{records:?}");
    assert!(rebuilt.exists());
}

/// An older-format artifact whose manifest records the current source identity
/// is deleted by default. One that records another identity, or none, is kept
/// as `source_unknown`, and deleted with `--include-all-superseded`.
#[test]
fn an_older_format_is_deleted_by_default_only_when_it_records_the_current_source() {
    let fixture = siblings();
    let same = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xc1),
        |m| {
            m["format_generation"] = 1.into();
        },
    );
    let other = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xc2),
        |m| {
            m["format_generation"] = 1.into();
            m["source_identity"] = "cd".repeat(32).into();
        },
    );
    let none = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xc3),
        |m| {
            m["format_generation"] = 1.into();
            m.as_object_mut().unwrap().remove("source_identity");
        },
    );

    let dry_run = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let disk_keep = |path: &Path| {
        of_type(&dry_run, "realization")
            .into_iter()
            .find(|record| record["path"] == path_string(path))
            .unwrap()["kept_by_prune"]
            .clone()
    };
    assert!(disk_keep(&same).is_null());
    assert_eq!(disk_keep(&other), "source_unknown");
    assert_eq!(disk_keep(&none), "source_unknown");

    let records = prune_source(&fixture, &[]);
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([path_string(&same), path_string(&fixture.other_analyzer)])
    );
    assert_eq!(
        kept(&records),
        BTreeMap::from([
            (path_string(&other), "source_unknown".to_owned()),
            (path_string(&none), "source_unknown".to_owned()),
            (
                path_string(&fixture.other_reader),
                "reader_variant".to_owned()
            ),
        ])
    );

    let records = prune_source(&fixture, &["--include-all-superseded"]);
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            path_string(&other),
            path_string(&none),
            path_string(&fixture.other_reader),
        ])
    );
    assert!(fixture.current.exists());
}

/// With the source missing nothing can be rebuilt, so the default prune keeps
/// every realization of it as `source_unknown`, even with `--keep` naming one.
#[test]
fn realizations_of_a_missing_source_are_kept_by_default() {
    let fixture = siblings();
    fs::remove_file(&fixture.source).unwrap();
    let keep = fixture.current.to_str().unwrap();
    let records = prune_source(&fixture, &["--keep", keep]);
    assert_eq!(removals(&records), BTreeMap::new(), "{records:?}");
    assert_eq!(
        kept(&records),
        BTreeMap::from([
            (
                path_string(&fixture.other_analyzer),
                "source_unknown".to_owned()
            ),
            (
                path_string(&fixture.other_reader),
                "source_unknown".to_owned()
            ),
        ])
    );
    assert!(fixture.other_analyzer.exists() && fixture.other_reader.exists());

    let records = prune_source(&fixture, &["--keep", keep, "--include-all-superseded"]);
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            path_string(&fixture.other_analyzer),
            path_string(&fixture.other_reader),
        ])
    );
    assert!(fixture.current.exists());
}

/// The hidden directories in `directory` whose names mark an interrupted
/// prune.
fn pruning_leftovers(directory: &Path) -> Vec<PathBuf> {
    let mut found = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with('.') && name.contains(".pruning-"))
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

fn prune_command(fixture: &crate::disk_protocol::Siblings) -> std::process::Output {
    lingrep()
        .args(["--json", "corpus", "prune", "--execute", "--dict-path"])
        .arg(fixture.dictionary.path())
        .arg("--source")
        .arg(&fixture.source)
        .output()
        .unwrap()
}

/// A parent directory that cannot be written fails the prune before any
/// count table or realization changes.
#[cfg(unix)]
#[test]
fn a_read_only_parent_fails_before_anything_changes() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = siblings();
    let directory = fixture.directory.path();
    write_table(&fixture.other_analyzer, &directory.join("t.lingtable"));
    let before = crate::disk_protocol::tree_listing(directory);
    fs::set_permissions(directory, fs::Permissions::from_mode(0o555)).unwrap();
    let failed = prune_command(&fixture);
    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!failed.status.success(), "{failed:?}");
    assert_eq!(crate::disk_protocol::tree_listing(directory), before);
}

/// A realization whose own directory cannot be written is renamed to a hidden
/// name before its deletion fails. `corpus build` then publishes a new copy
/// beside it, `corpus disk` reports the hidden directory as an interrupted
/// prune, and once the directory is writable the next prune deletes it.
#[cfg(unix)]
#[test]
fn a_read_only_artifact_leaves_an_interrupted_prune_that_a_rerun_finishes() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = siblings();
    let directory = fixture.directory.path();
    let analyzer = fixture.other_analyzer.clone();
    fs::set_permissions(&analyzer, fs::Permissions::from_mode(0o555)).unwrap();
    let failed = prune_command(&fixture);
    assert_eq!(failed.status.code(), Some(1), "{failed:?}");
    let leftovers = pruning_leftovers(directory);
    assert_eq!(leftovers.len(), 1, "{failed:?}");
    let leftover = leftovers[0].clone();
    let records = crate::support::parse_jsonl(&failed.stdout);
    let attempted = of_type(&records, "removal");
    assert_eq!(attempted.len(), 1, "{records:?}");
    assert_eq!(attempted[0]["path"], path_string(&analyzer));
    assert!(
        attempted[0]["failure"]
            .as_str()
            .is_some_and(|failure| failure.contains(leftover.to_str().unwrap())),
        "the failure names the hidden directory: {records:?}"
    );
    assert_eq!(of_type(&records, "complete")[0]["failed"], 1);
    assert!(!analyzer.exists(), "the canonical name is free");
    lingrep_corpus::CorpusArtifact::open(&leftover).unwrap();

    let again = prune_command(&fixture);
    assert!(
        !again.status.success(),
        "the leftover still cannot be deleted"
    );
    assert_eq!(
        pruning_leftovers(directory),
        std::slice::from_ref(&leftover)
    );

    let rebuilt =
        crate::disk_protocol::build(&fixture.dictionary, &fixture.source, &["--profile", "text"]);
    assert_eq!(rebuilt, analyzer, "corpus build ignores the leftover");
    let disk = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let reported = of_type(&disk, "leftover");
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0]["status"], "interrupted_prune");
    assert_eq!(reported[0]["pruning"], path_string(&leftover));

    fs::set_permissions(&leftover, fs::Permissions::from_mode(0o755)).unwrap();
    let records = prune_source(&fixture, &[]);
    assert_eq!(
        of_type(&records, "removal")
            .iter()
            .map(|record| (
                record["path"].as_str().unwrap().to_owned(),
                record["reason"].as_str().unwrap().to_owned()
            ))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            (path_string(&leftover), "interrupted_prune".to_owned()),
            (path_string(&analyzer), "superseded".to_owned()),
        ])
    );
    assert!(pruning_leftovers(directory).is_empty());
    assert!(fixture.current.exists() && fixture.other_reader.exists());
}

/// A prune killed partway through a deletion leaves a hidden, partly deleted
/// directory. `corpus build` ignores it, `corpus disk` reports it, and the
/// next prune deletes it.
#[test]
fn an_interrupted_prune_is_ignored_by_build_and_finished_by_the_next_prune() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let name = fixture
        .other_analyzer
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let leftover = directory.join(format!(".{name}.pruning-1-0"));
    fs::rename(&fixture.other_analyzer, &leftover).unwrap();
    fs::remove_file(leftover.join("tokens.v1.zst")).unwrap();

    let rebuilt =
        crate::disk_protocol::build(&fixture.dictionary, &fixture.source, &["--profile", "text"]);
    assert_eq!(rebuilt, fixture.other_analyzer);
    assert!(leftover.exists(), "corpus build leaves it");

    let disk = run_json(&["corpus".as_ref(), "disk".as_ref(), directory.as_os_str()]);
    let reported = of_type(&disk, "leftover");
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0]["status"], "interrupted_prune");
    assert_eq!(reported[0]["published_name"], name);
    assert!(
        of_type(&disk, "realization")
            .iter()
            .all(|record| record["path"] != path_string(&leftover))
    );

    let records = prune_source(&fixture, &[]);
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([path_string(&leftover), path_string(&fixture.other_analyzer)])
    );
    assert!(!leftover.exists() && !fixture.other_analyzer.exists());
    assert!(fixture.current.exists() && fixture.other_reader.exists());
}

/// A copy of the current realization under a name that records another
/// identity, and that sorts before it, is `misnamed`: the correctly named one
/// stays current and both are kept.
#[test]
fn a_misnamed_identical_copy_does_not_displace_the_current_realization() {
    let fixture = siblings();
    let generation = lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION;
    let copy = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", generation, 0x00),
        |_| {},
    );
    assert!(copy < fixture.current, "the copy is scanned first");

    let records = prune_source(&fixture, &[]);
    assert_eq!(
        of_type(&records, "source")[0]["current"]["path"],
        path_string(&fixture.current)
    );
    assert_eq!(
        removals(&records).into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([path_string(&fixture.other_analyzer)])
    );
    assert_eq!(
        kept(&records),
        BTreeMap::from([(
            path_string(&fixture.other_reader),
            "reader_variant".to_owned()
        )])
    );
    assert!(fixture.current.exists() && copy.exists());
    let disk = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let status = |path: &Path| {
        of_type(&disk, "realization")
            .into_iter()
            .find(|record| record["path"] == path_string(path))
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status(&copy), "misnamed");
    assert_eq!(status(&fixture.current), "current");
}

/// A realization, older-format artifact or interrupted prune that holds a
/// file or directory besides the components its manifest lists is kept as
/// `foreign_entries` in both prune modes, and nothing inside it is deleted.
#[cfg(unix)]
#[test]
fn a_directory_with_foreign_entries_is_kept() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let older = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xd1),
        |m| {
            m["format_generation"] = 1.into();
        },
    );
    let name = fixture.other_reader.file_name().unwrap().to_str().unwrap();
    let leftover = directory.join(format!(".{name}.pruning-1-0"));
    fs::rename(&fixture.other_reader, &leftover).unwrap();
    fs::write(fixture.other_analyzer.join("my-notes.txt"), b"notes").unwrap();
    fs::create_dir(fixture.other_analyzer.join("sub")).unwrap();
    fs::write(fixture.other_analyzer.join("sub").join("f"), b"f").unwrap();
    fs::write(older.join("my-notes.txt"), b"notes").unwrap();
    fs::write(leftover.join("my-notes.txt"), b"notes").unwrap();
    let before = crate::disk_protocol::tree_listing(directory);

    for all in [&[][..], &["--include-all-superseded"][..]] {
        let records = prune_source(&fixture, all);
        assert_eq!(removals(&records), BTreeMap::new(), "{records:?}");
        assert_eq!(
            kept(&records),
            BTreeMap::from([
                (
                    path_string(&fixture.other_analyzer),
                    "foreign_entries".to_owned()
                ),
                (path_string(&older), "foreign_entries".to_owned()),
                (path_string(&leftover), "foreign_entries".to_owned()),
            ])
        );
        let message = of_type(&records, "kept")
            .into_iter()
            .find(|record| record["path"] == path_string(&fixture.other_analyzer))
            .unwrap()["message"]
            .clone();
        assert!(
            message
                .as_str()
                .is_some_and(|message| message.ends_with("my-notes.txt, sub/")),
            "{message}"
        );
        assert_eq!(crate::disk_protocol::tree_listing(directory), before);
    }
}

/// The removal primitives delete nothing from a directory that holds an
/// entry its manifest does not list.
#[test]
fn the_removal_primitives_reject_a_directory_with_foreign_entries() {
    use lingrep_corpus::{CorpusArtifact, remove_realization};

    let fixture = siblings();
    let identity = *CorpusArtifact::open(&fixture.other_analyzer)
        .unwrap()
        .analysis_identity();
    let table = fixture.directory.path().join("t.lingtable");
    write_table(&fixture.other_analyzer, &table);
    fs::write(fixture.other_analyzer.join("my-notes.txt"), b"notes").unwrap();
    fs::write(table.join("my-notes.txt"), b"notes").unwrap();
    let before = crate::disk_protocol::tree_listing(fixture.directory.path());
    assert!(remove_realization(&fixture.other_analyzer, "source.md", &identity).is_err());
    let bound = lingrep_corpus::CountTable::read_key(&table)
        .unwrap()
        .analysis_identity;
    assert!(lingrep_corpus::CountTable::remove(&table, &bound).is_err());
    assert_eq!(
        crate::disk_protocol::tree_listing(fixture.directory.path()),
        before
    );
}

/// A prune killed partway through a table deletion leaves a hidden directory.
/// `ling table` rejects it, `summarize --write-table` writes a new table under
/// the name it had, `corpus disk` reports it, and the next prune deletes it
/// before the tables and realizations it plans.
#[test]
fn an_interrupted_table_prune_is_rejected_by_readers_and_finished_by_the_next_prune() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let table = directory.join("t.lingtable");
    write_table(&fixture.other_analyzer, &table);
    let leftover = directory.join(".t.lingtable.pruning-1-0");
    fs::rename(&table, &leftover).unwrap();

    let read_leftover = || {
        lingrep_without_dictionary()
            .args(["table", "--patterns"])
            .arg(table.with_extension("lx"))
            .arg("--corpus")
            .arg(&fixture.other_analyzer)
            .arg(&leftover)
            .output()
            .unwrap()
    };
    let rejected = read_leftover();
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("corpus prune"), "{stderr}");
    fs::remove_file(leftover.join("values.utf8")).unwrap();
    let rejected = read_leftover();
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("corpus prune"), "{stderr}");

    write_table(&fixture.other_analyzer, &table);
    crate::support::assert_success(&read_table(&table, &fixture.other_analyzer));

    for scope in [
        vec!["--source".as_ref(), fixture.source.as_os_str()],
        vec![directory.as_os_str()],
    ] {
        let mut args: Vec<&OsStr> = vec![
            "corpus".as_ref(),
            "disk".as_ref(),
            "--dict-path".as_ref(),
            fixture.dictionary.path().as_os_str(),
        ];
        args.extend(scope);
        let disk = run_json(&args);
        let reported = of_type(&disk, "leftover");
        assert_eq!(reported.len(), 1, "{reported:?}");
        assert_eq!(reported[0]["status"], "interrupted_prune");
        assert_eq!(reported[0]["published_name"], "t.lingtable");
        assert_eq!(reported[0]["pruning"], path_string(&leftover));
        assert_eq!(
            of_type(&disk, "table")
                .iter()
                .map(|record| record["path"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            vec![path_string(&table)]
        );
    }

    let records = prune_source(&fixture, &[]);
    assert_eq!(
        removal_reasons(&records),
        BTreeSet::from([
            (path_string(&leftover), "interrupted_prune".to_owned()),
            (path_string(&table), "table_of_removed".to_owned()),
            (
                path_string(&fixture.other_analyzer),
                "superseded".to_owned()
            ),
        ])
    );
    let order = of_type(&records, "removal")
        .iter()
        .map(|record| record["reason"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        ["interrupted_prune", "table_of_removed", "superseded"]
    );
    assert!(!leftover.exists() && !table.exists() && !fixture.other_analyzer.exists());
    assert!(fixture.current.exists() && fixture.other_reader.exists());
}

/// A count table bound only to a planned removal that holds a file or
/// directory besides its components, and an interrupted table prune that
/// does, are kept as `foreign_entries`, and the realization the table binds
/// is kept as `kept_table`, so that the table is not left bound to nothing.
/// Nothing inside them is deleted.
#[cfg(unix)]
#[test]
fn a_table_with_foreign_entries_is_kept_with_the_realization_it_binds() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let table = directory.join("t.lingtable");
    write_table(&fixture.other_analyzer, &table);
    fs::write(table.join("my-notes.txt"), b"notes").unwrap();
    fs::create_dir(table.join("sub")).unwrap();
    fs::write(table.join("sub").join("f"), b"f").unwrap();
    let leftover = directory.join(".u.lingtable.pruning-1-0");
    write_table(&fixture.other_reader, &leftover);
    fs::write(leftover.join("my-notes.txt"), b"notes").unwrap();

    let disk = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let tables = of_type(&disk, "table");
    assert_eq!(tables.len(), 1, "{tables:?}");
    assert_eq!(tables[0]["kept_by_prune"], "foreign_entries");
    let realization = of_type(&disk, "realization")
        .into_iter()
        .find(|record| record["path"] == path_string(&fixture.other_analyzer))
        .unwrap();
    assert_eq!(realization["kept_by_prune"], "kept_table");
    assert_eq!(
        of_type(&disk, "leftover")[0]["kept_by_prune"],
        "foreign_entries"
    );

    let before = crate::disk_protocol::tree_listing(directory);
    let records = prune_source(&fixture, &[]);
    assert_eq!(removals(&records), BTreeMap::new(), "{records:?}");
    assert_eq!(
        kept(&records),
        BTreeMap::from([
            (path_string(&table), "foreign_entries".to_owned()),
            (path_string(&leftover), "foreign_entries".to_owned()),
            (
                path_string(&fixture.other_analyzer),
                "kept_table".to_owned()
            ),
            (
                path_string(&fixture.other_reader),
                "reader_variant".to_owned()
            ),
        ])
    );
    let message = |path: &Path| {
        of_type(&records, "kept")
            .into_iter()
            .find(|record| record["path"] == path_string(path))
            .unwrap()["message"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(
        message(&table).ends_with("my-notes.txt, sub/"),
        "{records:?}"
    );
    assert!(
        message(&fixture.other_analyzer).contains(&path_string(&table)),
        "{records:?}"
    );
    assert_eq!(crate::disk_protocol::tree_listing(directory), before);

    let records = prune_source(&fixture, &["--include-all-superseded"]);
    assert_eq!(
        removal_reasons(&records),
        BTreeSet::from([(path_string(&fixture.other_reader), "superseded".to_owned())])
    );
    assert_eq!(
        kept(&records),
        BTreeMap::from([
            (path_string(&table), "foreign_entries".to_owned()),
            (path_string(&leftover), "foreign_entries".to_owned()),
            (
                path_string(&fixture.other_analyzer),
                "kept_table".to_owned()
            ),
        ])
    );
    assert!(table.join("sub").join("f").exists() && leftover.join("my-notes.txt").exists());
    assert!(fixture.other_analyzer.exists());
}

/// With the source missing and only older-format artifacts of it left, the
/// error names the remedies that exist: neither a build nor `--keep` can
/// anchor it.
#[test]
fn a_missing_source_with_only_older_formats_names_the_remedies_that_exist() {
    let fixture = siblings();
    let older = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xe1),
        |m| {
            m["format_generation"] = 1.into();
        },
    );
    for readable in [
        &fixture.current,
        &fixture.other_analyzer,
        &fixture.other_reader,
    ] {
        fs::remove_dir_all(readable).unwrap();
    }
    fs::remove_file(&fixture.source).unwrap();
    let output = lingrep()
        .args([
            "--json",
            "corpus",
            "prune",
            "--execute",
            "--include-all-superseded",
            "--dict-path",
        ])
        .arg(fixture.dictionary.path())
        .arg("--source")
        .arg(&fixture.source)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no readable artifact to keep"), "{stderr}");
    assert!(!stderr.contains("build the current artifact"), "{stderr}");
    assert!(older.exists());
}
