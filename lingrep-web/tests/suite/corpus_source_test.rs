use std::fs;
use std::path::Path;

use lingrep_core::corpus::{AdmittedCorpusEvent, CorpusSource};
use lingrep_web::corpus_runtime;
use tempfile::tempdir;

fn read_config(path: &Path) -> lingrep_core::corpus::CorpusReadConfig {
    corpus_runtime::read_config(
        path,
        None,
        lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
    )
}

#[test]
fn jsonl_source_expands_sentences_in_document_order() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corpus.jsonl");
    fs::write(
        &path,
        "{\"sentences\":[\"一。\",\"二。\"]}\n{\"sentences\":[\"三。\"]}\n",
    )
    .unwrap();

    let source = CorpusSource::discover(&path, read_config(path.as_ref())).unwrap();
    let units = source
        .stream()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(units, ["一。", "二。", "三。"]);
}

#[test]
fn markdown_directory_is_sorted_recursively_and_filtered() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("b.MD"),
        "---\ntitle: B\n---\n\n二番。\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("a.md"),
        "---\ntitle: A\n---\n\n一番。\n\n::: {.references}\n除外。\n:::\n",
    )
    .unwrap();
    fs::write(directory.path().join("a.quality.json"), "{}\n").unwrap();
    fs::create_dir(directory.path().join("archive")).unwrap();
    fs::write(directory.path().join("archive/ignored.md"), "無視。\n").unwrap();

    let source = CorpusSource::discover(directory.path(), read_config(directory.path())).unwrap();
    let units = source
        .stream()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();

    assert_eq!(
        source
            .structured_stream()
            .map(Result::unwrap)
            .filter_map(|event| match event {
                AdmittedCorpusEvent::DocumentStart { source_name, .. } =>
                    Some(std::path::PathBuf::from(source_name)),
                _ => None,
            })
            .collect::<Vec<_>>(),
        [
            directory.path().join("a.md"),
            directory.path().join("archive/ignored.md"),
            directory.path().join("b.MD"),
        ]
    );
    assert_eq!(units, ["一番。", "無視。", "二番。"]);
}

#[test]
fn empty_markdown_directory_is_rejected() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("sidecar.json"), "{}\n").unwrap();

    let error =
        CorpusSource::discover(directory.path(), read_config(directory.path())).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn directory_stream_errors_name_the_malformed_document() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("broken.md");
    fs::write(&path, "<script>not corpus text</script>\n").unwrap();

    let source = CorpusSource::discover(directory.path(), read_config(directory.path())).unwrap();
    let error = source
        .stream()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap_err();

    assert!(error.to_string().contains("broken.md"), "{error}");
}

#[test]
fn configured_real_markdown_corpus_streams_without_structural_errors() {
    let Some(path) = std::env::var_os("LINGREP_MARKDOWN_CORPUS_PATH") else {
        eprintln!("skipping: LINGREP_MARKDOWN_CORPUS_PATH is not set");
        return;
    };

    let source = CorpusSource::discover(&path, read_config(path.as_ref())).unwrap();
    let unit_count = source
        .stream()
        .try_fold(0_u64, |count, unit| unit.map(|_| count + 1))
        .expect("configured Markdown corpus must stream without errors");
    assert!(unit_count > 0);
}

#[test]
fn structured_stream_assigns_global_ids_across_sorted_files() {
    let directory = tempdir().unwrap();
    fs::write(
        directory.path().join("b.md"),
        "---\ntitle: B\n---\n\n二。\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("a.md"),
        "---\ntitle: A\n---\n\n一。\n",
    )
    .unwrap();

    let events = CorpusSource::discover(directory.path(), read_config(directory.path()))
        .unwrap()
        .structured_stream()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    let documents = events
        .iter()
        .filter_map(|event| match event {
            AdmittedCorpusEvent::DocumentStart {
                id, source_name, ..
            } => Some((id.get(), source_name)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let sentences = events
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();

    assert_eq!(
        documents.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(documents[0].1.ends_with("a.md"));
    assert!(documents[1].1.ends_with("b.md"));
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.id.get())
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.document_sentence_ordinal)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    assert_ne!(
        sentences[0]
            .structure
            .divisions
            .iter()
            .find(|division| division.kind == "paragraph")
            .map(|division| division.id),
        sentences[1]
            .structure
            .divisions
            .iter()
            .find(|division| division.kind == "paragraph")
            .map(|division| division.id)
    );
}

#[test]
fn web_read_config_keeps_the_recovering_markdown_policy() {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("a.md"), "一。\n").unwrap();
    let source = CorpusSource::discover(directory.path(), read_config(directory.path())).unwrap();

    assert_ne!(source.config().reader_tag(), 0);
}
#[test]
fn a_bccwj_corpus_declaration_is_refused_at_validation() {
    use lingrep_core::corpus::{CorpusFormat, MarkdownReaderConfig, SectionPolicy};
    use lingrep_web::corpus_set::CorpusName;
    use lingrep_web::state::{Config, ConfigError, DetailLimits};

    let directory = tempdir().unwrap();
    // The corpus path need not exist: the refusal precedes any corpus
    // access, but the dictionary check runs first, so give it a file.
    let dictionary = directory.path().join("dict.zst");
    fs::write(&dictionary, b"").unwrap();
    let config = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: vec![(
            CorpusName::parse("bcc").unwrap(),
            directory.path().join("dist").to_path_buf(),
            "bcc".to_owned(),
        )],
        input_format: Some(CorpusFormat::Bccwj),
        markdown: MarkdownReaderConfig::new(SectionPolicy::default()),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 100 },
    };
    let error = config.validate().expect_err("bccwj must be refused");
    assert!(
        matches!(error, ConfigError::UnreadablePath { .. }),
        "unexpected variant: {error:?}"
    );
    assert!(error.to_string().contains("bccwj"), "{error}");
}

/// A prepared artifact is admitted where a corpus *source* is expected.
///
/// The neighbouring `a_bccwj_corpus_declaration_is_refused_at_validation` is
/// the other half of the same story and still holds: lingrep-web cannot read a
/// BCCWJ distribution, and says so. What it can do is serve one that
/// `ling corpus build --input-format bccwj` already prepared, because a
/// prepared artifact is producer-free — the reader that made it is recorded in
/// it, and nothing here re-reads the source.
#[test]
fn a_prepared_artifact_is_admitted_as_a_declared_corpus() {
    use lingrep_core::corpus::{MarkdownReaderConfig, SectionPolicy};
    use lingrep_web::corpus_set::CorpusName;
    use lingrep_web::state::{Config, DetailLimits};

    let fixture = crate::v6_support::build_artifact(vec![(
        "fixture.md",
        0,
        false,
        vec![vec![
            lingrep_core::token_match::TokenInfo::default().with_orth("猫"),
        ]],
    )]);
    let directory = tempdir().unwrap();
    let dictionary = directory.path().join("dict.zst");
    fs::write(&dictionary, b"").unwrap();

    let config = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: vec![(
            CorpusName::parse("bccwj").unwrap(),
            fixture.path.clone(),
            "bccwj".to_owned(),
        )],
        input_format: None,
        markdown: MarkdownReaderConfig::new(SectionPolicy::default()),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 100 },
    };
    config
        .validate()
        .expect("a prepared artifact is a valid declared corpus");
}

/// A directory that looks like an artifact but will not open reports the
/// artifact reader's own reason.
///
/// Before this path existed such a declaration fell through to the source
/// reader and produced "path is not a readable file" about a directory that
/// plainly exists and is readable — true of nothing the operator could act on.
#[test]
fn a_malformed_prepared_artifact_names_the_reason_it_was_refused() {
    use lingrep_core::corpus::{MarkdownReaderConfig, SectionPolicy};
    use lingrep_web::corpus_set::CorpusName;
    use lingrep_web::state::{Config, ConfigError, DetailLimits};

    let directory = tempdir().unwrap();
    let dictionary = directory.path().join("dict.zst");
    fs::write(&dictionary, b"").unwrap();
    // Artifact-shaped — a directory carrying a manifest — but the manifest is
    // not a manifest, so `CorpusArtifact::open` refuses it.
    let artifact = directory.path().join("broken.lingcorpus");
    fs::create_dir_all(&artifact).unwrap();
    fs::write(artifact.join("manifest.json"), "not json at all").unwrap();

    let config = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: vec![(
            CorpusName::parse("bccwj").unwrap(),
            artifact.clone(),
            "bccwj".to_owned(),
        )],
        input_format: None,
        markdown: MarkdownReaderConfig::new(SectionPolicy::default()),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 100 },
    };
    let error = config
        .validate()
        .expect_err("a malformed artifact must be refused");
    assert!(
        matches!(error, ConfigError::UnreadableArtifact { .. }),
        "unexpected variant: {error:?}"
    );
    let message = error.to_string();
    // Names the declaration, and carries a cause rather than stopping at
    // "unreadable".
    assert!(message.contains("bccwj"), "{message}");
    assert!(message.contains("prepared artifact"), "{message}");
    assert!(
        message.len()
            > "corpus \"bccwj\" is a prepared artifact that could not be opened: ".len()
                + artifact.display().to_string().len(),
        "the refusal must carry the artifact reader's own reason: {message}"
    );
}
