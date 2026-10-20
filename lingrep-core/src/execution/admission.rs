//! Which documents a run presents to matching.
//!
//! Scope reaches the runner as a resolved *set*, not as a clause: the clause
//! language is evaluated once per document at query start, and matching should
//! not carry a parser or a metadata reader into its inner loop.

use roaring::RoaringTreemap;

use crate::execution::SentenceLocation;

/// The documents a run admits.
///
/// [`RoaringTreemap`], not `RoaringBitmap`: [`DocumentId`](crate::corpus::DocumentId) wraps a `u64` and
/// `RoaringBitmap` is 32-bit, so the narrower type would silently fold every
/// document id above `u32::MAX` onto a low one.
#[derive(Clone, Debug, Default)]
pub enum SentenceAdmission {
    /// No scope clause: every sentence is presented to matching.
    #[default]
    All,
    /// Only sentences whose document is in the set.
    Documents(RoaringTreemap),
    /// Only sentences whose corpus ordinal is in the set.
    ///
    /// Meaningful only where ordinals are artifact identity. The direct path
    /// compacts ordinals as it reads, so a precomputed sentence set would name
    /// the wrong sentences there; it evaluates the clause inline instead.
    ///
    /// Retained beside [`Self::Documents`] rather than subsuming it: a clause
    /// naming no section is one lookup per *document* instead of one per
    /// sentence, and that is the path every run took before section scope
    /// existed.
    Sentences(RoaringTreemap),
}

impl SentenceAdmission {
    /// Whether a sentence reaches matching.
    ///
    /// Takes the whole location rather than a document id: the two restricted
    /// variants ask different questions of it, and a caller that passed only
    /// one field could not serve both.
    #[must_use]
    pub fn admits(&self, location: &SentenceLocation) -> bool {
        match self {
            Self::All => true,
            Self::Documents(documents) => documents.contains(location.document_id.get()),
            Self::Sentences(sentences) => sentences.contains(location.corpus_sentence_ordinal),
        }
    }

    /// Whether this admission can exclude anything.
    ///
    /// Lets a caller skip per-sentence work entirely on an unscoped run.
    #[must_use]
    pub const fn is_unrestricted(&self) -> bool {
        matches!(self, Self::All)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::corpus::DocumentId;

    /// Every field differs, so a variant reading the wrong one is visible.
    fn location() -> SentenceLocation {
        SentenceLocation {
            source_metadata: None,
            corpus_sentence_ordinal: 3,
            document_id: DocumentId::new(7),
            document_ordinal: 11,
            document_sentence_ordinal: 1,
            source_name: Arc::from("test"),
        }
    }

    #[test]
    fn all_admits_everything() {
        assert!(SentenceAdmission::All.admits(&location()));
        assert!(SentenceAdmission::All.is_unrestricted());
    }

    #[test]
    fn documents_reads_the_document_id() {
        let admission = SentenceAdmission::Documents(RoaringTreemap::from_iter([7_u64]));
        assert!(admission.admits(&location()));
        assert!(!admission.is_unrestricted());
        // 3 is the corpus ordinal and 1 the in-document ordinal; neither may
        // stand in for the document id.
        for wrong in [3_u64, 1, 11] {
            assert!(
                !SentenceAdmission::Documents(RoaringTreemap::from_iter([wrong]))
                    .admits(&location()),
                "document admission matched on {wrong}"
            );
        }
    }

    #[test]
    fn sentences_reads_the_corpus_ordinal() {
        let admission = SentenceAdmission::Sentences(RoaringTreemap::from_iter([3_u64]));
        assert!(admission.admits(&location()));
        assert!(!admission.is_unrestricted());
        // The in-document ordinal is the tempting substitute and it is wrong:
        // it repeats across documents, so it would admit sentences from every
        // one of them.
        for wrong in [1_u64, 7, 11] {
            assert!(
                !SentenceAdmission::Sentences(RoaringTreemap::from_iter([wrong]))
                    .admits(&location()),
                "sentence admission matched on {wrong}"
            );
        }
    }
}
