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

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
#[cfg(unix)]
use std::process::{Output, Stdio};
#[cfg(unix)]
use std::time::Duration;

use lingrep::cli::{Cli, Command, ProgressMode};
use lingrep::error::{
    FailureCode, Termination, exit_code, termination_for_failure, termination_for_signal,
};
use lingrep::output::ContextColorTheme;
use lingrep::presentation::{
    ExplicitLimit, OutputMode, resolve_match_context, resolve_match_limit, resolve_summary_top,
};
use lingrep::protocol::diagnostic::{DiagnosticEvent, DiagnosticStage};
use lingrep::protocol::validate_diagnostic_jsonl;

use crate::support::{fixture, lingrep, lingrep_std, parse_jsonl};

fn json_diagnostic_code(stderr: &[u8]) -> String {
    parse_jsonl(stderr)[0]["data"]["code"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn json_parse_failures_use_v3_without_embedding_rendered_help() {
    for args in [
        vec!["--json", "matchez"],
        vec!["check", "--json"],
        vec!["--json"],
    ] {
        let output = lingrep().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        validate_diagnostic_jsonl(&output.stderr).unwrap();
        let diagnostic = &parse_jsonl(&output.stderr)[0];
        assert_eq!(diagnostic["schema"], "lingrep.diagnostic/v1");
        assert!(
            !diagnostic["data"]["message"]
                .as_str()
                .unwrap()
                .contains("Usage:")
        );
        assert_eq!(diagnostic["data"]["retry"], "after_change");
    }
}

#[test]
fn json_detection_obeys_clap_boundaries_and_help_stays_human() {
    let after_delimiter = lingrep()
        .args(["check", "--pattern", "[].", "--", "--json"])
        .output()
        .unwrap();
    assert_eq!(after_delimiter.status.code(), Some(2));
    assert!(!after_delimiter.stderr.starts_with(b"{"));

    for args in [
        vec!["--json", "--help"],
        vec!["--json", "--help-all"],
        vec!["--json", "--version"],
    ] {
        let output = lingrep().args(args).output().unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.starts_with(b"{"));
    }
}

#[test]
fn crowded_command_help_leads_with_workflow_and_defers_specialized_controls() {
    let short = lingrep().args(["matches", "-h"]).output().unwrap();
    assert!(short.status.success());
    let short = String::from_utf8(short.stdout).unwrap();
    for heading in ["Catalog:", "Input:", "Output:", "Analysis:", "Execution:"] {
        assert!(short.contains(heading), "missing {heading:?}:\n{short}");
    }
    for deferred in [
        "--md-include-sections",
        "--sudachi-dict",
        "--max-traversal-depth",
    ] {
        assert!(
            !short.contains(deferred),
            "{deferred} should be long-help only:\n{short}"
        );
    }

    let long = lingrep().args(["matches", "--help-all"]).output().unwrap();
    assert!(long.status.success());
    let long = String::from_utf8(long.stdout).unwrap();
    for (heading, option) in [
        ("Markdown input:", "--md-include-sections"),
        ("Analyzer-specific:", "--sudachi-dict"),
        ("Safety limits:", "--max-traversal-depth"),
    ] {
        assert!(long.contains(heading), "missing {heading:?}:\n{long}");
        assert!(long.contains(option), "missing {option:?}:\n{long}");
    }

    let tokenize = lingrep().args(["tokenize", "--help-all"]).output().unwrap();
    assert!(tokenize.status.success());
    let tokenize = String::from_utf8(tokenize.stdout).unwrap();
    assert!(
        tokenize.contains("corpus_sentence_ordinal")
            && tokenize.contains("document_sentence_ordinal"),
        "tokenize TSV columns must be discoverable in its own help:\n{tokenize}"
    );
    assert!(
        tokenize.contains("--cache")
            && !tokenize.contains("--no-cache")
            && !tokenize.contains("--cached")
            && !tokenize.contains("--ephemeral"),
        "batch commands expose one cache boolean:\n{tokenize}"
    );

    let explore = lingrep().args(["explore", "--help"]).output().unwrap();
    assert!(explore.status.success());
    let explore = String::from_utf8(explore.stdout).unwrap();
    assert!(
        !explore.contains("--cache")
            && !explore.contains("--no-cache")
            && !explore.contains("--cached")
            && !explore.contains("--ephemeral"),
        "interactive execution must not advertise cache controls it does not honor:\n{explore}"
    );
    for unsupported in ["--where", "--analyzer", "--profile", "--segmentation"] {
        assert!(
            !explore.contains(unsupported),
            "explore must not advertise an option it does not honor: {unsupported}\n{explore}"
        );
    }

    assert!(
        !tokenize.contains("--where"),
        "tokenize must not advertise document selection it does not apply:\n{tokenize}"
    );
}

#[test]
fn workspace_and_nix_expose_only_lingrep() {
    let workspace = std::fs::read_to_string("../Cargo.toml").unwrap();
    let flake = std::fs::read_to_string("../flake.nix").unwrap();
    assert!(!workspace.contains("\"vp-pattern-matcher\""));
    assert!(!flake.contains("vp-pattern-matcher"));

    let metadata = std::process::Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        metadata.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "lingrep")
        .unwrap();
    let bins: Vec<&str> = package["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|target| {
            target["kind"]
                .as_array()
                .unwrap()
                .iter()
                .any(|kind| kind == "bin")
        })
        .map(|target| target["name"].as_str().unwrap())
        .collect();
    assert_eq!(bins, ["ling"]);

    let help = lingrep().arg("--help").output().unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for command in [
        "check",
        "matches",
        "summarize",
        "corpus",
        "tokenize",
        "explore",
    ] {
        assert!(help.contains(command), "{command}");
    }
}

#[test]
fn nix_package_generates_completions_and_manpages_from_the_binary() {
    let flake = std::fs::read_to_string("../flake.nix").unwrap();
    assert!(flake.contains("\"$out/bin/ling\" manpages --output"));
    assert!(flake.contains("$out/share/man/man1"));
    assert!(flake.contains("ling-corpus-build.1"));
}

#[test]
fn cli_rejects_a_regular_file_at_the_artifact_directory_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("not-an-artifact.bin");
    std::fs::write(&path, b"not an artifact directory").unwrap();
    let output = lingrep()
        .args(["--json", "matches", "--patterns"])
        .arg(fixture("one.lx"))
        .arg("--corpus")
        .arg(&path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        json_diagnostic_code(&output.stderr),
        "expected_artifact_directory"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn public_command_tree_parses_without_legacy_top_level_mode_switches() {
    let cases: &[&[&str]] = &[
        &["check", "--pattern", "[]."],
        &["matches", "--dict-path", "dict", "--pattern", "[].", "-"],
        &["summarize", "--dict-path", "dict", "--pattern", "[].", "-"],
        &[
            "corpus",
            "build",
            "--dict-path",
            "dict",
            "--output",
            "out",
            "root",
        ],
        &["corpus", "inspect", "artifact"],
        &["corpus", "inventory", "root"],
        &["tokenize", "--dict-path", "dict", "-"],
        &["explore", "--dict-path", "dict", "--pattern", "[].", "-"],
    ];

    for args in cases {
        let parsed = Cli::try_parse_from(std::iter::once("ling").chain(args.iter().copied()));
        assert!(parsed.is_ok(), "{args:?}: {parsed:?}");
    }
}

#[test]
fn human_and_machine_presentation_defaults_are_explicit() {
    for (output, context, limit, top) in [
        (OutputMode::Human, 5, Some(100), Some(20)),
        (OutputMode::Jsonl, 0, None, None),
        (OutputMode::Tsv, 0, None, None),
    ] {
        assert_eq!(resolve_match_context(output, None), context, "{output:?}");
        assert_eq!(resolve_match_limit(output, None), limit, "{output:?}");
        assert_eq!(resolve_summary_top(output, None), top, "{output:?}");
        for context in [0, 9] {
            assert_eq!(resolve_match_context(output, Some(context)), context);
        }
        for (explicit, expected) in [
            (ExplicitLimit::All, None),
            (ExplicitLimit::Value(7), Some(7)),
        ] {
            assert_eq!(resolve_match_limit(output, Some(explicit)), expected);
            assert_eq!(resolve_summary_top(output, Some(explicit)), expected);
        }
    }
}

#[test]
fn tokenize_exposes_tsv_and_conflicts_with_json() {
    let error = Cli::try_parse_from([
        "ling",
        "--json",
        "tokenize",
        "--dict-path",
        "dict",
        "--tsv",
        "-",
    ])
    .unwrap_err();

    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("cannot be used with"));
}

#[test]
fn human_match_options_compose_and_reject_machine_output() {
    let parsed = Cli::try_parse_from([
        "ling",
        "matches",
        "--pattern",
        "[].",
        "--align-hits",
        "--morpheme-separator",
        "|",
        "--context-color-depth",
        "3",
        "--context-color-theme",
        "light",
        "-",
    ])
    .unwrap();
    let Command::Matches(args) = parsed.command else {
        panic!("expected matches")
    };
    assert!(args.align_hits);
    assert_eq!(args.morpheme_separator.as_str(), "|");
    assert_eq!(args.context_color_depth, 3);
    assert_eq!(args.context_color_theme, ContextColorTheme::Light);

    for argv in [
        vec![
            "ling",
            "--json",
            "matches",
            "--pattern",
            "[].",
            "--align-hits",
            "-",
        ],
        vec![
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--tsv",
            "--morpheme-separator",
            "|",
            "-",
        ],
        vec![
            "ling",
            "--json",
            "matches",
            "--pattern",
            "[].",
            "--morpheme-separator",
            "",
            "-",
        ],
        vec![
            "ling",
            "--json",
            "matches",
            "--pattern",
            "[].",
            "--context-color-depth",
            "5",
            "-",
        ],
        vec![
            "ling",
            "--json",
            "matches",
            "--pattern",
            "[].",
            "--context-color-theme",
            "auto",
            "-",
        ],
        vec![
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--tsv",
            "--context-color-depth",
            "3",
            "-",
        ],
        vec![
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--tsv",
            "--context-color-theme",
            "light",
            "-",
        ],
    ] {
        assert_eq!(Cli::try_parse_from(argv).unwrap_err().exit_code(), 2);
    }

    let defaults = Cli::try_parse_from(["ling", "matches", "--pattern", "[].", "-"]).unwrap();
    let Command::Matches(defaults) = defaults.command else {
        panic!("expected matches")
    };
    assert_eq!(defaults.morpheme_separator.as_str(), "");
    assert_eq!(defaults.context_color_depth, 5);
    assert_eq!(defaults.context_color_theme, ContextColorTheme::Auto);
    assert!(Cli::try_parse_from(["ling", "--json", "matches", "--pattern", "[].", "-"]).is_ok());
    assert_eq!(
        Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--context-colors",
            "bright-cyan",
            "-",
        ])
        .unwrap_err()
        .exit_code(),
        2
    );
    for separator in [" ", "　"] {
        assert!(
            Cli::try_parse_from([
                "ling",
                "matches",
                "--pattern",
                "[].",
                "--morpheme-separator",
                separator,
                "-",
            ])
            .is_ok()
        );
    }
}

#[test]
fn unsafe_morpheme_separators_are_usage_errors() {
    for separator in ["\n", "\u{0085}", "\u{202e}", "\u{2028}", "\u{2029}"] {
        let error = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--morpheme-separator",
            separator,
            "missing-input",
        ])
        .unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("terminal-unsafe"));
    }
}

/// `--trace-policy` is refused with `--raw-matches`, before the run.
///
/// Raw publication applies no policy, so there is nothing to explain. Accepting
/// the pair and printing an empty trace would answer a researcher's question
/// with silence when the honest answer is that they asked the wrong command.
#[test]
fn matches_tracing_conflicts_with_raw_matches() {
    let error = Cli::try_parse_from([
        "ling",
        "matches",
        "--patterns",
        "catalog.lx",
        "--trace-policy",
        "10",
        "--raw-matches",
        "-",
    ])
    .unwrap_err();

    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("cannot be used with"));
}

/// The two flags whose names suggest each other's job are not the same flag.
///
/// `--plan` validates the resolved invocation and never scans;
/// `--trace-policy` scans and reports per record. Nothing stops a caller asking
/// for both, and the pair is not contradictory — the run stops before matching,
/// so no trace is produced. This pins that they parse independently, which is
/// what makes the help text's distinction the only thing a reader has to learn.
#[test]
fn matches_explain_and_trace_policy_are_independent() {
    let cli = Cli::try_parse_from(["ling", "matches", "--patterns", "catalog.lx", "--plan", "-"])
        .expect("--plan alone parses");
    let Command::Matches(args) = cli.command else {
        panic!("expected the matches subcommand")
    };
    assert!(args.plan);
    assert_eq!(args.trace_policy, None, "--plan asks for no trace");
}

#[test]
fn traversal_defaults_and_explicit_progress_are_stable() {
    let cli = Cli::try_parse_from([
        "ling",
        "corpus",
        "inventory",
        "--progress",
        "always",
        "corpus",
    ])
    .unwrap();
    let args = cli.corpus_sources().unwrap();
    assert_eq!(args.traversal.max_traversal_depth, 64);
    assert_eq!(args.traversal.max_source_entries, 100_000_000);
    assert_eq!(args.traversal.max_directory_entries, 1_000_000);
    assert_eq!(args.traversal.max_documents, 10_000_000);
    assert_eq!(args.traversal.max_source_index_bytes, 536_870_912);
    assert!(matches!(args.progress, ProgressMode::Always));

    let cli = Cli::try_parse_from(["ling", "corpus", "inventory", "corpus"]).unwrap();
    assert!(matches!(
        cli.corpus_sources().unwrap().progress,
        ProgressMode::Auto
    ));
}

#[test]
fn every_zero_traversal_bound_is_rejected_by_clap() {
    for flag in [
        "--max-traversal-depth",
        "--max-source-entries",
        "--max-directory-entries",
        "--max-documents",
        "--max-source-index-bytes",
    ] {
        let error =
            Cli::try_parse_from(["ling", "corpus", "inventory", flag, "0", "corpus"]).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{flag}");
    }
}

#[test]
fn every_zero_execution_bound_is_rejected_by_clap() {
    for flag in [
        "--max-sentence-bytes",
        "--max-record-bytes",
        "--batch-sentences",
        "--batch-raw-bytes",
        "--max-tokenized-bytes",
        "--max-temp-bytes",
        "--workers",
    ] {
        let error = Cli::try_parse_from([
            "ling",
            "matches",
            "--dict-path",
            "dict",
            "--pattern",
            "[].",
            flag,
            "0",
        ])
        .unwrap_err();
        assert_eq!(error.exit_code(), 2, "{flag}");
    }

    for (command, flag) in [("matches", "--limit"), ("summarize", "--top")] {
        let error = Cli::try_parse_from([
            "ling",
            command,
            "--dict-path",
            "dict",
            "--pattern",
            "[].",
            flag,
            "0",
        ])
        .unwrap_err();
        assert_eq!(error.exit_code(), 2, "{command} {flag}");
    }
}

#[test]
fn parser_projects_typed_execution_configuration() {
    let cli = Cli::try_parse_from([
        "ling",
        "matches",
        "--dict-path",
        "dict",
        "--pattern",
        "[].",
        "--max-traversal-depth",
        "3",
        "--batch-sentences",
        "7",
        "--progress",
        "always",
        "-",
    ])
    .unwrap();
    let config = cli.execution_config().unwrap();

    assert_eq!(config.traversal.max_depth, 3);
    assert_eq!(config.limits.max_stage_sentences, 7);
    assert_eq!(config.progress, ProgressMode::Always);
}

/// A record bound and a batch budget are separate quantities: the default
/// record bound is not the stage input budget, and neither flag moves the
/// other.
#[test]
fn the_record_bound_is_independent_of_the_stage_batch_budget() {
    let defaults = Cli::try_parse_from([
        "ling",
        "matches",
        "--dict-path",
        "dict",
        "--pattern",
        "[].",
        "-",
    ])
    .unwrap()
    .execution_config()
    .unwrap();
    assert_eq!(
        defaults.limits.max_input_record_bytes,
        lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES
    );
    assert_eq!(
        defaults.limits.max_stage_input_bytes,
        lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES
    );
    assert_ne!(
        defaults.limits.max_input_record_bytes,
        defaults.limits.max_stage_input_bytes
    );

    let raised = Cli::try_parse_from([
        "ling",
        "matches",
        "--dict-path",
        "dict",
        "--pattern",
        "[].",
        "--max-record-bytes",
        "4194304",
        "-",
    ])
    .unwrap()
    .execution_config()
    .unwrap();
    assert_eq!(raised.limits.max_input_record_bytes, 4 * 1024 * 1024);
    assert_eq!(
        raised.limits.max_stage_input_bytes,
        lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES,
        "raising the record bound must not enlarge an execution batch"
    );
}

#[test]
fn explicit_sentence_and_record_limits_are_independent() {
    let config = Cli::try_parse_from([
        "ling",
        "matches",
        "--pattern",
        "[].",
        "--max-sentence-bytes",
        "65536",
        "--max-record-bytes",
        "1024",
        "--batch-raw-bytes",
        "16",
        "-",
    ])
    .unwrap()
    .execution_config()
    .unwrap();
    assert_eq!(config.limits.max_sentence_bytes, 65536);
    assert_eq!(config.limits.max_input_record_bytes, 1024);
    assert_eq!(config.limits.max_stage_input_bytes, 16);
}

#[test]
fn unsupported_protocol_version_exits_two_before_stdout() {
    let output = lingrep()
        .args(["--protocol-version", "2", "check", "--pattern", "[]."])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn check_has_single_object_success_and_empty_stdout_on_compile_error() {
    let success = lingrep()
        .args(["--json", "check", "--pattern", "[]."])
        .output()
        .unwrap();
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let lines = success
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(lines[0]).unwrap()["schema"],
        "lingrep.check/v1"
    );

    let failure = lingrep()
        .args(["check", "--pattern", "[text=/unterminated"])
        .output()
        .unwrap();
    assert_eq!(failure.status.code(), Some(2));
    assert!(failure.stdout.is_empty());
    assert!(!failure.stderr.is_empty());
}

#[cfg(unix)]
fn signal_check_while_catalog_input_is_blocked(signal: i32) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let catalog = directory.path().join("catalog.lx");
    let catalog_c = CString::new(catalog.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(catalog_c.as_ptr(), 0o600) }, 0);

    let child = lingrep_std()
        .args(["check", "--patterns"])
        .arg(&catalog)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut catalog_writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&catalog)
        .unwrap();
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    // The FIFO keeps compilation and publication impossible while the signal
    // iterator transfers the already-delivered signal to the atomic arbiter.
    std::thread::sleep(Duration::from_millis(50));
    catalog_writer.write_all(b"p := [].\n").unwrap();
    drop(catalog_writer);
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
#[test]
fn shared_wrapper_preserves_pid_for_signal_exit_codes() {
    for (signal, expected) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let output = signal_check_while_catalog_input_is_blocked(signal);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn check_completion_winner_ignores_a_signal_observed_after_success_output() {
    let mut child = lingrep_std()
        .args(["--json", "check", "--pattern", "[]."])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first = String::new();
    assert_ne!(reader.read_line(&mut first).unwrap(), 0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&first).unwrap()["schema"],
        "lingrep.check/v1"
    );

    let killed = unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    if killed != 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[cfg(unix)]
#[test]
fn check_output_failure_after_completion_claim_exits_one() {
    let Some(failing_stdout) = crate::support::always_failing_stdout() else {
        return;
    };
    let output = lingrep_std()
        .args(["check", "--pattern", "[]."])
        .stdout(failing_stdout)
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn check_descriptors_preserve_each_authored_expression_and_span() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = directory.path().join("two.lx");
    let source = "a := [text=a].\nb := [text=b].\n";
    std::fs::write(&catalog, source).unwrap();

    let output = lingrep()
        .args(["--json", "check", "--patterns", catalog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    let patterns = value["data"]["definitions"].as_array().unwrap();
    assert_eq!(patterns[0]["body"]["expression"], "[text=a]");
    assert_eq!(patterns[1]["body"]["expression"], "[text=b]");
    assert_eq!(
        &source[patterns[0]["source_span"]["start"].as_u64().unwrap() as usize
            ..patterns[0]["source_span"]["end"].as_u64().unwrap() as usize],
        "a := [text=a]."
    );
    assert_eq!(
        &source[patterns[1]["source_span"]["start"].as_u64().unwrap() as usize
            ..patterns[1]["source_span"]["end"].as_u64().unwrap() as usize],
        "b := [text=b]."
    );
}

#[test]
fn inline_catalog_rejects_zero_multiple_and_fragment_declarations() {
    for (inline, case) in [
        ("", "zero definitions"),
        ("fragment helper := [].", "fragment only"),
        ("a := [].\nb := [].", "multiple public definitions"),
        (
            "a := [].\nfragment helper := [].",
            "public definition plus fragment",
        ),
    ] {
        let output = lingrep()
            .args(["check", "--pattern", inline])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{case}");
        assert!(output.stdout.is_empty(), "{case}");
        assert!(!output.stderr.is_empty(), "{case}");
    }
}

#[test]
fn human_check_success_and_error_use_real_line_framing() {
    let success = lingrep()
        .args(["check", "--pattern", "ok := []."])
        .output()
        .unwrap();
    assert!(success.status.success());
    assert!(success.stdout.ends_with(b"\n"));
    assert!(!success.stdout.windows(8).any(|bytes| bytes == b"\\u{000a}"));

    let failure = lingrep()
        .args(["check", "--pattern", "[text=/unterminated"])
        .output()
        .unwrap();
    assert_eq!(failure.status.code(), Some(2));
    assert!(failure.stderr.ends_with(b"\n"));
    for escaped in [b"\\u{0009}".as_slice(), b"\\u{000a}", b"\\u{000d}"] {
        assert!(
            !failure
                .stderr
                .windows(escaped.len())
                .any(|bytes| bytes == escaped)
        );
    }
}

#[test]
fn selected_json_diagnostics_cover_clap_invocation_failures() {
    let cases: &[&[&str]] = &[
        &["--json", "check"],
        &[
            "matches",
            "--json",
            "--dict-path",
            "dict",
            "--pattern",
            "[].",
            "--workers",
            "0",
        ],
        &["--json", "check", "--patterns", "p.lx", "--pattern", "[]."],
        &[
            "matches",
            "--json",
            "--dict-path",
            "dict",
            "--pattern",
            "[].",
            "--dfa",
        ],
    ];

    for args in cases {
        let output = lingrep().args(*args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        validate_diagnostic_jsonl(&output.stderr).unwrap();
        let values = parse_jsonl(&output.stderr);
        assert_eq!(values.len(), 1, "{args:?}");
        let envelope = serde_json::from_value::<lingrep::protocol::diagnostic::DiagnosticEnvelope>(
            values.into_iter().next().unwrap(),
        )
        .unwrap();
        let DiagnosticEvent::Diagnostic(data) = envelope.event;
        assert_eq!(data.stage, DiagnosticStage::Invocation, "{args:?}");
        assert_eq!(data.code, "invalid_invocation", "{args:?}");
    }
}

#[test]
fn json_diagnostic_selection_does_not_reclassify_help_as_an_error() {
    let output = lingrep().args(["--json", "--help"]).output().unwrap();

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
}

#[test]
fn json_catalog_failures_preserve_compiler_codes_and_locations() {
    for (pattern, expected_code) in [
        ("[text=/unterminated", "Syntax"),
        ("p := @missing.", "UndefinedReference"),
    ] {
        let output = lingrep()
            .args(["--json", "check", "--pattern", pattern])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{pattern}");
        assert!(output.stdout.is_empty(), "{pattern}");
        validate_diagnostic_jsonl(&output.stderr).unwrap();
        let diagnostics = parse_jsonl(&output.stderr);
        assert!(!diagnostics.is_empty(), "{pattern}");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic["data"]["code"] == expected_code),
            "{pattern}: {diagnostics:?}"
        );
        for diagnostic in diagnostics {
            let data = &diagnostic["data"];
            assert_ne!(data["code"], "catalog_validation", "{pattern}");
            assert_eq!(data["stage"], "catalog", "{pattern}");
            assert_eq!(data["location"]["source_name"], "inline.lx", "{pattern}");
            assert!(
                data["location"]["source_span"]["start"].is_u64(),
                "{pattern}"
            );
            assert!(data["location"]["source_span"]["end"].is_u64(), "{pattern}");
            assert!(data["location"]["line"].as_u64().unwrap() >= 1, "{pattern}");
            assert!(
                data["location"]["column"].as_u64().unwrap() >= 1,
                "{pattern}"
            );
        }
    }
}

#[test]
fn json_catalog_failures_include_compiler_related_locations() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = directory.path().join("duplicate.lx");
    std::fs::write(&catalog, "same := [].\nsame := [].\n").unwrap();

    let output = lingrep()
        .args(["--json", "check", "--patterns", catalog.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    validate_diagnostic_jsonl(&output.stderr).unwrap();
    let diagnostics = parse_jsonl(&output.stderr);
    let duplicate = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["data"]["code"] == "DuplicateDefinition")
        .expect("duplicate-definition compiler diagnostic");
    let related = duplicate["data"]["related_locations"]
        .as_array()
        .expect("related locations");
    assert_eq!(related.len(), 1);
    assert_eq!(
        related[0]["location"]["source_name"],
        catalog.to_str().unwrap()
    );
    assert!(related[0]["location"]["source_span"]["start"].is_u64());
    assert!(related[0]["location"]["line"].as_u64().unwrap() >= 1);
    assert!(related[0]["message"].is_string());
}

#[test]
fn retired_or_conflicting_cli_shapes_exit_two() {
    let cases: &[&[&str]] = &[
        &["--pattern", "[]."],
        &["--detail", "kwic"],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--sample-sentences",
            "10",
        ],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--sample-results",
            "10",
        ],
        &["matches", "--dict-path", "d", "--pattern", "[].", "--dfa"],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--prefilter",
        ],
        &["matches", "--dict-path", "d", "--pattern", "[].", "--simd"],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--pattern-parallel",
        ],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--processing-mode",
            "raw",
        ],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--timeout",
            "10",
        ],
        &[
            "matches",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--max-results",
            "10",
        ],
        &[
            "matches",
            "--dict-path",
            "d",
            "--patterns",
            "p.lx",
            "--pattern",
            "[].",
        ],
        &[
            "matches",
            "--patterns",
            "p.lx",
            "--corpus",
            "a.lingcorpus",
            "root",
        ],
        &[
            "summarize",
            "--dict-path",
            "d",
            "--pattern",
            "[].",
            "--format",
            "aligned",
        ],
    ];
    for args in cases {
        assert_eq!(
            Cli::try_parse_from(std::iter::once("ling").chain(args.iter().copied()))
                .unwrap_err()
                .exit_code(),
            2,
            "{args:?}"
        );
    }
}

#[test]
fn catalog_and_input_conflicts_are_enforced_by_the_parser() {
    let cli = Cli::try_parse_from([
        "ling",
        "matches",
        "--pattern",
        "[].",
        "--corpus",
        "a.lingcorpus",
        "root",
    ]);
    assert_eq!(cli.unwrap_err().exit_code(), 2);

    let cli = Cli::try_parse_from([
        "ling",
        "matches",
        "--corpus",
        "a.lingcorpus",
        "--dict-path",
        "dict",
        "--pattern",
        "[].",
    ]);
    assert_eq!(cli.unwrap_err().exit_code(), 2);
}

#[test]
fn prepared_queries_do_not_inherit_a_direct_dictionary_override() {
    let cli = Cli::try_parse_from([
        "ling",
        "matches",
        "--corpus",
        "a.lingcorpus",
        "--pattern",
        "[].",
    ]);
    assert!(cli.is_ok(), "{cli:?}");
}

#[test]
fn prepared_queries_reject_direct_reader_and_analysis_options() {
    for option in [
        ["--md-exclude-sections", ""],
        ["--normalization", "none"],
        ["--analyzer", "vibrato"],
        ["--profile", "text"],
        ["--segmentation", "analyzer"],
    ] {
        let error = Cli::try_parse_from([
            "ling",
            "matches",
            "--pattern",
            "[].",
            "--corpus",
            "fixture.lingcorpus",
            option[0],
            option[1],
        ])
        .expect_err("option must be rejected");
        assert!(
            error.to_string().contains("direct input"),
            "{option:?}: {error}"
        );
    }
}

#[test]
fn command_accessor_selects_only_corpus_sources() {
    let sources = Cli::try_parse_from(["ling", "corpus", "inventory", "root"]).unwrap();
    assert!(sources.corpus_sources().is_some());

    let check = Cli::try_parse_from(["ling", "check", "--pattern", "[]."]).unwrap();
    assert!(matches!(check.command, Command::Check(_)));
    assert!(check.corpus_sources().is_none());
}

#[test]
fn termination_exit_codes_are_authoritative() {
    assert_eq!(exit_code(Termination::Success), 0);
    assert_eq!(exit_code(Termination::Usage), 2);
    assert_eq!(exit_code(Termination::Failure), 1);
    assert_eq!(exit_code(Termination::SigInt), 130);
    assert_eq!(exit_code(Termination::SigTerm), 143);
}

#[test]
fn publication_durability_uncertain_is_an_artifact_failure() {
    assert_eq!(
        termination_for_failure(FailureCode::PublicationDurabilityUncertain),
        Termination::Failure
    );
    assert_eq!(
        exit_code(termination_for_failure(
            FailureCode::PublicationDurabilityUncertain
        )),
        1
    );
}

#[cfg(unix)]
#[test]
fn process_signals_have_distinct_cooperative_exit_codes() {
    assert_eq!(
        termination_for_signal(libc::SIGINT),
        Some(Termination::SigInt)
    );
    assert_eq!(
        termination_for_signal(libc::SIGTERM),
        Some(Termination::SigTerm)
    );
    assert_eq!(termination_for_signal(libc::SIGHUP), None);
}

#[test]
fn check_compare_with_reports_drift_and_exits_zero() {
    // Drift is a notice, never an error: the exit status is unchanged
    // even when every compared name diverges.
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    let other = directory.path().join("b.lx");
    std::fs::write(&primary, "p := [text=\u{305F}].\n").unwrap();
    std::fs::write(&other, "p := [text=\u{3060}].\n").unwrap();

    let output = lingrep()
        .args([
            "--json",
            "check",
            "--patterns",
            primary.to_str().unwrap(),
            "--compare-with",
            other.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "drift must not change the exit status: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let value: serde_json::Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    // Additive fields keep the current /v2 schema string.
    assert_eq!(value["schema"], "lingrep.check/v1");
    let drift = value["data"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["name"], "p");
    let bodies = drift[0]["bodies"].as_array().unwrap();
    assert_eq!(bodies.len(), 2);
    // Each body is an OBJECT with a `carriers` key, never a bare list — the
    // node has to be able to absorb an additive field.
    assert_eq!(
        bodies[0]["carriers"][0]["catalog"],
        primary.to_str().unwrap()
    );
    assert_eq!(bodies[0]["carriers"][0]["kind"], "pattern");
    assert_eq!(bodies[1]["carriers"][0]["catalog"], other.to_str().unwrap());
}

#[test]
fn check_can_fail_when_comparison_finds_drift() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    let other = directory.path().join("b.lx");
    std::fs::write(&primary, "p := [text=た].\n").unwrap();
    std::fs::write(&other, "p := [text=だ].\n").unwrap();

    let output = lingrep()
        .args(["check", "--fail-on-drift", "--patterns"])
        .arg(&primary)
        .args(["--compare-with"])
        .arg(&other)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("drift: p"));
}

/// `--schema` answers "can this catalog bind to that token schema" without a
/// corpus: unavailable attributes are a listing (exit 0), while a dependency
/// `where` clause a schema cannot satisfy fails the check.
#[test]
fn check_schema_lists_unavailable_attributes_and_stays_successful() {
    let output = lingrep()
        .args([
            "--json",
            "check",
            "--pattern",
            "pattern p := [dep.rel=nsubj].",
            "--schema",
            "unidic-morphology",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    let binding = &value["data"]["binding"];
    assert_eq!(binding["schema"], "unidic-morphology");
    assert_eq!(binding["unavailable"][0]["attribute"], "dep.rel");
    assert_eq!(binding["unavailable"][0]["sites"][0]["definition"], "p");
    assert_eq!(
        binding["unavailable"][0]["sites"][0]["source_file"],
        "inline.lx"
    );
    assert!(binding.get("refusal").is_none());
}

#[test]
fn check_schema_binds_a_dependency_catalog_cleanly_against_conllu() {
    let output = lingrep()
        .args([
            "--json",
            "check",
            "--pattern",
            "pattern p := [dep.rel=nsubj].",
            "--schema",
            "conllu",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["data"]["binding"]["unavailable"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn check_schema_refuses_a_scalar_key_over_a_conllu_feature_set() {
    let output = lingrep()
        .args([
            "check",
            "--pattern",
            "key: [feat.Tense]\npattern p := [feat.Tense has Past].",
            "--schema",
            "conllu",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("binding conllu: refused"), "{stdout}");
    assert!(stdout.contains("feat.Tense"), "{stdout}");
    assert!(stdout.contains("requires scalar values"), "{stdout}");
}

#[test]
fn check_schema_fails_when_a_where_clause_cannot_bind() {
    let output = lingrep()
        .args([
            "check",
            "--pattern",
            "pattern p := (?<s> [text=猫]) (?<v> [text=走る]) where $v -[nsubj]-> $s.",
            "--schema",
            "unidic-morphology",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("binding unidic-morphology: refused"),
        "{stdout}"
    );
    assert!(stdout.contains("dep.head"), "{stdout}");
}

#[test]
fn check_compare_with_identical_catalogs_emits_no_drift_field() {
    // `skip_serializing_if` keeps a no-drift run byte-identical to today's
    // output, which is what makes the golden compatible.
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    let other = directory.path().join("b.lx");
    let source = "p := [text=\u{305F}].\n";
    std::fs::write(&primary, source).unwrap();
    std::fs::write(&other, source).unwrap();

    let output = lingrep()
        .args([
            "check",
            "--patterns",
            primary.to_str().unwrap(),
            "--compare-with",
            other.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let line = String::from_utf8(output.stdout).unwrap();
    assert!(
        !line.contains("drift"),
        "a no-drift run must not carry the key at all: {line}"
    );
}

#[test]
fn check_without_compare_with_is_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    std::fs::write(&primary, "p := [text=\u{305F}].\n").unwrap();

    let output = lingrep()
        .args(["check", "--patterns", primary.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let line = String::from_utf8(output.stdout).unwrap();
    assert!(!line.contains("drift"), "{line}");
}

#[test]
fn check_compare_with_a_broken_catalog_is_a_compile_error() {
    // Drift over a broken catalog is meaningless: the compared catalog takes
    // the same path as the primary.
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    let other = directory.path().join("b.lx");
    std::fs::write(&primary, "p := [text=\u{305F}].\n").unwrap();
    std::fs::write(&other, "p := @missing.\n").unwrap();

    let output = lingrep()
        .args([
            "check",
            "--patterns",
            primary.to_str().unwrap(),
            "--compare-with",
            other.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn check_compare_with_rejects_an_inline_primary() {
    // An inline catalog has no stable name to report as a drift carrier.
    let directory = tempfile::tempdir().unwrap();
    let other = directory.path().join("b.lx");
    std::fs::write(&other, "p := [text=\u{3060}].\n").unwrap();

    let output = lingrep()
        .args([
            "check",
            "--pattern",
            "p := [text=\u{305F}].",
            "--compare-with",
            other.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn check_compare_with_is_repeatable_and_human_format_names_each_carrier() {
    let directory = tempfile::tempdir().unwrap();
    let primary = directory.path().join("a.lx");
    let second = directory.path().join("b.lx");
    let third = directory.path().join("c.lx");
    std::fs::write(&primary, "p := [text=\u{305F}].\n").unwrap();
    std::fs::write(&second, "p := [text=\u{3060}].\n").unwrap();
    std::fs::write(&third, "p := [text=\u{305F}].\n").unwrap();

    let output = lingrep()
        .args([
            "check",
            "--patterns",
            primary.to_str().unwrap(),
            "--compare-with",
            second.to_str().unwrap(),
            "--compare-with",
            third.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("drift: p has 2 bodies"), "{text}");
    // a.lx and c.lx share a body; b.lx carries the other.
    assert!(text.contains(primary.to_str().unwrap()), "{text}");
    assert!(text.contains(third.to_str().unwrap()), "{text}");
}

/// The common case: no catalog at all. `CatalogArgs` is `required = true`,
/// so flattening it here would reject this before `run` ever sees it.
#[test]
fn attributes_accepts_a_bare_coverage_run() {
    assert!(Cli::try_parse_from(["ling", "coverage", "corpus.jsonl"]).is_ok());
}

/// Two record shapes in one TSV stream is the coupling this project's review
/// lens rejects, and silently dropping the exclusion table would be worse
/// than refusing it.
/// Asserted against the binary, not `try_parse_from`: clap's generated
/// conflict text names only the two flags, so the refusal that names the
/// recovery is `run`'s and is reachable only through a real invocation.
#[test]
fn attributes_refuses_tsv_together_with_a_catalog() {
    let output = lingrep()
        .args(["coverage", "--tsv", "--patterns", "x.lx", "corpus.jsonl"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("--json"),
        "must name the recovery: {stderr}"
    );
}

#[test]
fn attributes_accepts_tsv_without_a_catalog() {
    assert!(Cli::try_parse_from(["ling", "coverage", "--tsv", "corpus.jsonl"]).is_ok());
}

#[test]
fn attributes_accepts_a_catalog_without_tsv() {
    assert!(
        Cli::try_parse_from(["ling", "coverage", "--patterns", "x.lx", "corpus.jsonl"]).is_ok()
    );
}

#[test]
fn attributes_refuses_two_catalog_spellings_at_once() {
    assert!(
        Cli::try_parse_from([
            "ling",
            "coverage",
            "--patterns",
            "x.lx",
            "--pattern",
            "p := [].",
            "corpus.jsonl",
        ])
        .is_err()
    );
}

#[test]
fn corpus_build_output_is_optional_and_defaults_to_none() {
    use lingrep::cli::{CorpusArgs, CorpusCommand};

    let cli = Cli::try_parse_from(["ling", "corpus", "build", "--dict-path", "d", "root"]).unwrap();
    match &cli.command {
        Command::Corpus(CorpusArgs {
            command: CorpusCommand::Build(args),
        }) => assert_eq!(
            args.output, None,
            "omitted --output means the canonical destination"
        ),
        other => panic!("expected corpus build, got {other:?}"),
    }

    let cli = Cli::try_parse_from([
        "ling",
        "corpus",
        "build",
        "--dict-path",
        "d",
        "--output",
        "a.lingcorpus",
        "root",
    ])
    .unwrap();
    match &cli.command {
        Command::Corpus(CorpusArgs {
            command: CorpusCommand::Build(args),
        }) => {
            assert_eq!(args.output, Some(PathBuf::from("a.lingcorpus")))
        }
        other => panic!("expected corpus build, got {other:?}"),
    }
}
/// `--input-format` is accepted at all eight command positions and conflicts
/// with `--corpus` (an artifact
/// carries its own reader identity), and `bccwj` refuses file roots and
/// stdin with the distribution-root message.
#[test]
fn input_format_flag_is_accepted_at_all_command_positions() {
    // The six QueryInputArgs positions take `--corpus`, so clap's conflict
    // fires at parse time and proves the flag is present: an unknown flag
    // would fail with "unexpected argument" instead.
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    for command in [
        ["matches"],
        ["summarize"],
        ["tokenize"],
        ["coverage"],
        ["explore"],
    ] {
        let output = lingrep()
            .args(command)
            .arg("--dict-path")
            .arg(dictionary.path())
            .arg("--input-format")
            .arg("bccwj")
            .arg("--corpus")
            .arg("/must/not/exist.lingcorpus")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{command:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("cannot be used with"),
            "{command:?}: expected the clap conflict, got: {stderr}"
        );
    }
}

#[test]
fn bccwj_input_must_be_a_distribution_root() {
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("corpus.jsonl");
    std::fs::write(&file, "{}\n").unwrap();

    // A file root is refused, with the message, on the corpus commands.
    for args in [
        vec!["corpus", "build", "--input-format", "bccwj"],
        vec!["corpus", "inventory", "--input-format", "bccwj"],
    ] {
        let mut command = lingrep();
        command.args(&args);
        if args[1] == "build" {
            command.arg("--dict-path").arg(dictionary.path());
        }
        let output = command.arg(&file).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("bccwj input must be a distribution root"),
            "{args:?}: {stderr}"
        );
    }

    // Stdin is not a distribution either.
    let output = lingrep()
        .args([
            "tokenize",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--input-format",
            "bccwj",
            "-",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bccwj input must be a distribution root"),
        "{stderr}"
    );
}

#[test]
fn soranoha_input_must_be_an_archive_file() {
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let directory = tempfile::tempdir().unwrap();

    // A directory root is refused, with the message, on the corpus commands.
    for args in [
        vec!["corpus", "build", "--input-format", "soranoha"],
        vec!["corpus", "inventory", "--input-format", "soranoha"],
    ] {
        let mut command = lingrep();
        command.args(&args);
        if args[1] == "build" {
            command.arg("--dict-path").arg(dictionary.path());
        }
        let output = command.arg(directory.path()).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("soranoha input must be a bulk archive file"),
            "{args:?}: {stderr}"
        );
    }

    // Stdin is not an archive either.
    let output = lingrep()
        .args([
            "tokenize",
            "--dict-path",
            dictionary.path().to_str().unwrap(),
            "--input-format",
            "soranoha",
            "-",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("soranoha input must be a bulk archive file"),
        "{stderr}"
    );
}

#[test]
fn a_directory_without_an_override_still_resolves_to_markdown() {
    // An explicit override is required to change what a directory means. The
    // Run event's reader identity must be the Markdown-reader identity the
    // golden baseline pins.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("corpus");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.md"), "---\ntitle: 一\n---\n\n猫が来る。\n").unwrap();
    let output = lingrep()
        .args(["--json", "corpus", "inventory"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = parse_jsonl(&output.stdout);
    let run = &events[0]["data"];
    assert_eq!(
        run["reader_identity"],
        "ea21e88cc5d2a659d5320dc455846c10392dae1a031bc3fe1f8c14280e9223f1"
    );
}
#[test]
fn a_file_root_override_resolves_the_declared_format() {
    // The explicit override wins over the extension inference, and the declared
    // format is what the reader identity is derived from.
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("corpus.jsonl");
    std::fs::write(&file, "{\"sentences\":[\"一。\"]}\n").unwrap();
    let run = |extra: &[&str]| {
        let mut args = vec!["--json", "corpus", "inventory"];
        args.extend_from_slice(extra);
        let output = lingrep().args(&args).arg(&file).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        parse_jsonl(&output.stdout)[0]["data"]["reader_identity"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let inferred = run(&[]);
    let overridden = run(&["--input-format", "plain"]);
    assert_ne!(
        inferred, overridden,
        "the declared format must change the reader identity"
    );
}

#[test]
fn routine_help_keeps_advanced_options_available_by_explicit_request() {
    let check = lingrep().args(["check", "--help-all"]).output().unwrap();
    assert!(check.status.success());
    let check = String::from_utf8(check.stdout).unwrap();
    for unrelated in [
        "--workers",
        "--progress",
        "--max-traversal-depth",
        "--max-temp-bytes",
    ] {
        assert!(!check.contains(unrelated), "{check}");
    }
    for command in [
        vec!["matches"],
        vec!["summarize"],
        vec!["tokenize"],
        vec!["coverage"],
        vec!["explore"],
        vec!["corpus", "build"],
        vec!["corpus", "inventory"],
    ] {
        let help = |flag: &str| {
            let output = lingrep().args(&command).arg(flag).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        let normal = help("--help");
        let short = help("-h");
        let complete = help("--help-all");
        assert_eq!(normal, short);
        assert!(normal.contains("--help-all"));
        for flag in [
            "--batch-raw-bytes",
            "--max-source-index-bytes",
            "--context-color-theme",
            "--external-executable",
            "--trace-policy",
            "--normalization",
        ] {
            assert!(
                !normal.contains(flag),
                "{command:?} includes {flag} in ordinary help:\n{normal}"
            );
        }
        assert!(complete.contains("--max-source-index-bytes"));
        assert!(normal.contains("ROOT"));
    }
}
