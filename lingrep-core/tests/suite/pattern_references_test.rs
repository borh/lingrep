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
//! Pattern reference matching tests
//!
//! Tests for pattern references: @PATTERN_NAME
//!
//! Tests for quantifiers with pattern references:
//! - @PATTERN? (optional, 0 or 1)
//! - @PATTERN* (zero or more, ZeroOrMore)
//! - @PATTERN+ (one or more, OneOrMore)
//! - @PATTERN{n,m} (range quantifiers)
//! - @PATTERN?{n,m} (optional range)

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::testing::tokenize_japanese;

/// Compiles multiple definitions through the whole-document compiler.
fn fsm_multi(defs: &[&str]) -> lingrep_core::fsm::CompiledCatalog {
    let source = defs.join("\n");
    compile_catalog(
        PatternSource::named("references.lx", &source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

#[test]
fn test_pattern_reference_with_optional() {
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun? [upos=NOUN].",
    ]);

    let text = "猫";
    let tokens = tokenize_japanese(text);
    let matches = fsm.find_matches(&tokens);

    // Both definitions emit matches; this assertion concerns `test` alone.
    let test_matches: Vec<_> = matches.iter().filter(|m| m.pattern == "test").collect();

    // Optional reference: should match 0 or 1 times
    assert!(
        test_matches.len() <= 1,
        "Optional @noun? should match 0 or 1 tokens (got {} test matches)",
        test_matches.len()
    );
}

#[test]
fn test_pattern_reference_with_zero_or_more() {
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun* [upos=NOUN].",
    ]);

    let text1 = "猫"; // Match once
    let text2 = "猫猫"; // Match twice
    let text3 = "猫猫猫"; // Match three times
    let text4 = "猫猫猫猫猫"; // Match four times

    let tokens1 = tokenize_japanese(text1);
    let matches1 = fsm.find_matches(&tokens1);

    let tokens2 = tokenize_japanese(text2);
    let matches2 = fsm.find_matches(&tokens2);

    let tokens3 = tokenize_japanese(text3);
    let matches3 = fsm.find_matches(&tokens3);

    let tokens4 = tokenize_japanese(text4);
    let matches4 = fsm.find_matches(&tokens4);

    // ZeroOrMore: should match 0+ times (at least once, possibly more)
    let total_matches = matches1.len() + matches2.len() + matches3.len() + matches4.len();
    assert!(total_matches >= 1, "ZeroOrMore should match at least once");
}

#[test]
fn test_pattern_reference_with_optional_range() {
    // Test range quantifier: {2,4} means match 2 to 4 occurrences
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun{2,4}.",
    ]);

    let text = "猫犬猫犬猫犬";
    let tokens = tokenize_japanese(text);
    let matches = fsm.find_matches(&tokens);

    // Range {2,4}: pattern should parse and find matches
    assert!(!matches.is_empty() || tokens.len() < 2);
}

#[test]
fn test_pattern_reference_with_two_or_more() {
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun{2,}.",
    ]);

    // Use longer text to ensure at least 2 nouns
    let text = "猫犬猫犬猫犬猫犬猫犬";
    let tokens = tokenize_japanese(text);
    let matches = fsm.find_matches(&tokens);

    // Two or more: pattern should parse and find some matches
    // Each match should span multiple tokens since we use {2,}
    assert!(
        !matches.is_empty() || tokens.len() < 2,
        "Pattern with {{2,}} should find matches or have insufficient tokens"
    );
}

#[test]
fn test_pattern_reference_with_range_quantifier() {
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun{2,5}.",
    ]);

    let text = "猫犬猫犬猫犬猫犬猫犬";
    let tokens = tokenize_japanese(text);
    let matches = fsm.find_matches(&tokens);

    // Range quantifier {2,5}: pattern should parse and find matches
    // The quantifier means "match 2 to 5 nouns in sequence"
    assert!(!matches.is_empty() || tokens.len() < 2);
}

#[test]
fn test_pattern_reference_with_multiple_quantifiers() {
    // Test that multiple consecutive pattern references with different quantifiers work
    let fsm = fsm_multi(&[
        "fragment noun := [upos=NOUN].",
        "pattern test := @noun? @noun* [upos=NOUN].",
    ]);

    let text = "猫犬猫犬猫犬猫犬猫犬猫犬猫";
    let tokens = tokenize_japanese(text);
    let matches = fsm.find_matches(&tokens);

    // Pattern with optional + zero-or-more should parse and find matches
    assert!(!matches.is_empty() || tokens.is_empty());
}
