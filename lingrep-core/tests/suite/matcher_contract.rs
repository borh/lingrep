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
use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, FsmBuilder, PatternSource, compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn token(orth: &str, pos1: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Pos1, pos1.into());
    token
}

fn nfa_matches(fsm: &CompiledCatalog, tokens: &[TokenInfo]) -> Vec<lingrep_core::fsm::Match> {
    fsm.find_matches(tokens)
}

fn assert_compiled_type(_: &CompiledCatalog) {}

fn particle_run_fsm() -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("particle-run.lx", "particle_run := [unidic.pos1=助詞]+."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn particle_run_tokens() -> Vec<TokenInfo> {
    vec![
        token("私", "代名詞"),
        token("は", "助詞"),
        token("と", "助詞"),
        token("も", "助詞"),
        token("行く", "動詞"),
    ]
}

#[test]
fn repetition_stops_before_the_first_nonmatching_terminator() {
    let fsm = particle_run_fsm();
    assert_compiled_type(&fsm);
    let tokens = particle_run_tokens();
    let matches = nfa_matches(&fsm, &tokens);
    assert_eq!(
        matches
            .iter()
            .map(|m| (m.pattern.as_str(), m.start, m.end))
            .collect::<Vec<_>>(),
        vec![
            ("particle_run", 1, 4),
            ("particle_run", 2, 4),
            ("particle_run", 3, 4),
        ],
    );
    assert!(matches.iter().all(|matched| matched.captures.is_empty()));
}

#[test]
fn empty_pattern_set_and_empty_tokens_return_no_matches() {
    let fsm = compile_catalog(
        PatternSource::named("empty.lx", ""),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    assert!(nfa_matches(&fsm, &[]).is_empty());

    let populated = compile_catalog(
        PatternSource::named("p.lx", "p := [text=a]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    assert!(nfa_matches(&populated, &[]).is_empty());
}

#[test]
fn canonical_sorting_is_not_the_legacy_tie_order_contract() {
    let fsm = compile_catalog(
        PatternSource::named("ties.lx", "b := [text=a].\na := [text=a]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let matches = nfa_matches(&fsm, &[token("a", "X")]);
    // Assert the matcher's NATIVE output order — canonicalization is the
    // matcher's contract, so sorting here would launder a violation.
    assert_eq!(
        matches
            .iter()
            .map(|m| m.pattern.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"],
    );
}

#[test]
fn pattern_reference_uses_the_definition_in_the_document() {
    let fsm = compile_catalog(
        PatternSource::named("reference.lx", "x := [text=x].\nxy := @x [text=y]."),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    let matches = nfa_matches(&fsm, &[token("x", "X"), token("y", "X")]);
    assert!(
        matches
            .iter()
            .any(|m| m.pattern == "xy" && m.start == 0 && m.end == 2)
    );
}

#[test]
fn canonicalization_orders_ties_by_start_end_then_pattern() {
    let mut matches = vec![
        match_at("b", 2, 3),
        match_at("a", 1, 2),
        match_at("b", 1, 2),
    ];
    lingrep_core::fsm::matcher::canonicalize_matches(&mut matches);
    assert_eq!(
        matches
            .iter()
            .map(|m| (m.start, m.end, m.pattern.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, 2, "a"), (1, 2, "b"), (2, 3, "b")],
    );
}

/// `Match` equality must stay discriminating over everything it still carries.
///
/// A match carries no context window, so two matches cannot differ in one.
/// Nothing is weakened in the oracle differential by that: a window is a pure
/// function of `start`, `end`, and sentence length, all of which the
/// comparison still covers.
#[test]
fn canonical_comparison_rejects_capture_only_divergence() {
    let bare = vec![match_at("p", 1, 2)];
    let with_capture = vec![with_capture(match_at("p", 1, 2), "g", 1, 2)];
    assert_ne!(bare, with_capture);
    assert_eq!(bare, vec![match_at("p", 1, 2)]);
}

fn with_capture(
    mut matched: lingrep_core::fsm::Match,
    name: &str,
    start: usize,
    end: usize,
) -> lingrep_core::fsm::Match {
    matched.captures = vec![lingrep_core::fsm::Capture {
        name: name.into(),
        path: format!("{}/{}", matched.pattern, name).into(),
        start,
        end,
    }]
    .into_boxed_slice();
    matched
}

fn match_at(pattern: &str, start: usize, end: usize) -> lingrep_core::fsm::Match {
    lingrep_core::fsm::Match {
        pattern: pattern.into(),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

#[test]
fn public_builder_produces_the_only_standard_matcher() {
    let mut builder = FsmBuilder::new();
    builder
        .add_definition(PatternSource::named("p.lx", "p := [text=a]."))
        .unwrap();
    let matcher = builder.build().unwrap().matcher;

    assert_eq!(
        matcher.find_matches(&[token("a", "X")]),
        vec![match_at("p", 0, 1)],
    );
}

#[test]
fn captureless_regular_lookaround_guards_matches_at_token_boundaries() {
    let matcher = compile_catalog(
        PatternSource::named(
            "lookaround.lx",
            concat!(
                "pattern before := preceded_by([text=a] [text=b]*) [text=x].\n",
                "pattern not_before := not_preceded_by([text=z]+) [text=x].\n",
                "pattern after := [text=x] followed_by([text=a] [text=b]).\n",
                "pattern not_after := [text=x] not_followed_by([text=z]).\n",
                "pattern nested := [text=x] followed_by([text=a] not_followed_by([text=z])).",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("captureless regular lookaround must compile")
    .matcher;
    let tokens = [
        token("a", "X"),
        token("b", "X"),
        token("x", "X"),
        token("a", "X"),
        token("b", "X"),
    ];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![
            match_at("after", 2, 3),
            match_at("before", 2, 3),
            match_at("nested", 2, 3),
            match_at("not_after", 2, 3),
            match_at("not_before", 2, 3),
        ],
    );
}

#[test]
fn captures_and_span_equality_share_the_sentence_lookaround_facts() {
    let capture = compile_catalog(
        PatternSource::named(
            "capture-lookaround.lx",
            "pattern p := (?<whole> [text=x] followed_by([text=a])).",
        ),
        CompileLimits::default(),
    )
    .expect("lookaround inside an outer capture must compile")
    .matcher;
    let tokens = [token("x", "X"), token("a", "X")];
    let matches = capture.find_matches(&tokens);
    assert_eq!(capture.find_matches_with_trace(&tokens).matches, matches);
    assert_eq!((matches[0].start, matches[0].end), (0, 1));
    assert_eq!(
        (matches[0].captures[0].start, matches[0].captures[0].end),
        (0, 1)
    );

    let equality = compile_catalog(
        PatternSource::named(
            "equality-lookaround.lx",
            "pattern p := (?<unit> [text=x] followed_by([text=a])) [text=a] same_as($unit).",
        ),
        CompileLimits::default(),
    )
    .expect("lookaround inside an outer span-equality definition must compile")
    .matcher;
    let tokens = [token("x", "X"), token("a", "X"), token("x", "X")];
    let matches = equality.find_matches(&tokens);
    assert_eq!(equality.find_matches_with_trace(&tokens).matches, matches);
    assert_eq!(
        matches
            .iter()
            .map(|matched| (matched.start, matched.end))
            .collect::<Vec<_>>(),
        vec![(0, 3)]
    );
}

#[test]
fn lookaround_handles_sentence_boundaries_and_nullable_bodies() {
    let matcher = compile_catalog(
        PatternSource::named(
            "lookaround-boundaries.lx",
            concat!(
                "pattern start_negative := not_preceded_by([text=a]) [text=x].\n",
                "pattern end_negative := [text=x] not_followed_by([text=a]).\n",
                "pattern start_anchor := preceded_by(^) [text=x].\n",
                "pattern end_anchor := [text=x] followed_by($).\n",
                "pattern nullable := followed_by([text=a]?) [text=x].",
            ),
        ),
        CompileLimits::default(),
    )
    .expect("boundary and nullable lookaround must compile")
    .matcher;

    assert_eq!(
        matcher
            .find_matches(&[token("x", "X")])
            .into_iter()
            .map(|matched| matched.pattern.to_string())
            .collect::<Vec<_>>(),
        vec![
            "end_anchor",
            "end_negative",
            "nullable",
            "start_anchor",
            "start_negative",
        ]
    );
}

/// A `preceded_by` whose body is a single token is compiled as a specialized
/// previous-token assertion with no assertion program. Nested inside another
/// lookaround's body it must instead go through the general occurrence path,
/// and this pins that it does so *correctly* rather than merely compiling.
///
/// Before the fix this shape aborted the process at `assertion.rs`, so no test
/// could describe its behaviour at all — `captureless_regular_lookaround_…`
/// nests `not_followed_by`, which was never specialized and never reached it.
#[test]
fn a_nested_specialized_preceded_by_matches_only_when_the_inner_context_holds() {
    let matcher = compile_catalog(
        PatternSource::named(
            "nested-previous.lx",
            "pattern p := preceded_by(preceded_by([text=a]) [text=b]) [text=x].",
        ),
        CompileLimits::default(),
    )
    .expect("a nested specialized preceded_by must compile")
    .matcher;

    // `x` preceded by a `b` that is itself preceded by an `a`.
    let holds = [token("a", "X"), token("b", "X"), token("x", "X")];
    // Same shape, but the inner context fails: `b` is preceded by `c`.
    let fails = [token("c", "X"), token("b", "X"), token("x", "X")];

    assert_eq!(
        matcher.find_matches(&holds).len(),
        1,
        "`a b x` satisfies both the outer and the inner assertion"
    );
    assert!(
        matcher.find_matches(&fails).is_empty(),
        "`c b x` must not match: the inner preceded_by is unsatisfied, and a \
         nested assertion that is silently ignored would still match here"
    );
}
