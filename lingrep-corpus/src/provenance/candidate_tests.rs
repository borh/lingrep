use std::path::PathBuf;

use lingrep_core::analysis::{
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId, NormalizationPolicy,
    SegmentationMode, SudachiConfig, SudachiEdition, SudachiPlan, SudachiResources, SudachiSplit,
    TextUnit, TextUnitKind,
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

#[hegel::test(test_cases = 200, derandomize = true)]
fn bounded_decode_agrees_with_unbounded_decode_exactly_within_the_raw_limit(tc: hegel::TestCase) {
    use hegel::generators as g;
    let size = tc.draw(g::integers::<usize>()) % 4097;
    let character = char::from_u32(tc.draw(g::integers::<u32>())).unwrap_or('\0');
    let expected = vec![CandidateSentence {
        text: format!("字<&{}", character.to_string().repeat(size)),
        surfaces: vec![],
    }];
    let artifact = encode(&expected).unwrap();
    let raw = u64::from_le_bytes(artifact[9..17].try_into().unwrap()) as usize;
    let limit = match tc.draw(g::integers::<u8>().max_value(3)) {
        0 => raw.saturating_sub(1),
        1 => raw,
        2 => raw + 1,
        _ => tc.draw(g::integers::<usize>()),
    };
    assert_eq!(
        super::candidate::decode_bounded(&artifact, limit).unwrap(),
        (raw <= limit).then_some(expected)
    );
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn bounded_decode_rejects_every_truncated_encoding(tc: hegel::TestCase) {
    use hegel::generators as g;
    let artifact = encode(&fixture()).unwrap();
    let cut = tc.draw(g::integers::<usize>()) % artifact.len();
    assert!(super::candidate::decode_bounded(&artifact[..cut], usize::MAX).is_err());
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
            SudachiConfig::content_digests(&dictionary, &SudachiResources::directory(&resources))
                .unwrap();
        for split in [SudachiSplit::A, SudachiSplit::B, SudachiSplit::C] {
            for profile in [ExtractionProfile::Tokenize, ExtractionProfile::Morphology] {
                for segmentation in [SegmentationMode::Input, SegmentationMode::Analyzer] {
                    let plan = SudachiPlan::load(
                        SudachiConfig::new(
                            &dictionary,
                            SudachiResources::directory(&resources),
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

#[hegel::test(test_cases = 200, derandomize = true)]
fn streamed_sentence_decode_returns_the_selected_value_within_its_limit(tc: hegel::TestCase) {
    use hegel::generators as g;
    let count = 1 + tc.draw(g::integers::<usize>()) % 8;
    let mut sentences = Vec::new();
    for _ in 0..count {
        let size = tc.draw(g::integers::<usize>()) % 65;
        let character = char::from_u32(tc.draw(g::integers::<u32>())).unwrap_or('字');
        let text = character.to_string().repeat(size);
        let gap = tc.draw(g::integers::<u32>()) % 4;
        let width = 1 + tc.draw(g::integers::<u32>()) % 4;
        let surfaces = text
            .char_indices()
            .step_by(1 + gap as usize)
            .enumerate()
            .map(|(word, (start, character))| CandidateSurface {
                source_start: start as u32,
                source_end: (start + character.len_utf8()) as u32,
                word_start: word as u32 * (gap + width),
                word_end: word as u32 * (gap + width) + width,
            })
            .collect::<Vec<_>>();
        sentences.push(CandidateSentence { text, surfaces });
    }
    let selected = tc.draw(g::integers::<usize>()) % count;
    let expected = &sentences[selected];
    let required = expected.text.len() + 4 * expected.surfaces.len();
    let limit = match tc.draw(g::integers::<u8>().max_value(3)) {
        0 => required.saturating_sub(1),
        1 => required,
        2 => required + 1,
        _ => tc.draw(g::integers::<usize>()),
    };
    let encoded = encode(&sentences).unwrap();
    let actual =
        super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), selected, limit)
            .unwrap();
    assert_eq!(actual, (required <= limit).then(|| expected.clone()));
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn streamed_sentence_decode_rejects_truncated_frames(tc: hegel::TestCase) {
    use hegel::generators as g;
    let encoded = encode(&fixture()).unwrap();
    let cut = tc.draw(g::integers::<usize>()) % encoded.len();
    assert!(
        super::candidate::decode_sentence_bounded(&encoded[..cut], encoded.len(), 0, usize::MAX)
            .is_err()
    );
}

#[test]
fn streamed_sentence_decode_rejects_invalid_envelopes_and_indices() {
    let encoded = encode(&fixture()).unwrap();
    assert!(
        super::candidate::decode_sentence_bounded(
            &encoded[..],
            encoded.len(),
            fixture().len(),
            usize::MAX
        )
        .is_err()
    );
    for offset in [0, 8, 9, 17] {
        let mut invalid = encoded.clone();
        invalid[offset] ^= 1;
        assert!(
            super::candidate::decode_sentence_bounded(&invalid[..], invalid.len(), 0, usize::MAX)
                .is_err(),
            "offset {offset}"
        );
    }
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn streamed_sentence_decode_is_independent_of_read_partitioning(tc: hegel::TestCase) {
    use std::io::Read;
    struct Fragmented<'a> {
        bytes: &'a [u8],
        chunk: usize,
    }
    impl Read for Fragmented<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let len = self.chunk.min(output.len());
            self.bytes.read(&mut output[..len])
        }
    }
    let expected = fixture();
    let encoded = encode(&expected).unwrap();
    let selected = tc.draw(hegel::generators::integers::<usize>()) % expected.len();
    let chunk = 1 + tc.draw(hegel::generators::integers::<usize>()) % encoded.len();
    let actual = super::candidate::decode_sentence_bounded(
        Fragmented {
            bytes: &encoded,
            chunk,
        },
        encoded.len(),
        selected,
        usize::MAX,
    )
    .unwrap();
    assert_eq!(actual, Some(expected[selected].clone()));
}

fn frame(raw: &[u8]) -> Vec<u8> {
    let stored = zstd::bulk::compress(raw, 3).unwrap();
    let mut frame = b"LINGPRV1\0".to_vec();
    frame.extend_from_slice(&(raw.len() as u64).to_le_bytes());
    frame.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    frame.extend_from_slice(&stored);
    frame
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn streamed_sentence_decode_rejects_invalid_utf8_when_text_fits(tc: hegel::TestCase) {
    let byte = tc.draw(hegel::generators::integers::<u8>().min_value(128));
    let encoded = frame(&[1, 1, byte, 0]);
    let limit = tc.draw(hegel::generators::integers::<usize>().min_value(1));
    assert!(
        super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), 0, limit).is_err()
    );
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn oversized_text_returns_none_before_utf8_validation(tc: hegel::TestCase) {
    let byte = tc.draw(hegel::generators::integers::<u8>().min_value(128));
    let encoded = frame(&[1, 1, byte, 0]);
    assert_eq!(
        super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), 0, 0).unwrap(),
        None
    );
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn unselected_invalid_utf8_requires_full_verification(tc: hegel::TestCase) {
    let byte = tc.draw(hegel::generators::integers::<u8>().min_value(128));
    let encoded = frame(&[2, 0, 0, 1, byte, 0]);
    assert_eq!(
        super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), 0, 0).unwrap(),
        Some(CandidateSentence {
            text: String::new(),
            surfaces: vec![],
        })
    );
    assert!(decode(&encoded).is_err());
}

#[test]
fn streamed_sentence_decode_rejects_invalid_counts_ranges_and_trailing_payloads() {
    let cases: &[(&str, &[u8])] = &[
        ("noncanonical count", &[0x81, 0, 0, 0]),
        ("overflowing count", &[0xff; 10]),
        ("count exceeds fixed bound", &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        ("count exceeds payload", &[2, 0, 0]),
        ("truncated text", &[1, 2, b'a', 0]),
        (
            "surface count exceeds payload",
            &[1, 1, b'a', 2, 0, 1, 0, 1],
        ),
        ("zero source length", &[1, 1, b'a', 1, 0, 0, 0, 1]),
        ("source exceeds text", &[1, 1, b'a', 1, 0, 2, 0, 1]),
        ("zero word length", &[1, 1, b'a', 1, 0, 1, 0, 0]),
        (
            "source end overflow",
            &[1, 1, b'a', 1, 0xff, 0xff, 0xff, 0xff, 0x0f, 1, 0, 1],
        ),
        ("mid-codepoint start", &[1, 2, 0xc3, 0xa9, 1, 1, 1, 0, 1]),
        ("trailing raw bytes", &[1, 0, 0, 0]),
    ];
    for (label, raw) in cases {
        let encoded = frame(raw);
        assert!(
            super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), 0, usize::MAX,)
                .is_err(),
            "{label}"
        );
    }
}

#[hegel::test(test_cases = 40, derandomize = true)]
fn streamed_sentence_decode_rejects_excessive_zstd_history(tc: hegel::TestCase) {
    let expected = fixture();
    let mut encoded = encode(&expected).unwrap();
    let compressed = &mut encoded[25..];
    assert_eq!(compressed[4] & 0xe0, 0x20);
    let window_log = tc.draw(
        hegel::generators::integers::<u8>()
            .min_value(24)
            .max_value(27),
    );
    compressed[4] &= !0x20;
    compressed[5] = (window_log - 10) << 3;
    assert_eq!(decode(&encoded).unwrap(), expected);
    assert!(
        super::candidate::decode_sentence_bounded(&encoded[..], encoded.len(), 0, usize::MAX)
            .is_err()
    );
}
