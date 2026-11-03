//! Per-sentence raw-candidate and publication outcome pairing.
//!
//! An opt-in diagnostic value. Production corpus execution uses the direct
//! publication seam; consumers that require per-sentence explanation retain
//! the raw match that led to each visible result.

use super::Match;

/// One researcher-visible raw candidate and its result-policy outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplainedMatch {
    /// The canonical match as the pattern recognizer produced it.
    pub raw: Match,
    /// What this candidate published after result policy, if it survived.
    ///
    /// The published match may have a different span or pattern name. `None`
    /// means the policy suppressed this otherwise visible candidate.
    pub published: Option<Match>,
}

/// Explanation of one sentence's researcher-visible pattern candidates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceExplanation {
    /// Candidates in canonical raw-match order. Synthesized policy evidence is
    /// intentionally absent: it is engine input, not a researcher-visible
    /// pattern.
    pub candidates: Box<[ExplainedMatch]>,
}
