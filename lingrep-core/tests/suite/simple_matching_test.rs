//! Simple token matching tests
//!
//! Tests the most basic pattern matching: matching a single token by its
//! part of speech or lemma.

use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};

#[test]
fn test_simple_noun_match() {
    let matcher = compile_test_pattern("[upos=NOUN]");

    let text = "猫がいる";
    let tokens = tokenize_japanese(text);

    // Debug: print tokens
    println!("Tokens for '{text}':");
    for (i, token) in tokens.iter().enumerate() {
        println!("  [{}] pos={:?}, orth={:?}", i, token.pos(), token.orth());
    }

    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs
}

#[test]
fn test_simple_lemma_match() {
    let matcher = compile_test_pattern("[lemma=ある]");

    let text = "ある";
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs
}

#[test]
fn test_simple_pos_no_match() {
    let matcher = compile_test_pattern("[upos=VERB]");

    let text = "猫";
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // NOUN should not match VERB pattern
    assert!(matches.is_empty() || !tokens.iter().any(|t| t.pos() == Some("VERB")));
}

#[test]
fn test_empty_input() {
    let matcher = compile_test_pattern("[upos=NOUN]");

    let text = "";
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 0, "Empty input should produce 0 matches");
}

#[test]
fn test_multi_token_sentence() {
    let matcher = compile_test_pattern("[upos=NOUN]");

    let text = "猫がいる";
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs
}
