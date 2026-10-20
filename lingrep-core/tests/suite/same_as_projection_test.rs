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
//! Canonical `same_as` projection: `by=` uses the same closed `AttributeKey`
//! grammar as token predicates and compares the complete scalar or set value.

use lingrep_core::fsm::{CompileLimits, DiagnosticCode, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

#[test]
fn same_as_compares_complete_canonical_sets() {
    use lingrep_core::token_match::{
        AttributeKind, BoundTokenSentence, ProjectedAttributeValue, SchemaAttribute, TokenSchema,
    };

    let compiled =
        matcher("pattern p := (?<u> [feat.Number has Sing]) same_as($u, by=feat.Number).");
    let schema =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();
    let bound = compiled.bind(&schema).unwrap();
    let sentence = |right: &[&str]| {
        BoundTokenSentence::from_projected_attribute_values(
            bound.projection(),
            2,
            vec![
                ProjectedAttributeValue::string_set(["Plur", "Sing"]),
                ProjectedAttributeValue::string_set(right.iter().copied()),
            ],
        )
        .unwrap()
    };

    assert_eq!(
        bound
            .find_matches(&sentence(&["Sing", "Plur"]))
            .unwrap()
            .len(),
        1
    );
    assert!(bound.find_matches(&sentence(&["Sing"])).unwrap().is_empty());
}

fn matcher(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("same-as-projection.lx", source),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

#[test]
fn same_as_accepts_any_known_attribute() {
    let compiled = compile_catalog(
        PatternSource::named(
            "same-as-projection.lx",
            "pattern v_ni_v := (?<v> [unidic.pos1=動詞 & unidic.cform=連用形-一般]) [text=に] \
             same_as($v, by=unidic.orthbase).",
        ),
        CompileLimits::default(),
    );
    assert!(
        compiled.is_ok(),
        "orthbase is a real token attribute: {:?}",
        compiled.err()
    );
}

#[test]
fn same_as_resolves_by_names_exactly_as_predicates_do() {
    for attribute in [
        "text",
        "lemma",
        "norm",
        "upos",
        "xpos",
        "feat.Number",
        "unidic.pos1",
        "spacy.morph.reading",
    ] {
        let source = format!("pattern p := (?<u> []) same_as($u, by={attribute}).");
        let compiled = compile_catalog(
            PatternSource::named("same-as-projection.lx", &source),
            CompileLimits::default(),
        );
        assert!(
            compiled.is_ok(),
            "{attribute} is a canonical token attribute: {:?}",
            compiled.err()
        );
    }
}

#[test]
fn same_as_still_rejects_unknown_attribute() {
    let error = match compile_catalog(
        PatternSource::named(
            "same-as-projection.lx",
            "x := (?<v> []) same_as($v, by=flavor).",
        ),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("flavor is not a token attribute"),
        Err(error) => error,
    };
    let diagnostic = &error.diagnostics[0];
    assert_eq!(diagnostic.code(), DiagnosticCode::Syntax, "{error}");
}

fn token(pairs: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
    let mut token = TokenInfo::default();
    for (attribute, value) in pairs {
        token.set(*attribute, (*value).into());
    }
    token
}

#[test]
fn projection_compares_the_projected_attribute_not_orth() {
    // Two spans equal on `pos1` but different on `orth` match under
    // `by=unidic.pos1`; the same tokens do not match under the default projection.
    use TokenAttributeIndex::{Orth, Pos1};
    let tokens = [
        token(&[(Orth, "走り"), (Pos1, "動詞")]),
        token(&[(Orth, "泣い"), (Pos1, "動詞")]),
    ];

    let by_pos1 = matcher("pattern p := (?<u> []) same_as($u, by=unidic.pos1).");
    let matches = by_pos1.find_matches(&tokens);
    assert_eq!(matches.len(), 1, "{matches:?}");
    assert_eq!((matches[0].start, matches[0].end), (0, 2));

    let by_default_orth = matcher("pattern p := (?<u> []) same_as($u).");
    assert!(by_default_orth.find_matches(&tokens).is_empty());
}

#[test]
fn a_missing_projected_attribute_on_either_side_fails() {
    // Pins the `spans_equal` Option behavior: a missing attribute satisfies
    // nothing, consistent with the language's three-valued stance.
    use TokenAttributeIndex::{Goshu, Orth};
    let matcher = matcher("pattern p := (?<u> []) same_as($u, by=unidic.goshu).");

    // Missing on the referenced side.
    assert!(
        matcher
            .find_matches(&[token(&[(Orth, "a")]), token(&[(Orth, "b"), (Goshu, "和")])])
            .is_empty()
    );
    // Missing on the compared side.
    assert!(
        matcher
            .find_matches(&[token(&[(Orth, "a"), (Goshu, "和")]), token(&[(Orth, "b")])])
            .is_empty()
    );
    // Missing on both sides is still not equality.
    assert!(
        matcher
            .find_matches(&[token(&[(Orth, "a")]), token(&[(Orth, "b")])])
            .is_empty()
    );
    // Present and equal on both sides matches.
    assert_eq!(
        matcher
            .find_matches(&[
                token(&[(Orth, "a"), (Goshu, "和")]),
                token(&[(Orth, "b"), (Goshu, "和")])
            ])
            .len(),
        1
    );
}

#[test]
fn a_projection_only_attribute_reaches_required_attributes() {
    // `goshu` appears nowhere but the projection, so a cache-backed caller
    // must learn about it from `required_attributes()` alone.
    let matcher = matcher("pattern p := (?<u> [unidic.pos1=動詞]) same_as($u, by=unidic.goshu).");
    assert_eq!(
        matcher
            .required_attributes()
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["unidic.goshu", "unidic.pos1"]
    );
}
