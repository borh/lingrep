// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use crate::support::refuses_with;
use lingrep_core::fsm::{
    Capture, CompileError, CompileLimits, CompiledCatalog, DiagnosticCode, Match, PatternSource,
    compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn token(text: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, text.into());
    token
}

fn tokens(texts: &[&str]) -> Vec<TokenInfo> {
    texts.iter().map(|text| token(text)).collect()
}

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("any-order.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

fn compile_err(source: &str) -> CompileError {
    match compile_catalog(
        PatternSource::named("any-order.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("expected a compile error, but this compiled: {source}"),
        Err(error) => error,
    }
}

fn match_conllu(source: &str, sentence: &str) -> Vec<Match> {
    let compiled = compile(source);
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("any-order fixture binds to the CoNLL-U schema");
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(sentence.as_bytes()),
        "any-order-test",
    );
    let sentence = reader
        .next()
        .expect("fixture holds one sentence")
        .expect("fixture parses")
        .project(bound.projection())
        .expect("fixture projects to the matcher schema");
    bound.find_matches(&sentence).expect("schema matches")
}

fn capture(name: &str, start: usize) -> Capture {
    Capture {
        name: name.into(),
        path: format!("p/{name}").into(),
        start,
        end: start + 1,
    }
}

#[test]
fn any_order_matches_every_surface_order_with_minimal_span_and_surface_captures() {
    let matcher = compile(
        "pattern p := any_order(\
         (?<head> [text=h]), \
         (?<middle> [text=m]), \
         (?<leaf> [text=l])).",
    );

    for (surface, names) in [
        (["h", "m", "l"], ["head", "middle", "leaf"]),
        (["h", "l", "m"], ["head", "leaf", "middle"]),
        (["m", "h", "l"], ["middle", "head", "leaf"]),
        (["m", "l", "h"], ["middle", "leaf", "head"]),
        (["l", "h", "m"], ["leaf", "head", "middle"]),
        (["l", "m", "h"], ["leaf", "middle", "head"]),
    ] {
        assert_eq!(
            matcher.find_matches(&tokens(&surface)),
            vec![Match {
                pattern: "p".into(),
                start: 0,
                end: 3,
                captures: names
                    .iter()
                    .enumerate()
                    .map(|(start, name)| capture(name, start))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                graph_bindings: Box::default(),
                lexical: None,
            }],
            "surface order {surface:?}",
        );
    }

    assert_eq!(
        matcher.find_matches(&tokens(&["x", "l", "gap", "h", "gap", "m", "x"])),
        vec![Match {
            pattern: "p".into(),
            start: 1,
            end: 6,
            captures: vec![capture("leaf", 1), capture("head", 3), capture("middle", 5)]
                .into_boxed_slice(),
            graph_bindings: Box::default(),
            lexical: None,
        }],
        "implicit gaps are internal; unmatched edges stay outside the span",
    );
}

#[test]
fn any_order_binds_distinct_tokens_and_keeps_preexisting_capture_references() {
    let distinct = compile("pattern p := any_order((?<left> []), (?<right> [])).");
    assert!(distinct.find_matches(&tokens(&["x"])).is_empty());
    let matched = distinct.find_matches(&tokens(&["x", "y"]));
    assert_eq!(matched.len(), 2);
    assert_eq!(
        matched
            .iter()
            .map(|matched| matched
                .captures
                .iter()
                .map(|capture| (capture.name.as_str(), capture.start, capture.end))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![
            vec![("left", 0, 1), ("right", 1, 2)],
            vec![("right", 0, 1), ("left", 1, 2)],
        ],
    );
    assert!(
        matched.iter().all(|matched| matched
            .captures
            .iter()
            .all(|capture| capture.end - capture.start == 1)),
        "every assignment binds two distinct one-token spans",
    );

    let outer = compile(
        "pattern p := (?<seed> [text=x]) \
         any_order((?<same> same_as($seed)), (?<other> [text=y])).",
    );
    assert_eq!(
        outer.find_matches(&tokens(&["x", "y", "x"])),
        vec![Match {
            pattern: "p".into(),
            start: 0,
            end: 3,
            captures: vec![capture("seed", 0), capture("other", 1), capture("same", 2)]
                .into_boxed_slice(),
            graph_bindings: Box::default(),
            lexical: None,
        }],
    );
}

#[test]
fn any_order_preserves_one_longest_candidate_per_surface_permutation() {
    let matcher = compile(
        "pattern p := any_order(\
         (?<head> [text=h]), \
         (?<middle> [text=m]), \
         (?<leaf> [text=l])).",
    );

    assert_eq!(
        matcher.find_matches(&tokens(&["h", "m", "l", "m"])),
        vec![
            Match {
                pattern: "p".into(),
                start: 0,
                end: 3,
                captures: vec![capture("head", 0), capture("middle", 1), capture("leaf", 2)]
                    .into_boxed_slice(),
                graph_bindings: Box::default(),
                lexical: None,
            },
            Match {
                pattern: "p".into(),
                start: 0,
                end: 4,
                captures: vec![capture("head", 0), capture("leaf", 2), capture("middle", 3)]
                    .into_boxed_slice(),
                graph_bindings: Box::default(),
                lexical: None,
            },
        ],
        "surface permutations are independent counting alternatives",
    );
}

#[test]
fn composed_any_order_constructs_preserve_the_complete_branch_path() {
    let matcher = compile(
        "pattern p := any_order((?<a> []), (?<b> [])) \
         any_order((?<c> []), (?<d> [])).",
    );

    let matches = matcher.find_matches(&tokens(&["a", "b", "c", "d"]));
    assert_eq!(matches.len(), 4);
    assert_eq!(
        matches
            .iter()
            .map(|matched| matched
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        vec![
            vec!["a", "b", "c", "d"],
            vec!["a", "b", "d", "c"],
            vec!["b", "a", "c", "d"],
            vec!["b", "a", "d", "c"],
        ],
    );
}

#[test]
fn any_order_filters_relations_before_each_permutation_selects_longest() {
    let source = "pattern p := any_order(\
         (?<head> [text=h]), \
         (?<middle> [text=m]), \
         (?<leaf> [text=l])) \
         where $head -[obj]-> $middle, $middle -[amod]-> $leaf.";
    let sentence = "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
                    2\tm\tm\tNOUN\t_\t_\t1\tobj\t_\t_\n\
                    3\tl\tl\tADJ\t_\t_\t2\tamod\t_\t_\n\
                    4\tl\tl\tADJ\t_\t_\t1\tamod\t_\t_\n";

    assert_eq!(
        match_conllu(source, sentence),
        vec![Match {
            pattern: "p".into(),
            start: 0,
            end: 3,
            captures: vec![capture("head", 0), capture("middle", 1), capture("leaf", 2)]
                .into_boxed_slice(),
            graph_bindings: Box::default(),
            lexical: None,
        }],
        "a longer failing assignment must not shadow the shorter passing one",
    );
}

#[test]
fn any_order_refuses_shapes_that_are_not_distinct_named_nodes() {
    for (source, code) in [
        (
            "pattern p := any_order((?<only> [])).",
            DiagnosticCode::AnyOrderTooFewOperands,
        ),
        (
            "pattern p := any_order([text=a], (?<b> [text=b])).",
            DiagnosticCode::AnyOrderOperandMustBeCapture,
        ),
        (
            "pattern p := any_order((?<a> [] []), (?<b> [])).",
            DiagnosticCode::AnyOrderOperandNotSingleToken,
        ),
        (
            "pattern p := any_order((?<a> (?<nested> [])), (?<b> [])).",
            DiagnosticCode::AnyOrderNestedCapture,
        ),
        (
            "pattern p := any_order((?<a> []), (?<b> same_as($a))).",
            DiagnosticCode::CaptureNotDefinitelyAssigned,
        ),
    ] {
        let error = compile_err(source);
        assert!(refuses_with(&error, code), "{source}: {error:?}");
    }
}

#[test]
fn any_order_uses_the_existing_expansion_budget_instead_of_a_fixed_operand_limit() {
    let operands = (0..10)
        .map(|index| format!("(?<n{index}> [text={index}])"))
        .collect::<Vec<_>>()
        .join(", ");
    let error = compile_err(&format!("pattern p := any_order({operands})."));
    assert!(
        refuses_with(&error, DiagnosticCode::AnyOrderTooComplex),
        "{error:?}",
    );
}

#[test]
fn any_order_remains_contextual_outside_call_position() {
    compile("any_order := []. pattern literal := [text=any_order].");
}
