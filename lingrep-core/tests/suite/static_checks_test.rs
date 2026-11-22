//! Unsatisfiable token tests and span containment between definitions.

use lingrep_core::fsm::{
    CompileLimits, CompileOutput, Containment, PatternSource, UnsatisfiableKind, WitnessToken,
    WitnessValue, compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn compile(source: &str) -> CompileOutput {
    compile_catalog(
        PatternSource::named("static.lx", source),
        CompileLimits::default(),
    )
    .expect("the fixture compiles")
}

fn unsatisfiable(source: &str) -> Vec<(String, UnsatisfiableKind, usize)> {
    compile(source)
        .no_match
        .unsatisfiable()
        .into_iter()
        .map(|finding| (finding.definition, finding.kind, finding.location.column))
        .collect()
}

fn token_test(test: &str) -> UnsatisfiableKind {
    UnsatisfiableKind::TokenTest {
        test: test.to_owned(),
    }
}

#[test]
fn a_token_test_no_value_satisfies_is_reported_at_its_position() {
    assert_eq!(
        unsatisfiable("a := [text=x] [upos=VERB & upos=NOUN]?."),
        vec![("a".to_owned(), token_test("[upos=VERB & upos=NOUN]"), 15)],
    );
}

#[test]
fn unsatisfiable_tests_by_operator() {
    for (test, rendered) in [
        ("[text=x & text!=x]", Some("[text=x & text not in {x}]")),
        ("[text in {x, y} & text not in {x, y}]", None),
        ("[text=x & text~y]", Some("[text=x & text~y]")),
        ("[text~xy & text!~x]", Some("[text~xy & text!~x]")),
        ("[text=x & text~/^y/]", Some("[text=x & text~/^y/]")),
        ("[text=x & !(text=x)]", Some("[text=x & !(text=x)]")),
    ] {
        let findings = unsatisfiable(&format!("a := [lemma=k] | {test}."));
        assert_eq!(findings.len(), 1, "{test}: {findings:?}");
        if let Some(rendered) = rendered {
            assert_eq!(findings[0].1, token_test(rendered), "{test}");
        }
    }
}

#[test]
fn satisfiable_and_undecided_tests_are_not_reported() {
    for test in [
        "[text=x & lemma=y]",
        "[text in {x, y} & text!=x]",
        "[text~x & text~y]",
        "[text~x & text=xy]",
        // A value that lacks the attribute satisfies both complements.
        "[!(text=x) & !(text~x)]",
        // Regexes are not compared with each other.
        "[text~/a/ & text!~/a/]",
        "[text!=x & text~/^x$/]",
    ] {
        assert_eq!(unsatisfiable(&format!("a := {test}.")), vec![], "{test}");
    }
}

#[test]
fn a_body_that_requires_an_unsatisfiable_test_matches_nothing() {
    let findings = unsatisfiable(concat!(
        "fragment DEAD := [text=x & text=y].\n",
        "required := [text=a] @DEAD.\n",
        "optional := [text=a] @DEAD?.\n",
        "alternative := [text=a] | @DEAD.\n",
    ));
    assert_eq!(
        findings
            .iter()
            .map(|(definition, kind, _)| (definition.as_str(), kind.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("DEAD", token_test("[text=x & text=y]")),
            ("DEAD", UnsatisfiableKind::Definition),
            ("required", UnsatisfiableKind::Definition),
        ],
    );
}

#[test]
fn no_tracked_catalog_has_an_unsatisfiable_test() {
    for (name, text) in lingrep_core::testing::catalogs::tracked_catalog_sources() {
        let output = compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(output.no_match.unsatisfiable(), vec![], "{name}");
    }
}

fn containment(source: &str, contained: &str, containing: &str) -> Containment {
    compile(source).no_match.containment(contained, containing)
}

fn witness_tokens(witness: &[WitnessToken]) -> Vec<TokenInfo> {
    witness
        .iter()
        .map(|token| {
            let mut info = TokenInfo::default();
            for attribute in &token.attributes {
                let index = TokenAttributeIndex::from_attribute_key(&attribute.key)
                    .expect("the fixtures test fixed-layout attributes");
                match &attribute.value {
                    WitnessValue::Named(value) | WitnessValue::Unnamed(value) => {
                        info.set(index, value.clone());
                    }
                    // The fixtures test scalar attributes only.
                    WitnessValue::Set(_) => {}
                }
            }
            info
        })
        .collect()
}

/// The definitions that publish the leading `span` tokens of `tokens`.
fn publishing(output: &CompileOutput, tokens: &[TokenInfo], span: usize) -> Vec<String> {
    output
        .matcher
        .find_matches(tokens)
        .into_iter()
        .filter(|found| found.start == 0 && found.end == span)
        .map(|found| found.pattern.to_string())
        .collect()
}

#[test]
fn containment_by_form() {
    const SOURCE: &str = concat!(
        "fragment K := [lemma in {こと, 事}].\n",
        "specific := @K [text=も].\n",
        "general := @K [text=は] | @K [text=も].\n",
        "optional := [text=a] [text=b]?.\n",
        "bounded := [text=a] [text=b]{0,1}.\n",
        "starred := [text=a] [text=b]*.\n",
        "both := ([text=a] []) & ([] [text=b]).\n",
        "pair := (?<first> [text=a]) [text=b].\n",
        "negated := [text!=a].\n",
        "other := [text=b].\n",
        "anchored := ^ [text=a].\n",
    );
    for (contained, containing, expected) in [
        ("specific", "general", true),
        ("general", "specific", false),
        ("optional", "bounded", true),
        ("bounded", "optional", true),
        // On `a b b` the starred body publishes three tokens.
        ("optional", "starred", false),
        ("starred", "optional", false),
        ("both", "pair", true),
        ("pair", "both", true),
        ("other", "negated", true),
        ("negated", "other", false),
    ] {
        let verdict = containment(SOURCE, contained, containing);
        if expected {
            assert_eq!(
                verdict,
                Containment::Contained,
                "{contained} in {containing}"
            );
            continue;
        }
        let Containment::Distinguished { tokens, span } = verdict else {
            panic!("{contained} in {containing}: {verdict:?}");
        };
        let publishing = publishing(&compile(SOURCE), &witness_tokens(&tokens), span);
        assert!(
            publishing.iter().any(|name| name == contained)
                && !publishing.iter().any(|name| name == containing),
            "{contained} in {containing}: {tokens:?} published {publishing:?}"
        );
    }
    assert_eq!(
        containment(SOURCE, "anchored", "other"),
        Containment::NotDecided {
            construct: "a position assertion"
        }
    );
    assert_eq!(
        containment(SOURCE, "specific", "missing"),
        Containment::UnknownDefinition
    );
}

#[test]
fn a_distinguishing_sequence_is_a_shortest_one() {
    let verdict = containment(
        "long := [text=a] [text=b]* [text=c].\nshort := [text=a] [text=b] [text=c].",
        "long",
        "short",
    );
    let Containment::Distinguished { tokens, span } = verdict else {
        panic!("{verdict:?}");
    };
    assert_eq!((tokens.len(), span), (2, 2), "{tokens:?}");
}

#[test]
fn a_longer_match_from_the_same_start_is_a_difference() {
    // Both match `a`; on `a b` only `long` publishes two tokens, so the one
    // token `short` publishes there is not a span of `long`.
    const SOURCE: &str = "short := [text=a].\nlong := [text=a] [text=b]?.";
    assert!(matches!(
        containment(SOURCE, "long", "short"),
        Containment::Distinguished { span: 2, .. }
    ));
    let verdict = containment(SOURCE, "short", "long");
    let Containment::Distinguished { tokens, span } = verdict else {
        panic!("{verdict:?}");
    };
    assert_eq!((tokens.len(), span), (2, 1), "{tokens:?}");
    assert_eq!(
        publishing(&compile(SOURCE), &witness_tokens(&tokens), span),
        vec!["short"]
    );
}

#[test]
fn a_regex_test_leaves_containment_undecided_and_a_difference_found() {
    const SOURCE: &str = "regex := [text~/^a/].\nliteral := [text=ab].\nsame := [text~/^a/].";
    assert!(matches!(
        containment(SOURCE, "regex", "same"),
        Containment::NotDecided { .. }
    ));
    assert!(matches!(
        containment(SOURCE, "regex", "literal"),
        Containment::NotDecided { .. }
    ));
    // `ab` is a named value, so the search evaluates the regex on it.
    assert!(matches!(
        containment(SOURCE, "literal", "same"),
        Containment::NotDecided { .. }
    ));
}

/// A pattern body drawn from `bytes`, over `text` in `{a, b}` and `lemma` in
/// `{x}`.
fn body(bytes: &mut impl Iterator<Item = u8>, depth: usize) -> String {
    const TESTS: [&str; 8] = [
        "[text=a]",
        "[text=b]",
        "[text in {a, b}]",
        "[text!=a]",
        "[lemma=x]",
        "[text=a & lemma=x]",
        "[!(text=a)]",
        "[]",
    ];
    let choice = bytes.next().unwrap_or(0);
    if depth == 0 {
        return TESTS[usize::from(choice) % TESTS.len()].to_owned();
    }
    let part = |bytes: &mut _| body(bytes, depth - 1);
    match choice % 10 {
        0 | 1 => format!("{} {}", part(bytes), part(bytes)),
        2 => format!("({} | {})", part(bytes), part(bytes)),
        3 => format!("({})?", part(bytes)),
        4 => format!("({})*", part(bytes)),
        5 => format!("({})+", part(bytes)),
        6 => format!("({}) & ({})", part(bytes), part(bytes)),
        7 => format!("({}){{1,2}}", part(bytes)),
        _ => TESTS[usize::from(choice) % TESTS.len()].to_owned(),
    }
}

#[hegel::test(test_cases = 300)]
fn the_verdict_agrees_with_the_matcher(tc: hegel::TestCase) {
    let bytes = tc.draw(hegel::generators::binary().max_size(96));
    let mut bytes = bytes.into_iter();
    let source = format!(
        "left := {}.\nright := {}.",
        body(&mut bytes, 2),
        body(&mut bytes, 2)
    );
    let Ok(output) = compile_catalog(
        PatternSource::named("generated.lx", &source),
        CompileLimits::default(),
    ) else {
        return;
    };
    match output.no_match.containment("left", "right") {
        Containment::Distinguished { tokens, span } => {
            let publishing = publishing(&output, &witness_tokens(&tokens), span);
            assert_eq!(publishing, vec!["left"], "{source}\n{tokens:?} {span}");
        }
        Containment::Contained => {
            let tokens = bytes
                .map(|byte| {
                    let mut token = TokenInfo::default();
                    match byte % 4 {
                        0 => token.set(TokenAttributeIndex::Orth, "a".into()),
                        1 => token.set(TokenAttributeIndex::Orth, "b".into()),
                        2 => token.set(TokenAttributeIndex::Orth, "c".into()),
                        _ => {}
                    }
                    if byte & 4 != 0 {
                        token.set(TokenAttributeIndex::Lemma, "x".into());
                    }
                    token
                })
                .collect::<Vec<_>>();
            let spans = |name: &str| {
                output
                    .matcher
                    .find_matches(&tokens)
                    .into_iter()
                    .filter(|found| found.pattern == name)
                    .map(|found| (found.start, found.end))
                    .collect::<std::collections::BTreeSet<_>>()
            };
            let (left, right) = (spans("left"), spans("right"));
            assert!(left.is_subset(&right), "{source}\n{left:?}\n{right:?}");
        }
        verdict => panic!("{source}\n{verdict:?}"),
    }
}

#[test]
fn sf_experimental_has_five_contained_definitions() {
    let sources = lingrep_core::testing::catalogs::tracked_catalog_sources();
    let (name, text) = sources
        .iter()
        .find(|(path, _)| path == "patterns/sf-experimental.lx")
        .expect("sf-experimental.lx is tracked");
    let report = compile_catalog(PatternSource::named(name, text), CompileLimits::default())
        .expect("sf-experimental.lx compiles")
        .no_match
        .containments();
    assert_eq!(
        report
            .pairs
            .iter()
            .map(|pair| (
                pair.contained.as_str(),
                pair.containing.as_str(),
                pair.equivalent
            ))
            .collect::<Vec<_>>(),
        vec![
            ("ことも", "こと", false),
            ("であろう", "だろう", false),
            ("としておこう", "としておく", false),
            ("と見られる", "とみられる", false),
            ("のである", "のだ", false),
        ],
    );
}
