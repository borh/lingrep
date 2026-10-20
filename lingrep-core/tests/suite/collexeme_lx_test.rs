//! The two structural invariants of the simple-collexeme catalog that prose
//! cannot hold on its own.
//!
//! `catalog_examples.rs` discovers `patterns/collexeme.lx` and runs its
//! `examples:` claims with every other tracked catalog's. This file is what
//! remains after that sweep: the two claims an annotation cannot state.
//!
//! The invariants are the れる/られる partition — the catalog's whole answer to
//! a tag that carries four readings — and the presence of a `key:` on every
//! emitting pattern, which is what makes a collexeme list a collexeme list
//! rather than a surface-frequency list.

use std::collections::BTreeSet;

use lingrep_core::fsm::{CompileLimits, CompiledCatalog, PatternSource, compile_catalog};
use lingrep_core::testing::tokenize_japanese;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo, TokenInfoTrait};

fn matcher() -> CompiledCatalog {
    compile_catalog(
        PatternSource::named(
            "patterns/collexeme.lx",
            include_str!("../../../patterns/collexeme.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("collexeme lingrep patterns must compile strictly")
    .matcher
}

// `every_documented_collexeme_claim_holds_against_the_dictionary` lived here,
// checking this catalog and no other. `catalog_examples.rs` now checks every
// tracked catalog through `verify_examples`; two implementations of one check
// drift.

/// Sentences carrying れる/られる in every reading the tag conflates: direct
/// passive with an agent, adversative passive with an agent, agentless
/// passive, potential, honorific, and a suru-compound passive whose verb is in
/// the 未然形-サ rather than the 未然形-一般.
const RARERU_SENTENCES: [&str; 7] = [
    "彼は先生に叱られた。",
    "私は雨に降られた。",
    "子供に泣かれて困った。",
    "この本はよく読まれている。",
    "ここから富士山が見られる。",
    "先生が話される。",
    "部屋を掃除される。",
];

/// Every (未然形 verb, れる/られる) bigram in a sentence, by start index.
///
/// Computed from the tokens rather than from the catalog, so it is an
/// independent statement of where the two patterns are obliged to land. A
/// version derived from the matcher would agree with the matcher by
/// construction and prove nothing.
fn rareru_sites(tokens: &[TokenInfo]) -> BTreeSet<usize> {
    let attribute = |index: usize, key: TokenAttributeIndex| {
        tokens
            .get(index)
            .and_then(|token| token.get(key))
            .map(compact_str::CompactString::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    (0..tokens.len().saturating_sub(1))
        .filter(|&index| {
            attribute(index, TokenAttributeIndex::Pos1) == "動詞"
                && attribute(index, TokenAttributeIndex::CForm).starts_with("未然形")
                && attribute(index + 1, TokenAttributeIndex::Pos1) == "助動詞"
                && matches!(
                    attribute(index + 1, TokenAttributeIndex::Lemma).as_str(),
                    "れる" | "られる"
                )
        })
        .collect()
}

/// `rareru_ni_agent` and `rareru_no_agent` partition the れる/られる sites:
/// disjoint, and together exhaustive.
///
/// This is the catalog's entire answer to a tag that does not disambiguate
/// passive from potential from honorific — it splits on the one observable
/// fact, the presence of a に-marked agent, and the split is only honest if it
/// loses nothing and double-counts nothing. Both halves fail differently: a
/// widened lookaround body on one pattern alone breaks disjointness, and a
/// narrowed one on both breaks exhaustiveness. Prose in the catalog header
/// asserts this; nothing but a test can hold it.
#[test]
fn the_two_rareru_patterns_partition_every_site() {
    let matcher = matcher();

    for sentence in RARERU_SENTENCES {
        let tokens = tokenize_japanese(sentence);
        let expected = rareru_sites(&tokens);
        assert!(
            !expected.is_empty(),
            "{sentence}: the probe carries no れる/られる site at all"
        );

        let starts = |pattern: &str| -> BTreeSet<usize> {
            matcher
                .find_matches(&tokens)
                .into_iter()
                .filter(|matched| matched.pattern == pattern)
                .map(|matched| matched.start)
                .collect()
        };
        let with_agent = starts("rareru_ni_agent");
        let without_agent = starts("rareru_no_agent");

        assert!(
            with_agent.is_disjoint(&without_agent),
            "{sentence}: a site is claimed by both patterns: {:?}",
            with_agent.intersection(&without_agent).collect::<Vec<_>>()
        );
        assert_eq!(
            with_agent
                .union(&without_agent)
                .copied()
                .collect::<BTreeSet<_>>(),
            expected,
            "{sentence}: the two patterns do not cover the れる/られる sites"
        );
    }
}

/// Every emitting pattern declares a `key:`.
///
/// A collexeme analysis consumes a frequency list over the slot filler. A
/// pattern that forgets its key still matches, still publishes, and still
/// summarizes — grouped by matched surface, so 読ん, 読み and 読む become three
/// collexemes instead of one. The failure is silent at every layer except the
/// research conclusion, which is why it is asserted here.
#[test]
fn every_collexeme_pattern_is_keyed() {
    let matcher = matcher();
    let patterns = matcher.patterns();
    assert_eq!(patterns.len(), 13, "thirteen emitting constructions");

    // `key:` is compiler-owned and lands in the catalog policy rather than in
    // the descriptive metadata map, so this reads the policy — the same place
    // the engine reads it from when it projects a record.
    let policy = matcher.catalog_policy();
    let unkeyed: Vec<&str> = patterns
        .keys()
        .map(compact_str::CompactString::as_str)
        .filter(|name| {
            policy
                .get(name)
                .is_none_or(|pattern| pattern.key.is_empty())
        })
        .collect();
    assert!(
        unkeyed.is_empty(),
        "these patterns would summarize by surface rather than by collexeme: {unkeyed:?}"
    );
}
