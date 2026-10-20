//! Where a sentence's active section labels come from.
//!
//! Two implementations, one evaluator. The direct path holds the labels
//! themselves; the prepared path holds a per-label bitmap and a sentence
//! ordinal. Neither materialises the other's representation, and neither owns
//! a second copy of the clause semantics.

/// Whether a section label is active at some sentence.
pub trait ActiveLabels {
    /// Whether `label` is active here.
    fn contains(&self, label: &str) -> bool;
}

/// The direct path's labels, as the reader produced them.
///
/// A linear scan rather than a hash: an active set is one to three labels in
/// practice and sixteen at the observed maximum on the reference corpus, so the
/// scan wins and — unlike building a set — it allocates nothing per sentence.
impl ActiveLabels for [String] {
    fn contains(&self, label: &str) -> bool {
        self.iter().any(|active| active == label)
    }
}

/// Nothing is active.
///
/// The evaluator's own type parameter needs an inhabitant when no labels are
/// supplied, which is what [`ScopeClause::evaluate_document`] uses it for. A
/// *corpus* that records no labels is refused before it reaches here, because
/// an empty set would otherwise answer "in no section" with the same confidence
/// as a real one.
///
/// [`ScopeClause::evaluate_document`]: super::ScopeClause::evaluate_document
pub struct NoLabels;

impl ActiveLabels for NoLabels {
    fn contains(&self, _label: &str) -> bool {
        false
    }
}
