// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use lingrep_core::fsm::{
    CompileLimits, ContextSegment, ContextWindow, DiagnosticCode, LookaroundContext,
    LookaroundReach, LookaroundScope, Match, PatternSource, compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn tokens(texts: &[&str]) -> Vec<TokenInfo> {
    texts
        .iter()
        .map(|text| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, (*text).into());
            token
        })
        .collect()
}

fn compile(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("scoped.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("{source}\nmust compile, but: {error:?}"))
    .matcher
}

fn whole(sentences: &[Vec<TokenInfo>]) -> Vec<ContextSegment<'_, [TokenInfo]>> {
    sentences
        .iter()
        .map(|sentence| ContextSegment {
            tokens: sentence.as_slice(),
            range: 0..sentence.len(),
        })
        .collect()
}

fn document<'a>(
    before: Vec<ContextSegment<'a, [TokenInfo]>>,
    after: Vec<ContextSegment<'a, [TokenInfo]>>,
) -> LookaroundContext<'a, [TokenInfo]> {
    LookaroundContext {
        windows: BTreeMap::from([(LookaroundScope::Document, ContextWindow { before, after })]),
    }
}

fn named(matches: Vec<Match>, name: &str) -> Vec<Match> {
    matches
        .into_iter()
        .filter(|matched| matched.pattern == name)
        .map(|mut matched| {
            matched.pattern = "_".into();
            matched
        })
        .collect()
}

fn spans_in_context(
    source: &str,
    sentence: &[&str],
    before: &[&[&str]],
    after: &[&[&str]],
) -> Vec<(usize, usize)> {
    let before: Vec<_> = before.iter().map(|texts| tokens(texts)).collect();
    let after: Vec<_> = after.iter().map(|texts| tokens(texts)).collect();
    let sentence = tokens(sentence);
    let context = document(whole(&before), whole(&after));
    compile(source)
        .find_matches_in_context(sentence.as_slice(), &context)
        .into_iter()
        .map(|matched| (matched.start, matched.end))
        .collect()
}

const BODIES: [&str; 3] = [
    "[text=a] [text=b]?",
    "[text=a] | [text=b] [text=b]",
    "[text=b]{2,3}",
];
const FORMS: [&str; 4] = [
    "followed_by",
    "not_followed_by",
    "preceded_by",
    "not_preceded_by",
];

fn sentence_strategy() -> impl Strategy<Value = Vec<TokenInfo>> {
    prop::collection::vec(prop::sample::select(vec!["x", "a", "b"]), 0..4)
        .prop_map(|texts| tokens(&texts))
}

fn skipping_sentence_strategy() -> impl Strategy<Value = Vec<TokenInfo>> {
    prop::collection::vec(prop::sample::select(vec!["x", "a", "b", "s"]), 0..5)
        .prop_map(|texts| tokens(&texts))
}

/// The last `reach` tokens of `before`, then `sentence`, then the first
/// `reach` tokens of `after`, with the index where `sentence` starts.
fn joined_within_reach(
    before: &[Vec<TokenInfo>],
    sentence: &[TokenInfo],
    after: &[Vec<TokenInfo>],
    reach: LookaroundReach,
) -> (Vec<TokenInfo>, usize) {
    let before: Vec<&TokenInfo> = before.iter().flatten().collect();
    let before = &before[before.len().saturating_sub(reach.before)..];
    let joined = before
        .iter()
        .copied()
        .chain(sentence)
        .chain(after.iter().flatten().take(reach.after))
        .cloned()
        .collect();
    (joined, before.len())
}

proptest! {
    /// A `within=document` lookaround over whole neighbouring sentences
    /// matches as the sentence-local lookaround does over the concatenated
    /// sentences, and an unscoped one ignores the context.
    #[test]
    fn a_document_lookaround_matches_as_over_the_joined_sentences(
        before in prop::collection::vec(sentence_strategy(), 0..3),
        sentence in sentence_strategy(),
        after in prop::collection::vec(sentence_strategy(), 0..3),
        body in prop::sample::select(BODIES.to_vec()),
        form in prop::sample::select(FORMS.to_vec()),
    ) {
        let source = format!(
            "pattern local := [text=x] {form}({body}).\n\
             pattern scoped := [text=x] {form}({body}, within=document)."
        );
        let matcher = compile(&source);
        let context = document(whole(&before), whole(&after));
        let in_context = matcher.find_matches_in_context(sentence.as_slice(), &context);

        let offset: usize = before.iter().map(Vec::len).sum();
        let joined: Vec<TokenInfo> =
            before.iter().chain([&sentence]).chain(&after).flatten().cloned().collect();
        let expected: Vec<Match> = named(matcher.find_matches(joined.as_slice()), "local")
            .into_iter()
            .filter(|matched| matched.start >= offset && matched.end <= offset + sentence.len())
            .map(|mut matched| {
                matched.start -= offset;
                matched.end -= offset;
                matched
            })
            .collect();
        prop_assert_eq!(named(in_context.clone(), "scoped"), expected);
        prop_assert_eq!(
            named(in_context, "local"),
            named(matcher.find_matches(sentence.as_slice()), "local")
        );
    }
}

proptest! {
    /// Under `skip:`, a `within=document` lookaround matches as the
    /// sentence-local lookaround does over the sentence joined with the
    /// window trimmed to the lookaround's reach, skipped tokens included.
    #[test]
    fn a_skipping_document_lookaround_matches_as_over_the_sentences_within_its_reach(
        before in prop::collection::vec(skipping_sentence_strategy(), 0..3),
        sentence in skipping_sentence_strategy(),
        after in prop::collection::vec(skipping_sentence_strategy(), 0..3),
        body in prop::sample::select(BODIES.to_vec()),
        form in prop::sample::select(FORMS.to_vec()),
    ) {
        let source = format!(
            "test s := text=s.\n\
             skip: @s\npattern local := [text=x] {form}({body}).\n\
             skip: @s\npattern scoped := [text=x] {form}({body}, within=document)."
        );
        let matcher = compile(&source);
        let reach = matcher.lookaround_reach()[&LookaroundScope::Document];
        let context = document(whole(&before), whole(&after));
        let in_context = matcher.find_matches_in_context(sentence.as_slice(), &context);

        let (joined, offset) = joined_within_reach(&before, &sentence, &after, reach);
        let expected: Vec<Match> = named(matcher.find_matches(joined.as_slice()), "local")
            .into_iter()
            .filter(|matched| matched.start >= offset && matched.end <= offset + sentence.len())
            .map(|mut matched| {
                matched.start -= offset;
                matched.end -= offset;
                matched
            })
            .collect();
        prop_assert_eq!(named(in_context, "scoped"), expected);
    }
}

#[test]
fn under_skip_the_reach_is_the_body_width_and_skipped_context_tokens_count_toward_it() {
    let source = "test s := text=s.\n\
                  skip: @s\npattern p := preceded_by([text=p], within=document) [text=a].";
    assert_eq!(
        compile(source).lookaround_reach()[&LookaroundScope::Document],
        LookaroundReach {
            before: 1,
            after: 0
        }
    );
    assert_eq!(
        spans_in_context(source, &["s", "a"], &[&["p"]], &[]),
        [(1, 2)]
    );
    assert_eq!(spans_in_context(source, &["a"], &[&["p", "s"]], &[]), []);
}

#[test]
fn under_skip_sentence_anchors_in_a_scoped_body_step_over_skipped_tokens() {
    let skip = "test s := text=s.\nskip: @s\n";
    let followed =
        format!("{skip}pattern p := [text=x] followed_by($ [text=a] [text=b]?, within=document).");
    assert_eq!(
        spans_in_context(&followed, &["x"], &[], &[&["s", "a"]]),
        [(0, 1)]
    );
    assert_eq!(spans_in_context(&followed, &["x"], &[], &[&["b", "a"]]), []);

    let preceded = format!(
        "{skip}pattern p := preceded_by(^ [text=b]? [text=x] $, within=document) [text=a]."
    );
    assert_eq!(
        spans_in_context(&preceded, &["a"], &[&["x", "s"]], &[]),
        [(0, 1)]
    );
    assert_eq!(spans_in_context(&preceded, &["a"], &[&["x", "b"]], &[]), []);
}

#[test]
fn sentence_anchors_in_a_scoped_body_hold_at_the_neighbouring_sentence_boundaries() {
    let followed = "pattern p := [text=x] followed_by($ [text=a], within=document).";
    assert_eq!(spans_in_context(followed, &["x"], &[], &[&["a"]]), [(0, 1)]);
    assert_eq!(spans_in_context(followed, &["x", "b"], &[], &[&["a"]]), []);

    let preceded = "pattern p := preceded_by(^ [text=x], within=document) [text=a].";
    assert_eq!(spans_in_context(preceded, &["a"], &[&["x"]], &[]), [(0, 1)]);
    assert_eq!(spans_in_context(preceded, &["a"], &[&["b", "x"]], &[]), []);
}

#[test]
fn a_segment_cut_inside_its_sentence_has_no_anchor_at_the_cut() {
    let source = "pattern p := preceded_by(^ [text=x], within=document) [text=a].";
    let previous = tokens(&["x", "x"]);
    let sentence = tokens(&["a"]);
    let cut = ContextSegment {
        tokens: previous.as_slice(),
        range: 1..2,
    };
    let context = document(vec![cut], Vec::new());
    assert_eq!(
        compile(source).find_matches_in_context(sentence.as_slice(), &context),
        []
    );
}

#[test]
fn a_scope_without_a_window_matches_sentence_locally() {
    let source = "pattern p := [text=x] followed_by([text=a], within=paragraph).";
    assert_eq!(spans_in_context(source, &["x"], &[], &[&["a"]]), []);
    assert_eq!(spans_in_context(source, &["x", "a"], &[], &[]), [(0, 1)]);
}

#[test]
fn lookaround_reach_is_the_widest_body_per_scope_and_side() {
    let matcher = compile(
        "pattern p := [text=x] preceded_by([text=a] [text=b]?, within=document) \
         followed_by([text=a]{1,3}, within=document).\n\
         pattern q := [text=x] followed_by([text=a], within=paragraph) followed_by([text=a]+).",
    );
    assert_eq!(
        matcher.lookaround_reach(),
        BTreeMap::from([
            (
                LookaroundScope::Document,
                LookaroundReach {
                    before: 2,
                    after: 3
                }
            ),
            (
                LookaroundScope::Division("paragraph".into()),
                LookaroundReach {
                    before: 0,
                    after: 1
                }
            ),
        ])
    );
}

#[test]
fn scoped_lookarounds_reject_unbounded_bodies_and_nested_lookarounds() {
    let rows = [
        (
            "pattern p := [text=x] followed_by([text=a]+, within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
        (
            "pattern p := [text=x] preceded_by([text=a] followed_by([text=b]), within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
        (
            "fragment f := [text=a] followed_by([text=b]).\n\
             pattern p := [text=x] followed_by(@f, within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
        (
            "test s := text=s.\nskip: @s\npattern p := [text=x] followed_by([text=a], within=document).\n\
             pattern q := [text=y] followed_by([@s]* [text=a & !@s], within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
        (
            "test s := text=s.\nskip: @s\npattern p := [text=x] followed_by([text=a]+, within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
        (
            "test s := text=s.\nskip: @s\npattern p := [text=x] followed_by([text=a] followed_by([text=b]), within=document).",
            DiagnosticCode::ScopedLookaroundUnsupported,
        ),
    ];
    for (catalog, expected) in rows {
        let error = match compile_catalog(
            PatternSource::named("scoped.lx", catalog),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("compiled: {catalog}"),
            Err(error) => error,
        };
        assert_eq!(
            error
                .diagnostics
                .iter()
                .map(lingrep_core::fsm::Diagnostic::code)
                .collect::<Vec<_>>(),
            [expected],
            "{catalog}: {:?}",
            error.diagnostics
        );
    }
}
