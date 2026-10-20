//! Unit tests for lingrep-core/src/testing.rs module

use lingrep_core::testing::{run_matching_test, test_tokenizer};

#[test]
fn test_run_matching_empty() {
    let stats = run_matching_test(&[], &[]);

    // Empty input should process 0 sentences, 0 matches, 0 tokens
    assert_eq!(stats.sentences_processed, 0);
    assert_eq!(stats.matches_found, 0);
    assert_eq!(stats.tokens_processed, 0);
    // processing_time_ms may be > 0 even for empty operations
}

#[test]
fn test_run_matching_simple() {
    let stats = run_matching_test(&[("test", "[upos=NOUN]")], &["これはテストです。"]);

    assert_eq!(stats.sentences_processed, 1);
    assert!(stats.tokens_processed > 0);
}

#[test]
fn test_tokenizer_available() {
    let _tokenizer = test_tokenizer();
}

#[test]
fn test_multiple_sentences() {
    let sentences = vec![
        "これはテストです。",
        "日本語の文章です。",
        "形態素解析のテスト。",
    ];

    let stats = run_matching_test(&[("verb", "[upos=VERB]")], &sentences);

    assert_eq!(stats.sentences_processed, 3);
}
