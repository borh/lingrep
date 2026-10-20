//! Behavioral contract for the consolidated sentence-final-expression catalog.
//!
//! `patterns/sfe.lx` is the retained test catalog.  These cases pin the
//! readings incorporated from the retired `patterns/new-list.lx`; removing
//! any of them would make the retained catalog narrower than its predecessor.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo, TokenSequence};

fn token(attributes: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
    let mut token = TokenInfo::default();
    for (attribute, value) in attributes {
        token.set(*attribute, (*value).into());
    }
    token
}

fn spans_for(pattern: &str, sentence: &dyn TokenSequence) -> Vec<(usize, usize)> {
    let matcher = compile_catalog(
        PatternSource::named("patterns/sfe.lx", include_str!("../../../patterns/sfe.lx")),
        CompileLimits::default(),
    )
    .expect("the retained SFE catalog must compile")
    .matcher;

    matcher
        .find_matches(sentence)
        .into_iter()
        .filter(|matched| matched.pattern == pattern)
        .map(|matched| (matched.start, matched.end))
        .collect()
}

#[test]
fn sfe_recognizes_the_retained_sentence_final_readings() {
    use TokenAttributeIndex::{CForm, Lemma, Orth, Pos, Pos1, Pos2, Pos3};

    let cases = [
        (
            "終助詞",
            vec![token(&[(Orth, "ね"), (Pos2, "終助詞")])],
            vec![(0, 1)],
        ),
        (
            "形容動詞",
            vec![token(&[(Orth, "可能"), (Pos3, "形状詞可能")])],
            vec![(0, 1)],
        ),
        (
            "ようだ",
            vec![token(&[(Orth, "よう")]), token(&[(Orth, "だ")])],
            vec![(0, 2)],
        ),
        (
            "必要がある",
            vec![
                token(&[(Lemma, "必要")]),
                token(&[(Orth, "が")]),
                token(&[(Lemma, "ある"), (Orth, "あっ")]),
                token(&[(Orth, "た")]),
            ],
            vec![(0, 4)],
        ),
        (
            "かもしれない",
            vec![
                token(&[(Pos, "NOUN")]),
                token(&[(Orth, "か")]),
                token(&[(Orth, "も")]),
                token(&[(Lemma, "知れる")]),
                token(&[(Orth, "ない")]),
            ],
            vec![(0, 5)],
        ),
        ("用言連用形", vec![token(&[(Pos1, "形容詞")])], vec![(0, 1)]),
        (
            "う",
            vec![token(&[(Orth, "う"), (CForm, "意志推量形")])],
            vec![(0, 1)],
        ),
    ];

    for (pattern, sentence, expected) in cases {
        assert_eq!(
            spans_for(pattern, &sentence),
            expected,
            "{pattern} must retain its documented sentence-final reading"
        );
    }
}
