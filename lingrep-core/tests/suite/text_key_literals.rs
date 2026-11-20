//! `text` literals compare as their folded key; `surface` literals compare
//! exactly.

#![allow(clippy::unwrap_used, clippy::panic)]

use lingrep_core::fsm::{
    BoundCatalog, CompileLimits, DiagnosticCode, Match, PatternSource, compile_catalog,
};
use lingrep_core::token_match::{BoundTokenSentence, TokenSchema};

/// `(surface, text)` of each token of a Vibrato sentence since 0.9, whose
/// `text` is the folded key of `surface`. The first four are spellings the
/// key folds; the last four are the plain spellings they fold to.
const FOLDED: [(&str, &str); 9] = [
    ("ｶﾞﾗｽ", "ガラス"),
    ("ＮＨＫ", "NHK"),
    ("葛\u{E0100}", "葛"),
    ("長～い", "長〜い"),
    ("ガラス", "ガラス"),
    ("NHK", "NHK"),
    ("葛", "葛"),
    ("長〜い", "長〜い"),
    ("\u{FA5C}氣", "臭氣"),
];

/// The written spelling and the plain spelling it folds to.
const SPELLINGS: [(&str, &str); 4] = [
    ("ｶﾞﾗｽ", "ガラス"),
    ("ＮＨＫ", "NHK"),
    ("葛\u{E0100}", "葛"),
    ("長～い", "長〜い"),
];

/// Every position a `text` literal is written in, as a catalog over one
/// spelling. Each pattern captures what it matches, so the comparison covers
/// captures as well as spans. A bare word is not among them: the grammar
/// admits none of the characters the fold changes in one.
const FORMS: [&str; 6] = [
    "p := (?<w> [text=VALUE]).",
    "p := (?<w> [text!=VALUE]).",
    "p := (?<w> [text in {VALUE, 窓}]).",
    "p := (?<w> [text not in {VALUE, 窓}]).",
    "set S := {VALUE, 窓}.\np := (?<w> [text in @S]).",
    "p := (?<w> [text~VALUE]).",
];

fn compile(source: &str) -> lingrep_core::fsm::CompileOutput {
    compile_catalog(
        PatternSource::named("probe", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("{source}: {error:?}"))
}

/// A Vibrato schema when `folded`, otherwise one prepared before 0.9 that
/// carries `text` as the analyzer wrote it and no `surface`.
fn schema(folded: bool) -> TokenSchema {
    if folded {
        TokenSchema::from_keys(["text", "surface", "lemma"]).unwrap()
    } else {
        TokenSchema::from_keys(["text", "lemma"]).unwrap()
    }
}

fn bind(source: &str, schema: &TokenSchema) -> BoundCatalog {
    compile(source)
        .matcher
        .bind_with_projection(schema, schema.keys().iter().cloned())
        .unwrap()
}

/// One sentence of `tokens` as `(surface, text)` pairs; `lemma` repeats
/// `text`, and a schema without `surface` drops it.
fn sentence(bound: &BoundCatalog, tokens: &[(&str, &str)]) -> BoundTokenSentence {
    let keys = bound.projection().keys();
    let values = tokens
        .iter()
        .flat_map(|(surface, text)| {
            keys.iter().map(move |key| {
                Some(match key.as_str() {
                    "surface" => (*surface).into(),
                    _ => (*text).into(),
                })
            })
        })
        .collect();
    BoundTokenSentence::from_projected_values(bound.projection(), tokens.len(), values).unwrap()
}

fn run(source: &str, schema: &TokenSchema, tokens: &[(&str, &str)]) -> Vec<Match> {
    let bound = bind(source, schema);
    bound.find_matches(&sentence(&bound, tokens)).unwrap()
}

#[test]
fn a_folded_spelling_matches_what_its_plain_spelling_matches_on_a_folded_corpus() {
    let schema = schema(true);
    for form in FORMS {
        for (written, plain) in SPELLINGS {
            let expected = run(&form.replace("VALUE", plain), &schema, &FOLDED);
            assert!(!expected.is_empty(), "{form} {plain}");
            assert_eq!(
                run(&form.replace("VALUE", written), &schema, &FOLDED),
                expected,
                "{form} {written}"
            );
        }
    }
}

#[test]
fn a_folded_lexicon_member_matches_with_its_authored_term() {
    let schema = schema(true);
    for (written, plain) in SPELLINGS {
        let lexicon = |member: &str| format!("lexicon p by [text] := {{\"{member}\"}}.");
        let plain_matches = run(&lexicon(plain), &schema, &FOLDED);
        let written_matches = run(&lexicon(written), &schema, &FOLDED);
        assert_eq!(plain_matches.len(), 2, "{plain}");
        let expected: Vec<Match> = plain_matches
            .into_iter()
            .map(|mut record| {
                record.lexical.as_mut().unwrap().term = written.into();
                record
            })
            .collect();
        assert_eq!(written_matches, expected, "{written}");
    }
}

#[test]
fn a_text_literal_is_its_folded_key_on_any_corpus() {
    // A 0.8 artifact's `text` is the analyzer's orth, unfolded, so a folded
    // literal does not find the spelling it was written in there.
    let tokens: Vec<(&str, &str)> = FOLDED
        .iter()
        .map(|(surface, _)| (*surface, *surface))
        .collect();
    let positions: Vec<usize> = run("p := [text=ＮＨＫ].", &schema(false), &tokens)
        .iter()
        .map(|record| record.start)
        .collect();
    assert_eq!(positions, [5]);
}

#[test]
fn spellings_that_fold_to_one_key_are_one_set_member() {
    let schema = schema(true);
    for source in [
        "p := (?<w> [text in {ＮＨＫ, NHK}]).",
        "set S := {ＮＨＫ, NHK}.\np := (?<w> [text in @S]).",
    ] {
        assert_eq!(
            run(source, &schema, &FOLDED),
            run("p := (?<w> [text=NHK]).", &schema, &FOLDED),
            "{source}"
        );
    }
}

#[test]
fn extracting_a_set_preserves_attribute_specific_uniqueness() {
    let members = r"{カ\u3099, ガ}";
    let declaration = format!("set S := {members}.\n");
    let tokens = [("カ\u{3099}", "ガ"), ("ガ", "ガ"), ("窓", "窓")];
    for op in ["in", "not in"] {
        let pattern = |operand| format!("p := (?<w> [surface {op} {operand}]).");
        let expected = run(&pattern(members), &schema(true), &tokens);
        assert_eq!(expected.len(), if op == "in" { 2 } else { 1 });
        assert_eq!(
            run(
                &(declaration.clone() + &pattern("@S")),
                &schema(true),
                &tokens
            ),
            expected
        );
    }
    compile(&(declaration.clone() + "p := []."));
    for attr_op in ["lemma in", "text in", "conllu.feats has any"] {
        let named = format!("{declaration}p := [surface in @S].\nq := [{attr_op} @S].");
        let inline = format!("q := [{attr_op} {members}].");
        for source in [&named, &inline] {
            let error = compile_catalog(
                PatternSource::named("probe", source),
                CompileLimits::default(),
            )
            .err()
            .expect("NFC-equivalent members collide");
            assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
            if source == &named {
                let diagnostic = &error.diagnostics[0];
                assert_eq!(diagnostic.location().line(), 3);
                assert!(diagnostic.message.contains("@S"));
                assert!(
                    diagnostic.message.contains("カ\u{3099}") && diagnostic.message.contains("ガ")
                );
            }
        }
    }
    for members in [r"{ガ, \u30AC}", r"{ＮＨＫ, \uFF2EＨＫ}"] {
        for source in [
            format!("set S := {members}.\np := []."),
            format!("p := [text in {members}]."),
        ] {
            assert!(
                compile_catalog(
                    PatternSource::named("probe", &source),
                    CompileLimits::default()
                )
                .is_err(),
                "{source}"
            );
        }
    }
}

#[test]
fn a_surface_literal_matches_only_its_exact_spelling() {
    let schema = schema(true);
    for (literal, expected) in [
        ("ｶﾞﾗｽ", vec![0]),
        ("ガラス", vec![4]),
        ("葛\u{E0100}", vec![2]),
        ("長～い", vec![3]),
        // Not NFC: a surface literal is not normalized either.
        ("\\uFA5C氣", vec![8]),
        ("臭氣", vec![]),
    ] {
        for form in [
            "p := [surface=VALUE].",
            "p := [surface in {VALUE, 窓}].",
            "set S := {VALUE, 窓}.\np := [surface in @S].",
        ] {
            let source = form.replace("VALUE", literal);
            let positions: Vec<usize> = run(&source, &schema, &FOLDED)
                .iter()
                .map(|record| record.start)
                .collect();
            assert_eq!(positions, expected, "{source}");
            assert!(
                compile(&source)
                    .diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.code() != DiagnosticCode::FoldedLiteral),
                "{source}"
            );
        }
    }
}

#[test]
fn a_text_regex_is_compiled_as_written_against_the_folded_key() {
    let folded = schema(true);
    let unfolded = schema(false);
    let as_written: Vec<(&str, &str)> = FOLDED
        .iter()
        .map(|(surface, _)| (*surface, *surface))
        .collect();
    let positions = |source: &str, schema: &TokenSchema, tokens: &[(&str, &str)]| {
        run(source, schema, tokens)
            .iter()
            .map(|record| record.start)
            .collect::<Vec<_>>()
    };
    for (source, on_folded, on_unfolded) in [
        // The class holds ｶ and ﾞ, neither of which the key ガラス contains.
        ("p := [text~/\\A[ｶﾞ]/].", vec![], vec![0]),
        ("p := [text~/\\Aガ/].", vec![0, 4], vec![4]),
        // U+FF5E never occurs in the key: it is U+301C there.
        ("p := [text~/～/].", vec![], vec![3]),
        ("p := [text~/〜/].", vec![3, 7], vec![7]),
        ("p := [text~/ＮＨＫ/].", vec![], vec![1]),
        ("p := [text~/NHK/].", vec![1, 5], vec![5]),
        ("p := [surface~/\\A[ｶﾞ]/].", vec![0], vec![]),
    ] {
        assert_eq!(positions(source, &folded, &FOLDED), on_folded, "{source}");
        assert_eq!(
            positions(source, &unfolded, &as_written),
            on_unfolded,
            "{source}"
        );
    }
}

#[test]
fn the_compiler_names_each_folded_text_literal_and_unfolded_text_regex() {
    let codes = |source: &str| {
        compile(source)
            .diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.code(), diagnostic.message))
            .collect::<Vec<_>>()
    };
    for source in [
        "p := [text=ＮＨＫ].",
        "p := [text in {ＮＨＫ, 窓}].",
        "lexicon p by [text, norm] := {\"ＮＨＫ\"}.",
        "set S := {ＮＨＫ, 窓}.\np := [text in @S].",
        "p := (?<v> [upos=VERB]) where $v -[obj]-> [text in {ＮＨＫ, 窓}].",
        "set S := {ＮＨＫ, 窓}.\np := (?<v> [upos=VERB]) where $v -[obj]-> [text in @S].",
        "set S := {ＮＨＫ, 窓}.\np := (?<v> [upos=VERB]) where $v -[obj]-> (?<o> [text in @S]).",
        "set S := {ＮＨＫ, 窓}.\np := (?<v> [upos=VERB]) where $v -[enhanced obj]-> (?<o> [text in @S]).",
    ] {
        let diagnostics = codes(source);
        assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:?}");
        let (code, message) = &diagnostics[0];
        assert_eq!(*code, DiagnosticCode::FoldedLiteral, "{source}");
        assert!(
            message.contains("`p`") && message.contains("ＮＨＫ") && message.contains("`NHK`"),
            "{message}"
        );
    }
    for source in [
        "p := [text~/[ｶﾞ]/].",
        "p := [text~/～/].",
        "p := [text!~/ＮＨＫ/].",
    ] {
        let diagnostics = codes(source);
        assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:?}");
        assert_eq!(diagnostics[0].0, DiagnosticCode::RegexNotFolded, "{source}");
        assert!(diagnostics[0].1.contains("`p`"), "{}", diagnostics[0].1);
    }
    for source in [
        "p := [text=NHK].",
        "p := [lemma=ＮＨＫ].",
        "p := [norm in {ＮＨＫ}].",
        "p := [surface=ＮＨＫ].",
        "lexicon p by [norm] := {\"ＮＨＫ\"}.",
        "set S := {ＮＨＫ}.\np := [lemma in @S].",
        "p := [lemma~/ＮＨＫ/].",
        "p := [surface~/[ｶﾞ]/].",
        "p := [text~/NHK/].",
    ] {
        assert_eq!(codes(source), Vec::new(), "{source}");
    }
}

#[test]
fn a_lemma_literal_is_compared_as_written_on_every_corpus() {
    for folded in [true, false] {
        let schema = schema(folded);
        let tokens: Vec<(&str, &str)> = FOLDED
            .iter()
            .map(|(surface, text)| (*surface, if folded { *text } else { *surface }))
            .collect();
        let positions: Vec<usize> = run("p := [lemma=ＮＨＫ].", &schema, &tokens)
            .iter()
            .map(|record| record.start)
            .collect();
        assert_eq!(positions, if folded { vec![] } else { vec![1] });
    }
}

/// `set S` with one member that is not NFC: カ followed by U+3099.
const NFD_SET: &str = "set S := {カ\u{3099}ラス, 窓}.\n";

fn normalized_literals(output: &lingrep_core::fsm::CompileOutput) -> Vec<&str> {
    output
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == DiagnosticCode::NormalizedLiteral)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

#[test]
fn a_set_that_only_surface_reads_does_not_report_normalized_literal() {
    for references in [
        "p := [surface in @S].",
        "p := [surface not in @S].",
        "p := [surface in @S] [surface not in @S].\nq := [!(surface in @S)].",
        "test T := surface in @S.\np := [@T].",
        "p := (?<v> [upos=VERB]) where $v -[obj]-> [surface in @S].",
        "p := (?<v> [upos=VERB]) where $v -[obj]-> (?<o> [surface in @S]).",
        "p := (?<v> [upos=VERB]) where $v -[enhanced obj]-> (?<o> [surface in @S]).",
    ] {
        let source = format!("{NFD_SET}{references}");
        assert_eq!(
            normalized_literals(&compile(&source)),
            Vec::<&str>::new(),
            "{source}"
        );
    }
}

#[test]
fn a_set_another_attribute_reads_reports_normalized_literal_at_its_member() {
    for references in [
        "p := [lemma in @S].",
        "p := [lemma not in @S].",
        "p := [text in @S].",
        "p := [conllu.feats has any @S].",
        "p := [surface in @S].\nq := [lemma in @S].",
        "p := [surface in @S | lemma in @S].",
        "test T := lemma in @S.\np := [surface in @S].",
        "p := (?<v> [surface in @S]) where $v -[obj]-> [lemma in @S].",
        "p := (?<v> [surface in @S]) where $v -[obj]-> (?<o> [lemma in @S]).",
        "p := (?<v> [surface in @S]) where $v -[enhanced obj]-> (?<o> [lemma in @S]).",
        // No reference shows that only `surface` reads the set.
        "p := [surface=窓].",
    ] {
        let source = format!("{NFD_SET}{references}");
        let output = compile(&source);
        let warnings: Vec<_> = output
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DiagnosticCode::NormalizedLiteral)
            .collect();
        assert_eq!(warnings.len(), 1, "{source}: {:?}", output.diagnostics);
        assert_eq!(
            warnings[0].message,
            "In definition `S`, the literal `カ\u{3099}ラス` (U+30AB U+3099 U+30E9 U+30B9) is \
             normalized to `ガラス` (U+30AC U+30E9 U+30B9) for matching",
            "{source}"
        );
        let location = warnings[0].location();
        assert_eq!((location.line(), location.column()), (1, 11), "{source}");
    }
}

#[test]
fn a_set_reference_in_another_document_decides_normalized_literal() {
    for (reference, expected) in [("surface", 0), ("lemma", 1)] {
        let mut builder = lingrep_core::fsm::FsmBuilder::new();
        builder
            .add_document(PatternSource::named("sets.lx", NFD_SET))
            .unwrap();
        builder
            .add_document(PatternSource::named(
                "catalog.lx",
                &format!("p := [{reference} in @S]."),
            ))
            .unwrap();
        let output = builder.build().unwrap();
        let warnings = normalized_literals(&output);
        assert_eq!(warnings.len(), expected, "{reference}: {warnings:?}");
    }
}

#[test]
fn an_inline_surface_literal_does_not_report_normalized_literal() {
    for source in [
        "p := [surface=カ\u{3099}ラス].",
        "p := [surface!=カ\u{3099}ラス].",
        "p := [surface in {カ\u{3099}ラス, 窓}].",
        "p := [surface not in {カ\u{3099}ラス, 窓}].",
    ] {
        assert_eq!(
            normalized_literals(&compile(source)),
            Vec::<&str>::new(),
            "{source}"
        );
    }
}
