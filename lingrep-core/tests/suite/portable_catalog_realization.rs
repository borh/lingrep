#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId, NormalizationPolicy,
    SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan, SudachiSplit, TextUnit,
    VibratoPlan,
};
use lingrep_core::fsm::{
    AnalyzerCompatibility, BoundCatalog, CompileLimits, PatternSource, compile_catalog,
};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema,
};

const CATALOG: &str = include_str!("../../../patterns/portable-content-words.lx");

fn compiled_catalog() -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("patterns/portable-content-words.lx", CATALOG),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher
}

fn sudachi_schema() -> TokenSchema {
    SudachiConfig::new(
        "/immutable/system.dic",
        "/immutable/resources",
        [1; 32],
        [2; 32],
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
    )
    .unwrap()
    .schema()
    .clone()
}

fn spacy_schema() -> TokenSchema {
    TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("norm", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("xpos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("spacy.morph.inflection", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("spacy.morph.reading", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap()
}

fn stanza_schema() -> TokenSchema {
    TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("xpos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap()
}

#[test]
fn one_portable_catalog_binds_all_four_morphology_schemas_without_recompilation() {
    let compiled = compiled_catalog();
    assert_eq!(
        compiled
            .required_attributes()
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["upos"]
    );

    for schema in [
        TokenSchema::unidic_morphology(),
        sudachi_schema(),
        spacy_schema(),
        stanza_schema(),
    ] {
        let bound = compiled.bind(&schema).unwrap();
        let words = BoundTokenSentence::from_projected_values(
            bound.projection(),
            4,
            ["NOUN", "ADP", "VERB", "PUNCT"]
                .into_iter()
                .map(|value| Some(value.into()))
                .collect(),
        )
        .unwrap();
        let matches = bound.find_matches(&words).unwrap();
        assert_eq!(
            matches
                .iter()
                .map(|item| (item.pattern.as_str(), item.start, item.end))
                .collect::<Vec<_>>(),
            [("content_noun", 0, 1), ("lexical_predicate", 2, 3)]
        );
    }

    // A schema without `upos` still binds — the reference is reported as
    // unavailable and the positive tests can match nothing there.
    let degraded = compiled
        .bind(&TokenSchema::from_keys(["text"]).unwrap())
        .unwrap();
    assert_eq!(
        degraded
            .unavailable_attributes()
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["upos"]
    );
}

#[test]
fn portable_catalog_declares_each_observed_analyzer_realization() {
    let compiled = compiled_catalog();
    for (family, resource, split, segmentation) in [
        ("vibrato", "unidic-cwj", None, "input"),
        ("sudachi", "core", Some("C"), "input"),
        ("spacy", "ja_ginza", None, "input"),
        ("stanza", "fr-gsd-morphology", None, "analyzer"),
    ] {
        assert_eq!(
            compiled.analyzer_compatibility(family, Some(resource), split, segmentation),
            AnalyzerCompatibility::Compatible
        );
    }
}

fn assert_japanese_realization(plan: &impl AnalyzerPlan) {
    let compiled = compiled_catalog();
    let bound = compiled
        .bind_with_projection(plan.schema(), [AttributeKey::new("text").unwrap()])
        .unwrap();
    let mut session = plan.create_session();
    let batch = session
        .analyze_bounded_batch(
            vec![
                TextUnit::sentence(InputUnitId::new(1), "猫が走る。", NormalizationPolicy::None)
                    .unwrap(),
            ],
            bound.projection(),
        )
        .unwrap();
    assert_realization(&bound, batch.inputs()[0].sentences()[0].words());
}

fn assert_realization(bound: &BoundCatalog, words: &BoundTokenSentence) {
    let text = bound.projection().ordinal_key("text").unwrap();
    let matches = bound.find_matches(words).unwrap();
    let actual = matches
        .iter()
        .map(|item| {
            (
                item.pattern.as_str(),
                words.token(item.start).unwrap().get(text).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [("content_noun", "猫"), ("lexical_predicate", "走る")]
    );
}

#[test]
#[ignore = "requires fixed UniDic-CWJ and Sudachi core/C resources"]
fn real_vibrato_and_sudachi_realize_the_same_portable_catalog() {
    let dictionary = std::env::var("LINGREP_DICTIONARY_PATH").unwrap();
    let tokenizer = lingrep_core::dictionary::create_tokenizer_from_path(&dictionary).unwrap();
    assert_japanese_realization(&VibratoPlan::configured(
        tokenizer,
        [0; 32],
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
    ));

    let sudachi_dictionary = PathBuf::from(std::env::var_os("LINGREP_SUDACHI_DICT_CORE").unwrap());
    let sudachi_resources = PathBuf::from(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap());
    let (dictionary_digest, resource_digest) =
        SudachiConfig::content_digests(&sudachi_dictionary, &sudachi_resources).unwrap();
    let sudachi = SudachiPlan::load(
        SudachiConfig::new(
            sudachi_dictionary,
            sudachi_resources,
            dictionary_digest,
            resource_digest,
            SudachiEdition::Core,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            NormalizationPolicy::None,
        )
        .unwrap(),
    )
    .unwrap();
    assert_japanese_realization(&sudachi);
}
