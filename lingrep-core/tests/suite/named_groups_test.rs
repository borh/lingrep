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
//! Named group tests
//!
//! Tests for named grouping syntax: (?<name> pattern)

use crate::support::token_with;
use lingrep_core::fsm::{CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn compile_one(body: &str) -> lingrep_core::fsm::CompiledCatalog {
    // body-only: wraps as "pattern test := {body}."
    lingrep_core::testing::compile_test_pattern(body)
}

/// Compiles multiple definitions through the whole-document compiler.
fn compile_many(defs: &[&str]) -> lingrep_core::fsm::CompiledCatalog {
    let source = defs.join("\n");
    compile_catalog(
        PatternSource::named("named-groups.lx", &source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

fn token(pos: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Pos, pos.into());
    token
}

#[derive(Debug, PartialEq, Eq)]
struct TopLevelMatch {
    pattern: String,
    start: usize,
    end: usize,
}

fn top_level(matches: Vec<Match>) -> Vec<TopLevelMatch> {
    matches
        .into_iter()
        .map(|matched| TopLevelMatch {
            pattern: matched.pattern.to_string(),
            start: matched.start,
            end: matched.end,
        })
        .collect()
}

fn catalog_match(
    matcher: &lingrep_core::fsm::CompiledCatalog,
    pattern: &str,
    tokens: &[TokenInfo],
) -> TopLevelMatch {
    top_level(matcher.find_matches(tokens))
        .into_iter()
        .find(|matched| matched.pattern == pattern)
        .unwrap_or_else(|| panic!("expected {pattern} to match"))
}

#[test]
fn repository_named_groups_retain_top_level_matches() {
    use TokenAttributeIndex::{CForm, Lemma, Orth, Pos};

    let sfe = compile_catalog(
        PatternSource::named("patterns/sfe.lx", include_str!("../../../patterns/sfe.lx")),
        CompileLimits::default(),
    )
    .expect("sfe catalog compiles")
    .matcher;

    let terminal = || token_with(&[(Orth, "する"), (Pos, "VERB"), (CForm, "終止形-一般")]);
    let continuative = || token_with(&[(Orth, "し"), (Pos, "VERB"), (CForm, "連用形-一般")]);
    let orth = |value| token_with(&[(Orth, value)]);
    let lemma = |orth_value, lemma_value| token_with(&[(Orth, orth_value), (Lemma, lemma_value)]);

    let mut observed = Vec::new();
    for (pattern, tokens) in [
        (
            "恐れがある",
            vec![
                terminal(),
                lemma("恐れ", "恐れ"),
                orth("が"),
                lemma("ある", "ある"),
            ],
        ),
        ("だろう", vec![terminal(), orth("だろう")]),
        ("であろう", vec![terminal(), orth("で"), orth("あろう")]),
        (
            "よう",
            vec![
                continuative(),
                token_with(&[(Orth, "よう"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "かもしれない",
            vec![
                token_with(&[(Orth, "事"), (Pos, "NOUN")]),
                orth("か"),
                orth("も"),
                lemma("知れ", "知れる"),
                orth("ない"),
            ],
        ),
        ("だ", vec![orth("の"), orth("だ")]),
        (
            "必要がある",
            vec![
                terminal(),
                lemma("必要", "必要"),
                orth("が"),
                lemma("ある", "有る"),
            ],
        ),
        (
            "恐れがある",
            vec![
                terminal(),
                lemma("恐れ", "恐れ"),
                orth("が"),
                lemma("ある", "有る"),
            ],
        ),
        ("だろう", vec![terminal(), orth("だろう")]),
        ("であろう", vec![terminal(), orth("で"), orth("あろう")]),
        (
            "う",
            vec![
                terminal(),
                token_with(&[(Orth, "う"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "よう",
            vec![
                continuative(),
                token_with(&[(Orth, "よう"), (CForm, "意志推量形")]),
            ],
        ),
        (
            "かもしれない",
            vec![
                token_with(&[(Orth, "事"), (Pos, "NOUN")]),
                orth("か"),
                orth("も"),
                lemma("知れ", "知れる"),
                orth("ない"),
            ],
        ),
    ] {
        observed.push(catalog_match(&sfe, pattern, &tokens));
    }

    insta::assert_debug_snapshot!(observed);
}

fn matched(pattern: &str, start: usize, end: usize) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: Box::default(),
        graph_bindings: Box::default(),
        interpretations: Box::default(),
        lexical: None,
    }
}

fn captured(mut matched: Match, captures: &[(&str, usize, usize)]) -> Match {
    matched.captures = captures
        .iter()
        .map(|(name, start, end)| lingrep_core::fsm::Capture {
            name: (*name).into(),
            path: format!("{}/{}", matched.pattern, name).into(),
            start: *start,
            end: *end,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    matched
}

#[test]
fn test_named_group_single() {
    let matcher = compile_one("(?<命題> [upos=NOUN] [upos=VERB])");

    let tokens = [token("NOUN"), token("VERB")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(matched("test", 0, 2), &[("命題", 0, 2)])]
    );
}

#[test]
fn test_named_group_multiple() {
    let matcher = compile_one("(?<命題> [upos=NOUN]) (?<述語> [upos=VERB])");

    let tokens = [token("NOUN"), token("VERB")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(
            matched("test", 0, 2),
            &[("命題", 0, 1), ("述語", 1, 2)]
        )]
    );
}

#[test]
fn test_named_group_nested() {
    let matcher = compile_many(&[
        "phrase := [upos=NOUN] [upos=ADP].",
        "pattern test := (?<名詞句> @phrase).",
    ]);

    let tokens = [token("NOUN"), token("ADP")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![
            matched("phrase", 0, 2),
            captured(matched("test", 0, 2), &[("名詞句", 0, 2)])
        ]
    );
}

#[test]
fn test_named_group_with_quantifier() {
    let matcher = compile_one("(?<名詞> [upos=NOUN]{2})");

    let tokens = [token("NOUN"), token("NOUN")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![captured(matched("test", 0, 2), &[("名詞", 0, 2)])]
    );
}

#[test]
fn test_named_group_or_alternative() {
    let matcher = compile_many(&[
        "noun := [upos=NOUN].",
        "adj := [upos=ADJ].",
        "pattern test := (?<体言> @noun | @adj).",
    ]);

    let tokens = [token("NOUN")];

    assert_eq!(
        matcher.find_matches(&tokens),
        vec![
            matched("noun", 0, 1),
            captured(matched("test", 0, 1), &[("体言", 0, 1)])
        ]
    );
}

#[test]
fn capture_selection_is_source_ordered_greedy_and_last_participating() {
    use TokenAttributeIndex::Orth;

    let a = || token_with(&[(Orth, "a")]);
    let b = || token_with(&[(Orth, "b")]);

    let alternative = compile_many(&["pattern alt := ((?<left> [text=a]) | (?<right> [text=a]))."]);
    assert_eq!(
        alternative.find_matches(&[a()]),
        vec![captured(matched("alt", 0, 1), &[("left", 0, 1)])]
    );

    let greedy = compile_many(&["pattern greedy := (?<x> [text=a]{1,3}) [text=a]."]);
    assert_eq!(
        greedy.find_matches(&[a(), a(), a(), a()]),
        vec![
            captured(matched("greedy", 0, 4), &[("x", 0, 3)]),
            captured(matched("greedy", 1, 4), &[("x", 1, 3)]),
            captured(matched("greedy", 2, 4), &[("x", 2, 3)]),
        ]
    );

    let repeated = compile_many(&["pattern repeated := ((?<x> [text=a]) [text=b]?){1,2}."]);
    assert_eq!(
        repeated.find_matches(&[a(), b(), a()]),
        vec![
            captured(matched("repeated", 0, 3), &[("x", 2, 3)]),
            captured(matched("repeated", 2, 3), &[("x", 2, 3)]),
        ]
    );
}

#[test]
fn fragment_capture_paths_are_hygienic_per_reference_occurrence() {
    let matcher = compile_many(&[
        "fragment F := (?<unit> [upos=NOUN]).",
        "pattern p := @F @F.",
    ]);
    let matches = matcher.find_matches(&[token("NOUN"), token("NOUN")]);
    let matched = matches
        .iter()
        .find(|matched| matched.pattern == "p" && matched.start == 0)
        .expect("expanded pattern must match");
    assert_eq!(
        matched
            .captures
            .iter()
            .map(|capture| (capture.path.as_str(), capture.start, capture.end))
            .collect::<Vec<_>>(),
        vec![("p/F#1/unit", 0, 1), ("p/F#2/unit", 1, 2)]
    );
}

#[test]
fn interpretations_preserve_joint_assignments_across_repeated_alternatives() {
    let annotation = r#"interpret $a as ending {tense: "past", complete: true}
interpret $b as ending {tense: "nonpast", complete: false}
classify by ending"#;
    let pattern = "pattern t := ((?<a> [text=x]) | (?<b> [text=x])){2}.";
    let words = vec![token_with(&[(TokenAttributeIndex::Orth, "x")]); 2];
    let mut base = compile_many(&[pattern]).find_matches(&words);
    let mut actual = compile_many(&[annotation, pattern]).find_matches(&words);
    let alternatives = std::mem::take(&mut actual[0].interpretations);
    assert_eq!(actual, base);
    assert_eq!(alternatives.len(), 4);
    for interpretation in &alternatives {
        assert_eq!(interpretation.roles.len(), 2);
        for (start, occurrence) in interpretation.roles.iter().enumerate() {
            assert_eq!(
                (occurrence.role.as_str(), occurrence.start, occurrence.end),
                ("ending", start, start + 1)
            );
            assert_eq!(
                occurrence.labels["complete"] == lingrep_core::fsm::AnalyticalValue::Boolean(true),
                occurrence.labels["tense"]
                    == lingrep_core::fsm::AnalyticalValue::String("past".into())
            );
        }
    }
    base[0].interpretations = alternatives;
    assert_eq!(
        compile_many(&[annotation, pattern]).find_matches(&words),
        base
    );
}

#[test]
fn interpreted_optional_and_greedy_captures_keep_prioritized_ordinary_records() {
    let inputs = [
        "pattern p := (?<piece> []?) [].",
        "pattern p := []? (?<piece> []?) [].",
        "pattern p := ((?<piece> []{1,2}) | [])+.",
        "fragment H := (?<piece> []).\npattern p := @H @H.",
        "test gap := text=y.\nskip: @gap\npattern p := (?<piece> [text=x]) [text=x].",
    ];
    for source in inputs {
        let interpreted = source.replace(
            "pattern p :=",
            "interpret $piece as part {selected: true}\npattern p :=",
        );
        let plain = compile_many(&[source]);
        let analytical = compile_many(&[&interpreted]);
        for mask in 0..16 {
            let words = (0..4)
                .map(|i| {
                    token_with(&[(
                        TokenAttributeIndex::Orth,
                        if mask & (1 << i) == 0 { "x" } else { "y" },
                    )])
                })
                .collect::<Vec<_>>();
            let mut records = analytical.find_matches(&words);
            for record in &mut records {
                assert!(!record.interpretations.is_empty());
                record.interpretations = Box::default();
            }
            assert_eq!(records, plain.find_matches(&words), "{source} on {mask}");
        }
    }
}

#[test]
fn interpreted_assignments_equal_finite_role_products_under_helper_and_branch_reordering() {
    use lingrep_core::fsm::{AnalyticalValue, Interpretation, RoleOccurrence};
    use std::collections::{BTreeMap, BTreeSet};
    let projections = r#"
        interpret $x as slot {kind: "x", value: 1}
        interpret $any as slot {kind: "any", value: 0}
        classify by slot"#;
    let bodies = [
        (
            "fragment F := ((?<x> [text=x]) | (?<any> []) | []).",
            "pattern p := @F{2}.",
        ),
        (
            "fragment F := ([] | (?<any> []) | (?<x> [text=x])).",
            "pattern p := @F{2}.",
        ),
        (
            "fragment F := ((?<x> [text=x]) | (?<any> []) | []).\nfragment G := @F.",
            "pattern p := @G @G.",
        ),
    ];
    for (helper, pattern) in bodies {
        let analytical = compile_many(&[helper, projections, pattern]);
        for first in ["x", "y"] {
            for second in ["x", "y"] {
                let words =
                    [first, second].map(|text| token_with(&[(TokenAttributeIndex::Orth, text)]));
                let mut records = analytical.find_matches(&words);
                let got = std::mem::take(&mut records[0].interpretations);
                assert_eq!(
                    records,
                    compile_many(&[helper, pattern]).find_matches(&words)
                );
                let choices = |word| {
                    if word == "x" {
                        vec![None, Some(("any", 0)), Some(("x", 1))]
                    } else {
                        vec![None, Some(("any", 0))]
                    }
                };
                let mut expected = BTreeSet::new();
                for a in choices(first) {
                    for b in choices(second) {
                        let mut roles = Vec::new();
                        let mut classification: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
                        for (i, pair) in [a, b].into_iter().enumerate() {
                            if let Some((kind, value)) = pair {
                                let labels: BTreeMap<compact_str::CompactString, _> =
                                    BTreeMap::from([
                                        ("kind".into(), AnalyticalValue::String(kind.into())),
                                        ("value".into(), AnalyticalValue::Integer(value)),
                                    ]);
                                for (k, v) in &labels {
                                    classification
                                        .entry(k.clone())
                                        .or_default()
                                        .insert(v.clone());
                                }
                                roles.push(RoleOccurrence {
                                    role: "slot".into(),
                                    start: i,
                                    end: i + 1,
                                    labels,
                                });
                            }
                        }
                        expected.insert(Interpretation {
                            roles: roles.into_boxed_slice(),
                            classification: classification
                                .into_iter()
                                .map(|(k, v)| (k, v.into_iter().collect()))
                                .collect(),
                        });
                    }
                }
                assert_eq!(
                    got,
                    expected.into_iter().collect(),
                    "{helper} on {first} {second}"
                );
            }
        }
    }
}

#[test]
fn interpretation_limits_do_not_truncate_an_unambiguous_analysis() {
    let matcher = compile_many(&[
        "interpret $x as slot {seen: true}",
        "p := (?<x> [text=a]) ([text=x] | [text=x])* [text=z].",
    ]);
    let mut words = vec![token_with(&[(TokenAttributeIndex::Orth, "x")]); 1500];
    words[0] = token_with(&[(TokenAttributeIndex::Orth, "a")]);
    words[1499] = token_with(&[(TokenAttributeIndex::Orth, "z")]);
    assert!(
        matches!(matcher.try_find_matches(&words), Err(lingrep_core::fsm::MatchResourceError::Sentence {resource, ..}) if resource.starts_with("interpretation"))
    );
}

#[test]
fn interpretation_admission_identifies_unknown_roles_and_unsupported_expressions() {
    use lingrep_core::fsm::DiagnosticCode;
    for source in [
        "interpret $absent as part {}\np := (?<a> []).",
        "classify by missing\np := [].",
        "interpret $a as part {}\nclassify by missing\np := (?<a> []).",
        "interpret $a as part {}\np := (?<a> []) followed_by([]).",
        "interpret $a as part {}\np := ((?<a> []) & []).",
    ] {
        let error = compile_catalog(
            PatternSource::named("roles.lx", source),
            CompileLimits::default(),
        )
        .err()
        .expect("unsupported interpretation must be rejected");
        assert_eq!(
            error.diagnostics[0].code(),
            DiagnosticCode::InvalidInterpretation,
            "{source}"
        );
    }
}
