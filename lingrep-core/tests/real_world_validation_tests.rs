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
//! Opt-in real-world validation for the supported `ling` execution paths.
//!
//! The checked-in fallback makes the ignored smoke exercise the current
//! executable even when the large local corpora are absent. Performance gates
//! run only for an explicit or repository-local real corpus.
//!
//! ```bash
//! cargo build --release -p lingrep
//! cargo test -p lingrep-core --test real_world_validation_tests -- --ignored \
//!   integration_tests::test_100k_corpus_smoke_test --exact --nocapture
//! ```
//!
//! Set `LINGREP_REAL_WORLD_CORPUS_PATH` and, optionally,
//! `LINGREP_REAL_WORLD_PATTERNS_PATH` to use a corpus outside `test-caches/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const DIRECT_MIN_TOKENS_PER_SEC: u64 = 250_000;
const PREPARED_MIN_TOKENS_PER_SEC: u64 = 500_000;
const VALIDATION_WORKERS: usize = 4;

#[derive(Clone, Debug)]
struct ValidationInput {
    corpus: PathBuf,
    patterns: PathBuf,
    performance_fixture: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Counters {
    sentences: u64,
    tokens: u64,
    matches: u64,
}

#[derive(Debug)]
struct ProcessResult {
    elapsed: Duration,
    stdout: String,
    stderr: String,
    exit_code: i32,
    counters: Option<Counters>,
}

enum MatchInput<'a> {
    Direct(&'a Path),
    Prepared(&'a Path),
}

fn workspace_root() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    path.pop(); // target/
    path.pop(); // workspace root
    path
}

fn get_binary_path() -> PathBuf {
    let target = workspace_root().join("target");
    let release = target.join("release/ling");
    if release.is_file() {
        release
    } else {
        target.join("debug/ling")
    }
}

fn get_dict_path() -> PathBuf {
    std::env::var_os("LINGREP_DICTIONARY_PATH").map_or_else(
        || workspace_root().join("dictionary/optimized/unidic-cwj.dic.zst"),
        PathBuf::from,
    )
}

fn validation_input(default_name: &str) -> ValidationInput {
    let root = workspace_root();
    if let Some(corpus) = std::env::var_os("LINGREP_REAL_WORLD_CORPUS_PATH") {
        let patterns = std::env::var_os("LINGREP_REAL_WORLD_PATTERNS_PATH")
            .map_or_else(|| root.join("patterns/sfe.lx"), PathBuf::from);
        return ValidationInput {
            corpus: PathBuf::from(corpus),
            patterns,
            performance_fixture: true,
        };
    }

    let corpus = root.join("test-caches").join(default_name);
    if corpus.is_file() {
        return ValidationInput {
            corpus,
            patterns: root.join("patterns/sfe.lx"),
            performance_fixture: true,
        };
    }

    ValidationInput {
        corpus: root.join("lingrep/tests/fixtures/sentence-final.jsonl"),
        patterns: root.join("lingrep/tests/fixtures/one.lx"),
        performance_fixture: false,
    }
}

fn base_command() -> Command {
    Command::new(get_binary_path())
}

fn run_lingrep(input: &ValidationInput, source: MatchInput<'_>) -> ProcessResult {
    let mut command = base_command();
    command
        .arg("matches")
        .arg("--patterns")
        .arg(&input.patterns)
        .arg("--workers")
        .arg(VALIDATION_WORKERS.to_string())
        .arg("--batch-raw-bytes")
        .arg("262144")
        .arg("--progress")
        .arg("stderr");
    match source {
        MatchInput::Direct(corpus) => {
            command.arg("--dict-path").arg(get_dict_path()).arg(corpus);
        }
        MatchInput::Prepared(artifact) => {
            command.arg("--corpus").arg(artifact);
        }
    }

    let start = Instant::now();
    let output = command.output().unwrap_or_else(|error| {
        panic!("failed to execute {:?}: {error}", get_binary_path());
    });
    process_result(start.elapsed(), output)
}

fn process_result(elapsed: Duration, output: Output) -> ProcessResult {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let counters = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| event["type"] == "complete")
        .and_then(|event| {
            let counters = &event["data"]["counters"];
            Some(Counters {
                sentences: counters["sentences_matched"].as_u64()?,
                tokens: counters["tokens_matched"].as_u64()?,
                matches: counters["matches_found"].as_u64()?,
            })
        });
    ProcessResult {
        elapsed,
        stdout,
        stderr,
        exit_code: output.status.code().unwrap_or(-1),
        counters,
    }
}

fn build_prepared(input: &ValidationInput) -> (TempDir, PathBuf) {
    let directory = tempfile::tempdir().expect("prepared-artifact directory");
    let artifact = directory.path().join("corpus.lingcorpus");
    let output = base_command()
        .arg("corpus")
        .arg("build")
        .arg("--dict-path")
        .arg(get_dict_path())
        .arg("--output")
        .arg(&artifact)
        .arg("--encoding")
        .arg("compressed")
        .arg("--workers")
        .arg(VALIDATION_WORKERS.to_string())
        .arg("--batch-raw-bytes")
        .arg("262144")
        .arg(&input.corpus)
        .output()
        .expect("run ling corpus build");
    assert!(
        output.status.success(),
        "corpus build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (directory, artifact)
}

fn analyze_result(name: &str, result: &ProcessResult, minimum: Option<u64>) -> bool {
    let counters = result.counters.clone().unwrap_or_default();
    let throughput = if result.elapsed.is_zero() {
        0.0
    } else {
        counters.tokens as f64 / result.elapsed.as_secs_f64()
    };
    println!("  {name}");
    println!("    time: {:.2}s", result.elapsed.as_secs_f64());
    println!("    throughput: {throughput:.3} tokens/sec");
    println!("    sentences: {}", counters.sentences);
    println!("    matches: {}", counters.matches);

    if result.exit_code != 0 {
        eprintln!("    exit code: {}\n{}", result.exit_code, result.stderr);
        return false;
    }
    if result.counters.is_none() {
        eprintln!("    no ling complete event\n{}", result.stdout);
        return false;
    }
    if minimum.is_some_and(|minimum| {
        !throughput_strictly_exceeds(counters.tokens, result.elapsed, minimum)
    }) {
        eprintln!(
            "    throughput too low: {throughput:.3} (must exceed {})",
            minimum.unwrap()
        );
        return false;
    }
    true
}

fn throughput_strictly_exceeds(tokens: u64, elapsed: Duration, minimum: u64) -> bool {
    let elapsed_nanos = elapsed.as_nanos();
    if elapsed_nanos == 0 {
        return false;
    }

    // `u64::MAX * 1e9` fits in u128. Dividing before comparison avoids both
    // floating-point boundary loss and `minimum * elapsed_nanos` overflow.
    let scaled_tokens = u128::from(tokens) * 1_000_000_000;
    let whole_tokens_per_second = scaled_tokens / elapsed_nanos;
    let fractional_remainder = scaled_tokens % elapsed_nanos;
    whole_tokens_per_second > u128::from(minimum)
        || (whole_tokens_per_second == u128::from(minimum) && fractional_remainder != 0)
}

fn validate_direct(default_name: &str, label: &str) -> bool {
    let input = validation_input(default_name);
    let result = run_lingrep(&input, MatchInput::Direct(&input.corpus));
    let minimum = input
        .performance_fixture
        .then_some(DIRECT_MIN_TOKENS_PER_SEC);
    analyze_result(label, &result, minimum)
}

fn validate_direct_and_prepared() -> bool {
    let input = validation_input("memory-500.jsonl");
    let direct = run_lingrep(&input, MatchInput::Direct(&input.corpus));
    if !analyze_result("direct", &direct, None) {
        return false;
    }
    let (_directory, artifact) = build_prepared(&input);
    let prepared = run_lingrep(&input, MatchInput::Prepared(&artifact));
    if !analyze_result("compressed V5", &prepared, None) {
        return false;
    }
    if direct.counters != prepared.counters {
        eprintln!(
            "direct/prepared counters differ: {:?} != {:?}",
            direct.counters, prepared.counters
        );
        return false;
    }
    true
}

fn validate_performance() -> bool {
    let input = validation_input("memory-500.jsonl");
    if !input.performance_fixture {
        println!("real corpus absent; performance thresholds not evaluated");
        return true;
    }

    let direct = run_lingrep(&input, MatchInput::Direct(&input.corpus));
    let direct_ok = analyze_result(
        "direct performance",
        &direct,
        Some(DIRECT_MIN_TOKENS_PER_SEC),
    );
    let (_directory, artifact) = build_prepared(&input);
    let prepared = run_lingrep(&input, MatchInput::Prepared(&artifact));
    let prepared_ok = analyze_result(
        "compressed V5 performance",
        &prepared,
        Some(PREPARED_MIN_TOKENS_PER_SEC),
    );
    direct_ok && prepared_ok && direct.counters == prepared.counters
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn documented_throughput_thresholds_preserve_fractional_boundaries() {
        fn measured(tokens: u64) -> ProcessResult {
            ProcessResult {
                elapsed: Duration::from_secs(2),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                counters: Some(Counters {
                    sentences: 1,
                    tokens,
                    matches: 1,
                }),
            }
        }

        for (name, minimum, exactly_twice_minimum) in [
            (
                "documented direct threshold",
                DIRECT_MIN_TOKENS_PER_SEC,
                500_000,
            ),
            (
                "documented compressed threshold",
                PREPARED_MIN_TOKENS_PER_SEC,
                1_000_000,
            ),
        ] {
            assert!(!analyze_result(
                name,
                &measured(exactly_twice_minimum - 1),
                Some(minimum)
            ));
            assert!(!analyze_result(
                name,
                &measured(exactly_twice_minimum),
                Some(minimum)
            ));
            assert!(analyze_result(
                name,
                &measured(exactly_twice_minimum + 1),
                Some(minimum)
            ));
        }

        assert!(!throughput_strictly_exceeds(
            u64::MAX,
            Duration::MAX,
            u64::MAX
        ));
        assert!(!throughput_strictly_exceeds(u64::MAX, Duration::ZERO, 0));
    }

    #[test]
    #[ignore = "real_world"]
    fn test_100k_corpus_smoke_test() {
        let binary = get_binary_path();
        assert!(
            binary.is_file(),
            "binary not found: {binary:?}; run cargo build --release -p lingrep"
        );
        assert!(
            validate_direct("memory-500.jsonl", "100K corpus direct smoke"),
            "100K corpus smoke failed"
        );
    }

    #[test]
    #[ignore = "real_world"]
    fn test_full_validation_suite() {
        assert!(validate_direct("memory-500.jsonl", "100K corpus direct"));
        assert!(validate_direct("memory-1000.jsonl", "500K corpus direct"));
        assert!(validate_direct_and_prepared());
    }

    #[test]
    #[ignore = "real_world"]
    fn test_performance_regression_detection() {
        assert!(validate_performance());
    }

    #[test]
    #[ignore = "real_world"]
    fn test_correctness_validation() {
        assert!(validate_direct_and_prepared());
    }

    #[test]
    #[ignore = "real_world"]
    fn test_large_corpus_validation() {
        assert!(validate_direct("memory-500.jsonl", "100K corpus direct"));
        assert!(validate_direct("memory-1000.jsonl", "500K corpus direct"));
    }
}
