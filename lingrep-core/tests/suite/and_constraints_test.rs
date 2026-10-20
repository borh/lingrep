#![allow(clippy::needless_borrow)]
//! AND constraint matching tests
//!
//! Tests for AND constraints in pattern matching:
//! - Multiple constraints on single token: [upos=VERB & unidic.cform=終止形]
//!
//! # Conjunction depth contract
//!
//! The normalized `tree-sitter-lingrep` CST represents `a & b & c` as one flat
//! `attribute_conjunction` with three direct `attribute` children. The tests
//! below pin the end-to-end consequence: every direct child in a ≥3-member
//! conjunction is evaluated, not just the first.

use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};

#[test]
fn test_and_pos_and_cform() {
    // Test: [upos=VERB & unidic.cform=終止形] should parse correctly
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform=終止形]");

    let text = "食べる";
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs without error
    // Actual matches depend on dictionary tokenization
}

#[test]
fn test_and_both_constraints_must_match() {
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform=終止形]");

    let text = "行く"; // VERB but not 終止形
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // Should not match - cform doesn't satisfy both constraints
    assert_eq!(matches.len(), 0, "Should not match VERB without 終止形");
}

#[test]
fn test_and_three_constraints() {
    // Three-constraint conjunctions must evaluate ALL three, not just the
    // first. Real UniDic cform for 飷べる is `終止形-一般` (subcategorized),
    // so `[unidic.cform=終止形]` (Single, exact) does NOT match and `[unidic.cform~/.*る$/]`
    // (full-match regex on `終止形-一般`) does NOT match. Only `pos=VERB` holds.
    //
    // Regression: before the flat direct-child CST and strict collection, the
    // parser emitted only the first attribute, so this pattern matched (1) —
    // silently ignoring both cform constraints and producing false positives.
    // The correct count is 0.
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform=終止形 & unidic.cform~/.*る$/]");

    let text = "食べる"; // pos=VERB, cform=終止形-一般
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        0,
        "3-constraint AND should respect all 3; \
              pos=VERB holds but neither cform constraint matches the real \
              subcategorized cform 終止形-一般"
    );
}

#[test]
fn test_and_three_constraints_all_match() {
    // Positive control for the three-constraint flattening: when all three
    // constraints genuinely match (pos=VERB, cform prefix 終止形, cform suffix
    // -一般), the conjunction yields exactly one match. This proves the fix
    // did not over-correct into rejecting everything.
    let matcher =
        compile_test_pattern("[upos=VERB & unidic.cform~/終止形.*/ & unidic.cform~/.*一般$/]");

    let text = "食べる"; // pos=VERB, cform=終止形-一般
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        1,
        "3-constraint AND where all 3 hold should match"
    );
}

#[test]
fn test_and_three_constraints_first_passes_second_fails() {
    // Discriminating regression guard: first constraint passes (pos=VERB),
    // second fails (cform=連用形). If the parser dropped the 2nd/3rd attribute
    // (the old bug), this would incorrectly match. Correct count is 0.
    let matcher =
        compile_test_pattern("[upos=VERB & unidic.cform=連用形 & unidic.cform~/終止形.*/]");

    let text = "食べる";
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        0,
        "a failing 2nd constraint must veto the AND \
              even when the 1st and 3rd would pass"
    );
}

#[test]
fn test_and_no_constraint_matches() {
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform=終止形]");

    let text = "猫"; // NOUN, not VERB
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 0, "Should not match NOUN");
}

#[test]
fn test_and_empty_cform() {
    // Test token with empty cform value
    let matcher = compile_test_pattern("[upos=NOUN & unidic.cform~/^$/]");

    let text = "猫"; // NOUN with empty cform
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Test passes if no panic occurs - pattern handles empty cform
}

#[test]
fn test_and_or_combination() {
    // Test AND with OR inside: [upos=VERB & unidic.cform~/終止形|連用形/]
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform~/終止形|連用形/]");

    let text = "食べる";
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs without error
}

#[test]
fn test_and_multiple_tokens() {
    // Test AND constraint on first token in multi-token sequence
    let matcher = compile_test_pattern("[upos=VERB & unidic.cform=終止形] [upos=AUX]");

    let text = "食べるだ";
    let tokens = tokenize_japanese(text);
    let _matches = matcher.find_matches(&tokens);

    // Just verify pattern parses and runs without error
}
