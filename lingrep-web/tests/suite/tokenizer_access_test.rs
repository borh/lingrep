use lingrep_web::corpus_set::CorpusName;
use lingrep_web::state::{Config, DetailLimits, RequestStore};

#[test]
fn tokenizer_access_fails_cleanly_for_an_empty_dict_path() {
    // A default/empty-dict store must not panic on tokenizer(); it returns Err.
    let store = RequestStore::default();
    assert!(
        store.context.tokenizer().is_err(),
        "empty dict path yields TokenizerUnavailable, not a panic"
    );
}

#[test]
fn tokenizer_access_memoizes_a_real_dictionary() {
    let dict = lingrep_core::testing::dictionary_path();
    let store = RequestStore::new(Config {
        dict_path: dict,
        // new() builds a runtime; if that requires a real corpus,
        corpora: vec![(CorpusName::default_key(), "unused".into(), "unused".into())],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    });
    let a = store.context.tokenizer().expect("real dict builds");
    let b = store.context.tokenizer().expect("second call");
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "tokenizer is memoized, not rebuilt"
    );
}
