//! Tri-state evaluation of a clause against one document's metadata.
//!
//! `Unmet` is false in both polarities, so it cannot be modelled as a boolean.
//! The *reason* is carried separately because combining reasons is not a
//! boolean operation: a false clause may sit above both a missing field and a
//! shape mismatch, and source order decides which is reported.

use crate::corpus::{Metadata, MetadataScalar, MetadataValue};

use super::labels::{ActiveLabels, NoLabels};
use super::parse::Clause;
use super::{Comparison, Membership, Predicate, ScopeClause};

/// Why a document was not admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExclusionReason {
    /// Every predicate evaluated cleanly and the clause was false.
    Predicate,
    /// A predicate named a key the document does not carry.
    MissingField,
    /// A predicate named a key whose value has the wrong shape for it.
    ShapeMismatch,
}

/// A document's admission outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The clause held; the document's sentences reach matching.
    Admitted,
    /// The clause did not hold, for this reason.
    Excluded(ExclusionReason),
}

/// A document's outcome before any sentence has been looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentOutcome {
    /// Decided from document metadata alone; every sentence shares this.
    Decided(Verdict),
    /// A section predicate can still change the answer per sentence.
    RequiresSentences,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Truth {
    True,
    False,
    Unmet(ExclusionReason),
    /// A section leaf evaluated without labels. Not a third truth value of the
    /// scope language — a statement that this pass cannot answer it yet.
    Indeterminate,
}

impl Truth {
    const fn is_indeterminate(self) -> bool {
        matches!(self, Self::Indeterminate)
    }

    const fn unmet(self) -> Option<ExclusionReason> {
        match self {
            Self::Unmet(reason) => Some(reason),
            Self::True | Self::False | Self::Indeterminate => None,
        }
    }
}

/// Where a section leaf's answer comes from on this pass.
enum Sections<'a, L: ?Sized> {
    /// Pass one: no labels, so a section leaf is indeterminate.
    Unknown,
    /// Pass two: these labels.
    Active(&'a L),
}

/// Where a metadata leaf's per-sentence half comes from on this pass.
enum SentenceValues<'a> {
    /// Pass one: no sentence in hand. A leaf over a key the corpus declares
    /// at sentence grain is indeterminate, exactly as a section leaf is.
    Deferred,
    /// Pass two: this sentence's own metadata, layered over the document's.
    Present(&'a Metadata, &'a crate::corpus::StructuralContext),
    Division,
}

impl ScopeClause {
    /// Judges one document from its metadata alone.
    ///
    /// A clause naming no sections and no key the corpus carries per sentence
    /// is always [`DocumentOutcome::Decided`], so such a run takes exactly the
    /// path it took before section and sentence-metadata scope existed — the
    /// same per-document skip, at the same cost.
    #[must_use]
    pub fn evaluate_document(&self, metadata: &Metadata) -> DocumentOutcome {
        let mut first_unmet = None;
        let truth = evaluate::<NoLabels>(
            self.root(),
            metadata,
            &Sections::Unknown,
            &SentenceValues::Deferred,
            &mut first_unmet,
        );
        match truth {
            Truth::True => DocumentOutcome::Decided(Verdict::Admitted),
            Truth::False | Truth::Unmet(_) => DocumentOutcome::Decided(Verdict::Excluded(
                first_unmet.unwrap_or(ExclusionReason::Predicate),
            )),
            Truth::Indeterminate => DocumentOutcome::RequiresSentences,
        }
    }

    /// Judges one sentence, given its document's metadata, its own metadata,
    /// and its labels.
    ///
    /// Each metadata predicate reads only its explicit owner. A structural
    /// predicate evaluates all of its conditions against one covering division.
    #[must_use]
    pub fn evaluate_sentence<L: ActiveLabels + ?Sized>(
        &self,
        metadata: &Metadata,
        sentence_metadata: &Metadata,
        labels: &L,
        structure: &crate::corpus::StructuralContext,
    ) -> Verdict {
        let mut first_unmet = None;
        let truth = evaluate::<L>(
            self.root(),
            metadata,
            &Sections::Active(labels),
            &SentenceValues::Present(sentence_metadata, structure),
            &mut first_unmet,
        );
        if matches!(truth, Truth::True) {
            return Verdict::Admitted;
        }
        Verdict::Excluded(first_unmet.unwrap_or(ExclusionReason::Predicate))
    }
}

/// Evaluates in source order over the Kleene lattice, recording the first
/// non-evaluable predicate.
///
/// Every branch is visited even once the answer is settled: the reason is a
/// property of the clause as written, not of the shortest path to `false`, and
/// short-circuiting would make the reported reason depend on evaluation order.
///
/// Concretely: `year >= 2020 & absent = 1` would report `Predicate` while
/// `absent = 1 & year >= 2020` reported `MissingField`, for one clause with one
/// answer. These are folds rather than `all`/`any` for that reason — and, since
/// section scope, because `all` and `any` cannot express a three-valued
/// accumulator at all. The Kleene lattice also makes clippy's ordinary
/// `all`/`any` replacement inapplicable.
///
/// The `All` fold collapses a false conjunct to [`Truth::False`], discarding
/// its *reason* — but `first_unmet` has already recorded it. Separating the two
/// is what makes the reason independent of where in the tree the falsity was
/// found.
fn evaluate<L: ActiveLabels + ?Sized>(
    clause: &Clause,
    metadata: &Metadata,
    sections: &Sections<'_, L>,
    sentence_values: &SentenceValues<'_>,
    first_unmet: &mut Option<ExclusionReason>,
) -> Truth {
    match clause {
        Clause::Within {
            kind,
            body,
            negated,
        } => {
            let context = match sentence_values {
                SentenceValues::Deferred => return Truth::Indeterminate,
                SentenceValues::Present(_, context) => context,
                SentenceValues::Division => return Truth::False,
            };
            let mut result = Truth::False;
            if context.observes(kind) {
                for division in context
                    .divisions
                    .iter()
                    .filter(|division| division.kind == *kind)
                {
                    let mut unmet = None;
                    let value = evaluate::<NoLabels>(
                        body,
                        &division.metadata,
                        &Sections::Unknown,
                        &SentenceValues::Division,
                        &mut unmet,
                    );
                    if value == Truth::True {
                        result = Truth::True;
                        break;
                    }
                    if let Truth::Unmet(reason) = value
                        && result == Truth::False
                    {
                        result = Truth::Unmet(reason);
                    }
                }
            } else {
                result = Truth::Unmet(ExclusionReason::MissingField);
            }
            if *negated {
                result = match result {
                    Truth::True => Truth::False,
                    Truth::False => Truth::True,
                    unknown => unknown,
                };
            }
            if let Truth::Unmet(reason) = result {
                first_unmet.get_or_insert(reason);
            }
            result
        }
        Clause::All(parts) => parts
            .iter()
            .map(|part| evaluate(part, metadata, sections, sentence_values, first_unmet))
            .fold(Truth::True, |accumulated, truth| {
                if accumulated == Truth::False || truth == Truth::False {
                    Truth::False
                } else if let Truth::Unmet(reason) = accumulated {
                    Truth::Unmet(reason)
                } else if let Truth::Unmet(reason) = truth {
                    Truth::Unmet(reason)
                } else if accumulated.is_indeterminate() || truth.is_indeterminate() {
                    Truth::Indeterminate
                } else {
                    Truth::True
                }
            }),
        Clause::Any(parts) => parts
            .iter()
            .map(|part| evaluate(part, metadata, sections, sentence_values, first_unmet))
            .fold(Truth::False, |accumulated, truth| {
                if matches!(accumulated, Truth::True) || matches!(truth, Truth::True) {
                    Truth::True
                } else if accumulated.is_indeterminate() || truth.is_indeterminate() {
                    Truth::Indeterminate
                } else if let Truth::Unmet(reason) = accumulated {
                    Truth::Unmet(reason)
                } else if let Truth::Unmet(reason) = truth {
                    Truth::Unmet(reason)
                } else {
                    Truth::False
                }
            }),
        Clause::Leaf(Predicate::Section(membership, members)) => match sections {
            Sections::Unknown => Truth::Indeterminate,
            Sections::Active(labels) => {
                let present = members.iter().any(|member| labels.contains(member));
                truth(matches!(membership, Membership::In) == present)
            }
        },
        Clause::Leaf(predicate) => {
            let truth = judge(predicate, metadata, sentence_values);
            if let Some(reason) = truth.unmet() {
                first_unmet.get_or_insert(reason);
            }
            truth
        }
    }
}

fn judge(
    predicate: &Predicate,
    metadata: &Metadata,
    sentence_values: &SentenceValues<'_>,
) -> Truth {
    let key = match predicate {
        Predicate::Compare(key, _, _)
        | Predicate::Order(key, _, _)
        | Predicate::Member(key, _, _)
        | Predicate::Contains(key, _, _) => key,
        // Handled by `evaluate` before `judge` is called: a section predicate
        // names no metadata key, so there is nothing to look up. `False` rather
        // than a panic keeps the deny-floor satisfied, and
        // `a_failing_section_predicate_reports_predicate_and_nothing_else`
        // fails loudly if this arm ever becomes reachable.
        Predicate::Section(_, _) => return Truth::False,
    };
    let value = match sentence_values {
        SentenceValues::Division => lookup(metadata, key),
        SentenceValues::Deferred if key.starts_with("@sentence.") => {
            return Truth::Indeterminate;
        }
        SentenceValues::Deferred => key
            .strip_prefix("@document.")
            .and_then(|key| lookup(metadata, key)),
        SentenceValues::Present(sentence_metadata, _) => {
            if let Some(key) = key.strip_prefix("@sentence.") {
                lookup(sentence_metadata, key)
            } else {
                key.strip_prefix("@document.")
                    .and_then(|key| lookup(metadata, key))
            }
        }
    };
    let Some(value) = value else {
        return Truth::Unmet(ExclusionReason::MissingField);
    };
    match predicate {
        Predicate::Compare(_, comparison, literal) => {
            scalar(value).map_or(Truth::Unmet(ExclusionReason::ShapeMismatch), |scalar| {
                let equal = render(scalar) == *literal;
                truth(matches!(comparison, Comparison::Equal) == equal)
            })
        }
        Predicate::Order(_, comparison, literal) => scalar(value).and_then(decimal).map_or(
            Truth::Unmet(ExclusionReason::ShapeMismatch),
            |actual| match comparison {
                Comparison::Less => truth(actual < *literal),
                Comparison::LessEqual => truth(actual <= *literal),
                Comparison::Greater => truth(actual > *literal),
                Comparison::GreaterEqual => truth(actual >= *literal),
                // Unreachable by construction: the parser only builds `Order`
                // for the four ordered spellings. Answering `False` rather
                // than panicking keeps the deny-floor satisfied without an
                // `unreachable!`.
                Comparison::Equal | Comparison::NotEqual => Truth::False,
            },
        ),
        Predicate::Member(_, membership, members) => {
            scalar(value).map_or(Truth::Unmet(ExclusionReason::ShapeMismatch), |scalar| {
                let present = members.iter().any(|member| render(scalar) == *member);
                truth(matches!(membership, Membership::In) == present)
            })
        }
        Predicate::Contains(_, negated, member) => match value {
            MetadataValue::Scalars(list) => {
                let present = list.iter().any(|scalar| render(scalar) == *member);
                truth(*negated != present)
            }
            MetadataValue::Scalar(_) | MetadataValue::Opaque(_) => {
                Truth::Unmet(ExclusionReason::ShapeMismatch)
            }
        },
        // Unreachable: the key-extraction match above returns first.
        Predicate::Section(_, _) => Truth::False,
    }
}

const fn truth(holds: bool) -> Truth {
    if holds { Truth::True } else { Truth::False }
}

fn lookup<'a>(metadata: &'a Metadata, key: &str) -> Option<&'a MetadataValue> {
    metadata
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value)
}

const fn scalar(value: &MetadataValue) -> Option<&MetadataScalar> {
    match value {
        MetadataValue::Scalar(scalar) => Some(scalar),
        MetadataValue::Scalars(_) | MetadataValue::Opaque(_) => None,
    }
}

fn render(scalar: &MetadataScalar) -> String {
    match scalar {
        MetadataScalar::String(text) | MetadataScalar::Number(text) => text.clone(),
        MetadataScalar::Bool(value) => value.to_string(),
    }
}

/// Parses under this language's own rule, whatever variant the value arrived
/// as — the corpus stores every numeric-looking key as a string, so
/// dispatching on the arrived variant would answer "no" to every ordered
/// comparison.
fn decimal(scalar: &MetadataScalar) -> Option<f64> {
    match scalar {
        MetadataScalar::String(text) | MetadataScalar::Number(text) => text.parse().ok(),
        MetadataScalar::Bool(_) => None,
    }
}
