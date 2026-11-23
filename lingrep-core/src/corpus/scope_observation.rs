//! Counts at the document and sentence grains for a scoped reader.

use std::io;

use crate::scope::{ExclusionReason, Verdict};

/// A scoped direct run's admission counts, at both grains.
///
/// Every field is a count of events the reader observed. On the direct path
/// the denominator is not known until the stream ends, so these are read at
/// completion and never published as a running fraction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeCounts {
    pub documents_admitted: u64,
    pub documents_excluded_by_predicate: u64,
    pub documents_excluded_missing_field: u64,
    pub documents_excluded_shape_mismatch: u64,
    pub sentences_admitted: u64,
    pub sentences_excluded_by_predicate: u64,
    pub sentences_excluded_missing_field: u64,
    pub sentences_excluded_shape_mismatch: u64,
}

impl ScopeCounts {
    pub(crate) fn record_document(&mut self, verdict: Verdict) -> io::Result<()> {
        increment(match verdict {
            Verdict::Admitted => &mut self.documents_admitted,
            Verdict::Excluded(ExclusionReason::Predicate) => {
                &mut self.documents_excluded_by_predicate
            }
            Verdict::Excluded(ExclusionReason::MissingField) => {
                &mut self.documents_excluded_missing_field
            }
            Verdict::Excluded(ExclusionReason::ShapeMismatch) => {
                &mut self.documents_excluded_shape_mismatch
            }
        })
    }

    pub(crate) fn record_admitted_sentence(&mut self) -> io::Result<()> {
        increment(&mut self.sentences_admitted)
    }

    pub(crate) fn record_excluded_sentence(&mut self, reason: ExclusionReason) -> io::Result<()> {
        increment(match reason {
            ExclusionReason::Predicate => &mut self.sentences_excluded_by_predicate,
            ExclusionReason::MissingField => &mut self.sentences_excluded_missing_field,
            ExclusionReason::ShapeMismatch => &mut self.sentences_excluded_shape_mismatch,
        })
    }
}

fn increment(bucket: &mut u64) -> io::Result<()> {
    *bucket = bucket
        .checked_add(1)
        .ok_or_else(|| io::Error::other("scope accounting overflowed"))?;
    Ok(())
}
