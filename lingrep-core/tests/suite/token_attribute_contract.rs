use std::str::FromStr;

use lingrep_core::token_match::TokenAttributeIndex;

const EXPECTED: [(&str, TokenAttributeIndex); 32] = [
    ("pos1", TokenAttributeIndex::Pos1),
    ("pos2", TokenAttributeIndex::Pos2),
    ("pos3", TokenAttributeIndex::Pos3),
    ("pos4", TokenAttributeIndex::Pos4),
    ("ctype", TokenAttributeIndex::CType),
    ("cform", TokenAttributeIndex::CForm),
    ("lform", TokenAttributeIndex::LForm),
    ("lemma", TokenAttributeIndex::Lemma),
    ("orth", TokenAttributeIndex::Orth),
    ("pron", TokenAttributeIndex::Pron),
    ("orthbase", TokenAttributeIndex::OrthBase),
    ("pronbase", TokenAttributeIndex::PronBase),
    ("goshu", TokenAttributeIndex::Goshu),
    ("itype", TokenAttributeIndex::IType),
    ("iform", TokenAttributeIndex::IForm),
    ("ftype", TokenAttributeIndex::FType),
    ("fform", TokenAttributeIndex::FForm),
    ("icontype", TokenAttributeIndex::IConType),
    ("fcontype", TokenAttributeIndex::FConType),
    ("type", TokenAttributeIndex::Type),
    ("kana", TokenAttributeIndex::Kana),
    ("kanabase", TokenAttributeIndex::KanaBase),
    ("form", TokenAttributeIndex::Form),
    ("formbase", TokenAttributeIndex::FormBase),
    ("atype", TokenAttributeIndex::AType),
    ("acontype", TokenAttributeIndex::AConType),
    ("amodtype", TokenAttributeIndex::AModType),
    ("pos", TokenAttributeIndex::Pos),
    ("tag", TokenAttributeIndex::Tag),
    ("norm", TokenAttributeIndex::Norm),
    ("text", TokenAttributeIndex::Text),
    ("lextype", TokenAttributeIndex::LexType),
];

#[test]
fn public_attribute_ordinals_keys_and_parser_remain_stable() {
    for (index, (key, attribute)) in EXPECTED.into_iter().enumerate() {
        assert_eq!(TokenAttributeIndex::from_usize(index), Some(attribute));
        assert_eq!(TokenAttributeIndex::from_str(key), Ok(attribute));
    }
    assert_eq!(TokenAttributeIndex::from_usize(EXPECTED.len()), None);
}

#[test]
fn all_is_complete_unique_and_uses_stable_keys() {
    assert_eq!(TokenAttributeIndex::ALL.len(), EXPECTED.len());
    let actual = TokenAttributeIndex::ALL
        .into_iter()
        .map(TokenAttributeIndex::pattern_key)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual.len(), EXPECTED.len());
    assert_eq!(
        TokenAttributeIndex::ALL.map(TokenAttributeIndex::pattern_key),
        EXPECTED.map(|(key, _)| key),
    );
}
