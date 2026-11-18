// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Contracts for `corpus disk`.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use lingrep_core::testing::MiniDictionary;
use lingrep_corpus::{AnalysisIdentity, TrustClass, canonical_artifact_path};
use serde_json::Value;

use crate::support::{assert_success, lingrep, lingrep_without_dictionary, parse_jsonl};

fn artifacts(directory: &Path) -> BTreeSet<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".lingcorpus"))
        })
        .collect()
}

/// Builds `source` with `extra` flags and returns the artifact it published.
pub(crate) fn build(dictionary: &MiniDictionary, source: &Path, extra: &[&str]) -> PathBuf {
    let directory = source.parent().unwrap();
    let before = artifacts(directory);
    let output = lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .args(extra)
        .arg(source)
        .output()
        .unwrap();
    assert_success(&output);
    let mut published = artifacts(directory)
        .difference(&before)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(published.len(), 1, "{extra:?} publishes one new artifact");
    published.pop().unwrap()
}

/// Writes a count table over `artifact` to `table`.
pub(crate) fn write_table(artifact: &Path, table: &Path) {
    let catalog = table.with_extension("lx");
    fs::write(&catalog, "cat := [text=猫].\n").unwrap();
    let output = lingrep_without_dictionary()
        .args(["--json", "summarize", "--patterns"])
        .arg(&catalog)
        .arg("--corpus")
        .arg(artifact)
        .arg("--write-table")
        .arg(table)
        .output()
        .unwrap();
    assert_success(&output);
}

pub(crate) fn run_json(args: &[&OsStr]) -> Vec<Value> {
    let output = lingrep().arg("--json").args(args).output().unwrap();
    assert_success(&output);
    parse_jsonl(&output.stdout)
}

pub(crate) fn of_type<'a>(records: &'a [Value], kind: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["type"] == kind)
        .map(|record| &record["data"])
        .collect()
}

fn status_of<'a>(records: &'a [Value], path: &Path) -> &'a Value {
    &of_type(records, "realization")
        .into_iter()
        .find(|record| record["path"] == path.display().to_string())
        .unwrap_or_else(|| panic!("{} is reported: {records:?}", path.display()))["status"]
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

/// Copies `from` to `name` beside it, applying `edit` to the copied manifest.
pub(crate) fn sibling_with_manifest(
    from: &Path,
    name: &str,
    edit: impl FnOnce(&mut Value),
) -> PathBuf {
    let path = from.with_file_name(name);
    copy_tree(from, &path);
    let manifest_path = path.join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    edit(&mut manifest);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}

pub(crate) fn canonical_name(source: &str, generation: u32, byte: u8) -> String {
    let current = format!(".g{}.", lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION);
    let path = canonical_artifact_path(
        Path::new(source),
        &AnalysisIdentity::from_bytes([byte; 32]),
        TrustClass::LocallyExecuted,
    );
    path.file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .replacen(&current, &format!(".g{generation}."), 1)
}

/// The realizations of one source under three inputs: the default analysis,
/// another analyzer profile, and another reader policy.
#[allow(dead_code)]
pub(crate) struct Siblings {
    pub dictionary: MiniDictionary,
    pub directory: tempfile::TempDir,
    pub source: PathBuf,
    pub current: PathBuf,
    pub other_analyzer: PathBuf,
    pub other_reader: PathBuf,
}

pub(crate) fn siblings() -> Siblings {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.md");
    fs::write(&source, "猫。\n").unwrap();
    let current = build(&dictionary, &source, &[]);
    let other_analyzer = build(&dictionary, &source, &["--profile", "text"]);
    let other_reader = build(
        &dictionary,
        &source,
        &["--md-exclude-sections", "references,appendix"],
    );
    Siblings {
        dictionary,
        directory,
        source,
        current,
        other_analyzer,
        other_reader,
    }
}

/// With the analysis inputs `corpus build` would get, the realization it would
/// reuse is current and each other one is superseded, naming the dimension
/// that differs. The report deletes nothing.
#[test]
fn analysis_inputs_classify_current_and_superseded_with_their_differences() {
    let fixture = siblings();
    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);

    let run = of_type(&records, "run")[0];
    assert_eq!(run["current_classified"], true);
    let sources = of_type(&records, "source");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["name"], "source.md");
    assert_eq!(sources[0]["current"]["kind"], "present");
    assert_eq!(
        sources[0]["current"]["path"],
        fixture.current.display().to_string()
    );

    assert_eq!(status_of(&records, &fixture.current), "current");
    assert_eq!(status_of(&records, &fixture.other_analyzer), "superseded");
    assert_eq!(status_of(&records, &fixture.other_reader), "superseded");
    let differences = |path: &Path| {
        of_type(&records, "realization")
            .into_iter()
            .find(|record| record["path"] == path.display().to_string())
            .unwrap()["differences"]
            .clone()
    };
    assert_eq!(
        differences(&fixture.other_analyzer),
        serde_json::json!(["analyzer", "token_schema"])
    );
    assert_eq!(
        differences(&fixture.other_reader),
        serde_json::json!(["reader"])
    );
    let kept_by_prune = |path: &Path| {
        of_type(&records, "realization")
            .into_iter()
            .find(|record| record["path"] == path.display().to_string())
            .unwrap()["kept_by_prune"]
            .clone()
    };
    assert_eq!(kept_by_prune(&fixture.other_reader), "reader_variant");
    assert!(kept_by_prune(&fixture.other_analyzer).is_null());
    assert!(kept_by_prune(&fixture.current).is_null());

    let complete = of_type(&records, "complete")[0];
    let measured: u64 = of_type(&records, "realization")
        .iter()
        .map(|record| record["bytes"].as_u64().unwrap())
        .sum();
    assert!(measured > 0);
    assert_eq!(complete["bytes"], measured, "the total sums every item");
    for path in [
        &fixture.current,
        &fixture.other_analyzer,
        &fixture.other_reader,
    ] {
        assert!(path.exists(), "the report deletes nothing");
    }
}

/// Another analysis input selects another current realization, which is what
/// makes "current" depend on the inputs rather than on build time.
#[test]
fn a_different_analysis_input_selects_a_different_current_realization() {
    let fixture = siblings();
    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--md-exclude-sections".as_ref(),
        "references,appendix".as_ref(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert_eq!(status_of(&records, &fixture.other_reader), "current");
    assert_eq!(status_of(&records, &fixture.current), "superseded");
    assert_eq!(status_of(&records, &fixture.other_analyzer), "superseded");
}

/// Without analysis inputs nothing is current or superseded, and the human
/// report says which flags classify them.
#[test]
fn without_analysis_inputs_realizations_are_unclassified() {
    let fixture = siblings();
    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        fixture.directory.path().as_os_str(),
    ]);
    assert_eq!(of_type(&records, "run")[0]["current_classified"], false);
    assert_eq!(
        of_type(&records, "source")[0]["current"]["kind"],
        "not_classified"
    );
    for path in [
        &fixture.current,
        &fixture.other_analyzer,
        &fixture.other_reader,
    ] {
        assert_eq!(status_of(&records, path), "unclassified");
    }

    let human = lingrep_without_dictionary()
        .args(["corpus", "disk"])
        .arg(fixture.directory.path())
        .output()
        .unwrap();
    assert_success(&human);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("--current"),
        "the report names how to classify: {text}"
    );
}

/// `--current` classifies with the inputs `corpus build` uses by default,
/// including the dictionary it falls back to.
#[test]
fn current_alone_uses_the_corpus_build_defaults() {
    let fixture = siblings();
    let output = lingrep()
        .env("LINGREP_DICTIONARY_PATH", fixture.dictionary.path())
        .args(["--json", "corpus", "disk", "--current"])
        .arg(fixture.directory.path())
        .output()
        .unwrap();
    assert_success(&output);
    let records = parse_jsonl(&output.stdout);
    assert_eq!(status_of(&records, &fixture.current), "current");
}

/// Entries that are not deletable realizations are reported with their own
/// status: an older format, a damaged or newer one, a name that records
/// another identity, a symbolic link, and a non-canonical directory.
#[cfg(unix)]
#[test]
fn entries_beside_the_realizations_are_reported_by_what_they_are() {
    let fixture = siblings();
    let generation = lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION;
    let older = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", 1, 0xa1),
        |manifest| manifest["format_generation"] = 1.into(),
    );
    let newer = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", generation, 0xa2),
        |manifest| manifest["schema_version"] = 99.into(),
    );
    let misnamed = sibling_with_manifest(
        &fixture.current,
        &canonical_name("source.md", generation, 0xa3),
        |_| {},
    );
    let outside = tempfile::tempdir().unwrap();
    let symlink = fixture
        .directory
        .path()
        .join(canonical_name("source.md", generation, 0xa4));
    std::os::unix::fs::symlink(outside.path(), &symlink).unwrap();
    let explicit = sibling_with_manifest(&fixture.current, "source.md.mine.lingcorpus", |_| {});
    let other_source = sibling_with_manifest(
        &fixture.current,
        &canonical_name("other.md", 1, 0xa5),
        |manifest| manifest["format_generation"] = 1.into(),
    );

    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert_eq!(status_of(&records, &older), "older_format");
    assert_eq!(status_of(&records, &newer), "unreadable");
    assert_eq!(status_of(&records, &misnamed), "misnamed");
    assert_eq!(status_of(&records, &symlink), "symbolic_link");
    assert_eq!(status_of(&records, &explicit), "non_canonical_name");
    assert!(
        of_type(&records, "realization")
            .iter()
            .all(|record| record["path"] != other_source.display().to_string()),
        "a source report omits other sources"
    );

    let root = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        fixture.directory.path().as_os_str(),
    ]);
    let names = of_type(&root, "source")
        .iter()
        .map(|source| source["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["other.md", "source.md"],
        "a root report covers every source"
    );
}

/// A count table binds its artifact by analysis identity, so its binding is
/// that artifact's status; a table whose artifact is not in the directory is
/// absent.
#[test]
fn count_tables_report_the_status_of_the_artifact_they_bind() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let current_table = directory.join("current.lingtable");
    let superseded_table = directory.join("superseded.lingtable");
    let absent_table = directory.join("absent.lingtable");
    write_table(&fixture.current, &current_table);
    write_table(&fixture.other_analyzer, &superseded_table);
    let elsewhere = tempfile::tempdir().unwrap();
    let moved = elsewhere.path().join("moved.lingcorpus");
    fs::rename(&fixture.other_reader, &moved).unwrap();
    write_table(&moved, &absent_table);

    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        directory.as_os_str(),
    ]);
    let tables = of_type(&records, "table");
    let binding = |path: &Path| {
        tables
            .iter()
            .find(|table| table["path"] == path.display().to_string())
            .unwrap_or_else(|| panic!("{} is reported: {tables:?}", path.display()))
    };
    assert_eq!(binding(&current_table)["binding"], "current");
    assert_eq!(
        binding(&current_table)["artifacts"],
        serde_json::json!([fixture.current.display().to_string()])
    );
    assert_eq!(binding(&superseded_table)["binding"], "superseded");
    assert_eq!(binding(&absent_table)["binding"], "absent");
    assert!(binding(&absent_table)["bytes"].as_u64().unwrap() > 0);
}

/// A staging directory with no lock file is reported as a leftover of unknown
/// status, and a held lock as live.
#[cfg(unix)]
#[test]
fn staging_leftovers_are_reported_with_their_lock_status() {
    let fixture = siblings();
    let directory = fixture.directory.path();
    let name = fixture.current.file_name().unwrap().to_str().unwrap();
    let unlocked = directory.join(format!(".{name}.staging-1-0"));
    fs::create_dir(&unlocked).unwrap();
    fs::write(unlocked.join("tokens.v1.zst"), b"partial").unwrap();
    let live_lock = directory.join(format!(".{name}.stagelock-2-0"));
    fs::create_dir(directory.join(format!(".{name}.staging-2-0"))).unwrap();
    let held = fs::File::create(&live_lock).unwrap();
    held.lock().unwrap();

    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    let leftovers = of_type(&records, "leftover");
    assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    assert_eq!(leftovers[0]["staging"], unlocked.display().to_string());
    assert_eq!(leftovers[0]["status"], "unknown");
    assert_eq!(leftovers[0]["bytes"], 7);
    assert_eq!(leftovers[1]["lock"], live_lock.display().to_string());
    assert_eq!(leftovers[1]["status"], "live");
    assert!(unlocked.exists(), "the report deletes nothing");
    drop(held);
}

/// Named analyzer directories are reported with their size.
#[test]
fn a_named_analyzer_directory_is_reported_with_its_size() {
    let directory = tempfile::tempdir().unwrap();
    let analyzer = directory.path().join("analyzer");
    fs::create_dir_all(analyzer.join("model")).unwrap();
    fs::write(analyzer.join("model").join("weights"), [0_u8; 100]).unwrap();
    let records = run_json(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--analyzer-dir".as_ref(),
        analyzer.as_os_str(),
        directory.path().as_os_str(),
    ]);
    let caches = of_type(&records, "cache");
    assert_eq!(caches.len(), 1);
    assert_eq!(caches[0]["kind"], "analyzer");
    assert_eq!(caches[0]["bytes"], 100);
    assert_eq!(caches[0]["files"], 1);
}

#[test]
fn the_disk_contract_appears_in_schema_discovery() {
    let detail = lingrep_without_dictionary()
        .args(["contracts", "lingrep.disk/v1", "--json"])
        .output()
        .unwrap();
    assert_success(&detail);
}

/// FIFOs and symbolic links where a staging lock, staging directory,
/// manifest or table header is expected are skipped without being opened,
/// so `corpus disk` and `corpus prune` complete, and a regular held lock is
/// still live.
#[cfg(unix)]
#[test]
fn special_files_with_expected_names_are_skipped_without_opening_them() {
    use std::os::unix::ffi::OsStrExt;
    use std::time::Duration;

    let fifo = |path: &Path| {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    };
    let fixture = siblings();
    let directory = fixture.directory.path();
    let name = fixture.current.file_name().unwrap().to_str().unwrap();
    fifo(&directory.join(format!(".{name}.stagelock-1-0")));
    fifo(&directory.join(format!(".{name}.staging-2-0")));
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("fifo");
    fifo(&target);
    std::os::unix::fs::symlink(&target, directory.join(format!(".{name}.stagelock-3-0"))).unwrap();
    let live_lock = directory.join(format!(".{name}.stagelock-4-0"));
    fs::create_dir(directory.join(format!(".{name}.staging-4-0"))).unwrap();
    let held = fs::File::create(&live_lock).unwrap();
    held.lock().unwrap();
    let generation = lingrep_corpus::CORPUS_ARTIFACT_FORMAT_GENERATION;
    let canonical = directory.join(canonical_name("source.md", generation, 0xb1));
    let explicit = directory.join("fifo-output");
    let table = directory.join("fifo.lingtable");
    for (parent, file) in [
        (&canonical, "manifest.json"),
        (&explicit, "manifest.json"),
        (&table, "header.json"),
    ] {
        fs::create_dir(parent).unwrap();
        fifo(&parent.join(file));
    }
    std::os::unix::fs::symlink(&target, directory.join("fifo-link")).unwrap();

    let run = |args: &[&OsStr]| {
        let output = lingrep()
            .arg("--json")
            .args(args)
            .timeout(Duration::from_secs(20))
            .output()
            .unwrap();
        assert_success(&output);
        parse_jsonl(&output.stdout)
    };
    let records = run(&[
        "corpus".as_ref(),
        "disk".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        directory.as_os_str(),
    ]);
    let leftovers = of_type(&records, "leftover");
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    assert_eq!(leftovers[0]["lock"], live_lock.display().to_string());
    assert_eq!(leftovers[0]["status"], "live");
    assert_eq!(status_of(&records, &canonical), "unreadable");

    let records = run(&[
        "corpus".as_ref(),
        "prune".as_ref(),
        "--execute".as_ref(),
        "--dict-path".as_ref(),
        fixture.dictionary.path().as_os_str(),
        "--source".as_ref(),
        fixture.source.as_os_str(),
    ]);
    assert_eq!(
        of_type(&records, "removal")
            .iter()
            .map(|record| record["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [fixture.other_analyzer.display().to_string()]
    );
    assert!(canonical.exists() && explicit.exists() && table.exists());
    drop(held);
}
