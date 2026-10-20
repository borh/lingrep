//! Shape test for lingrep-web's shared CLI vocabulary migration.
//!
//! `Args` must flatten `lingrep-cli-common`'s `DictArgs`/`CorpusInputArgs`: `--dict`
//! is renamed `--dict-path` (short `-d` retained), `--corpus`/`-c` is
//! renamed `--input`/`-i`. The long flag `--corpus` is later reused (task B2)
//! for `NAME=PATH` multi-corpus declarations, so only the short `-c` alias
//! stays rejected; see `corpus_flag_now_declares_additional_corpora`.

use std::fs;

use clap::Parser;
use lingrep_web::Args;
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::state::{Config, DetailLimits};
use tempfile::tempdir;

#[test]
fn dict_path_and_input_parse() {
    let args = Args::try_parse_from(["lingrep-web", "--dict-path", "d", "--input", "c.jsonl"])
        .expect("--dict-path and --input should parse");

    assert_eq!(args.dict.dict_path.to_str(), Some("d"));
    assert_eq!(
        args.input.input.as_deref().and_then(|p| p.to_str()),
        Some("c.jsonl")
    );
}

/// `--corpus` was rejected outright before this task. B2 (CLAUDE.md's `--corpus`
/// quick reference) reuses the long flag for `NAME=PATH` additional-corpus
/// declarations: parsing now succeeds, and a value with no `=` is refused by
/// `declared_corpora`, not by clap.
#[test]
fn corpus_flag_now_declares_additional_corpora() {
    let args = Args::try_parse_from(["lingrep-web", "-d", "d", "--corpus", "sci=c.jsonl"])
        .expect("--corpus NAME=PATH should parse");
    assert_eq!(args.corpus, vec!["sci=c.jsonl".to_string()]);

    let malformed = Args::try_parse_from(["lingrep-web", "-d", "d", "--corpus", "c.jsonl"])
        .expect("clap admits any string; the NAME=PATH shape is validated separately");
    assert!(
        lingrep_web::cli::declared_corpora(&malformed)
            .unwrap_err()
            .contains("NAME=PATH")
    );
}

/// A two-file Markdown directory is a first-class corpus input: with no
/// `--input-format`, the shared resolver infers Markdown and startup
/// validation accepts it (rather than rejecting the directory as a single
/// file).
#[test]
fn startup_validation_accepts_a_markdown_directory_without_input_format() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("a.md"),
        "一。
",
    )
    .unwrap();
    fs::write(
        directory.path().join("b.md"),
        "二。
",
    )
    .unwrap();
    let dictionary = directory.path().join("dict.dic.zst");
    fs::write(&dictionary, []).unwrap();

    let args = Args::try_parse_from([
        "lingrep-web",
        "--dict-path",
        dictionary.to_str().unwrap(),
        "--input",
        directory.path().to_str().unwrap(),
    ])
    .expect("a directory --input should parse");

    let input_path = args.input.input.clone().expect("--input directory");
    let config = Config {
        dict_path: args.dict.dict_path.display().to_string(),
        corpora: vec![(CorpusName::default_key(), input_path, "d".to_owned())],
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 1_000 },
        input_format: args.input.input_format,
        markdown: args.markdown.resolve().expect("flags resolve"),
    };

    config
        .validate()
        .expect("startup validation should accept a Markdown directory");
}

/// `--input-format` overrides the inferred format and flows into the resolved
/// read config (so a `.txt` corpus can be forced to JSONL, for example).
#[test]
fn input_format_override_is_carried_on_the_args() {
    let args = Args::try_parse_from([
        "lingrep-web",
        "-d",
        "d",
        "-i",
        "c.txt",
        "--input-format",
        "jsonl",
    ])
    .expect("--input-format should parse alongside --input");

    assert_eq!(
        args.input.input_format,
        Some(lingrep_core::corpus::CorpusFormat::Jsonl)
    );
}
