use askama::Template;
use lingrep_web::corpus_set::CorpusName;
use lingrep_web::handlers::search::SearchForm;
use lingrep_web::state::{Config, DetailLimits};
use lingrep_web::templates::ErrorFragment;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn config_rejects_empty_or_missing_paths() {
    let directory =
        std::env::temp_dir().join(format!("lingrep-web-config-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let dictionary = directory.join("dictionary.dic");
    let corpus = directory.join("corpus.jsonl");
    fs::write(&dictionary, "dictionary").unwrap();
    fs::write(&corpus, "{\"text\":\"test\"}\n").unwrap();

    let valid = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: vec![(CorpusName::default_key(), corpus.clone(), "corpus".into())],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    };
    assert!(valid.validate().is_ok());

    for config in [
        Config {
            dict_path: String::new(),
            ..valid.clone()
        },
        Config {
            corpora: vec![(
                CorpusName::default_key(),
                directory.join("missing.jsonl"),
                "corpus".into(),
            )],
            ..valid.clone()
        },
        Config {
            dict_path: directory.to_string_lossy().into_owned(),
            ..valid.clone()
        },
        Config {
            input_format: None,
            markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            ),
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
            detail_limits: DetailLimits { max_kwic: 0 },
            ..valid.clone()
        },
        Config {
            max_record_bytes: 0,
            ..valid.clone()
        },
        // A record holds at least one sentence. lingrep-web executes on the default
        // sentence bound, so a record bound below it is refused at startup
        // rather than refusing every record at read time.
        Config {
            max_record_bytes: lingrep_core::execution::DEFAULT_MAX_SENTENCE_BYTES - 1,
            ..valid.clone()
        },
    ] {
        assert!(config.validate().is_err());
    }

    #[cfg(unix)]
    {
        let unreadable_dictionary = directory.join("unreadable.dic");
        fs::write(&unreadable_dictionary, "dictionary").unwrap();
        fs::set_permissions(&unreadable_dictionary, fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = Config {
            dict_path: unreadable_dictionary.to_string_lossy().into_owned(),
            ..valid.clone()
        };
        assert!(unreadable.validate().is_err());
        fs::set_permissions(&unreadable_dictionary, fs::Permissions::from_mode(0o644)).unwrap();
    }

    fs::remove_dir_all(directory).unwrap();
}

/// Two distinct "no path" failures must not collapse into one message: an
/// empty `--input` declares nothing at all, while `--corpus sci=` declares a
/// key with an empty path — an operator misreading the first as "did I forget
/// --corpus entirely" would look in the wrong place.
#[test]
fn empty_input_and_an_empty_declared_corpus_path_report_differently() {
    let directory =
        std::env::temp_dir().join(format!("lingrep-web-config-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let dictionary = directory.join("dictionary.dic");
    fs::write(&dictionary, "dictionary").unwrap();

    let base = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: Vec::new(),
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    };

    let nothing_declared = base.validate().unwrap_err().to_string();
    assert!(nothing_declared.contains("no corpus is declared"));
    assert!(!nothing_declared.contains("sci"));

    let declared_with_empty_path = Config {
        corpora: vec![(
            CorpusName::parse_declared("sci").unwrap(),
            std::path::PathBuf::new(),
            "sci".into(),
        )],
        ..base
    };
    let named = declared_with_empty_path.validate().unwrap_err().to_string();
    assert!(
        named.contains("\"sci\""),
        "expected the declared key in the message, got {named:?}"
    );
    assert!(!named.contains("no corpus is declared"));

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn config_accepts_a_markdown_corpus_directory() {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = directory.path().join("dictionary.dic");
    let corpus = directory.path().join("papers");
    fs::write(&dictionary, "dictionary").unwrap();
    fs::create_dir(&corpus).unwrap();
    fs::write(corpus.join("paper.md"), "本文。\n").unwrap();

    let config = Config {
        dict_path: dictionary.to_string_lossy().into_owned(),
        corpora: vec![(CorpusName::default_key(), corpus, "papers".into())],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    };

    assert!(config.validate().is_ok());
}

#[test]
fn search_ignores_client_path_fields() {
    let form: SearchForm = serde_urlencoded::from_str(
        "pattern=verb+%3A%3D+%5Bpos%3DVERB%5D.&dict_path=%2Fhostile%2Fdict&corpus_path=%2Fhostile%2Fcorpus",
    )
    .unwrap();

    assert_eq!(
        serde_urlencoded::to_string(form).unwrap(),
        "pattern=verb+%3A%3D+%5Bpos%3DVERB%5D."
    );
}

#[test]
fn error_fragment_escapes_dynamic_message() {
    let html = ErrorFragment {
        message: "<script>alert(1)</script>",
    }
    .render()
    .unwrap();

    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
}
