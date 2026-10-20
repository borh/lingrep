use std::sync::Arc;

use lingrep_core::token_match::TokenInfo;
use thiserror::Error;

use super::{
    CaptureOrdinal, InspectionProjector, MatchInspection, ResolvedCapture, TokenInspection,
};
use crate::corpus_runtime::{BoundCorpusReader, CorpusReadError, CorpusReader};
use crate::snapshot::{CompletedSnapshot, MatchRef, PatternKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InspectionError {
    #[error("the displayed results belong to an older corpus cache")]
    StaleCorpus,
    #[error("pattern not found")]
    UnknownPattern,
    #[error("retained match not found")]
    UnknownReference,
    #[error("stored match span is invalid")]
    InvalidSpan,
    #[error("the corpus cache could not be decoded")]
    CacheCorruption,
    #[error("token is outside the visible match context")]
    UnknownToken,
}

pub struct SnapshotInspector {
    snapshot: Arc<CompletedSnapshot>,
    reader: BoundCorpusReader,
}

impl SnapshotInspector {
    pub fn bind(
        snapshot: Arc<CompletedSnapshot>,
        reader: CorpusReader,
    ) -> Result<Self, InspectionError> {
        let bound = reader
            .bind(&snapshot.corpus_identity)
            .ok_or(InspectionError::StaleCorpus)?;
        Ok(Self {
            snapshot,
            reader: bound,
        })
    }

    pub async fn match_view(
        &self,
        pattern: PatternKey,
        reference_ordinal: usize,
        context: usize,
    ) -> Result<MatchInspection, InspectionError> {
        let (tokens, reference, captures) = self.resolve_match(pattern, reference_ordinal).await?;
        Ok(InspectionProjector::project_match(
            &tokens,
            reference.span,
            &captures,
            context,
        ))
    }

    pub fn reader_clone(&self) -> BoundCorpusReader {
        self.reader.clone()
    }

    /// Project an already-decoded sentence for a borrowed match reference.
    /// Synchronous and I/O-free: the caller decodes the sentence (via
    /// `BoundCorpusReader::decode_sentences`) first. Removes the ordinal re-scan.
    pub fn project_reference(
        &self,
        tokens: &[TokenInfo],
        reference: &MatchRef,
        context: usize,
    ) -> Result<MatchInspection, InspectionError> {
        if reference.span.start > reference.span.end || reference.span.end > tokens.len() {
            return Err(InspectionError::InvalidSpan);
        }
        let captures = self.resolve_captures(reference)?;
        Ok(InspectionProjector::project_match(
            tokens,
            reference.span.clone(),
            &captures,
            context,
        ))
    }
    fn resolve_captures(
        &self,
        reference: &MatchRef,
    ) -> Result<Vec<ResolvedCapture>, InspectionError> {
        reference
            .captures
            .iter()
            .enumerate()
            .map(|(index, capture)| {
                let descriptor = self
                    .snapshot
                    .capture(capture.capture_key)
                    .ok_or(InspectionError::InvalidSpan)?;
                if descriptor.pattern_key != reference.pattern_key
                    || capture.span.start > capture.span.end
                    || capture.span.start < reference.span.start
                    || capture.span.end > reference.span.end
                {
                    return Err(InspectionError::InvalidSpan);
                }
                Ok(ResolvedCapture {
                    ordinal: CaptureOrdinal::new(index),
                    name: descriptor.name.clone(),
                    path: descriptor.path.clone(),
                    span: capture.span.clone(),
                })
            })
            .collect()
    }

    pub async fn token(
        &self,
        pattern: PatternKey,
        reference_ordinal: usize,
        token_index: usize,
        context: usize,
    ) -> Result<TokenInspection, InspectionError> {
        let (tokens, reference, _captures) = self.resolve_match(pattern, reference_ordinal).await?;
        let visible = reference.span.start.saturating_sub(context)
            ..reference.span.end.saturating_add(context).min(tokens.len());
        if !visible.contains(&token_index) {
            return Err(InspectionError::UnknownToken);
        }
        Ok(InspectionProjector::project_token(&tokens[token_index]))
    }

    async fn resolve_match(
        &self,
        pattern: PatternKey,
        reference_ordinal: usize,
    ) -> Result<(Vec<TokenInfo>, MatchRef, Vec<ResolvedCapture>), InspectionError> {
        self.snapshot
            .pattern(pattern)
            .ok_or(InspectionError::UnknownPattern)?;
        let reference = self
            .snapshot
            .match_ref(pattern, reference_ordinal)
            .ok_or(InspectionError::UnknownReference)?;
        let tokens = self
            .reader
            .sentence(reference.sentence_index)
            .await
            .map_err(map_cache_error)?
            .ok_or(InspectionError::InvalidSpan)?;
        if reference.span.start > reference.span.end || reference.span.end > tokens.len() {
            return Err(InspectionError::InvalidSpan);
        }
        let captures = self.resolve_captures(reference)?;
        Ok((tokens, reference.clone(), captures))
    }
}

fn map_cache_error(error: CorpusReadError) -> InspectionError {
    match error {
        CorpusReadError::Corruption(_) => InspectionError::CacheCorruption,
    }
}
