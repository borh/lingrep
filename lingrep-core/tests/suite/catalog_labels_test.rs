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
//! `labels:` declarations and the literal check against them.

use lingrep_core::fsm::{
    CompileLimits, DiagnosticCode, FsmBuilder, PatternSource, compile_catalog,
};

fn outcome(source: &str) -> Result<(), (DiagnosticCode, String)> {
    compile_catalog(
        PatternSource::named("labels.lx", source),
        CompileLimits::default(),
    )
    .map(|_| ())
    .map_err(|error| (error.diagnostics[0].code(), error.to_string()))
}

#[test]
fn literals_are_checked_against_the_declared_set() {
    let rejected = Err(DiagnosticCode::LabelOutsideDeclaredSet);
    for (name, source, expected) in [
        (
            "in set",
            "labels: { upos: \"ud-v2\" }\np := [upos=VERB].",
            Ok(()),
        ),
        (
            "outside set",
            "labels: { upos: \"ud-v2\" }\np := [upos=VB].",
            rejected,
        ),
        (
            "every member of an inline set",
            "labels: { upos: \"ud-v2\" }\np := [upos in {NOUN, NN}].",
            rejected,
        ),
        (
            "a named set reference",
            "labels: { upos: \"ud-v2\" }\nset nouns := {NOUN, NN}.\np := [upos in @nouns].",
            rejected,
        ),
        (
            "an exclusion",
            "labels: { upos: \"ud-v2\" }\np := [upos!=NN].",
            rejected,
        ),
        (
            "inside a lookaround",
            "labels: { upos: \"ud-v2\" }\np := [] followed_by([upos=NN]).",
            rejected,
        ),
        (
            "a regex is not a literal",
            "labels: { upos: \"ud-v2\" }\np := [upos~ /^N/].",
            Ok(()),
        ),
        (
            "an undeclared attribute",
            "labels: { upos: \"ud-v2\" }\np := [xpos=whatever].",
            Ok(()),
        ),
        (
            "a UD subtype by its base",
            "labels: { dep.rel: \"ud-v2\" }\np := [dep.rel=nsubj:pass].",
            Ok(()),
        ),
        (
            "a where-clause relation label",
            "labels: { dep.rel: \"ud-v2\" }\np := (?<v> []) (?<o> []) where $v -[dobj]-> $o.",
            rejected,
        ),
        (
            "an enhanced relation label",
            "labels: { dep.rel: \"ud-v2\" }\np := (?<v> []) (?<o> []) where $v -[enhanced dobj]-> $o.",
            rejected,
        ),
        (
            "a union of named sets",
            "labels: { dep.rel: [\"clearnlp\", \"ud-v2\"] }\n\
             p := (?<v> []) (?<o> []) where $v -[dobj | obj]-> $o.",
            Ok(()),
        ),
        (
            "an explicit feature list",
            "labels: { feat.VerbForm: {Fin, Ger} }\np := [feat.VerbForm has Part].",
            rejected,
        ),
        (
            "spaCy's SPACE",
            "labels: { upos: \"spacy\" }\np := [upos=SPACE].",
            Ok(()),
        ),
    ] {
        assert_eq!(
            outcome(source).map_err(|(code, _)| code),
            expected,
            "{name}"
        );
    }
}

#[test]
fn a_malformed_declaration_is_rejected() {
    for (name, source, message) in [
        (
            "unknown name",
            "labels: { upos: \"ud-v3\" }\np := [].",
            "is not a label set for `upos`",
        ),
        (
            "set for another attribute",
            "labels: { upos: \"clearnlp\" }\np := [].",
            "is not a label set for `upos`",
        ),
        (
            "no named set applies",
            "labels: { lemma: \"ud-v2\" }\np := [].",
            "no named label set applies to `lemma`",
        ),
        (
            "duplicate attribute",
            "labels: { upos: \"ud-v2\", upos: \"spacy\" }\np := [].",
            "declares `upos` twice",
        ),
        (
            "duplicate name in a union",
            "labels: { dep.rel: [\"ud-v2\", \"ud-v2\"] }\np := [].",
            "names the set \"ud-v2\" twice",
        ),
        (
            "second entry",
            "labels: { upos: \"ud-v2\" }\nlabels: { xpos: \"penn\" }\np := [].",
            "at most one `labels:` entry",
        ),
        (
            "after a definition",
            "p := [].\nlabels: { upos: \"ud-v2\" }",
            "must precede annotations and definitions",
        ),
    ] {
        let (code, actual) = outcome(source).expect_err(name);
        assert_eq!(code, DiagnosticCode::Syntax, "{name}: {actual}");
        assert!(actual.contains(message), "{name}: {actual}");
    }
}

#[test]
fn a_rejected_relation_label_is_quoted_as_authored() {
    let (_, message) = outcome(
        "labels: { dep.rel: {nsubj:pass} }\np := (?<v> []) (?<s> []) where $v -[nsubj:*]-> $s.",
    )
    .unwrap_err();
    assert!(
        message.contains("value `nsubj:*`, which is outside the label set `{nsubj:pass}`"),
        "{message}"
    );
}

#[test]
fn a_declaration_applies_to_its_own_file() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "ud.lx",
            "labels: { dep.rel: \"ud-v2\" }\np := [dep.rel=obj].",
        ))
        .unwrap();
    builder
        .add_document(PatternSource::named("clear.lx", "q := [dep.rel=dobj]."))
        .unwrap();
    let output = builder.build().unwrap();
    let declarations = output.matcher.label_declarations();
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0].source_name(), "ud.lx");
    assert_eq!(declarations[0].set_name(), "ud-v2");
}
