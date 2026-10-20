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
use hegel::generators;
use lingrep_core::fsm::{CompileLimits, DiagnosticCode, PatternSource, compile_catalog};
use lingrep_core::token_match::{AttributeKey, TokenAttributeIndex, TokenInfo};

fn token(orth: &str, lemma: Option<&str>, kana: Option<&str>, pron: Option<&str>) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    for (attribute, value) in [
        (TokenAttributeIndex::Lemma, lemma),
        (TokenAttributeIndex::Kana, kana),
        (TokenAttributeIndex::Pron, pron),
    ] {
        if let Some(value) = value {
            token.set(attribute, value.into());
        }
    }
    token
}

fn matcher(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("span-equality.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

#[test]
fn matches_multi_token_onomatopoeia_and_reports_the_unit() {
    let matcher = matcher("pattern redup := (?<unit> a b) same_as($unit).");
    let matches = matcher.find_matches(&[
        token("a", Some("A"), Some("ア"), Some("ア")),
        token("b", Some("B"), Some("ブ"), Some("ブ")),
        token("a", Some("A"), Some("ア"), Some("ア")),
        token("b", Some("B"), Some("ブ"), Some("ブ")),
    ]);
    assert_eq!(matches.len(), 1);
    assert_eq!((matches[0].start, matches[0].end), (0, 4));
    assert_eq!(matches[0].captures[0].name, "unit");
    assert_eq!(matches[0].captures[0].path, "redup/unit");
    assert_eq!(
        (matches[0].captures[0].start, matches[0].captures[0].end),
        (0, 2)
    );
}

#[test]
fn projection_is_exact_and_missing_values_do_not_match() {
    for (projection, expected) in [
        ("text", 0),
        ("lemma", 1),
        ("unidic.kana", 1),
        ("unidic.pron", 1),
    ] {
        let source = format!("pattern p := (?<u> x) same_as($u, by={projection}).");
        let matcher = matcher(&source);
        let matches = matcher.find_matches(&[
            token("x", Some("same"), Some("同"), Some("ドウ")),
            token("y", Some("same"), Some("同"), Some("ドウ")),
        ]);
        assert_eq!(matches.len(), expected, "projection {projection}");
    }

    let matcher = matcher("pattern p := (?<u> x) same_as($u, by=lemma).");
    assert!(
        matcher
            .find_matches(&[token("x", None, None, None), token("x", None, None, None)])
            .is_empty()
    );
}

#[test]
fn greedy_capture_falls_back_to_a_shorter_successful_width() {
    let matcher = matcher("pattern p := (?<u> a{1,3}) same_as($u).");
    let tokens = (0..4)
        .map(|_| token("a", Some("a"), Some("a"), Some("a")))
        .collect::<Vec<_>>();
    let matches = matcher.find_matches(&tokens);
    assert!(
        matches
            .iter()
            .any(|matched| matched.start == 0 && matched.end == 4 && matched.captures[0].end == 2)
    );
}

#[test]
fn rejects_nullable_unbounded_and_too_wide_referenced_captures() {
    for (source, code) in [
        (
            "pattern p := (?<u> a?) same_as($u).",
            DiagnosticCode::BackreferencedCaptureNullable,
        ),
        (
            "pattern p := (?<u> a+) same_as($u).",
            DiagnosticCode::BackreferencedCaptureUnbounded,
        ),
        (
            "pattern p := (?<u> a{9}) same_as($u).",
            DiagnosticCode::BackreferencedCaptureTooWide,
        ),
    ] {
        let error = match compile_catalog(
            PatternSource::named("invalid.lx", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("fixture must fail: {source}"),
            Err(error) => error,
        };
        assert_eq!(error.diagnostics[0].code(), code, "source {source}");
    }
}

#[test]
fn required_attributes_and_trace_cover_the_history_plan() {
    let matcher = matcher("pattern p := (?<u> [lemma=x]) same_as($u, by=unidic.kana).");
    assert_eq!(
        matcher.required_attributes(),
        vec![
            AttributeKey::new("lemma").unwrap(),
            AttributeKey::new("unidic.kana").unwrap(),
        ]
    );
    let tokens = [
        token("a", Some("x"), Some("same"), Some("A")),
        token("b", Some("y"), Some("same"), Some("B")),
    ];
    let trace = matcher.find_matches_with_trace(&tokens);
    assert_eq!(trace.matches, matcher.find_matches(&tokens));
    assert!(
        trace
            .events
            .iter()
            .any(|event| matches!(event, lingrep_core::fsm::TraceEvent::CaptureClose { .. }))
    );
}

#[test]
fn repeated_same_as_occurrences_have_one_capture_derived_trace_comparison() {
    let matcher = matcher("pattern p := (?<u> a) same_as($u) same_as($u).");
    let tokens = [
        token("a", Some("a"), Some("a"), Some("a")),
        token("a", Some("a"), Some("a"), Some("a")),
        token("a", Some("a"), Some("a"), Some("a")),
    ];

    let trace = matcher.find_matches_with_trace(&tokens);
    assert_eq!(trace.matches, matcher.find_matches(&tokens));
    let comparisons = trace
        .events
        .iter()
        .filter_map(|event| match event {
            lingrep_core::fsm::TraceEvent::SpanComparison {
                path,
                projection,
                start,
                end,
                matched,
            } => Some((path.as_str(), projection.clone(), *start, *end, *matched)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        comparisons,
        [("p/u", AttributeKey::new("text").unwrap(), 0, 1, true)]
    );
}

#[hegel::test(test_cases = 500, derandomize = true)]
fn generated_bounded_units_match_exactly_and_mutation_breaks_equality(tc: hegel::TestCase) {
    let width = tc.draw(generators::integers::<usize>().min_value(1).max_value(8));
    let projection = tc.draw(generators::sampled_from(vec![
        "text",
        "lemma",
        "unidic.kana",
        "unidic.pron",
    ]));
    tc.target(width as f64);
    let source = format!("pattern p := (?<u> a{{{width}}}) same_as($u, by={projection}).");
    let matcher = matcher(&source);
    let mut tokens = (0..width * 2)
        .map(|_| token("a", Some("a"), Some("a"), Some("a")))
        .collect::<Vec<_>>();
    assert!(
        matcher
            .find_matches(&tokens)
            .iter()
            .any(|matched| matched.start == 0 && matched.end == width * 2)
    );

    let attribute = match projection {
        "text" => TokenAttributeIndex::Orth,
        "lemma" => TokenAttributeIndex::Lemma,
        "unidic.kana" => TokenAttributeIndex::Kana,
        "unidic.pron" => TokenAttributeIndex::Pron,
        _ => unreachable!(),
    };
    tokens[width].set(attribute, "different".into());
    assert!(
        !matcher
            .find_matches(&tokens)
            .iter()
            .any(|matched| matched.start == 0 && matched.end == width * 2)
    );
}

/// The narrow deduplication key's whole purpose, preserved by the wide one.
///
/// `deduplicate` used to key candidates on `(position, the same_as operand's
/// slot)`, and that one slot was in the key because `same_as` reads it. The
/// **parentheses are what make this bite**: `deduplicate` runs at the end of
/// every `evaluate`, so a group collapses its own candidates before the
/// `same_as` after it can filter them. Without the group, `same_as` is
/// another element of the same sequence and runs first, which is why the
/// flat spelling survives a position-only key and this one does not.
///
/// Here `[]*` lets `$unit` start anywhere, so two assignments leave the group
/// at position 2 — `unit` = (0,2) and (1,2) — and only (0,2) has its
/// repetition after it. `repeat` yields levels longest-first, so the failing
/// (1,2) is enumerated first and a position-only key keeps exactly that one.
/// Verified: this test fails with the key narrowed to the position.
///
/// The key is now the whole slot vector, of which that slot is one component,
/// so this is the same guarantee generalized rather than a behaviour change.
/// Pinned because deleting a special case should leave visible evidence that
/// what it protected still holds
/// (docs/design/2026-08-29-where-capture-assignment.md).
#[test]
fn a_span_equality_binding_survives_a_failing_one_at_the_same_position() {
    let matcher = matcher("pattern p := ([]* (?<unit> []{1,2})) same_as($unit).");
    let matches = matcher.find_matches(&[
        token("a", Some("A"), Some("ア"), Some("ア")),
        token("b", Some("B"), Some("ブ"), Some("ブ")),
        token("a", Some("A"), Some("ア"), Some("ア")),
        token("b", Some("B"), Some("ブ"), Some("ブ")),
    ]);
    assert_eq!(matches.len(), 1, "the ab/ab repetition must be found");
    assert_eq!((matches[0].start, matches[0].end), (0, 4));
    assert_eq!(
        (matches[0].captures[0].start, matches[0].captures[0].end),
        (0, 2),
        "the two-token unit, not the one-token binding that reaches the same \
         position first"
    );
}
