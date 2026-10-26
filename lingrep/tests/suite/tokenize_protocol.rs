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

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Output, Stdio};

use lingrep::protocol::{ProtocolError, validate_tokenize_jsonl};
use lingrep_core::testing::MiniDictionary;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{event_types, lingrep, lingrep_std, parse_jsonl};

fn run_tokenize(root: &std::path::Path, extra: &[&str]) -> Output {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep();
    command
        .arg("--json")
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path());
    command.args(extra).arg(root).output().unwrap()
}

fn sentence_events(bytes: &[u8]) -> Vec<Value> {
    parse_jsonl(bytes)
        .into_iter()
        .filter(|event| event["type"] == "sentence")
        .collect()
}

#[test]
fn tokenize_limits_whole_sentences_and_reports_prefix_coverage() {
    let directory = TempDir::new().unwrap();
    let dictionary = MiniDictionary::new();
    let text = "猫。\n犬。\n鳥。\n";
    let plain = directory.path().join("sentences.txt");
    let markdown = directory.path().join("sentences.md");
    let artifact = directory.path().join("sentences.lingcorpus");
    std::fs::write(&plain, text).unwrap();
    std::fs::write(&markdown, text).unwrap();
    lingrep()
        .args(["corpus", "build", "--dict-path"])
        .arg(dictionary.path())
        .arg("--output")
        .arg(&artifact)
        .arg(&plain)
        .assert()
        .success();

    for (name, path) in [
        ("plain", plain.as_path()),
        ("markdown", markdown.as_path()),
        ("stdin", std::path::Path::new("-")),
        ("prepared", artifact.as_path()),
    ] {
        let run = |limit: &str| {
            let mut command = lingrep();
            command.args(["tokenize", "--json", "--limit", limit]);
            if name == "prepared" {
                command.arg("--corpus").arg(path);
            } else {
                command.arg("--dict-path").arg(dictionary.path()).arg(path);
                if name == "stdin" {
                    command.write_stdin(text);
                }
            }
            command.output().unwrap()
        };
        let unlimited = run("all");
        assert!(unlimited.status.success(), "{name}");
        validate_tokenize_jsonl(&unlimited.stdout).unwrap();
        let all_sentences = sentence_events(&unlimited.stdout);
        assert_eq!(all_sentences.len(), 3, "{name}");
        for (limit, expected, coverage, termination) in [
            ("2", 2, "prefix", "result_limit"),
            ("3", 3, "prefix", "result_limit"),
            ("4", 3, "full", "eof"),
        ] {
            let output = run(limit);
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            validate_tokenize_jsonl(&output.stdout).unwrap();
            let mut actual = sentence_events(&output.stdout);
            let mut expected_sentences = all_sentences[..expected].to_vec();
            if name == "stdin" {
                for event in actual.iter_mut().chain(&mut expected_sentences) {
                    event["data"].as_object_mut().unwrap().remove("stream_id");
                }
            }
            assert_eq!(actual, expected_sentences, "{name}, limit {limit}");
            let events = parse_jsonl(&output.stdout);
            let complete = &events.last().unwrap()["data"];
            assert_eq!(complete["coverage"], coverage, "{name}, limit {limit}");
            assert_eq!(
                complete["termination"], termination,
                "{name}, limit {limit}"
            );
            assert_eq!(complete["sentences_emitted"], expected, "{name}");
        }
    }
}

#[test]
fn human_tokenize_defaults_to_one_hundred_sentences_and_all_removes_the_cap() {
    let directory = TempDir::new().unwrap();
    let dictionary = MiniDictionary::new();
    let corpus = directory.path().join("sentences.txt");
    std::fs::write(&corpus, "猫。\n".repeat(101)).unwrap();
    for (extra, expected) in [(vec![], 100), (vec!["--limit", "all"], 101)] {
        let output = lingrep()
            .args(["tokenize", "--dict-path"])
            .arg(dictionary.path())
            .args(extra)
            .arg(&corpus)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("sentence "))
                .count(),
            expected
        );
    }
}

#[test]
fn tokenize_tsv_process_smoke_emits_escaped_fixed_rows() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("doc\tname.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();

    let output = lingrep()
        .args(["tokenize", "--tsv", "--dict-path"])
        .arg(dictionary.path())
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let rows = String::from_utf8(output.stdout).unwrap();
    let rows = rows.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    for (token_index, row) in rows.iter().enumerate() {
        assert!(
            row.starts_with(&format!("0\t\"doc\tname.txt\"\t0\t0\t{token_index}\t")),
            "{row:?}"
        );
    }
}

#[test]
fn tokenize_emits_markdown_provenance_and_sparse_tokens_without_an_pattern_catalog() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    std::fs::write(
        directory.path().join("sfe.lx"),
        "not valid lingrep patterns at all",
    )
    .unwrap();
    let corpus = directory.path().join("doc.md");
    std::fs::write(&corpus, "😀\n").unwrap();

    let output = lingrep()
        .current_dir(directory.path())
        .arg("--json")
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .args([
            "--max-traversal-depth",
            "7",
            "--max-source-entries",
            "99",
            "--max-directory-entries",
            "11",
            "--max-documents",
            "9",
            "--max-source-index-bytes",
            "100000",
            "--max-temp-bytes",
            "12345",
        ])
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_tokenize_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "sentence", "complete"]);
    assert!(events.iter().all(|event| !matches!(
        event["type"].as_str(),
        Some("pattern" | "match" | "summary")
    )));

    let run = &events[0]["data"];
    assert_eq!(run["corpus_kind"], "direct");
    assert_eq!(run["state"], "pending");
    assert!(run.get("catalog_fingerprint").is_none());
    assert!(run.get("pattern_count").is_none());
    assert_eq!(
        run["traversal_limits"],
        serde_json::json!({
            "max_depth": 7,
            "max_source_entries": 99,
            "max_directory_entries": 11,
            "max_documents": 9,
            "max_index_bytes": 100000,
        })
    );
    assert_eq!(run["temporary_storage_limits"]["max_temp_bytes"], 12345);

    let sentence = &events[1]["data"];
    assert_eq!(sentence["corpus_sentence_ordinal"], 0);
    assert_eq!(sentence["source_name"], "doc.md");
    assert_eq!(sentence["document_ordinal"], 0);
    assert_eq!(sentence["document_sentence_ordinal"], 0);
    let token = &sentence["tokens"][0];
    assert_eq!(token["index"], 0);
    assert_eq!(token["attributes"]["text"], "😀");
    assert!(token["attributes"].get("unidic.text").is_none());
    assert_eq!(token["attributes"]["unidic.lextype"], "unknown");
    assert_eq!(token["attributes"]["lemma"], "😀");
    assert_eq!(token["attributes"]["norm"], "😀");
    assert_eq!(token["attributes"]["unidic.orthbase"], "😀");
    assert!(
        token["attributes"].get("unidic.pron").is_none(),
        "attributes absent after OOV featurization must remain omitted: {}",
        token["attributes"]
    );

    let complete = &events[2]["data"];
    assert_eq!(complete["coverage"], "full");
    assert_eq!(complete["corpus_kind"], "direct");
    assert_eq!(complete["state"], "known");
    assert!(complete["source_identity"].as_str().is_some());
    assert!(complete["analysis_identity"].as_str().is_some());
    assert_eq!(complete["inventory"]["documents"], 1);
    assert_eq!(
        complete["resources"]["temporary_storage_limits"]["max_temp_bytes"],
        12345
    );
    assert_eq!(complete["sentences_emitted"], 1);
    assert_eq!(complete["tokens_emitted"], 1);
    assert_eq!(complete["termination"], "eof");
}

#[test]
fn tokenize_stdin_uses_one_run_local_stream_id_and_only_an_observed_digest_at_eof() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .arg("--json")
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("-")
        .write_stdin("猫。\n")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    validate_tokenize_jsonl(&output.stdout).unwrap();
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "sentence", "complete"]);
    let stream_id = events[0]["data"]["stream_id"].as_str().unwrap();
    assert!(!stream_id.is_empty());
    assert_eq!(events[1]["data"]["source_name"], "<stdin>");
    assert_eq!(events[1]["data"]["stream_id"], stream_id);
    assert_eq!(events[1]["data"]["document_ordinal"], 0);
    assert_eq!(events[1]["data"]["document_sentence_ordinal"], 0);
    assert_eq!(events[1]["data"]["tokens"].as_array().unwrap().len(), 2);
    let complete = events.last().unwrap();
    assert_eq!(complete["data"]["stream_id"], stream_id);
    assert!(
        complete["data"]["observed_content_digest"]
            .as_str()
            .is_some()
    );
    assert!(complete["data"].get("source_identity").is_none());
    assert!(complete["data"].get("analysis_identity").is_none());
}

#[test]
fn tokenize_stdin_honors_the_declared_markdown_reader() {
    let dictionary = MiniDictionary::new();
    let output = lingrep()
        .arg("--json")
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .args(["--input-format", "markdown", "-"])
        .write_stdin("---\ntitle: Secret\n---\n本文。\n")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sentences = sentence_events(&output.stdout);
    assert_eq!(
        sentences.len(),
        1,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let surfaces = sentences[0]["data"]["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .map(|token| token["attributes"]["text"].as_str().unwrap())
        .collect::<String>();
    assert_eq!(surfaces, "本文。");
}

#[test]
fn tokenize_stdin_sentence_stream_ids_are_scoped_to_their_own_run() {
    let run = || {
        let dictionary = MiniDictionary::new();
        lingrep()
            .arg("--json")
            .arg("tokenize")
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg("-")
            .write_stdin("猫。\n")
            .output()
            .unwrap()
    };

    let first = run();
    let second = run();
    assert!(first.status.success());
    assert!(second.status.success());
    let first = parse_jsonl(&first.stdout);
    let second = parse_jsonl(&second.stdout);
    let first_id = first[0]["data"]["stream_id"].as_str().unwrap();
    let second_id = second[0]["data"]["stream_id"].as_str().unwrap();
    assert_ne!(first_id, second_id);
    assert_eq!(first[1]["data"]["stream_id"], first_id);
    assert_eq!(second[1]["data"]["stream_id"], second_id);
}

#[test]
fn tokenize_sentence_protocol_and_identity_are_invariant_to_batching_and_workers() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("sentences.txt");
    std::fs::write(&corpus, "猫。犬。鳥。\n").unwrap();

    let one = run_tokenize(
        &corpus,
        &[
            "--workers",
            "1",
            "--batch-sentences",
            "1",
            "--batch-raw-bytes",
            "65536",
        ],
    );
    let four = run_tokenize(
        &corpus,
        &[
            "--workers",
            "4",
            "--batch-sentences",
            "3",
            "--batch-raw-bytes",
            "196608",
        ],
    );

    for output in [&one, &four] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        validate_tokenize_jsonl(&output.stdout).unwrap();
    }
    assert_eq!(sentence_events(&one.stdout), sentence_events(&four.stdout));
    let one = parse_jsonl(&one.stdout);
    let four = parse_jsonl(&four.stdout);
    assert_eq!(
        one[0]["data"]["token_schema_identity"],
        four[0]["data"]["token_schema_identity"]
    );
    for field in ["source_identity", "analysis_identity"] {
        assert_eq!(
            one.last().unwrap()["data"][field],
            four.last().unwrap()["data"][field]
        );
    }
    assert_eq!(one[0]["data"]["execution_limits"]["max_stage_sentences"], 1);
    assert_eq!(
        four[0]["data"]["execution_limits"]["max_stage_sentences"],
        3
    );
    assert_eq!(one[0]["data"]["worker_count"], 1);
    assert_eq!(
        four[0]["data"]["worker_count"], 1,
        "serial tokenization must report its effective worker count"
    );
}

#[test]
fn tokenize_rejects_physical_records_before_parsing_and_tokenized_residency_before_publication() {
    let directory = TempDir::new().unwrap();
    let oversized = directory.path().join("oversized.jsonl");
    std::fs::write(
        &oversized,
        format!("{{\"sentences\":[\"{}\"]}}\n", "x".repeat(64)),
    )
    .unwrap();
    let output = run_tokenize(
        &oversized,
        &["--max-sentence-bytes", "32", "--max-record-bytes", "32"],
    );

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(event_types(&parse_jsonl(&output.stdout)), ["run"]);
    assert!(matches!(
        validate_tokenize_jsonl(&output.stdout),
        Err(ProtocolError::MissingComplete)
    ));
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "input_record_too_large");
    assert_eq!(diagnostic[0]["data"]["stage"], "input");
    assert_eq!(
        diagnostic[0]["data"]["run_id"],
        parse_jsonl(&output.stdout)[0]["data"]["run_id"]
    );

    let ordinary = directory.path().join("ordinary.txt");
    std::fs::write(&ordinary, "猫。\n").unwrap();
    let output = run_tokenize(&ordinary, &["--max-tokenized-bytes", "1"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(event_types(&parse_jsonl(&output.stdout)), ["run"]);
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(
        diagnostic[0]["data"]["code"],
        "tokenized_sentence_too_large"
    );
    assert_eq!(diagnostic[0]["data"]["stage"], "input");
}

#[test]
fn invalid_filesystem_root_is_rejected_before_tokenize_run_publication() {
    let directory = TempDir::new().unwrap();
    let missing = directory.path().join("missing.md");

    let output = run_tokenize(&missing, &[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "discovery_io");
    assert!(diagnostic[0]["data"]["run_id"].is_null());
}

#[cfg(unix)]
#[test]
fn later_discovery_failure_preserves_tokenized_sentence_prefix_without_complete() {
    use std::os::unix::fs::symlink;

    let directory = TempDir::new().unwrap();
    std::fs::write(directory.path().join("a.md"), "猫。\n").unwrap();
    symlink("missing.md", directory.path().join("z.md")).unwrap();

    let output = run_tokenize(directory.path(), &[]);

    assert_eq!(output.status.code(), Some(1));
    let events = parse_jsonl(&output.stdout);
    assert_eq!(event_types(&events), ["run", "sentence"]);
    assert_eq!(events[1]["data"]["source_name"], "a.md");
    assert!(matches!(
        validate_tokenize_jsonl(&output.stdout),
        Err(ProtocolError::MissingComplete)
    ));
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "dangling_link");
    assert_eq!(diagnostic[0]["data"]["stage"], "input");
    assert_eq!(diagnostic[0]["data"]["run_id"], events[0]["data"]["run_id"]);
}

#[test]
fn unsupported_tokenize_projection_fails_before_run() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫。\n").unwrap();

    let output = run_tokenize(&corpus, &["--tsv"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let diagnostic = parse_jsonl(&output.stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "invalid_invocation");
    assert!(diagnostic[0]["data"]["run_id"].is_null());
}

#[cfg(unix)]
#[test]
fn signal_cancels_tokenize_at_a_sentence_boundary_without_complete() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "猫。\n".repeat(100_000)).unwrap();
    let (_dictionary, mut child) = raw_tokenize_command(&corpus);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert_eq!(parse_jsonl(first.as_bytes())[0]["type"], "run");

    let result = unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    assert_eq!(result, 0);
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();

    let mut stdout = first.into_bytes();
    stdout.extend(rest);
    assert_eq!(status.code(), Some(130));
    let events = parse_jsonl(&stdout);
    assert_eq!(events[0]["type"], "run");
    assert!(
        events
            .iter()
            .skip(1)
            .all(|event| event["type"] == "sentence")
    );
}

#[cfg(unix)]
#[test]
fn closed_tokenize_stdout_is_failure_and_never_publishes_complete() {
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("many.txt");
    std::fs::write(&corpus, "猫。\n".repeat(20_000)).unwrap();
    let (_dictionary, mut child) = raw_tokenize_command(&corpus);
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert_eq!(parse_jsonl(first.as_bytes())[0]["type"], "run");
    drop(reader);

    let status = child.wait().unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();

    assert_eq!(status.code(), Some(1));
    let diagnostic = parse_jsonl(&stderr);
    assert_eq!(diagnostic[0]["data"]["code"], "broken_pipe");
    assert_eq!(diagnostic[0]["data"]["stage"], "output");
}

#[cfg(unix)]
fn raw_tokenize_command(root: &std::path::Path) -> (MiniDictionary, Child) {
    let dictionary = MiniDictionary::new();
    let mut command = lingrep_std();
    command
        .arg("--json")
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--batch-sentences")
        .arg("1")
        .arg(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    (dictionary, child)
}

/// Human `tokenize` renders the token surface, not a placeholder.
///
/// The renderer read `orth`, which is UniDic's name for that field and is in
/// no published namespace: the portable key is `text`. So the lookup missed
/// on every token of every run and printed the fallback, turning whole
/// corpora into `?`. It survived because every other test on this command
/// reads JSONL, where the attribute map is correct.
///
/// This asserts the rendered glyphs, which is the only place the defect was
/// visible.
#[test]
fn human_tokenize_rows_carry_the_token_surface() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫が鳴く。\n").unwrap();

    let output = lingrep()
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let row = stdout
        .lines()
        .find(|line| line.starts_with("sentence 0:"))
        .expect("a rendered sentence row");

    assert!(
        row.contains('猫'),
        "the surface must reach the rendered row, got {row:?}"
    );
    assert!(
        !row.split_whitespace().skip(2).any(|token| token == "?"),
        "no token may render as the missing-attribute placeholder, got {row:?}"
    );
}

/// Every content column of a `tokenize --tsv` row carries a value.
///
/// `orth`, `pos` and `cform` are UniDic/lingrep patterns-era spellings that are in no
/// published namespace — the published keys are the portable surface key,
/// `upos`, and `unidic.cform`. Three of the four content columns were
/// therefore empty on every row of every run, and `unwrap_or_default()`
/// turned that total miss into a well-formed record rather than a failure,
/// so the arity-and-escaping test that guards this row stayed green.
#[test]
fn tokenize_tsv_content_columns_are_populated() {
    let dictionary = MiniDictionary::new();
    let directory = TempDir::new().unwrap();
    let corpus = directory.path().join("one.txt");
    std::fs::write(&corpus, "猫が鳴く。\n").unwrap();

    let output = lingrep()
        .arg("tokenize")
        .arg("--dict-path")
        .arg(dictionary.path())
        .arg("--tsv")
        .arg(&corpus)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let row = stdout.lines().next().expect("a tsv row");
    let fields: Vec<&str> = row.split('\t').collect();
    assert_eq!(fields.len(), 9, "the fixed row arity is unchanged: {row:?}");

    for (index, name) in [(5, "orth"), (6, "lemma"), (7, "pos"), (8, "cform")] {
        assert!(
            !fields[index].is_empty(),
            "column {name} must carry a value, got row {row:?}"
        );
    }
}
