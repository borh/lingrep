// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use lingrep_core::fsm::{CompileLimits, FsmBuilder, PatternSource, compile_catalog};

fn compile_error(source: &str) -> String {
    match compile_catalog(
        PatternSource::named("invalid.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("invalid lexicon must be rejected"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn lexicon_is_a_typed_emitting_definition() {
    let output = compile_catalog(
        PatternSource::named(
            "lex.lx",
            r#"
key: [term, segments(text)]
lexicon words by [text, norm] := {"大学院", "行う"}.
"#,
        ),
        CompileLimits::default(),
    )
    .unwrap();

    assert!(output.matcher.patterns().is_empty());
    assert_eq!(output.matcher.lexicons()[0].name(), "words");
    assert_eq!(output.matcher.lexicons()[0].member_count(), 2);
    assert_eq!(output.stats.definitions(), 1);
    assert_eq!(output.stats.lexicon_members(), 2);
}

#[test]
fn lexicon_members_and_projections_are_validated_after_decoding() {
    for (source, needle) in [
        (
            r#"lexicon words by [text] := {"", "x"}."#,
            "must not be empty",
        ),
        (
            r#"lexicon words by [text] := {"\u884c\u3046", "行う"}."#,
            "duplicate decoded",
        ),
        (
            r#"lexicon words by [text, text] := {"x"}."#,
            "duplicate lexicon",
        ),
        (
            r#"lexicon words by [lemma] := {"x"}."#,
            "must be `text` or `norm`",
        ),
    ] {
        let error = compile_error(source);
        assert!(error.contains(needle), "{error}");
    }
}

#[test]
fn lexicon_names_share_the_definition_namespace_but_are_not_expressions() {
    let duplicate = compile_error("x := [].\nlexicon x by [text] := {\"x\"}.");
    assert!(duplicate.contains("duplicate definition"));

    let reference = compile_error("p := @words.\nlexicon words by [text] := {\"x\"}.");
    assert!(reference.contains("lexicon is not an expression"));
}

#[test]
fn lexicon_member_budget_is_independent_and_accumulates_across_documents() {
    let limits = CompileLimits::default()
        .tightened(|limits| limits.max_lexicon_members = 2)
        .unwrap();
    let mut builder = FsmBuilder::with_limits(limits);
    builder
        .add_document(PatternSource::named(
            "a.lx",
            "lexicon a by [text] := {\"a\", \"b\"}.",
        ))
        .unwrap();
    let error = match builder.add_document(PatternSource::named(
        "b.lx",
        "lexicon b by [norm] := {\"c\"}.",
    )) {
        Ok(()) => panic!("the catalog exceeds the member budget"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("lexicon members"));
}

#[test]
fn key_projection_kinds_are_definition_specific() {
    let ordinary = compile_error("key: [term]\np := [].");
    assert!(ordinary.contains("cannot use lexical"), "{ordinary}");

    let lexical = compile_error("key: [text]\nlexicon words by [text] := {\"x\"}.");
    assert!(lexical.contains("may contain only `term`"), "{lexical}");
}

#[test]
fn patterns_subject_keeps_ordinary_policy_usable_beside_lexicons() {
    compile_catalog(
        PatternSource::named(
            "mixed.lx",
            "p := [].\nlexicon words by [text] := {\"x\"}.\npolicy\n  resolve patterns rank: [longest, start, end, pattern].\nend\n",
        ),
        CompileLimits::default(),
    )
    .unwrap();

    let error = compile_error(
        "p := [].\nlexicon words by [text] := {\"x\"}.\npolicy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
    );
    assert!(error.contains("cannot select a lexicon"), "{error}");
}

#[test]
fn span_and_unit_rewrites_are_refused_on_lexicons() {
    for source in [
        "transform: [{op: \"join\"}]\nlexicon words by [text] := {\"x\"}.",
        "render: \"compact\"\nlexicon words by [text] := {\"x\"}.",
    ] {
        let error = compile_error(source);
        assert!(error.contains("cannot be applied to lexicon"), "{error}");
    }
}
