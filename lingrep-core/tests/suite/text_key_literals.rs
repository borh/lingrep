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
