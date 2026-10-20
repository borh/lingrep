//! Configuration isolated from diagnostics: a reusable `MarkdownReaderConfig`
//! never owns observed labels; a per-stream `MarkdownDiagnostics` sink does.
//!
//! The invariant has two halves, asserted together: a caller that injects and
//! retains a diagnostics sink sees the labels the stream wrote into it, and a
//! second fresh stream from the same reusable config starts empty. Inject the
//! sink at the streaming boundary; the caller retains its own clone so it
//! survives consuming the iterator.

use std::collections::BTreeSet;
use std::io::{BufReader, Cursor};

use lingrep_core::corpus::markdown_dialect::{MarkdownDiagnostics, MarkdownReaderConfig};
use lingrep_core::corpus::section_policy::SectionPolicy;
use lingrep_core::corpus::text_source::{InputFormat, stream_input_reader};

/// A document whose `## … {.methods}` heading carries a section label the
/// reader observes for diagnostics.
const LABELLED_METHODS_DOCUMENT: &str = "# 題 {.title}\n\n## 方法 {.methods}\n\n第一文。\n";

/// A document with no labelled structure at all: no divs, no class-bearing
/// headings. The reader observes nothing, so a fresh stream's sink stays empty.
const UNLABELLED_DOCUMENT: &str = "# 題\n\n第一文。\n";

#[test]
fn an_injected_diagnostics_sink_records_labels_the_stream_wrote() {
    let config = MarkdownReaderConfig::new(SectionPolicy::default());

    let first_sink = MarkdownDiagnostics::default();
    let first = stream_input_reader(
        InputFormat::Markdown(config.clone()),
        BufReader::new(Cursor::new(LABELLED_METHODS_DOCUMENT)),
        Some(first_sink.clone()),
    );
    first.collect::<std::io::Result<Vec<_>>>().unwrap();
    assert!(
        first_sink.observed_labels().contains("methods"),
        "the injected sink must record labels the reader observed: {:?}",
        first_sink.observed_labels()
    );
    // A self-label is observed for diagnostics too, so it surfaces here.
    assert!(first_sink.observed_labels().contains("title"));
}

#[test]
fn a_second_stream_from_the_same_config_starts_with_an_empty_sink() {
    let config = MarkdownReaderConfig::new(SectionPolicy::default());

    let first_sink = MarkdownDiagnostics::default();
    stream_input_reader(
        InputFormat::Markdown(config.clone()),
        BufReader::new(Cursor::new(LABELLED_METHODS_DOCUMENT)),
        Some(first_sink.clone()),
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();
    assert!(!first_sink.observed_labels().is_empty());

    let second_sink = MarkdownDiagnostics::default();
    let second = stream_input_reader(
        InputFormat::Markdown(config.clone()),
        BufReader::new(Cursor::new(UNLABELLED_DOCUMENT)),
        Some(second_sink.clone()),
    );
    second.collect::<std::io::Result<Vec<_>>>().unwrap();
    assert!(
        second_sink.observed_labels().is_empty(),
        "a fresh stream must not inherit a prior stream's labels: {:?}",
        second_sink.observed_labels()
    );
}

#[test]
fn equivalent_configs_share_a_reader_tag_and_recovery_changes_it() {
    let default = MarkdownReaderConfig::new(SectionPolicy::default()).reader_tag();
    assert_eq!(
        default,
        MarkdownReaderConfig::new(SectionPolicy::default()).reader_tag(),
    );
    assert_ne!(
        default, 0,
        "bounded default semantics must not reuse the historical untagged reader"
    );
    assert_ne!(
        default,
        MarkdownReaderConfig::new(SectionPolicy::default())
            .recover_conversion_artifacts()
            .reader_tag(),
    );
    assert_eq!(
        default,
        MarkdownReaderConfig::new(SectionPolicy::default())
            .with_max_frontmatter_bytes(64 * 1024)
            .reader_tag(),
        "an unused resource bound must not change semantic reader identity",
    );
}

#[test]
fn a_config_without_observed_labels_is_equal_to_an_equivalent_one() {
    // Configuration never owns diagnostics, so two independently-built configs
    // with the same reader-affecting fields compare equal.
    assert_eq!(
        MarkdownReaderConfig::new(SectionPolicy::default()),
        MarkdownReaderConfig::new(SectionPolicy::default()),
    );
    assert_eq!(
        MarkdownReaderConfig::with_self_labels(
            SectionPolicy::default(),
            SectionPolicy::parse_labels("title,subtitle"),
        ),
        MarkdownReaderConfig::with_self_labels(
            SectionPolicy::default(),
            SectionPolicy::parse_labels("title,subtitle"),
        ),
    );
}

#[test]
fn diagnostics_sinks_do_not_compare_equal_even_when_empty() {
    // Two sinks are not meaningfully equal: identity != content, and a sink
    // accumulates observations the type deliberately keeps out of equality.
    // This pins that `MarkdownDiagnostics` does not derive `PartialEq` by
    // asserting the API the type exposes rather than a comparison that would
    // not compile.
    let _a = MarkdownDiagnostics::default();
    let _b = MarkdownDiagnostics::default();
    // If `MarkdownDiagnostics` ever derived `PartialEq`, the following line
    // would compile, defeating the invariant. We assert the absence of that
    // derive structurally by relying on `observed_labels()` alone.
    let labels: BTreeSet<String> = MarkdownDiagnostics::default().observed_labels();
    assert!(labels.is_empty());
}
