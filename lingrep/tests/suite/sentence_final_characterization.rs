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

use lingrep::protocol::sentence_final::SentenceFinalEnvelope;
use lingrep::protocol::validate_sentence_final_stream;
use lingrep_core::testing::MiniDictionary;
use lingrep_core::token_match::TokenInfo;

use crate::support::{
    build_single_sentence_artifact, fixture, lingrep, lingrep_std, lingrep_without_dictionary,
    parse_jsonl,
};

#[test]
fn sentence_final_jsonl_process_smoke_emits_a_valid_complete_stream() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .arg("--json")
        .args(["sentence-final", "--patterns"])
        .arg(fixture("sentence-final.lx"))
        .args(["--dict-path"])
        .arg(dictionary.path())
        .arg(fixture("sentence-final.jsonl"))
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let records = parse_jsonl(&output.stdout);
    assert_eq!(records.first().unwrap()["type"], "run");
    assert_eq!(records.last().unwrap()["type"], "complete");
    assert!(records.iter().any(|record| record["type"] == "row"));
    let envelopes = records
        .into_iter()
        .map(serde_json::from_value::<SentenceFinalEnvelope>)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    validate_sentence_final_stream(envelopes).unwrap();
}

#[test]
fn new_sentence_final_owner_publishes_the_characterized_rows() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--batch-sentences",
            "2",
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        include_bytes!("../fixtures/sentence-final.expected")
    );
}

#[test]
fn prepared_sentence_final_publishes_the_same_characterized_rows() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    std::fs::write(corpus.join("sentence-final.md"), "研究。\n研究が進む。\n").unwrap();
    let artifact = directory.path().join("sentence-final.lingcorpus");
    let build = lingrep()
        .args([
            "corpus",
            "build",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--output",
            artifact.to_str().unwrap(),
            corpus.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let output = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--corpus",
            artifact.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        include_bytes!("../fixtures/sentence-final.expected")
    );
}

#[test]
fn direct_and_prepared_sentence_final_workers_are_byte_identical() {
    let dictionary = MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("corpus");
    std::fs::create_dir(&corpus).unwrap();
    std::fs::write(corpus.join("sentence-final.md"), "研究。\n研究が進む。\n").unwrap();
    let artifact = directory.path().join("parity.lingcorpus");
    let build = lingrep()
        .args([
            "corpus",
            "build",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--output",
            artifact.to_str().unwrap(),
            corpus.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let direct = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            corpus.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let prepared = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--corpus",
            artifact.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert_eq!(direct.stdout, prepared.stdout);
    assert_eq!(
        direct.stdout,
        include_bytes!("../fixtures/sentence-final.expected")
    );
}

#[test]
fn sentence_final_catalog_failure_is_usage_error_without_protocol_output() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--pattern",
            "broken := [",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid lingrep patterns syntax"));
}

#[test]
fn sentence_final_input_failure_is_runtime_error_without_partial_rows() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            fixture("missing.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn sentence_final_stdin_matches_the_exact_direct_fixture_bytes() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "-",
        ])
        .write_stdin("研究。\n研究が進む。\n")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        include_bytes!("../fixtures/sentence-final.expected")
    );
}

#[test]
fn sentence_final_stdin_honors_the_declared_markdown_reader() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--tsv",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--input-format",
            "markdown",
            "-",
        ])
        .write_stdin("---\ntitle: Secret\n---\n研究。\n研究が進む。\n")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        include_bytes!("../fixtures/sentence-final.expected")
    );
}

#[test]
fn sentence_final_temp_exhaustion_publishes_no_rows() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .args([
            "sentence-final",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--max-temp-bytes",
            "1",
            fixture("sentence-final.jsonl").to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn sentence_final_cancellation_publishes_no_rows() {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    let dictionary = MiniDictionary::new();

    let directory = tempfile::tempdir().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "研究。\n".repeat(100_000)).unwrap();
    let mut child = lingrep_std()
        .args([
            "sentence-final",
            "--patterns",
            fixture("sentence-final.lx").to_str().unwrap(),
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--progress",
            "always",
            "--batch-sentences",
            "1",
            corpus.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut progress = String::new();
    stderr.read_line(&mut progress).unwrap();
    assert!(progress.contains("sentence-final:"), "{progress}");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let status = child.wait().unwrap();
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();

    assert_eq!(status.code(), Some(130));
    assert!(stdout.is_empty());
}

#[test]
fn prepared_sentence_final_missing_orth_is_typed_and_publishes_no_rows() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let artifact = directory.path().join("missing-orth.lingcorpus");
    let token = TokenInfo::default().with_text("fallback-must-not-appear");
    runtime.block_on(build_single_sentence_artifact(
        &artifact,
        "fallback-must-not-appear",
        std::slice::from_ref(&token),
    ));
    let patterns = directory.path().join("patterns.lx");
    std::fs::write(&patterns, "pattern any := [].\n").unwrap();

    let output = lingrep_without_dictionary()
        .args([
            "--json",
            "sentence-final",
            "--patterns",
            patterns.to_str().unwrap(),
            "--corpus",
            artifact.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic: serde_json::Value = serde_json::from_slice(
        output
            .stderr
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(diagnostic["data"]["code"], "missing_required_attribute");
}
