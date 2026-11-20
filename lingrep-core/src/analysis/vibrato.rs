use std::path::{Path, PathBuf};

use vibrato_rkyv::{Tokenizer, dictionary::LexiconEntries, tokenizer::worker::Worker};

use crate::token_match::{
    BoundProjection, TokenInfo, TokenSchema, tokenize_folded, tokenize_worker_bound,
};

use super::{
    AnalysisError, AnalyzedInput, AnalyzedSentence, AnalyzerDescriptor, AnalyzerFamily,
    AnalyzerPlan, AnalyzerSession, ExtractionProfile, JapaneseSegmentationPolicy,
    NormalizationPolicy, SegmentationMode, SurfaceToken, TEXT_KEY_REVISION, TextUnit, TextUnitKind,
    ValidatedAnalysisBatch, ViewFold, analyzer::sealed, declared_token_schema,
};

/// Immutable Vibrato configuration and the scalar schema it can produce.
#[derive(Clone)]
pub struct VibratoPlan {
    tokenizer: Tokenizer,
    fold: ViewFold,
    schema: TokenSchema,
    segmentation: SegmentationMode,
    descriptor: AnalyzerDescriptor,
}

impl VibratoPlan {
    /// # Errors
    ///
    /// As [`Self::configured`].
    pub fn new(tokenizer: Tokenizer, dictionary: &Path) -> Result<Self, LexiconEnumerationError> {
        Self::with_mode(tokenizer, dictionary, SegmentationMode::Input)
    }

    /// # Errors
    ///
    /// As [`Self::configured`].
    pub fn with_mode(
        tokenizer: Tokenizer,
        dictionary: &Path,
        segmentation: SegmentationMode,
    ) -> Result<Self, LexiconEnumerationError> {
        Self::configured(
            tokenizer,
            dictionary,
            [0; 32],
            ExtractionProfile::Morphology,
            segmentation,
            NormalizationPolicy::None,
        )
    }

    /// Builds a plan that analyzes the [`ViewFold`] view of each sentence,
    /// protecting the surfaces of the tokenizer's system and user lexicons.
    ///
    /// `dictionary` is the path `tokenizer` was loaded from; it names the
    /// dictionary in the error.
    ///
    /// # Errors
    ///
    /// Returns [`LexiconEnumerationError`] if a lexicon of the tokenizer
    /// cannot be enumerated.
    pub fn configured(
        tokenizer: Tokenizer,
        dictionary: &Path,
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Result<Self, LexiconEnumerationError> {
        let schema = declared_token_schema(AnalyzerFamily::Vibrato, profile)
            .expect("Vibrato declares a schema for every profile");
        let descriptor =
            Self::descriptor_for(dictionary_digest, profile, segmentation, normalization);
        let fold = dictionary_fold(&tokenizer, dictionary)?;
        Ok(Self {
            tokenizer,
            fold,
            schema,
            segmentation,
            descriptor,
        })
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
            digest(&[b"lingrep.vibrato-adapter/v2", &ViewFold::rules_identity()]),
            dictionary_digest,
            digest(&[b"lingrep.vibrato-projection/v2", TEXT_KEY_REVISION]),
            *blake3::hash(b"lingrep.vibrato-alignment/v2").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-components/tokenizer").as_bytes(),
            *blake3::hash(b"lingrep.vibrato-output/v2").as_bytes(),
            schema.digest(),
        )
        .expect("the built-in Vibrato descriptor is valid")
    }

    #[must_use]
    pub const fn descriptor(&self) -> &AnalyzerDescriptor {
        &self.descriptor
    }

    /// A plan over the same tokenizer and protected-surface table with the
    /// descriptor [`Self::configured`] would build from these settings.
    #[must_use]
    pub fn reconfigured(
        &self,
        dictionary_digest: [u8; 32],
        profile: ExtractionProfile,
        segmentation: SegmentationMode,
        normalization: NormalizationPolicy,
    ) -> Self {
        Self {
            tokenizer: self.tokenizer.clone(),
            fold: self.fold.clone(),
            schema: declared_token_schema(AnalyzerFamily::Vibrato, profile)
                .expect("Vibrato declares a schema for every profile"),
            segmentation,
            descriptor: Self::descriptor_for(
                dictionary_digest,
                profile,
                segmentation,
                normalization,
            ),
        }
    }

    /// Tokenizes one sentence, as a session of this plan analyzes it, with
    /// every attribute.
    #[must_use]
    pub fn tokenize(&self, text: &str) -> Vec<TokenInfo> {
        tokenize_folded(text, &self.tokenizer, &self.fold)
    }
}

fn digest(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

/// A Vibrato dictionary whose lexicon cannot be enumerated, so the
/// protected-surface table of its [`ViewFold`] cannot be built.
#[derive(Debug, thiserror::Error)]
#[error("cannot enumerate the lexicon of Vibrato dictionary {}", .dictionary.display())]
pub struct LexiconEnumerationError {
    pub dictionary: PathBuf,
    #[source]
    pub source: vibrato_rkyv::errors::VibratoError,
}

fn dictionary_fold(
    tokenizer: &Tokenizer,
    dictionary: &Path,
) -> Result<ViewFold, LexiconEnumerationError> {
    fold_from_lexicons(
        dictionary,
        std::iter::once(tokenizer.system_lexicon())
            .chain(tokenizer.user_lexicon())
            .map(|lexicon| lexicon.entries()),
    )
}

fn fold_from_lexicons<'a>(
    dictionary: &Path,
    lexicons: impl IntoIterator<Item = vibrato_rkyv::errors::Result<LexiconEntries<'a>>>,
) -> Result<ViewFold, LexiconEnumerationError> {
    let lexicons = lexicons
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| LexiconEnumerationError {
            dictionary: dictionary.to_owned(),
            source,
        })?;
    Ok(ViewFold::from_dictionary_surfaces(
        lexicons
            .iter()
            .flat_map(|entries| entries.iter().map(|entry| entry.surface())),
    ))
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
            fold: self.fold.clone(),
            schema: self.schema.clone(),
            segmentation: self.segmentation,
        }
    }
}

/// Lane-owned Vibrato scratch state reused across bounded calls.
pub struct VibratoSession {
    worker: Worker,
    fold: ViewFold,
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
                    tokenize_worker_bound(&mut self.worker, sentence_text, &self.fold, projection)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_lexicon_is_an_error_naming_the_dictionary_and_its_cause() {
        let cause = || {
            vibrato_rkyv::errors::VibratoError::StdIo(std::io::Error::other("trie is truncated"))
        };
        let error = fold_from_lexicons(std::path::Path::new("/dict/unidic.dic"), [Err(cause())])
            .expect_err("an unreadable lexicon builds no fold");
        assert_eq!(error.dictionary, std::path::Path::new("/dict/unidic.dic"));
        assert_eq!(
            error.to_string(),
            "cannot enumerate the lexicon of Vibrato dictionary /dict/unidic.dic"
        );
        assert_eq!(
            std::error::Error::source(&error).map(ToString::to_string),
            Some(cause().to_string())
        );
    }
}
