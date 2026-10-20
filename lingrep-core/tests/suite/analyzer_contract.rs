#![allow(clippy::unwrap_used)]

use lingrep_core::analysis::{
    AnalysisError, AnalyzerPlan, AnalyzerSession, InputUnitId, JapaneseSegmentationPolicy,
    NormalizationPolicy, SegmentationMode, TextUnit, TextUnitKind, VibratoPlan,
};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, DenseStorageKind, TokenAttributeIndex, TokenSchema,
};

fn sentence(id: u64, text: &str) -> TextUnit {
    TextUnit::sentence(InputUnitId::new(id), text, NormalizationPolicy::None).unwrap()
}

#[test]
fn vibrato_session_returns_ordered_source_aligned_sentences() {
    let plan = VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let projection = plan
        .schema()
        .bind_projection([
            TokenAttributeIndex::Orth,
            TokenAttributeIndex::Lemma,
            TokenAttributeIndex::Pos,
        ])
        .unwrap();
    assert!(
        projection
            .kinds()
            .iter()
            .all(|kind| *kind == AttributeKind::Scalar)
    );
    let mut session = plan.create_session();

    let batch = session
        .analyze_bounded_batch(
            vec![sentence(10, "猫が走る。"), sentence(11, "犬も眠る。")],
            &projection,
        )
        .unwrap();

    assert_eq!(batch.inputs().len(), 2);
    for (expected_id, analyzed_input) in [10, 11].into_iter().zip(batch.inputs()) {
        assert_eq!(analyzed_input.input().id(), InputUnitId::new(expected_id));
        assert_eq!(analyzed_input.sentences().len(), 1);
        let analyzed = &analyzed_input.sentences()[0];
        assert_eq!(analyzed.analyzer_sentence_ordinal(), 0);
        assert_eq!(
            analyzed_input
                .input()
                .slice(analyzed.source_range())
                .unwrap(),
            analyzed_input.input().text()
        );
        assert_eq!(analyzed.surface_tokens().len(), analyzed.words().len());
        assert_eq!(analyzed.words().storage_kind(), DenseStorageKind::Scalars);
        for (index, surface) in analyzed.surface_tokens().iter().enumerate() {
            assert!(
                !analyzed_input
                    .input()
                    .slice(surface.source_range())
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(surface.word_range(), index as u32..index as u32 + 1);
        }
    }
}

#[test]
fn vibrato_result_is_invariant_to_bounded_batch_partitioning() {
    let plan = VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let projection = plan
        .schema()
        .bind_projection([TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma])
        .unwrap();
    let inputs = vec![sentence(20, "雨が降る。"), sentence(21, "風が吹く。")];
    let mut together_session = plan.create_session();
    let together = together_session
        .analyze_bounded_batch(inputs.clone(), &projection)
        .unwrap();
    let mut split_session = plan.create_session();
    let split: Vec<_> = inputs
        .into_iter()
        .map(|input| {
            split_session
                .analyze_bounded_batch(vec![input], &projection)
                .unwrap()
                .into_inputs()
                .into_vec()
                .pop()
                .unwrap()
        })
        .collect();

    assert_eq!(together.inputs(), split);
}

#[test]
fn vibrato_rejects_segment_input_and_non_native_scalar_projection() {
    let plan = VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let native = plan
        .schema()
        .bind_projection([TokenAttributeIndex::Orth])
        .unwrap();
    let segment = TextUnit::admit(
        InputUnitId::new(30),
        TextUnitKind::Segment,
        "sentence segmentation is upstream",
        NormalizationPolicy::None,
    )
    .unwrap();
    let mut session = plan.create_session();
    assert_eq!(
        session.analyze_bounded_batch(vec![segment], &native),
        Err(AnalysisError::WrongInputKind)
    );

    let dynamic_schema = TokenSchema::from_keys(["provider.scalar9"]).unwrap();
    let dynamic = dynamic_schema
        .bind_projection([AttributeKey::new("provider.scalar9").unwrap()])
        .unwrap();
    assert_eq!(
        session.analyze_bounded_batch(vec![sentence(31, "猫")], &dynamic),
        Err(AnalysisError::UnsupportedProjection)
    );
}

#[test]
fn native_japanese_segmentation_returns_source_ranges_matching_legacy_slices() {
    for (id, text) in [
        (40, "一。二！」末尾"),
        (41, "  一。  "),
        (42, ""),
        (43, "e\u{301}。末尾"),
    ] {
        let unit = TextUnit::admit(
            InputUnitId::new(id),
            TextUnitKind::Segment,
            text,
            NormalizationPolicy::None,
        )
        .unwrap();
        let ranges = JapaneseSegmentationPolicy::V1
            .sentence_ranges(&unit)
            .unwrap();
        let actual: Vec<_> = ranges
            .iter()
            .map(|range| unit.slice(*range).unwrap().to_owned())
            .collect();
        assert_eq!(actual, lingrep_core::utils::split_sentence_ja(text));
    }
}

#[test]
fn vibrato_analyzer_mode_groups_multiple_sentences_under_one_segment() {
    let plan = VibratoPlan::with_mode(
        lingrep_core::testing::test_tokenizer().clone(),
        SegmentationMode::Analyzer,
    );
    let projection = plan
        .schema()
        .bind_projection([TokenAttributeIndex::Orth])
        .unwrap();
    let input = TextUnit::admit(
        InputUnitId::new(50),
        TextUnitKind::Segment,
        "猫。犬！",
        NormalizationPolicy::None,
    )
    .unwrap();
    let mut session = plan.create_session();
    let batch = session
        .analyze_bounded_batch(vec![input], &projection)
        .unwrap();
    let analyzed = &batch.inputs()[0];
    assert_eq!(analyzed.input().text(), "猫。犬！");
    assert_eq!(analyzed.sentences().len(), 2);
    assert_eq!(
        analyzed
            .sentences()
            .iter()
            .map(|sentence| analyzed.input().slice(sentence.source_range()).unwrap())
            .collect::<Vec<_>>(),
        ["猫。", "犬！"]
    );
}
