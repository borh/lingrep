//! Boundary, empty-input, Unicode, and whitespace cases.

use lingrep_core::testing::{TokenInfo, compile_test_pattern, test_tokenizer, tokenize};
use std::time::Instant;

#[test]
fn test_very_long_text() {
    let long_text = "テスト ".repeat(5000); // ~30,000 characters with spaces
    let pattern_def = "[upos=NOUN]";

    let tokenizer = test_tokenizer();
    let tokens: Vec<TokenInfo> = tokenize(&long_text, tokenizer);

    println!("Token count: {}", tokens.len());

    let matcher = compile_test_pattern(pattern_def);

    let start = Instant::now();
    let matches = matcher.find_matches(&tokens);
    let duration = start.elapsed();

    println!("Matches: {}, Duration: {:?}", matches.len(), duration);
    assert!(duration.as_secs() < 5, "Should complete in < 5 seconds");
    assert!(!matches.is_empty(), "Should find matches in long text");
}

#[test]
fn test_single_character_texts() {
    // Test various single character inputs
    // Note: whitespace characters like \n may produce no tokens, which is expected
    let cases = vec!["あ", "。", "、"];

    for text in cases {
        let tokenizer = test_tokenizer();
        let tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
        assert!(!tokens.is_empty(), "Should tokenize single char: '{text}'");
    }

    // Whitespace characters may produce no tokens, which is valid behavior
    let whitespace_cases = vec!["\n", " ", "\t"];
    for text in whitespace_cases {
        let tokenizer = test_tokenizer();
        let _tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
        // Should not panic, even if tokens are empty
    }
}

// ============================================================================
// Empty/Null Inputs
// ============================================================================

#[test]
fn test_empty_text_with_simple_pattern() {
    let text = "";
    let pattern_def = "[upos=NOUN]";
    let tokenizer = test_tokenizer();

    let tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
    let matcher = compile_test_pattern(pattern_def);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 0);
}

#[test]
fn test_text_with_empty_pattern_should_error() {
    let pattern_def = ""; // Empty pattern

    let result = std::panic::catch_unwind(|| compile_test_pattern(pattern_def));

    assert!(result.is_err(), "Empty pattern should cause parse error");
}

// ============================================================================
// Unicode and Emoji Handling
// ============================================================================

#[test]
fn test_emoji_and_symbols() {
    let text = "🚀は速いです。😊";
    let tokenizer = test_tokenizer();
    let tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
    assert!(!tokens.is_empty(), "Should tokenize emoji-containing text");
}

#[test]
fn test_mixed_line_endings() {
    let cases = vec![
        "テスト\n",   // LF
        "テスト\r\n", // CRLF
        "テスト\r",   // CR
    ];

    for text in cases {
        let tokenizer = test_tokenizer();
        let tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
        let pattern_def = "[upos=NOUN]";
        let matcher = compile_test_pattern(pattern_def);
        matcher.find_matches(&tokens); // Should not panic
    }
}

// ============================================================================
// Whitespace Variations
// ============================================================================

#[test]
fn test_whitespace_variations() {
    let cases = vec!["     ", "\t\t\t", " \t \n ", "\x00\x01\x02"];

    for text in cases {
        let tokenizer = test_tokenizer();
        let tokens: Vec<TokenInfo> = tokenize(text, tokenizer);
        assert!(tokens.len() <= 1, "Whitespace should tokenize to ≤1 token");
    }
}
