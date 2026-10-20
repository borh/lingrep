//! Shared token projection for query protocol records.

use crate::protocol::query::{PublishedProjection, TokenAttributes, TokenEvidence};

pub(crate) fn token_evidence(
    tokens: &(impl lingrep_core::token_match::TokenSequence + ?Sized),
    range: std::ops::Range<usize>,
    projection: &PublishedProjection,
) -> Vec<TokenEvidence> {
    range
        .map(|index| TokenEvidence {
            index: index as u64,
            attributes: TokenAttributes::from_sequence_projected(tokens, index, projection),
        })
        .collect()
}
