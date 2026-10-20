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
//! Contract tests for `lingrep-core::corpus::source::CorpusSource`.
//!
//! These pin the discovery and streaming behavior: directory discovery selects
//! Markdown or CoNLL-U documents recursively (sorted, case-insensitive),
//! unsupported directory formats are rejected, fresh streams are independent,
//! and per-file failures are qualified with the offending path. A boundary
//! assertion also pins that the lingrep-core corpus module never references
//! the prepared-corpus provenance database (`provenance.sqlite`), which
//! `lingrep-corpus` owns.

use std::fs;
use std::io;
use std::path::PathBuf;

use clap::ValueEnum;
use lingrep_core::corpus::source::{
    CorpusFormat, CorpusReadConfig, CorpusSource, resolve_input_format,
};
use lingrep_core::corpus::{AdmittedCorpusEvent, MarkdownReaderConfig, SectionPolicy};
use tempfile::tempdir;

#[test]
fn source_module_does_not_reference_the_provenance_database() {
    let source_rs = include_str!("../../src/corpus/source.rs");
    let mod_rs = include_str!("../../src/corpus/mod.rs");
    let text_source_rs = include_str!("../../src/corpus/text_source.rs");
    for (name, content) in [
        ("source.rs", source_rs),
        ("mod.rs", mod_rs),
        ("text_source.rs", text_source_rs),
    ] {
        assert!(
            !content.contains("provenance.sqlite"),
            "lingrep-core corpus `{name}` must not reference the `provenance.sqlite` database"
        );
    }
}

fn markdown_config() -> CorpusReadConfig {
    CorpusReadConfig::new(
        CorpusFormat::Markdown,
        MarkdownReaderConfig::new(SectionPolicy::default()),
    )
}

fn document_paths(source: &CorpusSource) -> io::Result<Vec<PathBuf>> {
    source
        .structured_stream()
        .filter_map(|event| match event {
            Ok(AdmittedCorpusEvent::DocumentStart { source_name, .. }) => {
                Some(Ok(PathBuf::from(source_name)))
            }
            Err(error) => Some(Err(error)),
            _ => None,
        })
        .collect()
}

/// A single file uses the declared corpus format rather than inferring from the
/// extension: configuration is explicit, so a `.jsonl` file read with a Plain
/// config is plain text, not JSONL.
#[test]
fn single_file_uses_the_declared_format() -> io::Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("corpus.jsonl");
    fs::write(&path, "一\n二\n")?;

    let source = CorpusSource::discover(&path, CorpusReadConfig::Plain)?;
    let units = source.stream().collect::<io::Result<Vec<_>>>()?;
    assert_eq!(units, ["一".to_owned(), "二".to_owned()]);
    Ok(())
}

/// A directory selects Markdown files recursively in deterministic depth-first
/// order, case-insensitive on the extension. Sidecars and non-`.md` files are
/// ignored while nested Markdown is admitted.
#[test]
fn directory_selects_recursive_markdown_in_depth_first_order() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("b.MD"), "二。\n")?;
    fs::write(directory.path().join("a.md"), "一。\n")?;
    fs::write(directory.path().join("notes.txt"), "ignore\n")?;
    fs::write(directory.path().join("sidecar.json"), "{}\n")?;
    fs::create_dir(directory.path().join("sub"))?;
    fs::write(directory.path().join("sub/nested.md"), "無視。\n")?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let expected = &[
        directory.path().join("a.md"),
        directory.path().join("b.MD"),
        directory.path().join("sub/nested.md"),
    ];
    assert_eq!(document_paths(&source)?, expected);
    let units = source.stream().collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        units,
        ["一。".to_owned(), "二。".to_owned(), "無視。".to_owned()]
    );
    Ok(())
}

/// A CoNLL-U directory uses the same recursive deterministic source contract
/// while retaining the pretokenized sentence boundaries from every selected
/// document.
#[test]
fn directory_selects_and_streams_recursive_conllu() -> io::Result<()> {
    let directory = tempdir()?;
    fs::create_dir(directory.path().join("nested"))?;
    fs::write(
        directory.path().join("b.conllu"),
        "# sent_id = b\n# text = 犬。\n1\t犬\t犬\tNOUN\t_\t_\t0\troot\t_\t_\n2\t。\t。\tPUNCT\t_\t_\t1\tpunct\t_\t_\n\n",
    )?;
    fs::write(
        directory.path().join("nested/a.CONLLU"),
        "# sent_id = a\n# text = 猫。\n1\t猫\t猫\tNOUN\t_\t_\t0\troot\t_\t_\n2\t。\t。\tPUNCT\t_\t_\t1\tpunct\t_\t_\n\n",
    )?;
    fs::write(directory.path().join("README.md"), "ignored\n")?;

    let source = CorpusSource::discover(directory.path(), CorpusReadConfig::Conllu)?;
    assert_eq!(
        document_paths(&source)?,
        &[
            directory.path().join("b.conllu"),
            directory.path().join("nested/a.CONLLU"),
        ]
    );
    let units = source.stream().collect::<io::Result<Vec<_>>>()?;
    assert_eq!(units, ["犬。".to_owned(), "猫。".to_owned()]);
    Ok(())
}

/// Directory sources must not pre-enumerate every admitted path at construction:
/// a later malformed shard cannot prevent the first lexical document from being
/// streamed. Removing the lazy discovery cursor would make `discover` fail.
#[test]
fn directory_source_streams_the_first_document_before_later_discovery_completes() -> io::Result<()>
{
    use std::os::unix::fs::symlink;

    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "一。\n")?;
    fs::create_dir(directory.path().join("z"))?;
    symlink("absent.md", directory.path().join("z/missing.md"))?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let mut stream = source.stream();
    assert_eq!(stream.next().expect("first source item")?, "一。");
    let error = stream
        .collect::<io::Result<Vec<_>>>()
        .expect_err("later dangling link is surfaced while streaming");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("missing.md"), "{error}");
    Ok(())
}

/// A directory with no top-level Markdown files is rejected at discovery, not
/// silently streamed as nothing.
#[test]
fn empty_markdown_directory_is_rejected() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("sidecar.json"), "{}\n").unwrap();

    let error = CorpusSource::discover(directory.path(), markdown_config()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

/// Directory discovery rejects formats without a directory document selection
/// before any file is opened.
#[test]
fn directory_with_unsupported_config_is_rejected() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("a.md"), "一。\n").unwrap();

    for config in [CorpusReadConfig::Plain, CorpusReadConfig::Jsonl] {
        let error = CorpusSource::discover(directory.path(), config.clone()).unwrap_err();
        assert_eq!(
            error.kind(),
            io::ErrorKind::InvalidInput,
            "directory + unsupported config must be rejected"
        );
    }
}

/// Fresh streams from one source are independent — consuming one does not
/// exhaust the other, and the source may be streamed repeatedly.
#[test]
fn fresh_streams_are_independent() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(directory.path().join("a.md"), "一。\n")?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let first = source.stream().collect::<io::Result<Vec<_>>>()?;
    let second = source.stream().collect::<io::Result<Vec<_>>>()?;
    assert_eq!(first, ["一。".to_owned()]);
    assert_eq!(second, ["一。".to_owned()]);
    Ok(())
}

/// Per-file streaming failures are qualified with the offending path so a
/// caller can attribute the error to a document within a directory.
#[test]
fn stream_failures_are_qualified_with_the_current_path() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(
        directory.path().join("broken.md"),
        "<script>not corpus text</script>\n",
    )?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let error = source.stream().collect::<io::Result<Vec<_>>>().unwrap_err();
    assert!(
        error.to_string().contains("broken.md"),
        "error should name the malformed document: {error}"
    );
    Ok(())
}

/// A Markdown source shares one `MarkdownDiagnostics` sink across every selected
/// file, so labels observed in any document accumulate rather than resetting
/// per file.
#[test]
fn markdown_diagnostics_accumulate_across_files() -> io::Result<()> {
    let directory = tempdir()?;
    fs::write(
        directory.path().join("a.md"),
        "# Heading {.methods}\n一。\n",
    )?;
    fs::write(directory.path().join("b.md"), "## Sub {.results}\n二。\n")?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let mut stream = source.structured_stream();
    let _ = stream.by_ref().collect::<io::Result<Vec<_>>>()?;
    let diagnostics = stream
        .diagnostics()
        .expect("a Markdown source exposes its diagnostics");
    let observed = diagnostics.observed_labels();
    assert!(
        observed.contains("methods"),
        "labels from the first file must accumulate: {observed:?}"
    );
    assert!(
        observed.contains("results"),
        "labels from the second file must accumulate: {observed:?}"
    );
    Ok(())
}

/// A non-Markdown source exposes no diagnostics — the sink is a Markdown-only
/// concern.
#[test]
fn non_markdown_source_diagnostics_are_none() -> io::Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("corpus.jsonl");
    fs::write(&path, "{\"sentences\":[\"一。\"]}\n")?;

    let source = CorpusSource::discover(&path, CorpusReadConfig::Jsonl)?;
    let stream = source.stream();
    assert!(
        stream.diagnostics().is_none(),
        "a non-Markdown source exposes no diagnostics"
    );
    Ok(())
}

/// The structured stream assigns global document and sentence IDs across the
/// sorted file selection, preserving document order across files.
#[test]
fn structured_stream_assigns_global_ids_across_sorted_files() -> io::Result<()> {
    use lingrep_core::corpus::AdmittedCorpusEvent;

    let directory = tempdir()?;
    fs::write(
        directory.path().join("b.md"),
        "---\ntitle: B\n---\n\n二。\n",
    )?;
    fs::write(
        directory.path().join("a.md"),
        "---\ntitle: A\n---\n\n一。\n",
    )?;

    let source = CorpusSource::discover(directory.path(), markdown_config())?;
    let events = source.structured_stream().collect::<io::Result<Vec<_>>>()?;
    let documents = events
        .iter()
        .filter_map(|event| match event {
            AdmittedCorpusEvent::DocumentStart {
                id, source_name, ..
            } => Some((
                id.get(),
                source_name.ends_with("a.md") || source_name.ends_with("b.md"),
                source_name.as_str().ends_with("a.md"),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let sentences = events
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();

    assert_eq!(
        documents.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(),
        [0, 1]
    );
    // `a.md` is read first (sorted), so its document comes before `b.md`.
    assert!(documents[0].2, "a.md must be the first document");
    assert!(!documents[1].2, "b.md must be the second document");
    assert_eq!(
        sentences.iter().map(|s| s.id.get()).collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|s| s.document_sentence_ordinal)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    Ok(())
}

/// `CorpusFormat` tags the resolved format of a config and is case-insensitive
/// on the CLI via `clap::ValueEnum`, mapping PascalCase variants to lowercase.
#[test]
fn corpus_format_value_enum_is_lowercase() {
    assert_eq!(
        CorpusFormat::Plain,
        CorpusFormat::from_str("plain", true).unwrap()
    );
    assert_eq!(
        CorpusFormat::Jsonl,
        CorpusFormat::from_str("jsonl", true).unwrap()
    );
    assert_eq!(
        CorpusFormat::Markdown,
        CorpusFormat::from_str("markdown", true).unwrap()
    );
    assert_eq!(
        CorpusFormat::Soranoha,
        CorpusFormat::from_str("soranoha", true).unwrap()
    );
}

#[test]
fn corpus_read_config_format_reports_the_resolved_variant() {
    let markdown = MarkdownReaderConfig::new(SectionPolicy::default());
    assert_eq!(CorpusReadConfig::Plain.format(), CorpusFormat::Plain);
    assert_eq!(CorpusReadConfig::Jsonl.format(), CorpusFormat::Jsonl);
    assert_eq!(
        CorpusReadConfig::new(CorpusFormat::Markdown, markdown).format(),
        CorpusFormat::Markdown
    );
    assert_eq!(
        CorpusReadConfig::new(
            CorpusFormat::Soranoha,
            MarkdownReaderConfig::new(SectionPolicy::default()),
        )
        .format(),
        CorpusFormat::Soranoha
    );
}

#[test]
fn section_label_support_is_owned_by_the_format_catalog() {
    assert!(!CorpusFormat::Plain.supports_section_labels());
    assert!(!CorpusFormat::Jsonl.supports_section_labels());
    assert!(CorpusFormat::Markdown.supports_section_labels());
    assert!(CorpusFormat::Bccwj.supports_section_labels());
    assert!(!CorpusFormat::Soranoha.supports_section_labels());
}

/// A Soranoha archive is declared, never inferred: a `.zip` is a container
/// other formats use too, so the extension proves nothing about a catalog.
#[test]
fn a_zip_is_not_inferred_as_a_soranoha_archive() {
    let path = std::path::Path::new("archive.zip");
    assert_eq!(resolve_input_format(Some(path), None), CorpusFormat::Plain);
    assert_eq!(
        resolve_input_format(Some(path), Some(CorpusFormat::Soranoha)),
        CorpusFormat::Soranoha
    );
}

/// The web/file-corpus source streams `BufRead` readers only, so an archive is
/// refused at construction rather than at the first sentence.
#[test]
fn a_soranoha_archive_is_not_a_corpus_source() {
    let error = CorpusSource::discover("archive.zip", CorpusReadConfig::Soranoha).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("soranoha"), "{error}");
}
