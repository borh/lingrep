#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId, NormalizationPolicy,
    SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan, SudachiResources, SudachiSplit,
    TextUnit, TextUnitKind,
};
use lingrep_core::token_match::{AttributeKind, DenseStorageKind};

fn config(
    edition: SudachiEdition,
    split: SudachiSplit,
    profile: ExtractionProfile,
    segmentation: SegmentationMode,
) -> SudachiConfig {
    SudachiConfig::new(
        PathBuf::from("/immutable/system.dic"),
        SudachiResources::directory(PathBuf::from("/immutable/resources")),
        [1; 32],
        [2; 32],
        edition,
        split,
        profile,
        segmentation,
        NormalizationPolicy::None,
    )
    .unwrap()
}

#[test]
fn sudachi_profile_schemas_are_canonical_and_closed() {
    let tokenize = config(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Tokenize,
        SegmentationMode::Input,
    );
    assert_eq!(
        tokenize
            .schema()
            .keys()
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["text"]
    );

    let morphology = config(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
    );
    assert_eq!(
        morphology
            .schema()
            .keys()
            .iter()
            .map(lingrep_core::token_match::AttributeKey::as_str)
            .collect::<Vec<_>>(),
        [
            "text",
            "lemma",
            "norm",
            "upos",
            "xpos",
            "sudachi.cform",
            "sudachi.ctype",
            "sudachi.lextype",
            "sudachi.pos1",
            "sudachi.pos2",
            "sudachi.pos3",
            "sudachi.pos4",
            "sudachi.reading",
        ]
    );
    assert!(
        morphology
            .schema()
            .attributes()
            .iter()
            .all(|attribute| attribute.kind() == AttributeKind::Scalar)
    );
}

/// Sudachi analysis identity moves only by a deliberate edit: a new
/// `SUDACHI_REVISION`, adapter or mapping revision, or edition label. When
/// that edit is intended, replace this literal.
#[test]
fn sudachi_semantic_identity_is_pinned() {
    let identity = config(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
    )
    .semantic_identity();
    let hex: String = identity
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        hex,
        "a6db886dac3ec4682b63883307324ea317c92de08a033698b9d74f80fa5dc514"
    );
}

#[test]
fn sudachi_semantic_identity_covers_configuration_but_not_paths() {
    let base = config(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
    );
    for changed in [
        config(
            SudachiEdition::Small,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
        ),
        config(
            SudachiEdition::Core,
            SudachiSplit::A,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
        ),
        config(
            SudachiEdition::Core,
            SudachiSplit::C,
            ExtractionProfile::Tokenize,
            SegmentationMode::Input,
        ),
        config(
            SudachiEdition::Core,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Analyzer,
        ),
    ] {
        assert_ne!(base.semantic_identity(), changed.semantic_identity());
    }

    let relocated = SudachiConfig::new(
        "/elsewhere/system.dic",
        SudachiResources::directory("/elsewhere/resources"),
        [1; 32],
        [2; 32],
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
    )
    .unwrap();
    assert_eq!(base.semantic_identity(), relocated.semantic_identity());

    let changed_dictionary = SudachiConfig::new(
        "/immutable/system.dic",
        SudachiResources::directory("/immutable/resources"),
        [9; 32],
        [2; 32],
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        NormalizationPolicy::None,
    )
    .unwrap();
    assert_ne!(
        base.semantic_identity(),
        changed_dictionary.semantic_identity()
    );
}

#[test]
fn sudachi_load_rejects_a_claimed_digest_before_opening_the_dictionary() {
    let directory = tempfile::tempdir().unwrap();
    let dictionary = directory.path().join("system.dic");
    let resources = directory.path().join("resources");
    std::fs::create_dir(&resources).unwrap();
    std::fs::write(&dictionary, b"not a dictionary").unwrap();
    std::fs::write(resources.join("char.def"), b"DEFAULT 0 1 0").unwrap();
    let (_, resource_digest) =
        SudachiConfig::content_digests(&dictionary, &SudachiResources::directory(&resources))
            .unwrap();
    let config = SudachiConfig::new(
        dictionary,
        SudachiResources::directory(resources),
        [9; 32],
        resource_digest,
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Tokenize,
        SegmentationMode::Input,
        NormalizationPolicy::None,
    )
    .unwrap();
    assert!(matches!(
        SudachiPlan::load(config),
        Err(lingrep_core::analysis::SudachiLoadError::ContentMismatch(
            "dictionary"
        ))
    ));
}

fn dictionary_env(edition: SudachiEdition) -> &'static str {
    match edition {
        SudachiEdition::Small => "LINGREP_SUDACHI_DICT_SMALL",
        SudachiEdition::Core => "LINGREP_SUDACHI_DICT_CORE",
        SudachiEdition::Full => "LINGREP_SUDACHI_DICT_FULL",
    }
}

fn load_plan(
    edition: SudachiEdition,
    split: SudachiSplit,
    profile: ExtractionProfile,
    segmentation: SegmentationMode,
) -> SudachiPlan {
    let resources =
        SudachiResources::directory(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap());
    load_plan_with(edition, split, profile, segmentation, resources)
}

fn load_plan_with(
    edition: SudachiEdition,
    split: SudachiSplit,
    profile: ExtractionProfile,
    segmentation: SegmentationMode,
    resources: SudachiResources,
) -> SudachiPlan {
    let dictionary = PathBuf::from(std::env::var_os(dictionary_env(edition)).unwrap());
    let (dictionary_digest, resource_digest) =
        SudachiConfig::content_digests(&dictionary, &resources).unwrap();
    SudachiPlan::load(
        SudachiConfig::new(
            dictionary,
            resources,
            dictionary_digest,
            resource_digest,
            edition,
            split,
            profile,
            segmentation,
            NormalizationPolicy::None,
        )
        .unwrap(),
    )
    .unwrap()
}

fn analyze_forms(plan: &SudachiPlan, kind: TextUnitKind, text: &str) -> Vec<Vec<String>> {
    let projection = plan
        .schema()
        .bind_projection([lingrep_core::token_match::AttributeKey::new("text").unwrap()])
        .unwrap();
    let input =
        TextUnit::admit(InputUnitId::new(100), kind, text, NormalizationPolicy::None).unwrap();
    let mut session = plan.create_session();
    let batch = session
        .analyze_bounded_batch(vec![input], &projection)
        .unwrap();
    let analyzed = &batch.inputs()[0];
    let text = projection.ordinal_key("text").unwrap();
    analyzed
        .sentences()
        .iter()
        .map(|sentence| {
            assert_eq!(sentence.surface_tokens().len(), sentence.words().len());
            assert_eq!(sentence.words().storage_kind(), DenseStorageKind::Scalars);
            sentence
                .surface_tokens()
                .iter()
                .enumerate()
                .map(|(index, surface)| {
                    assert_eq!(surface.word_range(), index as u32..index as u32 + 1);
                    let source = analyzed.input().slice(surface.source_range()).unwrap();
                    let value = sentence.words().token(index).unwrap().get(text).unwrap();
                    assert_eq!(source, value);
                    value.to_owned()
                })
                .collect()
        })
        .collect()
}

/// The resource files compiled into `sudachi` and the pinned resource
/// directory give one analysis identity and the same tokens.
#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and LINGREP_SUDACHI_DICT_CORE"]
fn built_in_resources_match_the_pinned_resource_directory() {
    let directory =
        SudachiResources::directory(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap());
    let text = "ｱｲｳ　国家公務員が𠮷野家で働いた。";
    for split in [SudachiSplit::A, SudachiSplit::C] {
        let [from_directory, built_in] =
            [directory.clone(), SudachiResources::BuiltIn].map(|resources| {
                load_plan_with(
                    SudachiEdition::Core,
                    split,
                    ExtractionProfile::Morphology,
                    SegmentationMode::Input,
                    resources,
                )
            });
        assert_eq!(
            from_directory.descriptor().semantic_identity(),
            built_in.descriptor().semantic_identity()
        );
        assert_eq!(
            analyze_forms(&from_directory, TextUnitKind::Sentence, text),
            analyze_forms(&built_in, TextUnitKind::Sentence, text)
        );
    }
}

#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and LINGREP_SUDACHI_DICT_CORE"]
fn sudachi_core_preserves_exact_surfaces_for_every_split() {
    for (split, expected) in [
        (SudachiSplit::A, vec!["国家", "公務", "員"]),
        (SudachiSplit::B, vec!["国家", "公務員"]),
        (SudachiSplit::C, vec!["国家公務員"]),
    ] {
        let plan = load_plan(
            SudachiEdition::Core,
            split,
            ExtractionProfile::Tokenize,
            SegmentationMode::Input,
        );
        assert_eq!(
            analyze_forms(&plan, TextUnitKind::Sentence, "国家公務員"),
            [expected]
        );
    }
}

#[test]
#[ignore = "requires every LINGREP_SUDACHI_DICT_* variable and LINGREP_SUDACHI_RESOURCES"]
fn every_sudachi_edition_loads_and_produces_complete_morphology() {
    for edition in [
        SudachiEdition::Small,
        SudachiEdition::Core,
        SudachiEdition::Full,
    ] {
        let plan = load_plan(
            edition,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
        );
        let projection = plan
            .schema()
            .bind_projection(plan.schema().keys().iter().cloned())
            .unwrap();
        let input = TextUnit::sentence(
            InputUnitId::new(101),
            "🦀が走る。",
            NormalizationPolicy::None,
        )
        .unwrap();
        let mut session = plan.create_session();
        let batch = session
            .analyze_bounded_batch(vec![input], &projection)
            .unwrap();
        let sentence = &batch.inputs()[0].sentences()[0];
        assert!(!sentence.words().is_empty());
        for token_index in 0..sentence.words().len() {
            let token = sentence.words().token(token_index).unwrap();
            for ordinal in 0..projection.len() {
                assert!(
                    token
                        .get(lingrep_core::token_match::AttributeOrdinal::new(
                            ordinal as u8
                        ))
                        .is_some()
                );
            }
        }
    }
}

#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and LINGREP_SUDACHI_DICT_CORE"]
fn sudachi_analyzer_segmentation_groups_sentences_under_the_segment() {
    let plan = load_plan(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Tokenize,
        SegmentationMode::Analyzer,
    );
    assert_eq!(
        analyze_forms(&plan, TextUnitKind::Segment, "国家公務員。猫！"),
        [vec!["国家公務員", "。"], vec!["猫", "！"]]
    );
}

#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and LINGREP_SUDACHI_DICT_CORE"]
fn sudachi_provider_pos_hierarchy_maps_to_portable_upos() {
    let plan = load_plan(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
    );
    let projection = plan
        .schema()
        .bind_projection(
            ["text", "upos", "xpos", "sudachi.pos1", "sudachi.pos2"]
                .map(|key| lingrep_core::token_match::AttributeKey::new(key).unwrap()),
        )
        .unwrap();
    let input = TextUnit::sentence(
        InputUnitId::new(102),
        "猫が走るね。",
        NormalizationPolicy::None,
    )
    .unwrap();
    let mut session = plan.create_session();
    let batch = session
        .analyze_bounded_batch(vec![input], &projection)
        .unwrap();
    let words = batch.inputs()[0].sentences()[0].words();
    let text = projection.ordinal_key("text").unwrap();
    let upos = projection.ordinal_key("upos").unwrap();
    let actual: Vec<_> = (0..words.len())
        .map(|index| {
            let token = words.token(index).unwrap();
            (token.get(text).unwrap(), token.get(upos).unwrap())
        })
        .collect();
    assert_eq!(
        actual,
        [
            ("猫", "NOUN"),
            ("が", "ADP"),
            ("走る", "VERB"),
            ("ね", "PART"),
            ("。", "PUNCT"),
        ]
    );
}

#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and LINGREP_SUDACHI_DICT_CORE"]
fn long_sudachi_input_keeps_one_sentence_and_complete_source_alignment() {
    let plan = load_plan(
        SudachiEdition::Core,
        SudachiSplit::C,
        ExtractionProfile::Tokenize,
        SegmentationMode::Input,
    );
    let mut builder = lingrep_core::fsm::FsmBuilder::new();
    builder
        .add_definition(lingrep_core::fsm::PatternSource::named(
            "boundary.lx",
            "boundary := [text=、] [text=猫].",
        ))
        .unwrap();
    let matcher = builder.build().unwrap().matcher;
    let projection = plan
        .schema()
        .bind_projection(plan.schema().keys().iter().cloned())
        .unwrap();
    for text in [
        format!("{}😀猫", "猫、".repeat(20_000)),
        format!("{}😀{}", "a".repeat(49_148), "猫".repeat(20_000)),
    ] {
        let input =
            TextUnit::sentence(InputUnitId::new(1), &text, NormalizationPolicy::None).unwrap();
        let batch = plan
            .create_session()
            .analyze_bounded_batch(vec![input], &projection)
            .unwrap();
        let analyzed = &batch.inputs()[0];
        assert_eq!(analyzed.sentences().len(), 1);
        let sentence = &analyzed.sentences()[0];
        assert_eq!(sentence.source_range().as_usize(), 0..text.len());
        let mut end = 0;
        for (ordinal, token) in sentence.surface_tokens().iter().enumerate() {
            let range = token.source_range().as_usize();
            assert_eq!(range.start, end);
            assert_eq!(token.word_range(), ordinal as u32..ordinal as u32 + 1);
            end = range.end;
        }
        assert_eq!(end, text.len());
        if text.contains('、') {
            assert!(
                matcher
                    .find_matches(sentence.words())
                    .contains(&lingrep_core::fsm::Match {
                        pattern: "boundary".into(),
                        start: 16_381,
                        end: 16_383,
                        captures: Box::new([]),
                        graph_bindings: Box::new([]),
                        lexical: None,
                    })
            );
        }
    }
}
