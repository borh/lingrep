//! Golden file tests for output validation
//!
//! Uses insta crate to snapshot and validate outputs.

use lingrep_core::testing::{run_matching_test, test_tokenizer};
use lingrep_core::token_match::TokenInfo;
use lingrep_core::token_match::tokenize;

#[test]
fn test_tokenization_output() {
    let tokenizer = test_tokenizer();
    let text = "これはテストです。";
    let tokens = tokenize(text, tokenizer);

    // Snapshot tokenization details
    let formatted: Vec<String> = tokens
        .iter()
        .map(|t: &TokenInfo| {
            format!(
                "{}:{}:{}",
                t.orth().unwrap_or(""),
                t.pos().unwrap_or(""),
                t.pos1().unwrap_or("")
            )
        })
        .collect();

    insta::assert_snapshot!(formatted.join("\n"));
}

#[test]
fn test_pattern_matching_output() {
    let stats = run_matching_test(&[("test", "[upos=NOUN]")], &["これは日本語の文章です。"]);

    // Snapshot statistics
    let output = format!(
        "Sentences: {}\nMatches: {}\nTokens: {}",
        stats.sentences_processed, stats.matches_found, stats.tokens_processed
    );

    insta::assert_snapshot!(output);
}

#[test]
fn test_multiple_patterns_output() {
    let stats = run_matching_test(
        &[("noun", "[upos=NOUN]"), ("verb", "[upos=VERB]")],
        &["名詞と動詞があります。"],
    );

    let output = format!(
        "Sentences: {}\nMatches: {}\nTokens: {}",
        stats.sentences_processed, stats.matches_found, stats.tokens_processed
    );

    insta::assert_snapshot!(output);
}
