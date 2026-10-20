//! Quantifier matching tests
//!
//! Tests for quantifiers in pattern matching:
//! - ?: Zero or one (optional)
//! - *: Zero or more
//! - +: One or more
//! - {n,m}: Range quantifiers
//!
//! NOTE: These tests use Japanese particles (が, は, を) which UniDic maps to ADP
//! (adposition/case particle), not PART (sentence-final particle like よ, ね).
//! The pos=ADP attribute is used to match these correctly.

use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};

#[test]
fn test_quantifier_zero_or_one_optional() {
    // Test: [upos=ADP]? should match 0 or 1 adpositions (particles like が, は, を)
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]?");

    let text = "猫"; // NOUN without particle
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    // Should match - particle is optional
    assert_eq!(matches.len(), 1, "Should match NOUN without optional ADP");
}

#[test]
fn test_quantifier_zero_or_one_present() {
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]?");

    let text = "猫が"; // NOUN with particle (が maps to ADP)
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match NOUN with ADP");
}

#[test]
fn test_quantifier_zero_or_more() {
    // Test: [upos=ADP]* should match 0+ adpositions
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]*");

    let text = "猫"; // NOUN without particles
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match NOUN with 0 particles");
}

#[test]
fn test_quantifier_zero_or_more_multiple() {
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]*");

    let text = "猫がは"; // NOUN with 2 particles (が, は both map to ADP)
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        1,
        "Should match NOUN with multiple particles"
    );
}

#[test]
fn test_quantifier_one_or_more() {
    // Test: [upos=ADP]+ should match 1+ adpositions
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]+");

    let text = "猫が"; // NOUN with 1 particle
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match NOUN with 1 particle");
}

#[test]
fn test_quantifier_one_or_more_fails_with_zero() {
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]+");

    let text = "猫"; // NOUN without particle
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        0,
        "Should not match NOUN with 0 particles (requires 1+)"
    );
}

#[test]
fn test_quantifier_range_exact() {
    // Test: [upos=ADP]{2} should match exactly 2
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]{2}");

    let text = "猫がは"; // NOUN with 2 particles
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        1,
        "Should match NOUN with exactly 2 particles"
    );
}

#[test]
fn test_quantifier_range_min_max() {
    // Test: [upos=ADP]{1,3} should match 1-3 adpositions
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]{1,3}");

    let text = "猫がはを"; // NOUN with 3 particles (が, は, を all map to ADP)
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should match NOUN with 1-3 particles");
}

#[test]
fn test_quantifier_range_not_enough() {
    let matcher = compile_test_pattern("[upos=NOUN] [upos=ADP]{2,4}");

    let text = "猫が"; // Only 1 particle (below minimum)
    let tokens = tokenize_japanese(text);
    let matches = matcher.find_matches(&tokens);

    assert_eq!(
        matches.len(),
        0,
        "Should not match with less than minimum particles"
    );
}
