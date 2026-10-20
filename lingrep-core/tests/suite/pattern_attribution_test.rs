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
//! Tests for correct pattern name attribution in matches
//!
//! This tests that when multiple patterns overlap or share sub-patterns,
//! each match is correctly attributed to the pattern that actually matched.

use lingrep_core::fsm::{CompileLimits, CompiledCatalog, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn make_token(orth: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token
}

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("attribution.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

#[allow(dead_code)]
fn make_token_with_pos(orth: &str, pos: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Pos1, pos.into());
    token
}

#[test]
fn test_or_pattern_name_attribution() {
    let fsm = compile("or_test := [text=a] | [text=b].");

    let tokens = vec![make_token("a"), make_token("x"), make_token("b")];

    let matches = fsm.find_matches(&tokens);

    assert_eq!(matches.len(), 2, "Should find 2 matches for OR pattern");

    for m in &matches {
        assert_eq!(
            m.pattern.as_str(),
            "or_test",
            "All matches should have pattern 'or_test', got '{}'",
            m.pattern
        );
    }
}

#[test]
fn test_nested_pattern_reference_attribution() {
    let fsm = compile("simple_a := [text=a].\ncomplex := @simple_a [text=b].");

    let tokens = vec![make_token("a"), make_token("b")];

    let matches = fsm.find_matches(&tokens);

    for m in &matches {
        let surface: String = tokens[m.start..m.end]
            .iter()
            .map(|t| t.orth().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("");

        if surface == "a" {
            assert_eq!(
                m.pattern.as_str(),
                "simple_a",
                "Single token 'a' should match simple_a, got {}",
                m.pattern
            );
        } else if surface == "ab" {
            assert_eq!(
                m.pattern.as_str(),
                "complex",
                "Two tokens 'ab' should match complex, got {}",
                m.pattern
            );
        }
    }
}

#[test]
fn test_surface_form_matches_pattern_extent() {
    let fsm = compile("three_tokens := [text=a] [text=b] [text=c].");

    let tokens = vec![
        make_token("a"),
        make_token("b"),
        make_token("c"),
        make_token("d"),
    ];

    let matches = fsm.find_matches(&tokens);

    assert_eq!(matches.len(), 1, "Should find 1 match");

    let m = &matches[0];
    assert_eq!(m.start, 0);
    assert_eq!(m.end, 3, "End should be 3 (exclusive), not {}", m.end);

    let surface: String = tokens[m.start..m.end]
        .iter()
        .map(|t| t.orth().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        surface, "abc",
        "Surface form should be 'abc', got '{surface}'"
    );
}

#[test]
fn test_complex_or_pattern_attribution() {
    // Test pattern like: かもしれない := (@用言終止形 | [upos=NOUN] | @形状詞) か も [lemma=知る] @NAI
    // Where the first part is an OR of multiple alternatives

    let fsm = compile("sub_a := [text=a].\ncomplex_or := @sub_a [text=x] [text=y].");

    // Test with sub_a followed by x, y
    let tokens1 = vec![make_token("a"), make_token("x"), make_token("y")];

    let matches = fsm.find_matches(&tokens1);

    println!("Matches: {:?}", matches.len());
    for m in &matches {
        println!(
            "  pattern='{}', start={}, end={}",
            m.pattern, m.start, m.end
        );
    }

    let complex_matches: Vec<_> = matches
        .iter()
        .filter(|m| m.pattern.as_str() == "complex_or")
        .collect();

    assert_eq!(
        complex_matches.len(),
        1,
        "Should find exactly 1 complex_or match, found {}",
        complex_matches.len()
    );
}

/// Test that mimics the 'かもしれない' pattern structure from sfe.lx
///
/// Pattern: かもしれない := (?<命題> @用言終止形 | [upos=NOUN] | @形状詞) か も [lemma=知る] @NAI.
///
/// This pattern has:
/// - A capture group with OR of multiple alternatives
/// - Followed by specific tokens
///
/// The issue: 'かもしれない' and '形状詞' were showing identical surface forms
#[test]
fn test_kamoshirenai_style_pattern() {
    let fsm = compile(
        "verb_terminal := [upos=VERB].\n\
         keiyoushi := [upos=ADJ].\n\
         nai_pattern := [lemma=nai].\n\
         kamoshirenai_simple := @keiyoushi [text=KA] [text=MO] [lemma=shiru] @nai_pattern.\n\
         kamoshirenai := (@verb_terminal | @keiyoushi) [text=KA] [text=MO] [lemma=shiru] @nai_pattern.\n\
         simple_keiyoushi := [upos=ADJ].",
    );

    // Create tokens that should match kamoshirenai
    // Using ADJ branch: ADJ KA MO shiru nai
    let mut tokens = Vec::new();

    let mut t1 = TokenInfo::default();
    t1.set(TokenAttributeIndex::Orth, "yoi".into());
    t1.set(TokenAttributeIndex::Pos, "ADJ".into()); // Use Pos, not Pos1
    tokens.push(t1);

    let mut t2 = TokenInfo::default();
    t2.set(TokenAttributeIndex::Orth, "KA".into());
    tokens.push(t2);

    let mut t3 = TokenInfo::default();
    t3.set(TokenAttributeIndex::Orth, "MO".into());
    tokens.push(t3);

    let mut t4 = TokenInfo::default();
    t4.set(TokenAttributeIndex::Orth, "shimasu".into());
    t4.set(TokenAttributeIndex::Lemma, "shiru".into());
    tokens.push(t4);

    let mut t5 = TokenInfo::default();
    t5.set(TokenAttributeIndex::Orth, "nai".into());
    t5.set(TokenAttributeIndex::Lemma, "nai".into());
    tokens.push(t5);

    let matches = fsm.find_matches(&tokens);

    println!("\n=== kamoshirenai test ===");
    println!("Tokens:");
    for (i, t) in tokens.iter().enumerate() {
        println!(
            "  [{}] orth={:?}, pos={:?}, lemma={:?}",
            i,
            t.orth(),
            t.pos(),
            t.lemma()
        );
    }

    println!("\nMatches found: {}", matches.len());
    for m in &matches {
        let surface: String = tokens[m.start..m.end]
            .iter()
            .map(|t| t.orth().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "  pattern='{}', start={}, end={}, surface='{}'",
            m.pattern, m.start, m.end, surface
        );
    }

    // Collect matches by pattern name
    let kamoshirenai_matches: Vec<_> = matches
        .iter()
        .filter(|m| m.pattern.as_str().starts_with("kamoshirenai"))
        .collect();

    let keiyoushi_matches: Vec<_> = matches
        .iter()
        .filter(|m| m.pattern.as_str() == "keiyoushi" || m.pattern.as_str() == "simple_keiyoushi")
        .collect();

    // Verify kamoshirenai matched
    assert!(
        !kamoshirenai_matches.is_empty(),
        "kamoshirenai pattern should match at least once, but found {} matches total",
        matches.len()
    );

    // Verify that kamoshirenai matches are 5 tokens long
    for m in &kamoshirenai_matches {
        assert_eq!(
            m.end - m.start,
            5,
            "kamoshirenai should match 5 tokens, got {} (start={}, end={})",
            m.end - m.start,
            m.start,
            m.end
        );

        let surface: String = tokens[m.start..m.end]
            .iter()
            .map(|t| t.orth().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join(" ");

        // Surface should be something like "yoi KA MO shimasu nai"
        assert!(
            surface.contains("KA") && surface.contains("MO"),
            "kamoshirenai surface '{surface}' should contain KA and MO"
        );
    }

    // Verify that keiyoushi matches are 1 token long
    for m in &keiyoushi_matches {
        assert_eq!(
            m.end - m.start,
            1,
            "keiyoushi should match 1 token, got {} (pattern={})",
            m.end - m.start,
            m.pattern
        );

        let surface: String = tokens[m.start..m.end]
            .iter()
            .map(|t| t.orth().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join(" ");

        // Surface should NOT contain KA MO
        assert!(
            !surface.contains("KA") && !surface.contains("MO"),
            "keiyoushi surface '{surface}' should NOT contain KA or MO"
        );
    }

    println!("\nVerification passed:");
    println!(
        "  kamoshirenai matches: {} (all 5 tokens)",
        kamoshirenai_matches.len()
    );
    println!(
        "  keiyoushi matches: {} (all 1 token)",
        keiyoushi_matches.len()
    );
}
