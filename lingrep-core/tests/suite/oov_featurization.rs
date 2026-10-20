//! Surface-derived base forms for unknown words, against the real dictionary.
//!
//! The unit tests in `token_match` use a miniature in-memory dictionary whose
//! unknown-word entry was hand-built to reproduce UniDic's six-field record.
//! This holds that fixture to the real thing: if a future dictionary's
//! unknown-word record gains fields, the fixture keeps passing while the
//! pipeline stops exercising the case, and only this notices.

use lingrep_core::testing::{compile_test_pattern, tokenize_japanese};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo, TokenInfoTrait};

/// No UniDic entry covers this, in any dictionary this project ships.
const OUT_OF_VOCABULARY: &str = "qwertyzxcv";

fn unknown_token() -> TokenInfo {
    tokenize_japanese(&format!("これは{OUT_OF_VOCABULARY}です"))
        .into_iter()
        .find(|token| {
            token
                .get(TokenAttributeIndex::Orth)
                .map(compact_str::CompactString::as_str)
                == Some(OUT_OF_VOCABULARY)
        })
        .expect("the probe string must survive as one token")
}

#[test]
fn the_real_dictionary_still_produces_an_unknown_token_for_the_probe_string() {
    // Guards the two tests below against becoming vacuous: were this string
    // ever added to a dictionary, they would pass for the wrong reason.
    assert_eq!(
        unknown_token()
            .get(TokenAttributeIndex::LexType)
            .map(compact_str::CompactString::as_str),
        Some("unknown")
    );
}

#[test]
fn base_form_predicates_reach_unknown_word_material() {
    // The acceptance criterion. Before the fallback both counts were 0: a
    // missing attribute satisfies neither polarity, so the predicate could not
    // match and nothing said so.
    let tokens = tokenize_japanese(&format!("これは{OUT_OF_VOCABULARY}です"));

    for definition in [
        format!("[unidic.orthbase={OUT_OF_VOCABULARY}]"),
        format!("[lemma={OUT_OF_VOCABULARY}]"),
    ] {
        let matcher = compile_test_pattern(&definition);
        assert_eq!(
            matcher.find_matches(&tokens).len(),
            1,
            "{definition} must reach unknown-word material"
        );
    }
}

#[test]
fn the_real_dictionary_leaves_the_phonological_attributes_absent() {
    // The other half of the policy, against UniDic rather than the fixture. A
    // fallback that widened to these would invent pronunciations.
    let token = unknown_token();

    for attribute in [
        TokenAttributeIndex::Pron,
        TokenAttributeIndex::PronBase,
        TokenAttributeIndex::Kana,
        TokenAttributeIndex::KanaBase,
        TokenAttributeIndex::Form,
        TokenAttributeIndex::FormBase,
        TokenAttributeIndex::LForm,
    ] {
        assert!(
            token.get(attribute).is_none(),
            "{attribute:?} must stay absent on real unknown-word material"
        );
    }
}
