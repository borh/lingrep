//! Basic public NFA matcher invariants.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::testing::token;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn optional_token(orth: Option<&str>) -> TokenInfo {
    let mut token = TokenInfo::default();
    if let Some(orth) = orth {
        token.set(TokenAttributeIndex::Orth, orth.into());
    }
    token
}

#[test]
fn public_matcher_is_deterministic_and_canonical() {
    let matcher = compile_catalog(
        PatternSource::named("canonical.lx", "b := [text=a].\na := [text=a]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let tokens = vec![token("a")];

    let first = matcher.find_matches(&tokens);
    assert_eq!(first, matcher.find_matches(&tokens));
    assert_eq!(
        first.iter().map(|m| m.pattern.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}

#[hegel::test(test_cases = 100)]
fn named_and_unnamed_group_erasure_preserves_full_matches(tc: hegel::TestCase) {
    let matcher = compile_catalog(
        PatternSource::named(
            "group-erasure-property.lx",
            concat!(
                "plain := [text=a] [text=b].\n",
                "unnamed := ([text=a] [text=b]).\n",
                "named := (?<pair> [text=a] [text=b]).\n",
                "nested := ((?<outer> ([text=a] [text=b]))).",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("the fixed group-erasure fixture must compile")
    .matcher;
    let symbols = tc.draw(hegel::generators::binary().max_size(64));
    let tokens = symbols
        .into_iter()
        .map(|symbol| match symbol % 3 {
            0 => token("a"),
            1 => token("b"),
            _ => token("z"),
        })
        .collect::<Vec<_>>();
    let matches = matcher.find_matches(&tokens);

    let identities = |pattern: &str| {
        matches
            .iter()
            .filter(|matched| matched.pattern == pattern)
            .map(|matched| (matched.start, matched.end))
            .collect::<Vec<_>>()
    };
    let expected = identities("plain");

    assert_eq!(identities("unnamed"), expected);
    assert_eq!(identities("named"), expected);
    assert_eq!(identities("nested"), expected);
}

#[hegel::test(test_cases = 500, derandomize = true)]
fn specialized_and_general_previous_token_paths_have_identical_matches(tc: hegel::TestCase) {
    let matcher = compile_catalog(
        PatternSource::named(
            "previous-token-differential.lx",
            concat!(
                "pattern specialized := preceded_by([text=a]) [text=x].\n",
                "pattern general := preceded_by(([text=a])) [text=x].",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("differential fixture must compile")
    .matcher;
    let symbols = tc.draw(hegel::generators::binary().max_size(96));
    let tokens = symbols
        .into_iter()
        .map(|symbol| match symbol % 3 {
            0 => token("a"),
            1 => token("x"),
            _ => token("z"),
        })
        .collect::<Vec<_>>();
    let matches = matcher.find_matches(&tokens);
    let identities = |pattern: &str| {
        matches
            .iter()
            .filter(|matched| matched.pattern == pattern)
            .map(|matched| (matched.start, matched.end, matched.captures.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(identities("general"), identities("specialized"));
}

#[hegel::test(test_cases = 500, derandomize = true)]
fn token_predicates_agree_across_nfa_previous_lookaround_and_capture_contexts(tc: hegel::TestCase) {
    let matcher = compile_catalog(
        PatternSource::named(
            "predicate-context-differential.lx",
            concat!(
                "pattern nfa := [text~/^a+$/].\n",
                "pattern previous := preceded_by([text~/^a+$/]) [text=x].\n",
                "pattern assertion := [text=x] followed_by(([text~/^a+$/])).\n",
                "pattern capture := (?<v> [text~/^a+$/]).",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("four-context predicate fixture must compile")
    .matcher;
    let symbols = tc.draw(hegel::generators::binary().max_size(64));

    for symbol in symbols {
        let candidate = match symbol % 7 {
            0 => optional_token(Some("a")),
            1 => optional_token(Some("aa")),
            2 => optional_token(Some("aaa")),
            3 => optional_token(Some("b")),
            4 => optional_token(Some("あ")),
            5 => optional_token(Some("")),
            _ => optional_token(None),
        };
        let tokens = [token("x"), candidate, token("x")];
        let matches = matcher.find_matches(&tokens);
        let has = |pattern: &str, start: usize, end: usize| {
            matches.iter().any(|matched| {
                matched.pattern == pattern && matched.start == start && matched.end == end
            })
        };

        let nfa = has("nfa", 1, 2);
        assert_eq!(has("capture", 1, 2), nfa, "tokens={tokens:?}");
        assert_eq!(has("previous", 2, 3), nfa, "tokens={tokens:?}");
        assert_eq!(has("assertion", 0, 1), nfa, "tokens={tokens:?}");
    }
}
