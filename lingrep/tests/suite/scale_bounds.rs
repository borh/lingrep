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
//! Summaries and corpus builds at 1×, 10× and 100× one fixture, with the
//! size thresholds of `lingrep_core::test_scale` lowered so the fixture
//! reaches the spill, merge and retry paths, each under a fixed open-file
//! limit.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use lingrep::protocol::query::normalize_run_variance;
use lingrep::summary::SpillLayout;
use lingrep_core::test_scale::{SPILL_FILE_REPORT_VARIABLE, ScaleKnob, SpillFileCounts};
use lingrep_core::testing::MiniDictionary;
use serde_json::Value;

use crate::support::{assert_success, lingrep_std, parse_jsonl};

/// The soft and hard `RLIMIT_NOFILE` of every summary: macOS's default soft limit.
const SUMMARY_OPEN_FILE_LIMIT: u64 = 256;

/// `corpus build` of the 1× fixture fails with 24 open files and succeeds with 32.
const BUILD_OPEN_FILE_LIMIT: u64 = 32;

const SCALES: [usize; 3] = [1, 10, 100];

const WORKERS: usize = 8;

const DOCUMENTS: [&str; 5] = [
    "猫が犬を食べる。彼は鳥と走る。",
    "彼は実験を行う。私も調査と研究を行う。",
    "鳥は鳴く。犬も走る。猫は眠る。",
    "一と二と三。文は進む。",
    "私は猫と犬と鳥を研究した。",
];

/// One document per line, the fixture repeated `scale` times.
fn write_corpus_input(directory: &Path, scale: usize) -> PathBuf {
    let mut lines = String::new();
    for copy in 0..scale {
        for text in DOCUMENTS {
            writeln!(
                lines,
                "{}",
                serde_json::json!({"text": text, "metadata": {"copy": copy.to_string()}})
            )
            .unwrap();
        }
    }
    let input = directory.join(format!("corpus-{scale}.jsonl"));
    fs::write(&input, lines).unwrap();
    input
}

struct Run {
    output: Output,
    spill_files: SpillFileCounts,
}

/// Runs `ling` with `knobs` and a soft and hard `RLIMIT_NOFILE` of `open_file_limit`.
fn ling(
    directory: &Path,
    args: &[&std::ffi::OsStr],
    knobs: &[(ScaleKnob, usize)],
    open_file_limit: u64,
) -> Run {
    let report = tempfile::NamedTempFile::new_in(directory).unwrap();
    let mut command = lingrep_std();
    command
        .args(args)
        .env(SPILL_FILE_REPORT_VARIABLE, report.path());
    for (knob, value) in knobs {
        command.env(knob.variable(), value.to_string());
    }
    let limit = libc::rlimit {
        rlim_cur: open_file_limit,
        rlim_max: open_file_limit,
    };
    // SAFETY: `setrlimit` is async-signal-safe and `limit` is a valid `rlimit`.
    unsafe {
        command.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let output = command.output().unwrap();
    let report = fs::read_to_string(report.path()).unwrap();
    let counts = report
        .split_whitespace()
        .map(|count| count.parse::<usize>().unwrap())
        .collect::<Vec<_>>();
    let [peak_open, opened] = counts[..] else {
        panic!("malformed spill-file report {report:?}: is lingrep-core's `testing` feature on?");
    };
    Run {
        output,
        spill_files: SpillFileCounts { peak_open, opened },
    }
}

fn build_corpus(
    directory: &Path,
    dictionary: &MiniDictionary,
    input: &Path,
    name: &str,
    knobs: &[(ScaleKnob, usize)],
) -> (Run, PathBuf) {
    let artifact = directory.join(name);
    let run = ling(
        directory,
        &[
            "corpus".as_ref(),
            "build".as_ref(),
            "--dict-path".as_ref(),
            dictionary.path().as_os_str(),
            "--output".as_ref(),
            artifact.as_os_str(),
            input.as_os_str(),
        ],
        knobs,
        BUILD_OPEN_FILE_LIMIT,
    );
    (run, artifact)
}

/// Every file under `root`, by relative path.
fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    files
}

/// The `pattern` and `summary` records of a JSON Lines summary, without the
/// fields that differ between any two runs.
fn summary_records(output: &Output) -> Vec<Value> {
    parse_jsonl(&output.stdout)
        .into_iter()
        .filter(|record| {
            matches!(
                record["type"].as_str(),
                Some("pattern" | "summary" | "metadata_value")
            )
        })
        .map(|mut record| {
            normalize_run_variance(&mut record);
            record
        })
        .collect()
}

/// What `summarize` publishes, compared between layouts.
#[derive(Debug, PartialEq)]
struct Published {
    records: Vec<Value>,
    table: Option<BTreeMap<PathBuf, Vec<u8>>>,
}

enum Summary {
    /// `--by-metadata` over a catalog with no window path.
    Records,
    /// `--write-table` over the same catalog.
    Table,
    /// The shipped n-gram catalog, counted on the token-window path.
    Windows,
}

const TABLE_CATALOG: &str = "cat := [text=猫].\n\
    key: [text]\n\
    pattern word := [].\n\
    pattern pair := [] [].\n";

fn summarize(
    directory: &Path,
    artifact: &Path,
    summary: &Summary,
    name: &str,
    knobs: &[(ScaleKnob, usize)],
) -> (Run, Published) {
    let catalog = directory.join("table.lx");
    fs::write(&catalog, TABLE_CATALOG).unwrap();
    let ngrams = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../patterns/ngrams.lx");
    let table = directory.join(format!("{name}.lingtable"));
    let workers = WORKERS.to_string();
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "--json".as_ref(),
        "summarize".as_ref(),
        "--workers".as_ref(),
        workers.as_ref(),
        "--corpus".as_ref(),
        artifact.as_os_str(),
        "--patterns".as_ref(),
    ];
    match summary {
        Summary::Records => args.extend([
            catalog.as_os_str(),
            "--top".as_ref(),
            "all".as_ref(),
            "--by-metadata".as_ref(),
            "@document.copy".as_ref(),
        ]),
        Summary::Table => args.extend([
            catalog.as_os_str(),
            "--write-table".as_ref(),
            table.as_os_str(),
        ]),
        Summary::Windows => args.extend([ngrams.as_os_str(), "--top".as_ref(), "all".as_ref()]),
    }
    let run = ling(directory, &args, knobs, SUMMARY_OPEN_FILE_LIMIT);
    assert_success(&run.output);
    let published = Published {
        records: summary_records(&run.output),
        table: matches!(summary, Summary::Table).then(|| tree_bytes(&table)),
    };
    (run, published)
}

/// Summary containers besides the shards: one per `--by-metadata`.
fn tallies(summary: &Summary) -> usize {
    usize::from(matches!(summary, Summary::Records))
}

/// One record per sentence's spill: a run per distinct key, merged `fan_in`
/// at a time, and every matcher batch over its output budget.
fn shrunk(fan_in: Option<usize>) -> Vec<(ScaleKnob, usize)> {
    let mut knobs = vec![
        (ScaleKnob::SummaryResidentKeys, 1),
        (ScaleKnob::WindowRunRecords, 8),
        (ScaleKnob::MatchOutputBytes, 16 * 1024),
    ];
    knobs.extend(fan_in.map(|fan_in| (ScaleKnob::SummaryMergeFanIn, fan_in)));
    knobs
}

/// The layout `summarize` holds its spill files to, at `fan_in`.
fn layout(fan_in: Option<usize>) -> SpillLayout {
    let mut layout = SpillLayout::new(WORKERS);
    if let Some(fan_in) = fan_in {
        layout.merge_fan_in = fan_in;
    }
    layout
}

fn assert_bounded_at_every_scale(summary: &Summary) {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    for scale in SCALES {
        let input = write_corpus_input(directory.path(), scale);
        let (built, artifact) = build_corpus(
            directory.path(),
            &dictionary,
            &input,
            &format!("corpus-{scale}.lingcorpus"),
            &[],
        );
        assert_success(&built.output);
        let (_, expected) = summarize(
            directory.path(),
            &artifact,
            summary,
            &format!("default-{scale}"),
            &[],
        );
        assert!(!expected.records.is_empty() || expected.table.is_some());
        for fan_in in [None, Some(2)] {
            let (run, published) = summarize(
                directory.path(),
                &artifact,
                summary,
                &format!("shrunk-{scale}-{fan_in:?}"),
                &shrunk(fan_in),
            );
            assert!(
                published == expected,
                "scale {scale}, fan-in {fan_in:?}: the spill layout changed what summarize published"
            );
            let bound = layout(fan_in).files_open_at_once(tallies(summary));
            let counts = run.spill_files;
            assert!(
                counts.peak_open <= bound,
                "scale {scale}, fan-in {fan_in:?}: {} spill files open at once, more than {bound}",
                counts.peak_open
            );
            if scale == 100 {
                assert!(
                    counts.opened > 4 * bound,
                    "scale {scale}, fan-in {fan_in:?}: only {} spill files opened; the fixture \
                     does not reach the spill paths",
                    counts.opened
                );
            }
        }
    }
}

#[test]
fn summary_records_hold_bounded_spill_files_at_every_scale() {
    assert_bounded_at_every_scale(&Summary::Records);
}

#[test]
fn write_table_holds_bounded_spill_files_at_every_scale() {
    assert_bounded_at_every_scale(&Summary::Table);
}

#[test]
fn window_summary_holds_bounded_spill_files_at_every_scale() {
    assert_bounded_at_every_scale(&Summary::Windows);
}

#[test]
fn corpus_build_fits_a_fixed_open_file_limit_and_postings_runs_keep_the_artifact() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    for scale in SCALES {
        let input = write_corpus_input(directory.path(), scale);
        let (default, expected) = build_corpus(
            directory.path(),
            &dictionary,
            &input,
            &format!("default-{scale}.lingcorpus"),
            &[],
        );
        assert_success(&default.output);
        let (shrunk, actual) = build_corpus(
            directory.path(),
            &dictionary,
            &input,
            &format!("shrunk-{scale}.lingcorpus"),
            &[(ScaleKnob::PostingsRunBytes, 64)],
        );
        assert_success(&shrunk.output);
        assert!(
            tree_bytes(&actual) == tree_bytes(&expected),
            "scale {scale}: postings runs changed the artifact"
        );
    }
}
