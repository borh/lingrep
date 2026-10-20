//! Scope accounting shared between a reader and the command that owns it.
//!
//! The counts cannot be read off the reader at completion. A command hands its
//! event iterator to a source by value and the source is then moved into the
//! execution runner, so nothing holds the `AdmittedCorpus` by the time a
//! completion record is built. This is the shape the direct path already uses
//! for exactly that reason — see `StdinObservation` and `FilesystemObservation`
//! in `ling` — a cheap clonable handle over shared state, one clone kept
//! by the command and one by the reader.

use std::io;
use std::sync::{Arc, Mutex};

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
    /// Adds one document's tallies into a running total.
    fn merge(&mut self, other: Self) -> Option<()> {
        for (into, from) in [
            (&mut self.documents_admitted, other.documents_admitted),
            (
                &mut self.documents_excluded_by_predicate,
                other.documents_excluded_by_predicate,
            ),
            (
                &mut self.documents_excluded_missing_field,
                other.documents_excluded_missing_field,
            ),
            (
                &mut self.documents_excluded_shape_mismatch,
                other.documents_excluded_shape_mismatch,
            ),
            (&mut self.sentences_admitted, other.sentences_admitted),
            (
                &mut self.sentences_excluded_by_predicate,
                other.sentences_excluded_by_predicate,
            ),
            (
                &mut self.sentences_excluded_missing_field,
                other.sentences_excluded_missing_field,
            ),
            (
                &mut self.sentences_excluded_shape_mismatch,
                other.sentences_excluded_shape_mismatch,
            ),
        ] {
            *into = into.checked_add(from)?;
        }
        Some(())
    }

    /// Increments the bucket a verdict names, at document grain.
    ///
    /// Checked rather than wrapping because these counts are the denominator
    /// of a published research claim: the four buckets are stated to sum
    /// exactly to the universe, and a counter that quietly stopped counting
    /// would keep making that claim after it stopped being true. The reader
    /// around this refuses to guess for the same reason — the sentence-ordinal
    /// counter fails with "document sentence ordinal overflow" rather than
    /// wrapping.
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

#[derive(Debug, Default)]
struct Shared {
    counts: ScopeCounts,
    /// Set when a merge could not be represented. `counts` then describes less
    /// than the run did, so reporting it would be a false denominator.
    overflowed: bool,
}

/// A clonable handle over one run's [`ScopeCounts`].
///
/// The reader tallies into a plain [`ScopeCounts`] it owns and publishes here
/// **once per document**, not once per sentence. This keeps locking off the
/// per-sentence ingestion path.
#[derive(Clone, Debug, Default)]
pub struct ScopeObservation {
    state: Arc<Mutex<Shared>>,
}

impl ScopeObservation {
    /// A snapshot of the counts published so far.
    ///
    /// Complete once the reader has been dropped: the reader publishes each
    /// document as it closes and flushes any partially-read tail on drop.
    ///
    /// # Errors
    ///
    /// Returns an error if the shared state was poisoned by a panic, or if the
    /// accounting overflowed — reporting a truncated universe as though it
    /// were the whole one is the failure this report exists to prevent.
    pub fn counts(&self) -> io::Result<ScopeCounts> {
        let shared = self
            .state
            .lock()
            .map_err(|_| io::Error::other("scope observation lock poisoned"))?;
        if shared.overflowed {
            return Err(io::Error::other("scope accounting overflowed"));
        }
        Ok(shared.counts)
    }

    /// Publishes one document's tallies.
    pub(crate) fn publish(&self, pending: ScopeCounts) -> io::Result<()> {
        let mut shared = self
            .state
            .lock()
            .map_err(|_| io::Error::other("scope observation lock poisoned"))?;
        if shared.counts.merge(pending).is_none() {
            shared.overflowed = true;
            return Err(io::Error::other("scope accounting overflowed"));
        }
        Ok(())
    }

    /// Publishes from a drop, where there is no caller to return an error to.
    ///
    /// Failure is recorded rather than swallowed, so `counts` refuses instead
    /// of returning a number that is quietly too small.
    pub(crate) fn publish_on_drop(&self, pending: ScopeCounts) {
        if let Ok(mut shared) = self.state.lock()
            && shared.counts.merge(pending).is_none()
        {
            shared.overflowed = true;
        }
    }
}
