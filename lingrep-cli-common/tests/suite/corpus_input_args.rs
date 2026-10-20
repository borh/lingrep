//! Contract tests for shared CLI corpus-input arguments and format resolution.
//!
//! The resolver is intentionally non-fallible: it inspects ONLY the path's
//! extension and an explicit override, never opening the path. A directory
//! (a path with no file extension) defaults to Markdown without an `is_dir()`
//! I/O call. An explicit override on a directory-shaped path is not a resolver
//! error; the resolver returns it and the eventual source consumer decides
//! whether that format admits directories.

use std::path::Path;

use clap::Parser;
use lingrep_cli_common::CorpusInputArgs;
use lingrep_core::corpus::{CorpusFormat, resolve_input_format};

#[derive(Parser)]
struct Harness {
    #[command(flatten)]
    corpus: CorpusInputArgs,
}

fn parse(arguments: &[&str]) -> Result<Harness, clap::Error> {
    let mut full = vec!["x"];
    full.extend_from_slice(arguments);
    Harness::try_parse_from(full)
}

/// The shared arguments accept an input and a lowercase format, while both
/// values remain optional for stdin format inference.
#[test]
fn corpus_input_arguments_parse_known_values_and_defaults() {
    for (case, arguments, expected_input, expected_format) in [
        (
            "explicit input and Markdown format",
            &["--input", "/tmp/corpus.md", "--input-format", "markdown"][..],
            Some(Path::new("/tmp/corpus.md")),
            Some(CorpusFormat::Markdown),
        ),
        ("stdin with inferred format", &[][..], None, None),
        (
            "lowercase JSONL format",
            &["--input-format", "jsonl"][..],
            None,
            Some(CorpusFormat::Jsonl),
        ),
    ] {
        let parsed = parse(arguments).unwrap();
        assert_eq!(
            parsed.corpus.input.as_deref(),
            expected_input,
            "case: {case}"
        );
        assert_eq!(parsed.corpus.input_format, expected_format, "case: {case}");
    }
}

#[test]
fn unknown_input_format_is_rejected() {
    assert!(parse(&["--input-format", "weird"]).is_err());
}

#[test]
fn input_format_resolution_follows_override_then_path_shape() {
    for (case, path, explicit, expected) in [
        (
            "override wins for an extensionless path",
            Some(Path::new("data")),
            Some(CorpusFormat::Jsonl),
            CorpusFormat::Jsonl,
        ),
        (
            "override wins over a Markdown extension",
            Some(Path::new("notes.md")),
            Some(CorpusFormat::Plain),
            CorpusFormat::Plain,
        ),
        ("stdin defaults to Plain", None, None, CorpusFormat::Plain),
        (
            "uppercase md extension",
            Some(Path::new("paper.MD")),
            None,
            CorpusFormat::Markdown,
        ),
        (
            "mixed-case markdown extension",
            Some(Path::new("paper.Markdown")),
            None,
            CorpusFormat::Markdown,
        ),
        (
            "lowercase jsonl extension",
            Some(Path::new("data.jsonl")),
            None,
            CorpusFormat::Jsonl,
        ),
        (
            "uppercase JSONL extension",
            Some(Path::new("data.JSONL")),
            None,
            CorpusFormat::Jsonl,
        ),
        (
            "unknown extension falls through to Plain",
            Some(Path::new("notes.txt")),
            None,
            CorpusFormat::Plain,
        ),
        (
            "extensionless directory-shaped path",
            Some(Path::new("papers")),
            None,
            CorpusFormat::Markdown,
        ),
        (
            "trailing-slash directory-shaped path",
            Some(Path::new("corpus/")),
            None,
            CorpusFormat::Markdown,
        ),
        (
            "explicit override remains valid for a directory-shaped path",
            Some(Path::new("papers")),
            Some(CorpusFormat::Jsonl),
            CorpusFormat::Jsonl,
        ),
    ] {
        assert_eq!(
            resolve_input_format(path, explicit),
            expected,
            "case: {case}"
        );
    }
}
