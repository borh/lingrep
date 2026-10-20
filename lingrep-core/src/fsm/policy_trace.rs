//! Bounded, per-record policy explanations.
//!
//! [`ProgramStats`] aggregates each statement over a run; this trace explains
//! the decisions for individual records.
//!
//! # Why bounded, and bounded on records
//!
//! A run produces an unbounded number of records. The trace therefore limits
//! records, whose per-sentence count can vary, rather than statements.
//!
//! Reaching the bound sets [`PolicyTrace::truncated`]. A trace that stopped
//! collecting and did not say so reads as a complete account of the run, which
//! is the failure this whole tier exists to prevent.
//!
//! # Why the sentence is a caller-supplied label
//!
//! A record's location in a corpus is `SentenceLocation`, an `execution` type,
//! and this is `fsm`, which does not depend on that layer. The seam holds both,
//! so [`PolicyTrace::begin_sentence`] takes whatever string the seam wants to
//! locate a sentence by, and the engine stays free of the layer above it.
//!
//! [`ProgramStats`]: crate::fsm::ProgramStats

use compact_str::CompactString;

/// What one statement did to one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TracedOutcome {
    /// The statement's subject did not select this record.
    ///
    /// Distinct from [`Self::Kept`], and the distinction is the point: "no
    /// statement selected it" is the answer to "why is this record unchanged",
    /// and it is not deducible from the statements that did act.
    NotSelected,
    /// Selected, and left alone.
    Kept,
    /// Selected, and removed from publication.
    Dropped,
    /// A `detach` moved the public span. Raw geometry is untouched.
    Narrowed {
        start: usize,
        end: usize,
        captures_dropped: u32,
    },
}

/// One record's passage through the whole program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TracedRecord {
    /// The label the seam stamped for this record's sentence, or empty.
    pub sentence: CompactString,
    /// The record's authored definition name — what a statement's subject
    /// selects on, and what survives relabeling.
    pub definition: CompactString,
    /// The record's raw span. Its identity, and stable across the program.
    pub raw_start: usize,
    pub raw_end: usize,
    /// One entry per statement that ran while this record was a candidate, in
    /// authored order, paired with that statement's index into the program's
    /// `labels()`.
    ///
    /// A dropped record's entries stop at the statement that dropped it: it
    /// left the candidate set, so no later statement saw it. That truncation is
    /// the answer to "why is this absent", not a gap in the trace.
    pub decisions: Vec<(usize, TracedOutcome)>,
}

/// A capacity-bounded collector for per-record explanations.
///
/// Opt-in: nothing constructs one unless a caller asked for a trace. See the
/// module header for why the bound is on records and why the sentence is a
/// caller-supplied label.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyTrace {
    capacity: usize,
    records: Vec<TracedRecord>,
    truncated: bool,
    sentence: CompactString,
}

impl PolicyTrace {
    /// A collector that will explain at most `capacity` records.
    ///
    /// A capacity of zero collects nothing and is not an error: it is what a
    /// caller threading an optional trace through a code path that may not want
    /// one can pass without a branch.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            records: Vec::new(),
            truncated: false,
            sentence: CompactString::default(),
        }
    }

    /// Labels every record opened until the next call.
    ///
    /// The seam calls this before applying policy to a sentence. A trace whose
    /// caller never does still collects; its records carry an empty label.
    pub fn begin_sentence(&mut self, label: &str) {
        self.sentence = CompactString::new(label);
    }

    /// The records explained so far, in the order they were opened.
    #[must_use]
    pub fn records(&self) -> &[TracedRecord] {
        &self.records
    }

    /// Whether the capacity was reached and collection stopped.
    ///
    /// A caller printing a trace must print this. See the module header.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Opens a record, returning its index, or `None` at capacity.
    ///
    /// Sets [`Self::truncated`] on the first refusal rather than on every one,
    /// which makes the flag mean "there was more" and not "how much more" — a
    /// count of what was not collected would need the very traversal the bound
    /// exists to avoid.
    pub(crate) fn open(
        &mut self,
        definition: &str,
        raw_start: usize,
        raw_end: usize,
    ) -> Option<usize> {
        if self.records.len() >= self.capacity {
            self.truncated = true;
            return None;
        }
        self.records.push(TracedRecord {
            sentence: self.sentence.clone(),
            definition: CompactString::new(definition),
            raw_start,
            raw_end,
            decisions: Vec::new(),
        });
        Some(self.records.len() - 1)
    }

    /// Appends one statement's outcome to an open record.
    ///
    /// A no-op for an index this trace did not issue, for the same reason
    /// `ProgramStats::record` is: a reporting mismatch must not turn into a
    /// failed run.
    pub(crate) fn observe(&mut self, record: usize, statement: usize, outcome: TracedOutcome) {
        if let Some(traced) = self.records.get_mut(record) {
            traced.decisions.push((statement, outcome));
        }
    }
}
