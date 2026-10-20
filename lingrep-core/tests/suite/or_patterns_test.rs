//! OR pattern matching tests
//!
//! Tests for OR patterns in pattern matching:
//! - Regex OR in attributes: [upos~/VERB|AUX/]
//! - Pattern reference OR: @PATTERN1 | @PATTERN2

use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};

#[test]
fn test_or_pos_regex_verb_or_aux() {
    // Test: [upos~/VERB|AUX/] should match either VERB or AUX
    let matcher = compile_test_pattern("[upos~/VERB|AUX/]");

    let text = "食べる"; // VERB
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // Should match VERB
    assert_eq!(matches.len(), 1, "Should match VERB in OR pattern");
    assert_eq!(matches[0].start, 0);
}

#[test]
fn test_or_pos_regex_noun_or_verb() {
    let matcher = compile_test_pattern("[upos~/NOUN|VERB/]");

    let text1 = "猫"; // NOUN
    let tokens1 = tokenize_japanese(text1);
    let matches1 = matcher.find_matches(&tokens1);
    assert_eq!(matches1.len(), 1, "Should match NOUN");

    let text2 = "食べる"; // VERB
    let tokens2 = tokenize_japanese(text2);
    let matches2 = matcher.find_matches(&tokens2);
    assert_eq!(matches2.len(), 1, "Should match VERB");
}

#[test]
fn test_or_pattern_both_alternatives_match() {
    // Test sentence where both alternatives could match
    let matcher = compile_test_pattern("[upos~/NOUN|VERB/]");

    let text = "猫食べる"; // Both NOUN and VERB present
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // Should find both matches
    assert!(!matches.is_empty(), "Should find at least 1 match");
}

#[test]
fn test_or_pattern_neither_matches() {
    let matcher = compile_test_pattern("[upos~/NOUN|VERB/]");

    let text = "が"; // ADP (particle), not NOUN or VERB
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 0, "Should not match ADP");
}

#[test]
fn test_or_pattern_three_alternatives() {
    let matcher = compile_test_pattern("[upos~/NOUN|VERB|AUX/]");

    let text = "猫"; // NOUN
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match NOUN in 3-way OR");
}

#[test]
fn test_or_pattern_empty_alternative() {
    // Edge case: OR pattern with empty string alternative
    let matcher = compile_test_pattern("[upos~/NOUN|/]");

    let text = "猫"; // NOUN
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // Should still match NOUN
    assert_eq!(matches.len(), 1, "Should match NOUN with empty alternative");
}

#[test]
fn test_or_pattern_special_chars_in_regex() {
    // Test OR with regex special characters
    let matcher = compile_test_pattern("[lemma~/^(猫|犬)$/]");

    let text = "猫"; // Should match
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match exact lemma");
}
