use std::path::PathBuf;

use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId, NormalizationPolicy,
    SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan, SudachiSplit, TextUnit,
    TextUnitKind,
};

use super::candidate::{CandidateSentence, CandidateSurface, decode, encode, from_analysis_batch};

fn fixture() -> Vec<CandidateSentence> {
    vec![
        CandidateSentence {
            text: " leading  e\u{301} 日本語。 \n".into(),
            surfaces: vec![
                CandidateSurface {
                    source_start: 1,
                    source_end: 8,
                    word_start: 0,
                    word_end: 1,
                },
                CandidateSurface {
                    source_start: 10,
                    source_end: 13,
                    word_start: 1,
                    word_end: 3,
                },
                CandidateSurface {
                    source_start: 14,
                    source_end: 23,
                    word_start: 3,
                    word_end: 4,
                },
            ],
        },
        CandidateSentence {
            text: String::new(),
            surfaces: vec![],
        },
    ]
}

#[test]
fn selected_layout_round_trips_exact_text_ranges_and_word_alignment() {
    let expected = fixture();
    let first = encode(&expected).unwrap();
    let second = encode(&expected).unwrap();
    assert_eq!(first, second);
    assert_eq!(decode(&first).unwrap(), expected);
}

#[test]
fn decoder_rejects_corruption_truncation_and_trailing_bytes() {
    let valid = encode(&fixture()).unwrap();
    let mut magic = valid.clone();
    magic[0] ^= 1;
    assert!(decode(&magic).is_err());
    assert!(decode(&valid[..valid.len() - 1]).is_err());
    let mut trailing = valid;
    trailing.push(0);
    assert!(decode(&trailing).is_err());
}

#[test]
fn encoder_rejects_impossible_utf8_ranges_and_alignment() {
    let invalid = CandidateSentence {
        text: "é".into(),
        surfaces: vec![CandidateSurface {
            source_start: 1,
            source_end: 2,
            word_start: 1,
            word_end: 1,
        }],
    };
    assert!(encode(&[invalid]).is_err());
}

#[test]
#[ignore = "requires LINGREP_SUDACHI_RESOURCES and every LINGREP_SUDACHI_DICT_* variable"]
fn canonical_sudachi_batches_round_trip_through_candidate_provenance() {
    let resources = PathBuf::from(std::env::var_os("LINGREP_SUDACHI_RESOURCES").unwrap());
    for (edition, dictionary_variable) in [
        (SudachiEdition::Small, "LINGREP_SUDACHI_DICT_SMALL"),
        (SudachiEdition::Core, "LINGREP_SUDACHI_DICT_CORE"),
        (SudachiEdition::Full, "LINGREP_SUDACHI_DICT_FULL"),
    ] {
        let dictionary = PathBuf::from(std::env::var_os(dictionary_variable).unwrap());
        let (dictionary_digest, resource_digest) =
            SudachiConfig::content_digests(&dictionary, &resources).unwrap();
        for split in [SudachiSplit::A, SudachiSplit::B, SudachiSplit::C] {
            for profile in [ExtractionProfile::Tokenize, ExtractionProfile::Morphology] {
                for segmentation in [SegmentationMode::Input, SegmentationMode::Analyzer] {
                    let plan = SudachiPlan::load(
                        SudachiConfig::new(
                            &dictionary,
                            &resources,
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
                    .unwrap();
                    let projection = plan
                        .schema()
                        .bind_projection(plan.schema().keys().iter().cloned())
                        .unwrap();
                    let (kind, text) = match segmentation {
                        SegmentationMode::Input => (
                            TextUnitKind::Sentence,
                            " leading e\u{301} 🦀 国家公務員。 trailing ",
                        ),
                        SegmentationMode::Analyzer => (
                            TextUnitKind::Segment,
                            " leading e\u{301}。🦀 国家公務員！ trailing ",
                        ),
                    };
                    let input = TextUnit::admit(
                        InputUnitId::new(700),
                        kind,
                        text,
                        NormalizationPolicy::None,
                    )
                    .unwrap();
                    let mut session = plan.create_session();
                    let batch = session
                        .analyze_bounded_batch(vec![input], &projection)
                        .unwrap();
                    let candidate = from_analysis_batch(&batch).unwrap();
                    assert_eq!(decode(&encode(&candidate).unwrap()).unwrap(), candidate);

                    let analyzed = &batch.inputs()[0];
                    assert_eq!(candidate.len(), analyzed.sentences().len());
                    for (stored, sentence) in candidate.iter().zip(analyzed.sentences()) {
                        assert_eq!(
                            stored.text,
                            analyzed.input().slice(sentence.source_range()).unwrap()
                        );
                        assert_eq!(stored.surfaces.len(), sentence.surface_tokens().len());
                        for (stored_surface, surface) in
                            stored.surfaces.iter().zip(sentence.surface_tokens())
                        {
                            assert_eq!(
                                stored_surface.source_start,
                                surface.source_range().start() - sentence.source_range().start()
                            );
                            assert_eq!(
                                stored_surface.source_end,
                                surface.source_range().end() - sentence.source_range().start()
                            );
                            assert_eq!(
                                stored_surface.word_start..stored_surface.word_end,
                                surface.word_range()
                            );
                        }
                    }
                }
            }
        }
    }
}
