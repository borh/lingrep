//! Cross-catalog drift: same-named definitions whose canonical,
//! reference-closed bodies differ across catalogs.
//!
//! Comparison happens on the **bound** IR, the last stage that still retains
//! fragments, visibility, capture names, and un-inlined references. Probe P2
//! (`docs/results/2026-08-01-probe-p2-body-identity.md`) fixed the inputs:
//! attribute order and set-member order are not canonical under raw
//! structural equality, so normalization is mandatory; `patterns()` bodies are
//! already reference-closed but drop fragments and erase captures
//! (`compiler/nfa.rs`), which is why this does not compare compiled forms; and
//! `Debug` formatting is build-configuration-dependent with no stability
//! contract, so it is not a drift instrument.
//!
//! # Canonicalization decisions
//!
//! Token predicates have their `AttrValue` lists sorted with the existing
//! `Ord`, and `OneOf`/`NotIn` member lists are sorted — P2(a)/(b). Regexes
//! compare by `as_str()`, the `AttrValueType` manual-`Eq` precedent. Capture
//! *names* participate; hygienic paths do not, because a path encodes
//! reference-occurrence numbering, which is derived rather than authored.
//! Metadata never participates: comparison is metadata-insensitive.
//!
//! ## Alternation branch order is deliberately NOT sorted
//!
//! Branch order is observable output. `named_groups_test.rs`'s
//! `capture_selection_is_source_ordered_greedy_and_last_participating`
//! compiles `((?<left> [text=a]) | (?<right> [text=a]))` — two branches
//! matching the identical token — and pins the reported capture as `left`,
//! the first-listed branch. Swapping the branches therefore swaps the
//! published capture name, so `(a|b)` and `(b|a)` are different bodies and
//! reporting them as drift is correct.
//!
//! Scope that claim honestly: it is proven for branches carrying *captures*.
//! For a capture-free alternation the order is not observable in `Match`
//! output, so this comparator over-reports there. That is the deliberate
//! conservative direction for a notice-only report that the researcher
//! adjudicates: a reported non-difference costs a glance, an
//! unreported real difference costs a silent divergence. Both halves are
//! pinned by tests below — do not "fix" either without re-deriving them.

use std::collections::BTreeMap;
use std::sync::Arc;

use compact_str::CompactString;

use super::bind::bind;
use super::limits::CompileLimits;
use super::lower::lower_document;
use super::model::{
    BoundDefinition, BoundDefinitionBody, BoundExpr, BoundProgram, CaptureDeclId,
    CaptureProjection, DefinitionId, DefinitionVisibility, LookDirection, LookPolarity,
    RelationLabels,
};
use super::{CompileError, Diagnostic, DiagnosticCode, PatternSource};
use crate::fsm::{AttrValue, AttrValueType, PositionAssertion};

/// Whether a definition emits matches, reported alongside each carrier.
///
/// Kind is **reported, not compared** — bodies alone decide drift. A name
/// declared as a `fragment` in one catalog and as an emitting pattern in
/// another, with identical bodies, is not drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftDefinitionKind {
    Pattern,
    Fragment,
    Lexicon,
}

/// One catalog carrying one body.
///
/// A struct per carrier rather than parallel vectors: the catalog↔kind
/// correspondence is a type-level fact, not an index convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftCarrier {
    /// The `PatternSource::named` name.
    pub catalog: String,
    pub kind: DriftDefinitionKind,
}

/// One distinct canonical body and every catalog carrying it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftBody {
    /// Catalogs carrying this body, in input order.
    pub carriers: Vec<DriftCarrier>,
}

/// One definition name and its distinct reference-closed canonical bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriftEntry {
    pub name: String,
    /// One entry per distinct canonical body, in first-seen catalog order.
    pub bodies: Vec<DriftBody>,
}

/// Reference-closed body groups and definitions that drift across catalogs.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CatalogDriftReport {
    /// All definition bodies grouped by name, including unchanged and one-sided definitions.
    pub definitions: Vec<DriftEntry>,
    /// Sorted by name; only names with ≥ 2 distinct bodies appear.
    pub entries: Vec<DriftEntry>,
}

/// Compiles each source through lower+bind and compares canonical,
/// reference-closed bodies of same-named definitions.
///
/// Any source that fails to compile fails the whole call: drift measured over
/// a broken catalog is meaningless. Compilation stops at `bind` — proceeding
/// to materialization would drop fragments and erase captures.
pub fn report_catalog_drift(
    sources: &[PatternSource<'_>],
) -> Result<CatalogDriftReport, CompileError> {
    let limits = CompileLimits::default();
    let mut grouped: BTreeMap<
        CompactString,
        Vec<(String, DriftDefinitionKind, CanonicalDefinitionBody)>,
    > = BTreeMap::new();

    for source in sources {
        let program = bind(lower_document(source, &limits)?, &limits)?;
        // Reference closure inlines, so a chain of doubling references
        // (`a := @b @b. b := @c @c. …`) is exponential in depth. `bind` does
        // not bound expansion — only `materialize`/`nfa` do, and this stops
        // before both — so without an explicit budget a compared catalog that
        // `check` itself would reject with `LimitExceeded` could hang here.
        // Charge the same budget the compiler charges, so `--compare-with`
        // accepts exactly what compilation accepts.
        let mut canonicalizer = Canonicalizer {
            program: &program,
            budget: limits.max_expansion_work(),
            visits: 0,
        };
        for definition in &program.definitions {
            let (kind, canonical) = match &definition.body {
                BoundDefinitionBody::Pattern {
                    visibility,
                    expr,
                    relations,
                    enhanced_relations,
                } => {
                    let mut canonical_relations = relations
                        .iter()
                        .map(|relation| {
                            // Sorted, like the constraint list itself: a
                            // listed label set is a disjunction, so `-[a|b]->` and
                            // `-[b|a]->` are one meaning and must not drift.
                            let labels = match &relation.labels {
                                RelationLabels::Any => RelationLabels::Any,
                                RelationLabels::Listed(labels) => {
                                    let mut labels = labels.to_vec();
                                    labels.sort();
                                    RelationLabels::Listed(labels.into_boxed_slice())
                                }
                            };
                            CanonicalRelation {
                                head: relation.head.clone(),
                                labels,
                                target: match &relation.target {
                                    super::model::RelationTarget::Captured { capture, .. } => {
                                        CanonicalRelationTarget::Captured(capture.clone())
                                    }
                                    super::model::RelationTarget::Anonymous {
                                        polarity, ..
                                    } => CanonicalRelationTarget::Anonymous(*polarity),
                                    super::model::RelationTarget::Predicate {
                                        polarity,
                                        predicates,
                                        ..
                                    } => {
                                        let mut predicates = predicates.to_vec();
                                        predicates.sort();
                                        CanonicalRelationTarget::Predicate(
                                            *polarity,
                                            predicates.into_boxed_slice(),
                                        )
                                    }
                                },
                            }
                        })
                        .collect::<Vec<_>>();
                    canonical_relations.sort();
                    let mut canonical_enhanced_relations = enhanced_relations
                        .iter()
                        .map(|relation| {
                            let endpoint =
                                |endpoint: &super::model::BoundEnhancedEndpoint| match endpoint {
                                    super::model::BoundEnhancedEndpoint::Reference {
                                        name, ..
                                    } => CanonicalEnhancedEndpoint::Reference(name.clone()),
                                    super::model::BoundEnhancedEndpoint::Binding {
                                        name,
                                        predicates,
                                        ..
                                    } => {
                                        let mut predicates = predicates.to_vec();
                                        predicates.sort();
                                        CanonicalEnhancedEndpoint::Binding(
                                            name.clone(),
                                            predicates.into_boxed_slice(),
                                        )
                                    }
                                    super::model::BoundEnhancedEndpoint::BasicHead {
                                        dependent,
                                        ..
                                    } => CanonicalEnhancedEndpoint::BasicHead(dependent.clone()),
                                };
                            let labels = match &relation.labels {
                                RelationLabels::Any => RelationLabels::Any,
                                RelationLabels::Listed(labels) => {
                                    let mut labels = labels.to_vec();
                                    labels.sort();
                                    RelationLabels::Listed(labels.into_boxed_slice())
                                }
                            };
                            CanonicalEnhancedRelation {
                                polarity: relation.polarity,
                                head: endpoint(&relation.head),
                                labels,
                                edge: relation.edge.as_ref().map(|(name, _)| name.clone()),
                                dependent: endpoint(&relation.dependent),
                            }
                        })
                        .collect::<Vec<_>>();
                    canonical_enhanced_relations.sort();
                    (
                        kind_of(*visibility),
                        CanonicalDefinitionBody::Pattern {
                            expr: canonicalizer.canonicalize(expr, expr).map_err(|()| {
                                expansion_limit_error(source.name, &definition.name)
                            })?,
                            relations: canonical_relations.into_boxed_slice(),
                            enhanced_relations: canonical_enhanced_relations.into_boxed_slice(),
                        },
                    )
                }
                BoundDefinitionBody::Lexicon { by, members } => {
                    let mut sorted_members = members.to_vec();
                    sorted_members.sort();
                    (
                        DriftDefinitionKind::Lexicon,
                        CanonicalDefinitionBody::Lexicon {
                            by: by.clone(),
                            sorted_members: sorted_members.into_boxed_slice(),
                        },
                    )
                }
            };
            grouped.entry(definition.name.clone()).or_default().push((
                source.name.to_owned(),
                kind,
                canonical,
            ));
        }
    }

    let definitions: Vec<DriftEntry> = grouped
        .into_iter()
        .map(|(name, carriers)| {
            let mut bodies: Vec<(CanonicalDefinitionBody, Vec<DriftCarrier>)> = Vec::new();
            for (catalog, kind, canonical) in carriers {
                let carrier = DriftCarrier { catalog, kind };
                match bodies.iter_mut().find(|(body, _)| *body == canonical) {
                    Some((_, carriers)) => carriers.push(carrier),
                    None => bodies.push((canonical, vec![carrier])),
                }
            }
            DriftEntry {
                name: name.into_string(),
                bodies: bodies
                    .into_iter()
                    .map(|(_, carriers)| DriftBody { carriers })
                    .collect(),
            }
        })
        .collect();
    let entries = definitions
        .iter()
        .filter(|entry| entry.bodies.len() >= 2)
        .cloned()
        .collect();
    Ok(CatalogDriftReport {
        definitions,
        entries,
    })
}

const fn kind_of(visibility: DefinitionVisibility) -> DriftDefinitionKind {
    match visibility {
        DefinitionVisibility::Pattern => DriftDefinitionKind::Pattern,
        DefinitionVisibility::Fragment => DriftDefinitionKind::Fragment,
    }
}

/// A normalized mirror of `BoundExpr` with references resolved in place.
///
/// Spans and definition IDs are absent by construction: both are positional
/// facts about one catalog's text, and neither is body identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalExpr {
    Token(Vec<crate::fsm::SymbolicAttrValue>),
    Assertion(CanonicalAssertion),
    Lookaround {
        direction: LookDirection,
        polarity: LookPolarity,
        body: Box<CanonicalExpr>,
    },
    Sequence(Vec<CanonicalExpr>),
    Alternation(Vec<CanonicalExpr>),
    /// Its own variant, never folded into [`Self::Alternation`]: two catalogs
    /// differing only in `&` where the other wrote `|` mean different things,
    /// and sharing a variant would make them canonicalize equal.
    Intersection(Vec<CanonicalExpr>),
    /// Operand order is not semantic: captures name the nodes, and the
    /// construct explores every surface permutation.
    AnyOrder(Vec<CanonicalExpr>),
    Repeat {
        expr: Box<CanonicalExpr>,
        min: usize,
        max: Option<usize>,
    },
    Capture {
        name: CompactString,
        expr: Box<CanonicalExpr>,
    },
    SameAs {
        name: CompactString,
        projection: CaptureProjection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CanonicalDefinitionBody {
    Pattern {
        expr: CanonicalExpr,
        /// `where` constraints, sorted: the clause is a conjunction, so two
        /// catalogs spelling the same constraints in different order carry
        /// one meaning and must not drift.
        relations: Box<[CanonicalRelation]>,
        enhanced_relations: Box<[CanonicalEnhancedRelation]>,
    },
    Lexicon {
        by: Box<[crate::token_match::AttributeKey]>,
        sorted_members: Box<[CompactString]>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalEnhancedEndpoint {
    Reference(CompactString),
    Binding(CompactString, Box<[super::model::TokenPredicate]>),
    BasicHead(CompactString),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalEnhancedRelation {
    polarity: super::model::RelationPolarity,
    head: CanonicalEnhancedEndpoint,
    labels: RelationLabels<super::model::RelationLabel>,
    edge: Option<CompactString>,
    dependent: CanonicalEnhancedEndpoint,
}

/// One `where` edge in canonical form: operand capture names and the sorted
/// label set.
///
/// Capture *names* rather than identities, because drift stops at `bind`,
/// before capture identities exist — and names are the authored meaning here.
/// Labels are the *resolved* set, so a catalog that inlines `nsubj|csubj`
/// and one that names `@SUBJ` for the same two do not drift against each
/// other, which is the right answer: they mean the same thing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct CanonicalRelation {
    head: CompactString,
    labels: RelationLabels<super::model::RelationLabel>,
    target: CanonicalRelationTarget,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalRelationTarget {
    Captured(CompactString),
    Anonymous(super::model::RelationPolarity),
    Predicate(
        super::model::RelationPolarity,
        Box<[super::model::TokenPredicate]>,
    ),
}

/// `PositionAssertion` normalized the same way a token predicate is.
///
/// `Lookaround(AssertionOccurrenceId)` is deliberately absent: the id is an
/// occurrence index assigned per catalog, so it would make every lookaround
/// drift against an identical one in another catalog. Bound lookaround bodies
/// are carried by `BoundExpr::Lookaround`, which is canonicalized structurally.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CanonicalAssertion {
    SentenceStart,
    SentenceEnd,
    PreviousToken(Vec<crate::fsm::SymbolicAttrValue>),
    /// Occurrence-indexed lookaround; the index itself is not body identity.
    LookaroundOccurrence,
}

fn expansion_limit_error(source_name: &str, definition: &str) -> CompileError {
    CompileError::from_diagnostics(vec![Diagnostic::error(
        DiagnosticCode::LimitExceeded,
        source_name,
        0..0,
        format!(
            "{source_name}: reference closure for definition {definition} exceeds the expansion budget"
        ),
        1,
        1,
    )])
}

/// Carries the expansion budget across one catalog's canonicalization.
struct Canonicalizer<'a> {
    program: &'a BoundProgram,
    budget: usize,
    visits: usize,
}

impl Canonicalizer<'_> {
    /// Canonicalizes `expr`, resolving `same_as` declarations against `owner`
    /// — the root body of the definition `expr` currently sits in.
    ///
    /// `bind` collects capture declarations per definition and numbers them by
    /// traversal position (`bind.rs:402-425`), so a `CaptureDeclId` is
    /// meaningful only relative to its own definition and is not body
    /// identity: an unrelated earlier capture shifts every later number.
    /// Resolving the id back to the authored name makes `same_as` compare the
    /// way captures themselves do. `owner` therefore changes whenever the
    /// closure crosses into a referenced definition.
    ///
    /// `Err(())` means the expansion budget ran out; the caller turns it into
    /// a `LimitExceeded` diagnostic naming the definition.
    fn canonicalize(&mut self, expr: &BoundExpr, owner: &BoundExpr) -> Result<CanonicalExpr, ()> {
        self.visits += 1;
        if self.visits > self.budget {
            return Err(());
        }
        Ok(match expr {
            BoundExpr::Token(predicate) => CanonicalExpr::Token(canonical_predicate(predicate)),
            BoundExpr::Assertion(assertion) => CanonicalExpr::Assertion(match assertion {
                PositionAssertion::SentenceStart => CanonicalAssertion::SentenceStart,
                PositionAssertion::SentenceEnd => CanonicalAssertion::SentenceEnd,
                PositionAssertion::PreviousToken(predicate) => {
                    CanonicalAssertion::PreviousToken(canonical_predicate(predicate))
                }
                PositionAssertion::Lookaround(_) => CanonicalAssertion::LookaroundOccurrence,
            }),
            BoundExpr::Lookaround(lookaround) => CanonicalExpr::Lookaround {
                direction: lookaround.direction,
                polarity: lookaround.polarity,
                // Recurse into the body: it contains token predicates, and
                // leaving them un-normalized would resurrect the P2(a) false
                // positive inside lookaround.
                body: Box::new(self.canonicalize(&lookaround.body, owner)?),
            },
            BoundExpr::Sequence(items) => CanonicalExpr::Sequence(
                items
                    .iter()
                    .map(|item| self.canonicalize(item, owner))
                    .collect::<Result<Vec<_>, ()>>()?,
            ),
            // Branch order preserved — see the module doc.
            BoundExpr::Alternation(branches) => CanonicalExpr::Alternation(
                branches
                    .iter()
                    .map(|branch| self.canonicalize(branch, owner))
                    .collect::<Result<Vec<_>, ()>>()?,
            ),
            // Operand order preserved, for the same reason branch order is:
            // `&` is commutative as a language operation, but drift compares
            // authored text, and reordering here would report two differently
            // written catalogs as the same one.
            BoundExpr::Intersection { operands, .. } => CanonicalExpr::Intersection(
                operands
                    .iter()
                    .map(|operand| self.canonicalize(operand, owner))
                    .collect::<Result<Vec<_>, ()>>()?,
            ),
            BoundExpr::AnyOrder { operands, .. } => {
                let mut operands = operands
                    .iter()
                    .map(|operand| self.canonicalize(operand, owner))
                    .collect::<Result<Vec<_>, ()>>()?;
                operands.sort();
                CanonicalExpr::AnyOrder(operands)
            }
            BoundExpr::Repeat { expr, min, max } => CanonicalExpr::Repeat {
                expr: Box::new(self.canonicalize(expr, owner)?),
                min: *min,
                max: *max,
            },
            BoundExpr::Capture { name, expr, .. } => CanonicalExpr::Capture {
                name: name.clone(),
                expr: Box::new(self.canonicalize(expr, owner)?),
            },
            BoundExpr::SameAs {
                declaration,
                projection,
                ..
            } => CanonicalExpr::SameAs {
                name: capture_declaration_name(*declaration, owner),
                projection: projection.clone(),
            },
            // Closure: resolve the reference by canonicalizing the referenced
            // body in place. Termination is guaranteed because `bind` resolves
            // a reference only to an earlier accepted definition, so no cycle
            // exists. The referenced body becomes its own `owner`: its
            // captures and its `same_as` declarations are numbered within it.
            BoundExpr::Definition { id, .. } => {
                let referenced = definition_by_id(*id, self.program)
                    .expr()
                    .expect("lexicon references are refused during binding");
                self.canonicalize(referenced, referenced)?
            }
        })
    }
}

/// Finds the authored name of the capture `declaration` refers to.
///
/// Returns a stable placeholder when the id resolves to nothing. `bind` emits
/// the `u32::MAX` sentinel only alongside a diagnostic, and this function runs
/// only on a `BoundProgram` that compiled, so the fallback is unreachable in
/// practice — it exists so an unresolved id can never be silently canonicalized
/// as equal to a *resolved* one.
fn capture_declaration_name(declaration: CaptureDeclId, owner: &BoundExpr) -> CompactString {
    fn search(expr: &BoundExpr, wanted: CaptureDeclId) -> Option<CompactString> {
        match expr {
            BoundExpr::Capture {
                declaration, name, ..
            } if *declaration == wanted => Some(name.clone()),
            BoundExpr::Capture { expr, .. } | BoundExpr::Repeat { expr, .. } => {
                search(expr, wanted)
            }
            BoundExpr::Lookaround(lookaround) => search(&lookaround.body, wanted),
            BoundExpr::Sequence(items) | BoundExpr::Alternation(items) => {
                items.iter().find_map(|item| search(item, wanted))
            }
            BoundExpr::AnyOrder { operands, .. } => {
                operands.iter().find_map(|operand| search(operand, wanted))
            }
            BoundExpr::Intersection { operands, .. } => {
                operands.iter().find_map(|operand| search(operand, wanted))
            }
            BoundExpr::Token(_)
            | BoundExpr::Assertion(_)
            | BoundExpr::SameAs { .. }
            | BoundExpr::Definition { .. } => None,
        }
    }

    search(owner, declaration).unwrap_or_else(|| CompactString::from("__unresolved_capture__"))
}

fn definition_by_id(id: DefinitionId, program: &BoundProgram) -> &BoundDefinition {
    program
        .definitions
        .iter()
        .find(|definition| definition.id == id)
        .expect("bind resolves references only to accepted definitions")
}

/// Sorts a token predicate's attributes and each set operand's members.
///
/// This is the normalization pass P2(a)/(b) mandates: neither authored
/// attribute order nor authored set-member order is body identity.
fn canonical_predicate(
    predicate: &[crate::fsm::SymbolicAttrValue],
) -> Vec<crate::fsm::SymbolicAttrValue> {
    let mut attrs: Vec<crate::fsm::SymbolicAttrValue> = predicate
        .iter()
        .map(|attr| AttrValue {
            attr: attr.attr.clone(),
            complement: attr.complement,
            value: match &attr.value {
                // Sorting needs an owned copy: the list is shared with every
                // other site that resolved to the same set, and drift
                // normalization must not reorder theirs.
                AttrValueType::OneOf(members) => {
                    let mut members = members.to_vec();
                    members.sort_unstable();
                    AttrValueType::OneOf(Arc::from(members))
                }
                AttrValueType::NotIn(members) => {
                    let mut members = members.to_vec();
                    members.sort_unstable();
                    AttrValueType::NotIn(Arc::from(members))
                }
                other => other.clone(),
            },
        })
        .collect();
    attrs.sort_unstable();
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drift(sources: &[(&str, &str)]) -> CatalogDriftReport {
        let sources: Vec<PatternSource<'_>> = sources
            .iter()
            .map(|(name, text)| PatternSource::named(name, text))
            .collect();
        report_catalog_drift(&sources).expect("catalogs compile")
    }

    fn names(report: &CatalogDriftReport) -> Vec<&str> {
        report
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    #[test]
    fn attribute_and_member_order_do_not_drift() {
        // P2(a)/(b): raw structural equality is order-sensitive; the canonical
        // comparator must not be.
        let report = drift(&[
            ("a.lx", "p := [upos=X & lemma=Y] [text in {A, B}]."),
            ("b.lx", "p := [lemma=Y & upos=X] [text in {B, A}]."),
        ]);
        assert!(report.entries.is_empty());
        assert_eq!(report.definitions.len(), 1);
        assert_eq!(report.definitions[0].bodies.len(), 1);
    }

    #[test]
    fn attribute_order_inside_lookaround_does_not_drift() {
        // A lookaround body contains token predicates; leaving them
        // un-normalized would resurrect the P2(a) false positive one level in.
        let report = drift(&[
            (
                "a.lx",
                "p := not_preceded_by([unidic.cform=X & upos=Y]) [text=Z].",
            ),
            (
                "b.lx",
                "p := not_preceded_by([upos=Y & unidic.cform=X]) [text=Z].",
            ),
        ]);
        assert!(report.entries.is_empty());
        assert_eq!(report.definitions.len(), 1);
        assert_eq!(report.definitions[0].bodies.len(), 1);
    }

    #[test]
    fn reference_closure_reaches_through_identical_shells() {
        // Same shell `@F ?`, divergent fragment bodies: unresolved comparison
        // would report equality; closure must not.
        let report = drift(&[
            ("a.lx", "fragment F := [text=た].\np := [upos=X] @F?."),
            ("b.lx", "fragment F := [text=だ].\np := [upos=X] @F?."),
        ]);
        // The fragment drifts AND the closed pattern drifts.
        assert_eq!(names(&report), ["F", "p"]);
    }

    #[test]
    fn fragment_versus_pattern_with_one_body_is_not_drift() {
        // Visibility is reported, not compared.
        let report = drift(&[
            ("a.lx", "fragment F := [text=た].\nuse := [upos=X] @F."),
            ("b.lx", "F := [text=た]."),
        ]);
        assert!(report.entries.is_empty(), "{report:?}");
    }

    #[test]
    fn capture_names_participate_in_identity() {
        // Captures are observable output; nfa lowering erases them, which is
        // why comparison happens at the bound layer where they still exist.
        let report = drift(&[
            ("a.lx", "p := (?<left> [upos=X]) [upos=Y]."),
            ("b.lx", "p := (?<right> [upos=X]) [upos=Y]."),
        ]);
        assert_eq!(report.entries.len(), 1, "{report:?}");
    }

    #[test]
    fn alternation_order_is_reported_as_drift() {
        // The premise for not sorting branches: with captures, first-listed
        // wins, so branch order is observable output. If a matcher change ever
        // makes this untrue, this test fails and the decision gets re-derived
        // rather than silently becoming false.
        let report = drift(&[
            ("a.lx", "p := ((?<l> [text=a]) | (?<r> [text=b]))."),
            ("b.lx", "p := ((?<r> [text=b]) | (?<l> [text=a]))."),
        ]);
        assert_eq!(names(&report), ["p"]);
    }

    #[test]
    fn any_order_operand_order_is_not_drift() {
        let report = drift(&[
            (
                "a.lx",
                "p := any_order((?<head> [text=h]), (?<dependent> [text=d])).",
            ),
            (
                "b.lx",
                "p := any_order((?<dependent> [text=d]), (?<head> [text=h])).",
            ),
        ]);
        assert!(report.entries.is_empty());
        assert_eq!(report.definitions.len(), 1);
        assert_eq!(report.definitions[0].bodies.len(), 1);
    }

    #[test]
    fn capture_free_alternation_order_is_reported_conservatively() {
        // Deliberate over-report: without captures the branch order is not
        // observable in `Match` output, so these two bodies are semantically
        // equal. The notice-only report errs toward showing the researcher a
        // structural difference rather than hiding one. See the module doc.
        let report = drift(&[
            ("a.lx", "p := ([text=a] | [text=b])."),
            ("b.lx", "p := ([text=b] | [text=a])."),
        ]);
        assert_eq!(names(&report), ["p"]);
    }

    #[test]
    fn self_comparison_reports_nothing() {
        let source = "fragment F := [text=た].\np := [upos=X] @F.";
        let report = drift(&[("a.lx", source), ("b.lx", source)]);
        assert!(report.entries.is_empty(), "{report:?}");
    }

    #[test]
    fn enhanced_constraint_order_does_not_drift() {
        let left = "pattern p := (?<v> []) where \
                    $v -[enhanced obj as $edge_a]-> (?<a> []), \
                    $v -[enhanced obl as $edge_b]-> (?<b> []).";
        let right = "pattern p := (?<v> []) where \
                     $v -[enhanced obl as $edge_b]-> (?<b> []), \
                     $v -[enhanced obj as $edge_a]-> (?<a> []).";
        let unchanged = drift(&[("a.lx", left), ("b.lx", right)]);
        assert!(unchanged.entries.is_empty());
        assert_eq!(unchanged.definitions.len(), 1);
        assert_eq!(unchanged.definitions[0].bodies.len(), 1);

        let positive = format!("{left} pattern q := (?<v> []) where $v -[enhanced dep]-> $v.");
        let negative = format!("{left} pattern q := (?<v> []) where not $v -[enhanced dep]-> $v.");
        assert_eq!(
            names(&drift(&[("a.lx", &positive), ("b.lx", &negative)])),
            ["q"],
            "edge polarity is semantic even though constraint order is not",
        );
    }

    #[test]
    fn carriers_group_by_body_in_input_order() {
        let report = drift(&[
            ("a.lx", "p := [text=た]."),
            ("b.lx", "p := [text=だ]."),
            ("c.lx", "p := [text=た]."),
        ]);
        assert_eq!(
            report.entries,
            vec![DriftEntry {
                name: "p".to_owned(),
                bodies: vec![
                    DriftBody {
                        carriers: vec![
                            DriftCarrier {
                                catalog: "a.lx".to_owned(),
                                kind: DriftDefinitionKind::Pattern,
                            },
                            DriftCarrier {
                                catalog: "c.lx".to_owned(),
                                kind: DriftDefinitionKind::Pattern,
                            },
                        ],
                    },
                    DriftBody {
                        carriers: vec![DriftCarrier {
                            catalog: "b.lx".to_owned(),
                            kind: DriftDefinitionKind::Pattern,
                        }],
                    },
                ],
            }]
        );
    }

    #[test]
    fn metadata_never_participates_in_body_identity() {
        let report = drift(&[
            ("a.lx", "note: \"one\"\np := [upos=X]."),
            ("b.lx", "note: \"two\"\np := [upos=X]."),
        ]);
        assert!(report.entries.is_empty(), "{report:?}");
    }

    #[test]
    fn a_reference_chain_that_exceeds_the_expansion_budget_is_an_error() {
        // Reference closure inlines, so a chain of doubling references is
        // exponential in depth. `bind` bounds definition count but NOT
        // expansion — only `materialize`/`nfa` do that, and this path stops
        // before both. Without the explicit budget this call does not return.
        // `compile_catalog` rejects the same catalog with `LimitExceeded`, so
        // accepting it here would make `--compare-with` admit what `check`
        // refuses.
        let mut catalog = String::from("d0 := [text=a].\n");
        for depth in 1..40 {
            catalog.push_str(&format!("d{depth} := @d{} @d{}.\n", depth - 1, depth - 1));
        }
        let error = report_catalog_drift(&[PatternSource::named("deep.lx", &catalog)])
            .expect_err("the expansion budget must bind");
        // `bind`'s definition budget also reports `LimitExceeded`, so match the
        // message too — otherwise this test would pass vacuously if the
        // catalog were rejected before canonicalization ever ran.
        assert!(
            error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code() == DiagnosticCode::LimitExceeded
                    && diagnostic.message.contains("reference closure")
            }),
            "{error:?}"
        );
    }

    #[test]
    fn broken_catalog_fails_the_call() {
        let sources = [
            PatternSource::named("a.lx", "p := [upos=X]."),
            PatternSource::named("b.lx", "p := @missing."),
        ];
        assert!(report_catalog_drift(&sources).is_err());
    }
}
