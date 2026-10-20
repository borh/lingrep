use std::str::FromStr;

use lingrep_core::token_match::{AttributeKey, TokenAttributeIndex};

#[test]
fn v8_attribute_keys_accept_only_canonical_namespaces() {
    for key in [
        "text",
        "lemma",
        "norm",
        "upos",
        "xpos",
        "feat.Number",
        "feat.Gender[psor]",
        "unidic.pos1",
        "spacy.morph.reading",
    ] {
        assert_eq!(AttributeKey::new(key).unwrap().as_str(), key);
    }
}

#[test]
fn v8_attribute_keys_reject_aliases_case_drift_and_malformed_paths() {
    for key in [
        "",
        "orth",
        "form",
        "pos",
        "tag",
        "ORTH",
        "Feat.Number",
        "feat.number",
        "feat.Gender[PSOR]",
        "unidic.orth_base",
        "unidic..form",
        "Unidic.form",
        "provider",
    ] {
        assert!(AttributeKey::new(key).is_err(), "accepted {key:?}");
    }
}

/// V1 makes `AttributeKey` case-sensitive. This test is deleted deliberately then.
#[test]
fn attribute_lookup_is_currently_case_insensitive() {
    assert_eq!(
        TokenAttributeIndex::from_str("ORTH"),
        TokenAttributeIndex::from_str("orth")
    );
    assert_eq!(
        TokenAttributeIndex::from_str("Orth"),
        TokenAttributeIndex::from_str("orth")
    );
}

/// V1 deletes both underscore aliases. This test makes that deletion visible.
#[test]
fn underscore_aliases_currently_resolve() {
    assert_eq!(
        TokenAttributeIndex::from_str("orth_base"),
        TokenAttributeIndex::from_str("orthbase")
    );
    assert_eq!(
        TokenAttributeIndex::from_str("pron_base"),
        TokenAttributeIndex::from_str("pronbase")
    );
}

/// The complete inventory the V1 rename table must cover.
#[test]
fn every_attribute_round_trips_through_its_pattern_key() {
    for attribute in TokenAttributeIndex::ALL {
        assert_eq!(
            TokenAttributeIndex::from_str(attribute.pattern_key()),
            Ok(attribute),
            "{attribute:?} does not round-trip"
        );
    }
}

/// Current `form` is UniDic 語形, not the surface; V1 resolves this collision.
#[test]
fn form_is_currently_a_unidic_field_not_the_surface() {
    assert_eq!(
        TokenAttributeIndex::from_str("form"),
        Ok(TokenAttributeIndex::Form)
    );
    assert_ne!(
        TokenAttributeIndex::from_str("form"),
        Ok(TokenAttributeIndex::Text)
    );
}
