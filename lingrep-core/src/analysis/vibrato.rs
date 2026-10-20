use vibrato_rkyv::{Tokenizer, tokenizer::worker::Worker};

use crate::token_match::{BoundProjection, TokenSchema, tokenize_worker_bound};

use super::{
    AnalysisError, AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor, AnalyzerFamily,
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, JapaneseSegmentationPolicy,
    NormalizationPolicy, SegmentationMode, SurfaceToken, TextUnit, TextUnitKind,
    ValidatedAnalysisBatch, analyzer::sealed, declared_token_schema,
};

/// Immutable Vibrato configuration and the scalar schema it can produce.
#[derive(Clone)]
pub struct VibratoPlan {
    tokenizer: Tokenizer,
    schema: TokenSchema,
    segmentation: SegmentationMode,
    descriptor: AnalyzerDescriptor,
}

impl VibratoPlan {
    #[must_use]
    pub fn new(tokenizer: Tokenizer) -> Self {
        Self::with_mode(tokenizer, SegmentationMode::Input)
    }

    #[must_use]
    pub fn with_mode(tokenizer: Tokenizer, segmentation: SegmentationMode) -> Self {
        Self::configured(
            tokenizer,
            [0; 32],
            ExtractionProfile::Morphology,
            segmentation,
            NormalizationPolicy::None,
        )
    }

    #[must_use]
    pub fn configured(
        tokenizer: Tokenizer,
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Self {
        let schema = declared_token_schema(AnalyzerFamily::Vibrato, profile)
            .expect("Vibrato declares a schema for every profile");
        let descriptor =
            Self::descriptor_for(dictionary_digest, profile, segmentation, normalization);
        Self {
            tokenizer,
            schema,
            segmentation,
            descriptor,
        }
    }

    /// Builds the descriptor a [`VibratoPlan`] with these settings would carry,
    /// without loading a tokenizer.
    ///
    /// A caller that has to name a content-addressed artifact needs the
    /// analyzer's semantic identity *before* it decides whether to build, and
    /// loading a ~1.3 GB dictionary to answer "does this already exist" would
    /// invert the cost of the question. The descriptor is a pure function of
    /// these four inputs, so this is the same value [`VibratoPlan::configured`]
    /// stores. The adapter, projection, and output tags below own semantic
    /// versioning for built-in Vibrato tokenization: an output-affecting change
    /// must bump the applicable tag so prepared-corpus identity moves with it.
    #[must_use]
    pub fn descriptor_for(
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> AnalyzerDescriptor {
        let schema = declared_token_schema(AnalyzerFamily::Vibrato, profile)
            .expect("Vibrato declares a schema for every profile");
        AnalyzerDescriptor::new(
            AnalyzerFamily::Vibrato,
            profile,
            segmentation,
            normalization,
            "ja",
            match segmentation {
                SegmentationMode::Input => "input-v1",
                SegmentationMode::Analyzer => "ja-delimiter-v1",
            },
            *blake3::hash(b"lingrep.vibrato-adapter/v1").as_bytes(),
            dictionary_digest,
            *blake3::hash(b"lingrep.vibrato-projection/v1").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-alignment/v1").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-components/tokenizer").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-output/v1").as_bytes(),
            schema.digest(),
        )
        .expect("the built-in Vibrato descriptor is valid")
    }

    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }
}

impl sealed::Plan for VibratoPlan {}

impl AnalyzerPlan for VibratoPlan {
    type Session = VibratoSession;

    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn create_session(&self) -> Self::Session {
        VibratoSession {
            worker: self.tokenizer.new_worker(),
            schema: self.schema.clone(),
            segmentation: self.segmentation,
        }
    }
}

/// Lane-owned Vibrato scratch state reused across bounded calls.
pub struct VibratoSession {
    worker: Worker,
    schema: TokenSchema,
    segmentation: SegmentationMode,
}

impl sealed::Session for VibratoSession {}

impl AnalyzerSession for VibratoSession {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        if projection.physical_attributes().iter().any(Option::is_none) {
            return Err(AnalysisError::UnsupportedProjection);
        }

        let mut analyzed_inputs = Vec::with_capacity(inputs.len());
        for input in inputs {
            let expected_kind = match self.segmentation {
                SegmentationMode::Input => TextUnitKind::Sentence,
                SegmentationMode::Analyzer => TextUnitKind::Segment,
            };
            if input.kind() != expected_kind {
                return Err(AnalysisError::WrongInputKind);
            }
            let sentence_ranges = match self.segmentation {
                SegmentationMode::Input => vec![
                    input
                        .source_range(0, input.len())
                        .map_err(|_| AnalysisError::InvalidSourceRange)?,
                ]
                .into_boxed_slice(),
                SegmentationMode::Analyzer => {
                    JapaneseSegmentationPolicy::V1.sentence_ranges(&input)?
                }
            };
            let mut sentences = Vec::with_capacity(sentence_ranges.len());
            for (ordinal, sentence_range) in sentence_ranges.into_vec().into_iter().enumerate() {
                let sentence_text = input
                    .slice(sentence_range)
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let (words, token_ranges) =
                    tokenize_worker_bound(&mut self.worker, sentence_text, projection)
                        .map_err(|_| AnalysisError::ResourceAccounting)?;
                let sentence_start = sentence_range.start() as usize;
                let mut surface_tokens = Vec::with_capacity(token_ranges.len());
                for (index, range) in token_ranges.into_iter().enumerate() {
                    let start = sentence_start
                        .checked_add(range.start)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    let end = sentence_start
                        .checked_add(range.end)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    let source_range = input
                        .source_range(start, end)
                        .map_err(|_| AnalysisError::InvalidSourceRange)?;
                    let word_start =
                        u32::try_from(index).map_err(|_| AnalysisError::ResourceAccounting)?;
                    let word_end = word_start
                        .checked_add(1)
                        .ok_or(AnalysisError::ResourceAccounting)?;
                    surface_tokens.push(SurfaceToken::new(source_range, word_start, word_end));
                }
                sentences.push(AnalyzedSentence::new(
                    u32::try_from(ordinal).map_err(|_| AnalysisError::ResourceAccounting)?,
                    sentence_range,
                    surface_tokens.into_boxed_slice(),
                    words,
                )?);
            }
            analyzed_inputs.push(AnalyzedInput::new(input, sentences, self.segmentation)?);
        }
        ValidatedAnalysisBatch::new(analyzed_inputs)
    }
}
