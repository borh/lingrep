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
//! Named group tests
//!
//! Tests for named grouping syntax: (?<name> pattern)

use crate::support::token_with;
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn compile_one(body: &str) -> lingrep_core::fsm::CompiledCatalog {
    // body-only: wraps as "pattern test := {body}."
    lingrep_core::testing::compile_test_pattern(body)
}

/// Compiles multiple definitions through the whole-document compiler.
fn compile_many(defs: &[&str]) -> lingrep_core::fsm::CompiledCatalog {
    let source = defs.join("\n");
    compile_catalog(
        PatternSource::named("named-groups.lx", &source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

fn token(pos: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Pos, pos.into());
    token
}

#[derive(Debug, PartialEq, Eq)]
struct TopLevelMatch {
    pattern: String,
    start: usize,
    end: usize,
}

fn top_level(matches: Vec<Match>) -> Vec<TopLevelMatch> {
    matches
        .into_iter()
        .map(|matched| TopLevelMatch {
            pattern: matched.pattern.to_string(),
            start: matched.start,
            end: matched.end,
        })
        .collect()
}

fn catalog_match(
    matcher: &lingrep_core::fsm::CompiledCatalog,
    pattern: &str,
    tokens: &[TokenInfo],
) -> TopLevelMatch {
    top_level(matcher.find_matches(tokens))
        .into_iter()
        .find(|matched| matched.pattern == pattern)
        .unwrap_or_else(|| panic!("expected {pattern} to match"))
}

#[test]
fn repository_named_groups_retain_top_level_matches() {
    use TokenAttributeIndex::{CForm, Lemma, Orth, Pos};

    let sfe = compile_catalog(
        PatternSource::named("patterns/sfe.lx", include_str!("../../../patterns/sfe.lx")),
        CompileLimits::default(),
    )
    .expect("sfe catalog compiles")
    .matcher;

    let terminal = || token_with(&[(Orth, "する"), (Pos, "VERB"), (CForm, "終止形-一般")]);
    let continuative = || token_with(&[(Orth, "し"), (Pos, "VERB"), (CForm, "連用形-一般")]);
    let orth = |value| token_with(&[(Orth, value)]);
    let lemma = |orth_value, lemma_value| token_with(&[(Orth, orth_value), (Lemma, lemma_value)]);

    let mut observed = Vec::new();
    for (pattern, tokens) in [
        (
            "恐れがある",
            vec![
                terminal(),
                lemma("恐れ", "恐れ"),
                orth("が"),
                lemma("ある", "ある"),
            ],
        ),
        ("だろう", vec![terminal(), orth("だろう")]),
        ("であろう", vec![terminal(), orth("で"), orth("あろう")]),
        (
            "よう",
            vec![
                continuative(),
                token_with(&[(Orth, "よう"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "かもしれない",
            vec![
                token_with(&[(Orth, "事"), (Pos, "NOUN")]),
                orth("か"),
                orth("も"),
                lemma("知れ", "知れる"),
                orth("ない"),
            ],
        ),
        ("だ", vec![orth("の"), orth("だ")]),
        (
            "必要がある",
            vec![
                terminal(),
                lemma("必要", "必要"),
                orth("が"),
                lemma("ある", "有る"),
            ],
        ),
        (
            "恐れがある",
            vec![
                terminal(),
                lemma("恐れ", "恐れ"),
                orth("が"),
                lemma("ある", "有る"),
            ],
        ),
        ("だろう", vec![terminal(), orth("だろう")]),
        ("であろう", vec![terminal(), orth("で"), orth("あろう")]),
        (
            "う",
            vec![
                terminal(),
                token_with(&[(Orth, "う"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "よう",
            vec![
                continuative(),
                token_with(&[(Orth, "よう"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "かもしれない",
            vec![
                token_with(&[(Orth, "事"), (Pos, "NOUN")]),
                orth("か"),
                orth("も"),
                lemma("知れ", "知れる"),
                orth("ない"),
            ],
        ),
    ] {
        observed.push(catalog_match(&sfe, pattern, &tokens));
    }

    insta::assert_debug_snapshot!(observed);
}

fn matched(pattern: &str, start: usize, end: usize) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

fn captured(mut matched: Match, captures: &[(&str, usize, usize)]) -> Match {
    matched.captures = captures
        .iter()
        .map(|(name, start, end)| lingrep_core::fsm::Capture {
            name: (*name).into(),
            path: format!("{}/{}", matched.pattern, name).into(),
            start: *start,
            end: *end,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    matched
}

#[test]
fn test_named_group_single() {
    let matcher = compile_one("(?<命題> [upos=NOUN] [upos=VERB])");

    let tokens = [token("NOUN"), token("VERB")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(matched("test", 0, 2), &[("命題", 0, 2)])]
    );
}

#[test]
fn test_named_group_multiple() {
    let matcher = compile_one("(?<命題> [upos=NOUN]) (?<述語> [upos=VERB])");

    let tokens = [token("NOUN"), token("VERB")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(
            matched("test", 0, 2),
            &[("命題", 0, 1), ("述語", 1, 2)]
        )]
    );
}

#[test]
fn test_named_group_nested() {
    let matcher = compile_many(&[
        "phrase := [upos=NOUN] [upos=ADP].",
        "pattern test := (?<名詞句> @phrase).",
    ]);

    let tokens = [token("NOUN"), token("ADP")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![
            matched("phrase", 0, 2),
            captured(matched("test", 0, 2), &[("名詞句", 0, 2)])
        ]
    );
}

#[test]
fn test_named_group_with_quantifier() {
    let matcher = compile_one("(?<名詞> [upos=NOUN]{2})");

    let tokens = [token("NOUN"), token("NOUN")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(matched("test", 0, 2), &[("名詞", 0, 2)])]
    );
}

#[test]
fn test_named_group_or_alternative() {
    let matcher = compile_many(&[
        "noun := [upos=NOUN].",
        "adj := [upos=ADJ].",
        "pattern test := (?<体言> @noun | @adj).",
    ]);

    let tokens = [token("NOUN")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![
            matched("noun", 0, 1),
            captured(matched("test", 0, 1), &[("体言", 0, 1)])
        ]
    );
}

#[test]
fn capture_selection_is_source_ordered_greedy_and_last_participating() {
    use TokenAttributeIndex::Orth;

    let a = || token_with(&[(Orth, "a")]);
    let b = || token_with(&[(Orth, "b")]);

    let alternative = compile_many(&["pattern alt := ((?<left> [text=a]) | (?<right> [text=a]))."]);
    assert_eq!(
        alternative.find_matches(&[a()]),
        vec![captured(matched("alt", 0, 1), &[("left", 0, 1)])]
    );

    let greedy = compile_many(&["pattern greedy := (?<x> [text=a]{1,3}) [text=a]."]);
    assert_eq!(
        greedy.find_matches(&[a(), a(), a(), a()]),
        vec![
            captured(matched("greedy", 0, 4), &[("x", 0, 3)]),
            captured(matched("greedy", 1, 4), &[("x", 1, 3)]),
            captured(matched("greedy", 2, 4), &[("x", 2, 3)]),
        ]
    );

    let repeated = compile_many(&["pattern repeated := ((?<x> [text=a]) [text=b]?){1,2}."]);
    assert_eq!(
        repeated.find_matches(&[a(), b(), a()]),
        vec![
            captured(matched("repeated", 0, 3), &[("x", 2, 3)]),
            captured(matched("repeated", 2, 3), &[("x", 2, 3)]),
        ]
    );
}

#[test]
fn fragment_capture_paths_are_hygienic_per_reference_occurrence() {
    let matcher = compile_many(&[
        "fragment F := (?<unit> [upos=NOUN]).",
        "pattern p := @F @F.",
    ]);
    let matches = matcher.find_matches(&[token("NOUN"), token("NOUN")]);
    let matched = matches
        .iter()
        .find(|matched| matched.pattern == "p" && matched.start == 0)
        .expect("expanded pattern must match");
    assert_eq!(
        matched
            .captures
            .iter()
            .map(|capture| (capture.path.as_str(), capture.start, capture.end))
            .collect::<Vec<_>>(),
        vec![("p/F#1/unit", 0, 1), ("p/F#2/unit", 1, 2)]
    );
}
