use std::collections::BTreeMap;

use crate::{
    analysis::{AnalyzerSession, SegmentationMode},
    token_match::BoundProjection,
};

use super::{
    ExecutionBatch, ExecutionError, ExecutionLimits, ExecutionSentence, SentenceBatchSource,
};

/// Converts bounded admitted text batches into schema-bound native analysis.
pub struct NativeAnalysisSource<S, A> {
    source: S,
    analyzer: A,
    projection: BoundProjection,
    segmentation: SegmentationMode,
    next_corpus_sentence: u64,
    next_document_sentence: BTreeMap<u64, u64>,
    failed: bool,
}

impl<S, A> NativeAnalysisSource<S, A> {
    #[must_use]
    pub fn new(
        source: S,
        analyzer: A,
        projection: BoundProjection,
        segmentation: SegmentationMode,
    ) -> Self {
        Self {
            source,
            analyzer,
            projection,
            segmentation,
            next_corpus_sentence: 0,
            next_document_sentence: BTreeMap::new(),
            failed: false,
        }
    }

    pub fn into_parts(self) -> (S, A) {
        (self.source, self.analyzer)
    }
}

impl<S, A> SentenceBatchSource for NativeAnalysisSource<S, A>
where
    S: SentenceBatchSource,
    A: AnalyzerSession,
{
    fn next_batch(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        if self.failed {
            return Err(ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: "native analyzer source previously failed".to_owned(),
            });
        }
        let result = self.next_batch_inner(limits);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn wants_lookahead(&self) -> bool {
        self.source.wants_lookahead()
    }
}

impl<S, A> NativeAnalysisSource<S, A>
where
    S: SentenceBatchSource,
    A: AnalyzerSession,
{
    fn next_batch_inner(
        &mut self,
        limits: &ExecutionLimits,
    ) -> Result<Option<ExecutionBatch>, ExecutionError> {
        limits.validate()?;
        let Some(batch) = self.source.next_batch(limits)? else {
            self.analyzer
                .finish()
                .map_err(|error| ExecutionError::Source {
                    ordinal: self.next_corpus_sentence,
                    message: error.to_string(),
                })?;
            return Ok(None);
        };
        let ExecutionBatch::Text(sentences) = batch else {
            return Err(ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: "native analyzer source requires admitted text input".to_owned(),
            });
        };
        let locations = sentences
            .iter()
            .map(|sentence| sentence.location.clone())
            .collect::<Vec<_>>();
        let inputs = sentences
            .into_iter()
            .map(|sentence| sentence.value)
            .collect::<Vec<_>>();
        let analyzed = self
            .analyzer
            .analyze_bounded_batch(inputs, &self.projection)
            .map_err(|error| ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: error.to_string(),
            })?;
        if analyzed.inputs().len() != locations.len() {
            return Err(ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: "native analyzer changed admitted input cardinality".to_owned(),
            });
        }
        let output_count = analyzed
            .inputs()
            .iter()
            .map(|input| input.sentences().len())
            .sum::<usize>();
        if output_count > limits.max_stage_sentences {
            return Err(ExecutionError::Source {
                ordinal: self.next_corpus_sentence,
                message: format!(
                    "native analyzer produced {output_count} sentences; stage limit is {}",
                    limits.max_stage_sentences
                ),
            });
        }
        let mut output = Vec::with_capacity(output_count);
        let mut charges = Vec::with_capacity(output_count);
        let mut aggregate = 0_usize;
        let mut candidate_ordinal = self.next_corpus_sentence;
        let mut document_ordinals = self.next_document_sentence.clone();
        for (analyzed_input, input_location) in
            analyzed.into_inputs().into_vec().into_iter().zip(locations)
        {
            for analyzed_sentence in analyzed_input.into_parts().1.into_vec() {
                let sentence_bytes = analyzed_sentence.source_range().as_usize().len();
                if sentence_bytes > limits.max_sentence_bytes {
                    return Err(ExecutionError::SentenceTooLarge {
                        ordinal: candidate_ordinal,
                        actual: sentence_bytes,
                        limit: limits.max_sentence_bytes,
                        context: format!(" in {}", input_location.source_name),
                    });
                }
                let words = analyzed_sentence.into_words();
                let charge = words
                    .resident_bytes()
                    .map_err(|_| ExecutionError::ResourceAccounting)?;
                aggregate = aggregate
                    .checked_add(charge)
                    .ok_or(ExecutionError::ResourceAccounting)?;
                if charge > limits.max_tokenized_bytes || aggregate > limits.max_tokenized_bytes {
                    return Err(ExecutionError::TokenizedSentenceTooLarge {
                        ordinal: candidate_ordinal,
                        actual: aggregate.max(charge),
                        limit: limits.max_tokenized_bytes,
                    });
                }
                let location = match self.segmentation {
                    SegmentationMode::Input => input_location.clone(),
                    SegmentationMode::Analyzer => {
                        let document_sentence = document_ordinals
                            .entry(input_location.document_id.get())
                            .or_default();
                        let location = super::SentenceLocation {
                            source_metadata: input_location.source_metadata.clone(),
                            corpus_sentence_ordinal: candidate_ordinal,
                            document_id: input_location.document_id,
                            document_ordinal: input_location.document_ordinal,
                            document_sentence_ordinal: *document_sentence,
                            source_name: input_location.source_name.clone(),
                        };
                        *document_sentence = document_sentence
                            .checked_add(1)
                            .ok_or(ExecutionError::ResourceAccounting)?;
                        location
                    }
                };
                output.push(ExecutionSentence {
                    location,
                    value: super::BoundSentenceInput::new(
                        words,
                        super::EnhancedGraphInput::NotRequested,
                    ),
                });
                charges.push(charge);
                candidate_ordinal = candidate_ordinal
                    .checked_add(1)
                    .ok_or(ExecutionError::ResourceAccounting)?;
            }
        }
        self.next_corpus_sentence = candidate_ordinal;
        self.next_document_sentence = document_ordinals;
        Ok(Some(ExecutionBatch::bound_tokenized_with_charges(
            output, charges,
        )))
    }
}
