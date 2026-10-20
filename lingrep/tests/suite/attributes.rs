//! `ling coverage` against the real dictionary.
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

use std::io::{Read, pipe};
use std::process::Stdio;

use tempfile::TempDir;

use crate::support::{dict_path, fixture, lingrep, lingrep_std};

/// No dictionary entry can supply `pron` for this, so every attribute the
/// dictionary would have carried is genuinely absent on its tokens.
const OUT_OF_VOCABULARY: &str = "これはqwertyzxcvです";

struct Corpus {
    _directory: TempDir,
    path: std::path::PathBuf,
}

fn corpus_of(text: &str) -> Corpus {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("corpus.txt");
    std::fs::write(&path, format!("{text}\n")).unwrap();
    Corpus {
        _directory: directory,
        path,
    }
}

/// Invoke the binary over a one-sentence direct corpus. `--patterns` values
/// are resolved through `fixture()` so the test does not depend on the
/// process working directory.
fn run_on_text(args: &[&str], text: &str) -> String {
    let corpus = corpus_of(text);
    let mut command = lingrep();
    command.arg("coverage").arg("--dict-path").arg(dict_path());
    let mut arguments = args.iter().peekable();
    while let Some(argument) = arguments.next() {
        command.arg(argument);
        if *argument == "--patterns" {
            let value = arguments.next().expect("--patterns takes a value");
            command.arg(catalog_path(value));
        }
    }
    let output = command.arg(&corpus.path).output().unwrap();
    assert!(
        output.status.success(),
        "status {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn catalog_path(value: &str) -> std::path::PathBuf {
    match value.strip_prefix("fixtures/") {
        Some(name) => fixture(name),
        None => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(value),
    }
}

/// The command's reason to exist: every attribute reported, so a researcher
/// can see which are partial without writing a probe.
#[test]
fn coverage_reports_every_attribute() {
    let output = run_on_text(&["--json"], OUT_OF_VOCABULARY);

    assert_eq!(
        output
            .lines()
            .filter(|line| line.contains(r#""type":"coverage""#))
            .count(),
        31
    );
}

/// Pretokenized input already carries the values coverage is meant to count.
/// It must therefore receive the same full-schema projection as a prepared
/// artifact instead of being sent through the analyzer with no projection.
#[test]
fn catalog_free_conllu_coverage_counts_pretokenized_attributes() {
    let directory = TempDir::new().unwrap();
    let input = directory.path().join("one.conllu");
    std::fs::write(
        &input,
        "# sent_id = one\n# text = walked\n1\twalked\twalk\tVERB\tVBD\tTense=Past|VerbForm=Fin\t0\troot\t_\t_\n\n",
    )
    .unwrap();
    let output = lingrep()
        .args(["--json", "coverage", "--input-format", "conllu"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let feature = stdout
        .lines()
        .find(|line| line.contains(r#""attribute":"feat.Tense""#))
        .expect("the full CoNLL-U feature schema is reported");
    assert!(feature.contains(r#""present":1"#), "{feature}");
}

/// The only subject that exercises a non-zero absent join. Without it the
/// cross-reference is untested on the path it exists for.
#[test]
fn an_exclusion_over_a_partial_attribute_is_reported_exposed() {
    let output = run_on_text(
        &["--json", "--patterns", "fixtures/pron-exclusion.lx"],
        OUT_OF_VOCABULARY,
    );

    let exclusion = output
        .lines()
        .find(|line| line.contains(r#""type":"exclusion""#))
        .expect("the catalog has one exclusion");
    assert!(exclusion.contains(r#""status":"exposed""#), "{exclusion}");
    assert!(
        exclusion.contains(r#""attribute":"unidic.pron""#),
        "{exclusion}"
    );
    assert!(
        !exclusion.contains(r#""absent_in_corpus":0"#),
        "{exclusion}"
    );
}

/// The complement's own subject at the CLI boundary. An implementation keying
/// on "negative-looking operator" would pass every other test here.
#[test]
fn a_complemented_exclusion_is_not_reported_at_all() {
    let output = run_on_text(
        &["--json", "--patterns", "fixtures/pron-complement.lx"],
        OUT_OF_VOCABULARY,
    );

    assert!(!output.contains(r#""type":"exclusion""#), "{output}");
}

/// `double-wo.lx` uses `orthBase!=`, which depends on the surface fallback
/// making `orthbase` total under the stock dictionary.
#[test]
fn the_double_wo_catalog_is_clean_under_the_stock_dictionary() {
    let output = run_on_text(
        &["--json", "--patterns", "patterns/double-wo.lx"],
        OUT_OF_VOCABULARY,
    );

    let exclusions: Vec<&str> = output
        .lines()
        .filter(|line| line.contains(r#""type":"exclusion""#))
        .collect();
    assert!(
        !exclusions.is_empty(),
        "the catalog has exclusions to check"
    );
    for exclusion in exclusions {
        assert!(exclusion.contains(r#""status":"clean""#), "{exclusion}");
    }
}

/// `--where` must actually scope. Copying `tokenize`, which never reads
/// `args.input.scope`, would leave this admitting everything and reporting a
/// scope that is simply false.
#[test]
fn a_clause_admitting_nothing_reports_unknown() {
    let output = run_on_text(
        &[
            "--json",
            "--patterns",
            "fixtures/pron-exclusion.lx",
            "--where",
            "@document.genre = ジャンルなし",
        ],
        OUT_OF_VOCABULARY,
    );

    assert!(output.contains(r#""admitted_tokens":0"#), "{output}");
    let exclusion = output
        .lines()
        .find(|line| line.contains(r#""type":"exclusion""#))
        .expect("the exclusion is still reported");
    assert!(exclusion.contains(r#""status":"unknown""#), "{exclusion}");
}

/// `--plan` must report the catalog a real run would compile, or it
/// describes a different invocation than the one it is explaining.
#[test]
fn explain_reports_the_catalog_when_one_is_given() {
    let output = run_on_text(
        &[
            "--json",
            "--plan",
            "--patterns",
            "fixtures/pron-exclusion.lx",
        ],
        OUT_OF_VOCABULARY,
    );

    assert!(output.contains("pron-exclusion.lx"), "{output}");
}

/// The two projections the closed-pipe test below cannot observe: its reader
/// is gone before the run starts.
#[test]
fn the_tsv_and_human_projections_report_every_attribute_too() {
    let tsv = run_on_text(&["--tsv"], OUT_OF_VOCABULARY);
    assert_eq!(tsv.lines().count(), 31, "{tsv}");

    let human = run_on_text(&[], OUT_OF_VOCABULARY);
    assert_eq!(
        human.lines().filter(|line| line.starts_with("  ")).count(),
        31,
        "{human}"
    );
}

/// A closed JSONL pipe is a diagnosed failure, a closed TSV pipe is a quiet
/// failure, and closing human stdout is successful display termination.
/// Asserting only the JSONL case would leave two thirds of the documented rule
/// unproved, including the two that must not emit a diagnostic.
///
/// The reader is closed before the child is spawned rather than after reading
/// a line from it. `attributes` publishes one coverage table at the end of the
/// run, and a few kilobytes fit the pipe buffer whole, so the writer never
/// blocks: a test that read a line first was racing the child's own exit for a
/// window under a millisecond wide, and lost it whenever the reader thread was
/// descheduled. A reader that is already gone is the same condition for the
/// writer, decided before the run begins.
#[cfg(unix)]
#[test]
fn closing_stdout_distinguishes_protocol_failure_tsv_and_human_output() {
    for (flags, expected, diagnosed) in [
        (vec!["--json"], Some(1), true),
        (vec!["--tsv"], Some(1), false),
        (vec![], Some(0), false),
    ] {
        let corpus = corpus_of(OUT_OF_VOCABULARY);
        let (reader, writer) = pipe().unwrap();
        let mut command = lingrep_std();
        command.arg("coverage").arg("--dict-path").arg(dict_path());
        command
            .args(&flags)
            .arg(&corpus.path)
            .stdout(Stdio::from(writer))
            .stderr(Stdio::piped());
        drop(reader);
        let mut child = command.spawn().unwrap();

        let mut stderr = Vec::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_end(&mut stderr)
            .unwrap();
        let status = child.wait().unwrap();

        let stderr = String::from_utf8_lossy(&stderr);
        assert_eq!(status.code(), expected, "{flags:?}: {stderr}");
        // Without this the machine cases would accept an exit of 1 reached for
        // any other reason, which is most of what could go wrong here.
        assert_eq!(
            stderr.contains("broken_pipe"),
            diagnosed,
            "{flags:?}: {stderr}"
        );
    }
}

/// Both contracts must be discoverable, or the command's output is
/// undocumented. `schema` and `describe` are separate surfaces from
/// `--plan`, and only these exercise them.
#[test]
fn both_contracts_are_discoverable_from_the_cli() {
    let schema = lingrep()
        .args(["contracts", "lingrep.attributes/v1", "--json"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&schema.stdout).contains("lingrep.attributes/v1"),
        "{}",
        String::from_utf8_lossy(&schema.stdout)
    );

    let described = lingrep()
        .args(["describe", "coverage", "--json"])
        .output()
        .unwrap();
    let described = String::from_utf8_lossy(&described.stdout);
    assert!(described.contains("lingrep.attributes/v1"), "{described}");
    assert!(
        described.contains("lingrep.attributes-tsv/v1"),
        "{described}"
    );
}
