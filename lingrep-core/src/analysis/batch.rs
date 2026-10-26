use crate::token_match::BoundTokenSentence;

use super::{AnalysisError, SegmentationMode, SourceRange, TextUnit, TextUnitKind};

/// One exact source token and the ordered match words it covers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceToken {
    source_range: SourceRange,
    word_start: u32,
    word_end: u32,
}

impl SurfaceToken {
    pub(crate) const fn new(source_range: SourceRange, word_start: u32, word_end: u32) -> Self {
        Self {
            source_range,
            word_start,
            word_end,
        }
    }

    #[must_use]
    pub const fn source_range(&self) -> SourceRange {
        self.source_range
    }

    #[must_use]
    pub const fn word_range(&self) -> std::ops::Range<u32> {
        self.word_start..self.word_end
    }
}

/// One validated analyzer sentence relative to its owning admitted input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzedSentence {
    analyzer_sentence_ordinal: u32,
    source_range: SourceRange,
    pub(super) surface_tokens: Box<[SurfaceToken]>,
    pub(super) words: BoundTokenSentence,
}

impl AnalyzedSentence {
    pub(crate) fn new(
        analyzer_sentence_ordinal: u32,
        source_range: SourceRange,
        surface_tokens: Box<[SurfaceToken]>,
        words: BoundTokenSentence,
    ) -> Result<Self, AnalysisError> {
        let word_count =
            u32::try_from(words.len()).map_err(|_| AnalysisError::ResourceAccounting)?;
        let mut prior_source_end = source_range.start();
        let mut prior_word_end = 0_u32;
        for surface in &surface_tokens {
            if surface.source_range.start() < prior_source_end
                || surface.word_start != prior_word_end
                || surface.word_end <= surface.word_start
            {
                return Err(AnalysisError::InvalidAlignment);
            }
            prior_source_end = surface.source_range.end();
            prior_word_end = surface.word_end;
        }
        if prior_word_end != word_count {
            return Err(AnalysisError::InvalidAlignment);
        }
        Ok(Self {
            analyzer_sentence_ordinal,
            source_range,
            surface_tokens,
            words,
        })
    }

    #[must_use]
    pub const fn analyzer_sentence_ordinal(&self) -> u32 {
        self.analyzer_sentence_ordinal
    }

    #[must_use]
    pub const fn source_range(&self) -> SourceRange {
        self.source_range
    }

    #[must_use]
    pub fn surface_tokens(&self) -> &[SurfaceToken] {
        &self.surface_tokens
    }

    #[must_use]
    pub fn words(&self) -> &BoundTokenSentence {
        &self.words
    }

    pub fn into_words(self) -> BoundTokenSentence {
        self.words
    }
}

/// One admitted text unit and every ordered sentence produced from it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyzedInput {
    input: TextUnit,
    sentences: Box<[AnalyzedSentence]>,
}

impl AnalyzedInput {
    pub(crate) fn new(
        input: TextUnit,
        sentences: Vec<AnalyzedSentence>,
        segmentation: SegmentationMode,
    ) -> Result<Self, AnalysisError> {
        match segmentation {
            SegmentationMode::Input => {
                if input.kind() != TextUnitKind::Sentence {
                    return Err(AnalysisError::WrongInputKind);
                }
                if sentences.len() != 1 {
                    return Err(AnalysisError::InvalidAlignment);
                }
                let expected = input
                    .source_range(0, input.len())
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                if sentences[0].analyzer_sentence_ordinal != 0
                    || sentences[0].source_range != expected
                {
                    return Err(AnalysisError::InvalidAlignment);
                }
            }
            SegmentationMode::Analyzer if input.kind() != TextUnitKind::Segment => {
                return Err(AnalysisError::WrongInputKind);
            }
            SegmentationMode::Analyzer => {}
        }

        let mut prior_sentence_end = 0_u32;
        for (expected_ordinal, sentence) in sentences.iter().enumerate() {
            if sentence.analyzer_sentence_ordinal
                != u32::try_from(expected_ordinal).map_err(|_| AnalysisError::ResourceAccounting)?
                || sentence.source_range.start() < prior_sentence_end
            {
                return Err(AnalysisError::InvalidAlignment);
            }
            if input.slice(sentence.source_range).is_err() {
                return Err(AnalysisError::InvalidSourceRange);
            }
            for surface in &sentence.surface_tokens {
                if surface.source_range.start() < sentence.source_range.start()
                    || surface.source_range.end() > sentence.source_range.end()
                    || input.slice(surface.source_range).is_err()
                {
                    return Err(AnalysisError::InvalidAlignment);
                }
            }
            prior_sentence_end = sentence.source_range.end();
        }
        let sentences = sentences
            .into_iter()
            .map(|mut sentence| {
                let text = input
                    .slice(sentence.source_range)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let origin = sentence.source_range.start();
                let offsets = crate::token_match::CharSpan::from_surface_ranges(
                    text,
                    sentence.words.len(),
                    sentence.surface_tokens.iter().map(|surface| {
                        (
                            (surface.source_range.start() - origin) as usize
                                ..(surface.source_range.end() - origin) as usize,
                            surface.word_start as usize..surface.word_end as usize,
                        )
                    }),
                )
                .ok_or(AnalysisError::InvalidAlignment)?;
                sentence.words = sentence
                    .words
                    .with_char_offsets(Some(std::sync::Arc::new(offsets)));
                Ok(sentence)
            })
            .collect::<Result<Vec<_>, AnalysisError>>()?;
        Ok(Self {
            input,
            sentences: sentences.into_boxed_slice(),
        })
    }

    #[must_use]
    pub fn input(&self) -> &TextUnit {
        &self.input
    }

    #[must_use]
    pub fn sentences(&self) -> &[AnalyzedSentence] {
        &self.sentences
    }

    pub fn into_parts(self) -> (TextUnit, Box<[AnalyzedSentence]>) {
        (self.input, self.sentences)
    }
}

/// A completely validated, input-ordered native-analysis batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedAnalysisBatch {
    inputs: Box<[AnalyzedInput]>,
}

impl ValidatedAnalysisBatch {
    pub(crate) fn new(inputs: Vec<AnalyzedInput>) -> Result<Self, AnalysisError> {
        if inputs
            .windows(2)
            .any(|pair| pair[0].input.id() >= pair[1].input.id())
        {
            return Err(AnalysisError::InvalidAlignment);
        }
        Ok(Self {
            inputs: inputs.into_boxed_slice(),
        })
    }

    #[must_use]
    pub fn inputs(&self) -> &[AnalyzedInput] {
        &self.inputs
    }

    pub fn into_inputs(self) -> Box<[AnalyzedInput]> {
        self.inputs
    }
}

#[cfg(test)]
mod tests {
    use compact_str::CompactString;

    use crate::token_match::{BoundTokenSentence, TokenAttributeIndex, TokenSchema};

    use super::*;
    use crate::analysis::{InputUnitId, NormalizationPolicy};

    fn words(forms: &[&str]) -> BoundTokenSentence {
        let projection = TokenSchema::unidic_morphology()
            .bind_projection([TokenAttributeIndex::Orth])
            .unwrap();
        BoundTokenSentence::from_projected_values(
            &projection,
            forms.len(),
            forms
                .iter()
                .map(|form| Some(CompactString::from(*form)))
                .collect(),
        )
        .unwrap()
    }

    fn segment(id: u64, text: &str) -> TextUnit {
        TextUnit::admit(
            InputUnitId::new(id),
            TextUnitKind::Segment,
            text,
            NormalizationPolicy::None,
        )
        .unwrap()
    }

    fn sentence(input: &TextUnit, ordinal: u32, start: usize, end: usize) -> AnalyzedSentence {
        let range = input.source_range(start, end).unwrap();
        let surface = SurfaceToken::new(range, 0, 1);
        AnalyzedSentence::new(ordinal, range, Box::new([surface]), words(&["x"])).unwrap()
    }

    #[test]
    fn segment_owns_text_once_and_accepts_two_or_zero_sentences() {
        let input = segment(1, "一。二。");
        let first = sentence(&input, 0, 0, 6);
        let second = sentence(&input, 1, 6, 12);
        let grouped = AnalyzedInput::new(
            input.clone(),
            vec![first, second],
            SegmentationMode::Analyzer,
        )
        .unwrap();
        assert_eq!(grouped.input(), &input);
        assert_eq!(grouped.sentences().len(), 2);
        use crate::token_match::{CharSpan, TokenSequence};
        for sentence in grouped.sentences() {
            assert_eq!(sentence.words().char_span(0), Some(CharSpan::new(0, 2)));
        }
        let (owned, sentences) = grouped.into_parts();
        assert_eq!(owned, input);
        assert_eq!(sentences.len(), 2);

        let empty = segment(2, "");
        assert!(AnalyzedInput::new(empty, Vec::new(), SegmentationMode::Analyzer).is_ok());
    }

    #[test]
    fn normalized_analysis_offsets_cover_shared_surface_words() {
        use crate::token_match::{CharSpan, TokenSequence};
        let input = TextUnit::admit(
            InputUnitId::new(0),
            TextUnitKind::Sentence,
            "は\u{3099} 😀",
            NormalizationPolicy::Nfc,
        )
        .unwrap();
        let range = input.source_range(0, input.len()).unwrap();
        let sentence = AnalyzedSentence::new(
            0,
            range,
            Box::new([
                SurfaceToken::new(input.source_range(0, 3).unwrap(), 0, 1),
                SurfaceToken::new(input.source_range(4, 8).unwrap(), 1, 3),
            ]),
            words(&["ば", "x", "y"]),
        )
        .unwrap();
        let analyzed = AnalyzedInput::new(input, vec![sentence], SegmentationMode::Input).unwrap();
        let words = analyzed.sentences()[0].words();
        assert_eq!(words.char_span(0), Some(CharSpan::new(0, 1)));
        assert_eq!(words.char_span(1), Some(CharSpan::new(2, 3)));
        assert_eq!(words.char_span(2), Some(CharSpan::new(2, 3)));
    }

    #[test]
    fn grouped_validation_rejects_duplicate_ids_and_wrong_ordinals() {
        let first_input = segment(4, "一。");
        let second_input = segment(4, "二。");
        let first = AnalyzedInput::new(
            first_input.clone(),
            vec![sentence(&first_input, 0, 0, 6)],
            SegmentationMode::Analyzer,
        )
        .unwrap();
        let second = AnalyzedInput::new(
            second_input.clone(),
            vec![sentence(&second_input, 0, 0, 6)],
            SegmentationMode::Analyzer,
        )
        .unwrap();
        assert_eq!(
            ValidatedAnalysisBatch::new(vec![first, second]),
            Err(AnalysisError::InvalidAlignment)
        );

        let input = segment(5, "一。");
        assert_eq!(
            AnalyzedInput::new(
                input.clone(),
                vec![sentence(&input, 1, 0, 6)],
                SegmentationMode::Analyzer,
            ),
            Err(AnalysisError::InvalidAlignment)
        );
    }

    #[test]
    fn grouped_validation_rejects_ranges_outside_input_or_sentence() {
        let input = segment(6, "一。二。");
        let other = segment(7, "一。二。三。");
        let outside_input = AnalyzedSentence::new(
            0,
            other.source_range(0, other.len()).unwrap(),
            Box::new([]),
            words(&[]),
        )
        .unwrap();
        assert_eq!(
            AnalyzedInput::new(
                input.clone(),
                vec![outside_input],
                SegmentationMode::Analyzer,
            ),
            Err(AnalysisError::InvalidSourceRange)
        );

        let sentence_range = input.source_range(0, 6).unwrap();
        let later_surface = SurfaceToken::new(input.source_range(6, 12).unwrap(), 0, 1);
        let escaped = AnalyzedSentence::new(
            0,
            sentence_range,
            Box::new([later_surface]),
            words(&["二。"]),
        )
        .unwrap();
        assert_eq!(
            AnalyzedInput::new(input, vec![escaped], SegmentationMode::Analyzer),
            Err(AnalysisError::InvalidAlignment)
        );
    }
}
