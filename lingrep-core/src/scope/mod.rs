//! The `--where` scope clause language.
//!
//! Scope is a property of a query invocation, not of a pattern: it is written
//! once, applies uniformly to the whole catalog, and has no per-pattern form.
//! That placement — outside the pattern language entirely — is what makes the
//! ANNIS `meta::` failure modes unreachable, because there is nothing to bind
//! and nothing to distribute.
//!
//! Evaluation is tri-state. A predicate over a missing field, and a predicate
//! over a value whose shape does not fit it, are both false in *both*
//! polarities, so a scope and its apparent complement are not exhaustive. They
//! are different facts and are counted separately.

mod eval;
mod labels;
mod lex;
mod parse;

use std::collections::BTreeSet;
use std::ops::Range;

use parse::Clause;

pub use eval::{DocumentOutcome, ExclusionReason, Verdict};
pub use labels::{ActiveLabels, NoLabels};

/// A parsed, immutable scope clause.
#[derive(Clone, Debug)]
pub struct ScopeClause {
    root: Clause,
    source: String,
    division_kinds: BTreeSet<String>,
}

impl ScopeClause {
    /// Parses a clause, refusing anything a document could not disambiguate.
    ///
    /// # Errors
    ///
    /// Returns a [`ScopeError`] pointing at the offending bytes.
    pub fn parse(source: &str) -> Result<Self, ScopeError> {
        let root = parse::parse(source)?;
        let mut division_kinds = BTreeSet::new();
        collect_division_kinds(&root, &mut division_kinds);
        Ok(Self {
            root,
            source: source.to_owned(),
            division_kinds,
        })
    }

    /// The clause as written, for the echo every scoped run publishes.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether any leaf is a section predicate.
    ///
    /// The load-bearing question of the two-pass evaluator: `false` means the
    /// clause is always decided from document metadata alone, so the run takes
    /// the same per-document path it took before section scope existed.
    #[must_use]
    pub fn names_sections(&self) -> bool {
        !self.section_labels().is_empty()
    }

    /// Every label the clause names, for the unused-label diagnostic and for
    /// the prepared path's label index.
    ///
    /// Defined together with [`Self::names_sections`] rather than beside a
    /// second walk: the parser cannot build a section predicate with an empty
    /// member list — `value_set` requires one member and `=` supplies exactly
    /// one — so the two answers cannot diverge.
    #[must_use]
    pub fn section_labels(&self) -> BTreeSet<&str> {
        let mut labels = BTreeSet::new();
        collect_labels(&self.root, &mut labels);
        labels
    }

    /// Every explicitly owned metadata key named by the clause.
    #[must_use]
    pub fn metadata_keys(&self) -> BTreeSet<&str> {
        let mut keys = BTreeSet::new();
        collect_metadata_keys(&self.root, &mut keys);
        keys
    }

    /// Whether evaluation needs sentence fields or authored division membership.
    #[must_use]
    pub fn requires_sentence_values(&self) -> bool {
        self.names_divisions()
            || self
                .metadata_keys()
                .iter()
                .any(|key| key.starts_with("@sentence."))
    }

    pub fn names_divisions(&self) -> bool {
        !self.division_kinds.is_empty()
    }
    pub fn validate_structure(
        &self,
        context: &crate::corpus::StructuralContext,
    ) -> Result<(), String> {
        for kind in &self.division_kinds {
            if !context.observes(kind) {
                let reason = match context.observations.get(kind) {
                    Some(crate::corpus::DivisionObservation::Unavailable { reason }) => {
                        reason.as_str()
                    }
                    _ => "this source does not observe this division kind",
                };
                return Err(format!(
                    "structural scope `{kind}` is unavailable: {reason}"
                ));
            }
        }
        Ok(())
    }

    pub(crate) const fn root(&self) -> &Clause {
        &self.root
    }
}

/// A refusal, pointing at the bytes that caused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeError {
    /// What was wrong, in the author's terms.
    pub message: String,
    /// The offending bytes of the clause source.
    pub byte_range: Range<usize>,
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for ScopeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Membership {
    In,
    NotIn,
}

#[derive(Clone, Debug)]
pub(crate) enum Predicate {
    Compare(String, Comparison, String),
    Order(String, Comparison, f64),
    Member(String, Membership, Vec<String>),
    /// `(key, negated, member)`
    Contains(String, bool, String),
    /// `@section` against the sentence's active labels. Normalised at parse
    /// time: `= x` becomes `In [x]` and `!= x` becomes `NotIn [x]`, so
    /// evaluation has one case rather than four.
    Section(Membership, Vec<String>),
}

/// Collects every section label a clause names, in one walk.
fn collect_labels<'a>(clause: &'a Clause, labels: &mut BTreeSet<&'a str>) {
    match clause {
        Clause::Within { .. } => {}
        Clause::All(parts) | Clause::Any(parts) => {
            for part in parts {
                collect_labels(part, labels);
            }
        }
        Clause::Leaf(Predicate::Section(_, members)) => {
            labels.extend(members.iter().map(String::as_str));
        }
        Clause::Leaf(_) => {}
    }
}

/// Collects every metadata key a clause names, in one walk.
fn collect_metadata_keys<'a>(clause: &'a Clause, keys: &mut BTreeSet<&'a str>) {
    match clause {
        Clause::Within { .. } => {}
        Clause::All(parts) | Clause::Any(parts) => {
            for part in parts {
                collect_metadata_keys(part, keys);
            }
        }
        Clause::Leaf(
            Predicate::Compare(key, _, _)
            | Predicate::Order(key, _, _)
            | Predicate::Member(key, _, _)
            | Predicate::Contains(key, _, _),
        ) => {
            keys.insert(key.as_str());
        }
        Clause::Leaf(Predicate::Section(_, _)) => {}
    }
}

fn collect_division_kinds(clause: &Clause, kinds: &mut BTreeSet<String>) {
    match clause {
        Clause::Within { kind, .. } => {
            kinds.insert(kind.clone());
        }
        Clause::All(parts) | Clause::Any(parts) => {
            for part in parts {
                collect_division_kinds(part, kinds);
            }
        }
        Clause::Leaf(_) => {}
    }
}
