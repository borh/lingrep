mod interpret;
mod measure;

use super::assertion::AssertionSession;
use super::predicate::{
    CompiledPositionAssertion, Members, PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN,
    PREDICATE_SITE_CAPTURE_TOKEN, PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN,
    PREDICATE_SITE_EVALUATED_TOKEN, PredicateCatalog, PredicateId, PredicateSession,
};
use super::resource::{MAX_RELATION_DEPTH, MatchBudget, MatchResourceError, OutputBudget};
use crate::corpus::conllu::{EnhancedGraph, EnhancedHead, EnhancedNodeId};
use crate::fsm::compiler::model::{
    CaptureId, CaptureProjection, DependencyLayer, ExecutionDefinition, ExecutionEnhancedEndpoint,
    ExecutionExpr, ExecutionGraphBindingDecl, RelationLabel, RelationLabels, RelationPolarity,
    RelationTarget, TokenPredicate,
};
use crate::fsm::compiler::model::{DependentRequirement, NodeComparison, NodeConstraint};
use crate::fsm::{AttrValueType, Capture, GraphBinding, GraphNodeAttribute, PositionAssertion};
use crate::token_match::{
    AttributeKey, AttributeLocator, AttributeValueRef, BoundProjection, TokenSequence,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(crate) struct Program {
    definition: ExecutionDefinition,
    measures: Box<[measure::RuntimeMeasure]>,
    runtime: RuntimeExpr,
    captures: Box<[CaptureDescriptor]>,
    relations: Box<[RuntimeRelation]>,
    enhanced_relations: Box<[RuntimeEnhancedRelation]>,
    node_constraints: Box<[NodeConstraint<RuntimeEnhancedEndpoint>]>,
}

#[derive(Clone)]
struct RuntimeRelation {
    head: CaptureId,
    target: RuntimeRelationTarget,
    labels: RelationLabels<RelationLabel>,
    head_attribute: AttributeLocator,
    relation_attribute: AttributeLocator,
}

#[derive(Clone)]
enum RuntimeRelationTarget {
    Captured {
        capture: CaptureId,
        polarity: RelationPolarity,
    },
    Anonymous(RelationPolarity),
    Predicate {
        requirement: DependentRequirement,
        /// Each alternative with the scalar attributes it tests.
        predicates: Box<[(PredicateId, Box<[AttributeLocator]>)]>,
    },
}

#[derive(Clone)]
struct RuntimeEnhancedRelation {
    layer: RuntimeLayer,
    polarity: RelationPolarity,
    head: RuntimeEnhancedEndpoint,
    labels: RelationLabels<RelationLabel>,
    edge: Option<compact_str::CompactString>,
    dependent: RuntimeEnhancedEndpoint,
}

/// Where a graph relation reads its edges: the tokens' basic `dep.head` and
/// `dep.rel`, or the sentence's enhanced graph.
#[derive(Clone)]
enum RuntimeLayer {
    Basic {
        head_attribute: AttributeLocator,
        relation_attribute: AttributeLocator,
    },
    Enhanced,
}

#[derive(Clone)]
enum RuntimeEnhancedEndpoint {
    Surface(CaptureId),
    BasicHead {
        dependent: CaptureId,
        head_attribute: AttributeLocator,
    },
    Graph {
        name: compact_str::CompactString,
        symbolic: Box<[TokenPredicate]>,
        /// For each atom of each predicate in `symbolic`, the operands of
        /// its `in` or `not in` list; `None` for any other test.
        members: Box<[Box<[Option<Members>]>]>,
    },
}

#[derive(Clone)]
enum RuntimeExpr {
    Token(PredicateId),
    Assertion(CompiledPositionAssertion),
    Sequence(Box<[RuntimeExpr]>),
    Alternation(Box<[RuntimeExpr]>),
    Intersection(Box<[RuntimeExpr]>),
    AnyOrder(Box<[RuntimeExpr]>),
    Repeat {
        expr: Box<RuntimeExpr>,
        min: usize,
        max: Option<usize>,
    },
    Capture {
        id: CaptureId,
        expr: Box<RuntimeExpr>,
        relations: Box<[RuntimeRelation]>,
    },
    SameAs {
        capture: CaptureId,
        projection: AttributeLocator,
    },
}

#[derive(Clone)]
struct CaptureDescriptor {
    id: CaptureId,
    name: compact_str::CompactString,
    path: compact_str::CompactString,
}

#[derive(Clone)]
struct Candidate {
    position: usize,
    slots: Vec<Option<(usize, usize)>>,
    /// Authored operand indices in selected surface order for every completed
    /// `any_order` on this path. The complete path keeps composed constructs'
    /// implicit branches independent, exactly as explicit definitions are.
    any_order_branches: Vec<Box<[usize]>>,
}

#[derive(Clone)]
pub(crate) struct ProgramMatch {
    pub(crate) interpretations: Box<[crate::fsm::Interpretation]>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) captures: Box<[Capture]>,
    pub(crate) graph_bindings: Box<[GraphBinding]>,
}

struct AcceptedCandidate {
    candidate: Candidate,
    assignments: Vec<GraphAssignment>,
    counts: Vec<GraphBinding>,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct GraphAssignment {
    nodes: BTreeMap<compact_str::CompactString, EnhancedNodeId>,
    edges: BTreeMap<
        compact_str::CompactString,
        (EnhancedNodeId, EnhancedNodeId, compact_str::CompactString),
    >,
}

impl GraphAssignment {
    fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(btree_storage_bytes::<
                compact_str::CompactString,
                EnhancedNodeId,
            >(self.nodes.len()))
            .saturating_add(btree_storage_bytes::<
                compact_str::CompactString,
                (EnhancedNodeId, EnhancedNodeId, compact_str::CompactString),
            >(self.edges.len()))
            .saturating_add(
                self.nodes
                    .keys()
                    .map(compact_string_heap_bytes)
                    .sum::<usize>(),
            )
            .saturating_add(
                self.edges
                    .iter()
                    .map(|(name, (_, _, label))| {
                        compact_string_heap_bytes(name)
                            .saturating_add(compact_string_heap_bytes(label))
                    })
                    .sum::<usize>(),
            )
    }
}

fn compact_string_heap_bytes(value: &compact_str::CompactString) -> usize {
    if value.is_heap_allocated() {
        value.capacity()
    } else {
        0
    }
}

/// Upper storage estimate for maps constructed only by insertion and cloning.
pub(super) fn btree_storage_bytes<K, V>(entries: usize) -> usize {
    // Rust's alloc/collections/btree/node.rs uses eleven-slot nodes and a five-entry non-root minimum:
    // https://doc.rust-lang.org/src/alloc/collections/btree/node.rs.html
    const CAPACITY: usize = 11;
    const MIN_NON_ROOT: usize = 5;
    if entries == 0 {
        return 0;
    }
    let pointer_bytes = std::mem::size_of::<usize>();
    let leaf_bytes =
        2 * pointer_bytes + CAPACITY * (std::mem::size_of::<K>() + std::mem::size_of::<V>());
    if entries <= CAPACITY {
        return leaf_bytes;
    }
    let internal_bytes = leaf_bytes + (CAPACITY + 1) * pointer_bytes;
    entries
        .div_ceil(MIN_NON_ROOT)
        .saturating_mul(internal_bytes)
}

trait AssertionLookup {
    fn holds(&mut self, occurrence: crate::fsm::AssertionOccurrenceId, boundary: usize) -> bool;
}

impl<S: TokenSequence + ?Sized> AssertionLookup for AssertionSession<'_, '_, S> {
    #[inline]
    fn holds(&mut self, occurrence: crate::fsm::AssertionOccurrenceId, boundary: usize) -> bool {
        AssertionSession::holds(self, occurrence, boundary)
    }
}

struct NoAssertions;

impl AssertionLookup for NoAssertions {
    #[cold]
    #[expect(
        clippy::unreachable,
        reason = "an assertion-free execution plan cannot contain general lookaround"
    )]
    fn holds(&mut self, _: crate::fsm::AssertionOccurrenceId, _: usize) -> bool {
        unreachable!("an assertion-free execution plan cannot contain general lookaround")
    }
}

impl Program {
    pub(crate) fn new(definition: ExecutionDefinition, predicates: &mut PredicateCatalog) -> Self {
        let mut captures = Vec::new();
        collect_captures(&definition.expr, &mut captures);
        captures.sort_by_key(|capture| capture.id);
        let relations: Box<[RuntimeRelation]> = definition
            .relations
            .iter()
            .map(|relation| RuntimeRelation {
                head: relation.head,
                target: match &relation.target {
                    RelationTarget::Captured {
                        capture, polarity, ..
                    } => RuntimeRelationTarget::Captured {
                        capture: *capture,
                        polarity: *polarity,
                    },
                    RelationTarget::Anonymous { polarity, .. } => {
                        RuntimeRelationTarget::Anonymous(*polarity)
                    }
                    RelationTarget::Predicate {
                        requirement,
                        predicates: tests,
                        ..
                    } => RuntimeRelationTarget::Predicate {
                        requirement: requirement.clone(),
                        predicates: tests
                            .iter()
                            .map(|predicate| {
                                (
                                    predicates.intern(predicate),
                                    crate::fsm::scalar_attributes(predicate)
                                        .cloned()
                                        .map(AttributeLocator::symbolic)
                                        .collect(),
                                )
                            })
                            .collect(),
                    },
                },
                labels: relation.labels.clone(),
                head_attribute: AttributeLocator::symbolic(dependency_head_key()),
                relation_attribute: AttributeLocator::symbolic(dependency_relation_key()),
            })
            .collect();
        let mut assignments = BTreeMap::new();
        count_capture_assignments(&definition.expr, false, &mut assignments);
        let runtime = compile_runtime(&definition.expr, predicates, &relations, &assignments);
        let enhanced_relations = definition
            .enhanced_relations
            .iter()
            .map(|relation| RuntimeEnhancedRelation {
                layer: match relation.layer {
                    DependencyLayer::Basic => RuntimeLayer::Basic {
                        head_attribute: AttributeLocator::symbolic(dependency_head_key()),
                        relation_attribute: AttributeLocator::symbolic(dependency_relation_key()),
                    },
                    DependencyLayer::Enhanced => RuntimeLayer::Enhanced,
                },
                polarity: relation.polarity,
                head: compile_enhanced_endpoint(&relation.head),
                labels: relation.labels.clone(),
                edge: relation.edge.clone(),
                dependent: compile_enhanced_endpoint(&relation.dependent),
            })
            .collect();
        let node_constraints = definition
            .node_constraints
            .iter()
            .map(|order| NodeConstraint {
                operands: order
                    .operands
                    .iter()
                    .map(compile_enhanced_endpoint)
                    .collect(),
                comparison: order.comparison.clone(),
            })
            .collect();
        Self {
            measures: definition
                .measures
                .iter()
                .map(|m| measure::compile(m, predicates))
                .collect(),
            definition,
            runtime,
            captures: captures.into_boxed_slice(),
            relations,
            enhanced_relations,
            node_constraints,
        }
    }

    pub(crate) fn definition(&self) -> &ExecutionDefinition {
        &self.definition
    }

    /// Whether this program enforces any dependency-edge constraint.
    pub(crate) fn has_relations(&self) -> bool {
        !self.relations.is_empty()
    }

    pub(crate) fn has_enhanced_relations(&self) -> bool {
        self.has_graph_relations_on(DependencyLayer::Enhanced)
    }

    pub(crate) fn has_basic_graph_relations(&self) -> bool {
        self.has_graph_relations_on(DependencyLayer::Basic)
    }

    fn has_graph_relations_on(&self, layer: DependencyLayer) -> bool {
        self.definition
            .enhanced_relations
            .iter()
            .any(|relation| relation.layer == layer)
    }

    pub(crate) fn bind_attributes(&mut self, projection: &BoundProjection) {
        bind_runtime_attributes(&mut self.runtime, projection);
        for relation in &mut self.relations {
            for attribute in [
                &mut relation.head_attribute,
                &mut relation.relation_attribute,
            ] {
                let ordinal = projection
                    .ordinal_for_key(attribute.key())
                    .expect("schema binding projected every relation key");
                attribute.bind(ordinal);
            }
            if let RuntimeRelationTarget::Predicate { predicates, .. } = &mut relation.target {
                for attribute in predicates.iter_mut().flat_map(|(_, scalars)| scalars) {
                    let ordinal = projection
                        .ordinal_for_key(attribute.key())
                        .expect("schema binding projected every predicate-target key");
                    attribute.bind(ordinal);
                }
            }
        }
        for relation in &mut self.enhanced_relations {
            if let RuntimeLayer::Basic {
                head_attribute,
                relation_attribute,
            } = &mut relation.layer
            {
                for attribute in [head_attribute, relation_attribute] {
                    let ordinal = projection
                        .ordinal_for_key(attribute.key())
                        .expect("schema binding projected every basic graph-relation key");
                    attribute.bind(ordinal);
                }
            }
            for endpoint in [&mut relation.head, &mut relation.dependent] {
                let RuntimeEnhancedEndpoint::BasicHead { head_attribute, .. } = endpoint else {
                    continue;
                };
                let ordinal = projection
                    .ordinal_for_key(head_attribute.key())
                    .expect("schema binding projected the derived basic-head key");
                head_attribute.bind(ordinal);
            }
        }
    }

    pub(crate) fn extract<S: TokenSequence + ?Sized>(
        &self,
        output: &OutputBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Result<Option<Box<[Capture]>>, MatchResourceError> {
        self.extract_with(output, predicates, tokens, start, end, session)
    }

    fn extract_with<S: TokenSequence + ?Sized, A: AssertionLookup>(
        &self,
        output: &OutputBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        assertions: &mut A,
    ) -> Result<Option<Box<[Capture]>>, MatchResourceError> {
        self.candidates::<
            PREDICATE_SITE_CAPTURE_TOKEN,
            PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN,
            _,
            _,
        >(predicates, tokens, start, assertions)
            .into_iter()
            .find(|candidate| candidate.position == end)
            .map(|candidate| self.public_captures(&candidate, output)).transpose()
    }

    pub(crate) fn extract_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        output: &OutputBudget,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
    ) -> Result<Option<Box<[Capture]>>, MatchResourceError> {
        self.extract_with(output, predicates, tokens, start, end, &mut NoAssertions)
    }

    pub(crate) fn matches<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Result<Vec<ProgramMatch>, MatchResourceError> {
        self.matches_with(budget, predicates, tokens, session, None)
    }

    pub(crate) fn matches_with_graph<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
        graph: &EnhancedGraph,
    ) -> Result<Vec<ProgramMatch>, MatchResourceError> {
        self.matches_with(budget, predicates, tokens, session, Some(graph))
    }

    fn has_node_or_span_filters(&self) -> bool {
        !self.relations.is_empty()
            || !self.enhanced_relations.is_empty()
            || !self.node_constraints.is_empty()
            || !self.definition.span_constraints.is_empty()
    }

    fn span_constraints_hold(
        &self,
        candidate: &Candidate,
        start: usize,
        spans: &[crate::token_match::SentenceSpan],
    ) -> bool {
        use crate::fsm::compiler::model::SpanPosition;
        self.definition.span_constraints.iter().all(|path| {
            spans.iter().any(|leaf| {
                let mut current = Some(leaf);
                path.nodes.iter().all(|operand| {
                    let Some(span) = current else {
                        return false;
                    };
                    current = span.governor.and_then(|id| spans.get(id));
                    let range = match operand.capture {
                        Some(id) => candidate.slots[id.index()],
                        None => Some((start, candidate.position)),
                    };
                    span.kind == path.kind
                        && operand
                            .label
                            .as_ref()
                            .is_none_or(|l| span.label.as_ref() == Some(l))
                        && range.is_some_and(|(a, b)| match operand.position {
                            SpanPosition::Inside => a < b && span.start <= a && b <= span.end,
                            SpanPosition::Whole => (a, b) == (span.start, span.end),
                            SpanPosition::Head => span.head == Some(a) && b - a == 1,
                        })
                })
            })
        })
    }

    fn matches_with<S: TokenSequence + ?Sized, A: AssertionLookup>(
        &self,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        assertions: &mut A,
        enhanced_graph: Option<&EnhancedGraph>,
    ) -> Result<Vec<ProgramMatch>, MatchResourceError> {
        budget.output.check()?;
        let spans = tokens.spans();
        let constraint_count = self
            .definition
            .span_constraints
            .iter()
            .map(|path| path.nodes.len().saturating_sub(1).max(1))
            .sum::<usize>();
        if constraint_count > 0 && spans.is_empty() {
            return Ok(Vec::new());
        }
        let span_probes = spans.len().saturating_mul(constraint_count);
        let mut matches = Vec::new();
        let mut observations = measure::Observations::default();
        let mut basic_observation_usable = None;
        for start in 0..tokens.len() {
            let mut accepted = Vec::new();
            let candidates = self.candidates::<
                PREDICATE_SITE_EVALUATED_TOKEN,
                PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN, _, _
            >(predicates, tokens, start, assertions);
            for candidate in candidates {
                if candidate.position <= start {
                    continue;
                }
                let mut counts = Vec::new();
                let relations_hold = self.relations_hold(
                    predicates,
                    &candidate,
                    tokens,
                    &mut basic_observation_usable,
                    &mut counts,
                );
                if !relations_hold {
                    continue;
                }
                if span_probes > 0 {
                    budget.work(&self.definition.name, span_probes)?;
                    if !self.span_constraints_hold(&candidate, start, spans) {
                        continue;
                    }
                }
                let assignments =
                    self.enhanced_assignments(budget, &candidate, tokens, enhanced_graph)?;
                if !assignments.is_empty() {
                    if !counts.is_empty() {
                        let bytes = counts
                            .iter()
                            .map(|binding| size_of::<GraphBinding>() + binding.name().len())
                            .sum();
                        budget.allocate(&self.definition.name, bytes)?;
                        counts.sort();
                    }
                    accepted.push(AcceptedCandidate {
                        candidate,
                        assignments,
                        counts,
                    });
                }
            }
            accepted.sort_by_key(|hit| std::cmp::Reverse(hit.candidate.position));
            // The longest candidate of each branch selection is published. In
            // an interpreted pattern its other readings join it when they have
            // the same measures, and are published separately when they differ.
            let mut longest = BTreeMap::new();
            let mut measured: Vec<Option<Vec<GraphBinding>>> = Vec::new();
            if !self.measures.is_empty() {
                budget.work(&self.definition.name, accepted.len())?;
                for (index, hit) in accepted.iter().enumerate() {
                    let candidate = &hit.candidate;
                    let (position, first) = *longest
                        .entry(&candidate.any_order_branches)
                        .or_insert((candidate.position, index));
                    let reading =
                        self.definition.interpretation.is_some() && position == candidate.position;
                    measured.push(if reading || first == index {
                        Some(self.measure(
                            candidate,
                            start,
                            &mut observations,
                            predicates,
                            tokens,
                            budget,
                        )?)
                    } else {
                        None
                    });
                }
            }
            let mut seen = BTreeSet::new();
            for (index, hit) in accepted.iter().enumerate() {
                let AcceptedCandidate {
                    candidate,
                    assignments,
                    counts,
                } = hit;
                let measures = measured.get(index).and_then(Option::as_ref);
                let branch_seen = seen
                    .iter()
                    .any(|(branches, _)| *branches == &candidate.any_order_branches);
                if (branch_seen && measures.is_none())
                    || !seen.insert((&candidate.any_order_branches, measures))
                {
                    continue;
                }
                for assignment in assignments {
                    let graph_bytes = if self.enhanced_relations.is_empty() {
                        0
                    } else {
                        self.public_graph_bytes(budget, assignment, tokens, enhanced_graph)?
                    };
                    if !self.enhanced_relations.is_empty() {
                        budget.allocate(
                            self.definition.name.as_str(),
                            graph_bytes
                                .saturating_add(self.definition.name.len())
                                .saturating_add(
                                    self.captures
                                        .iter()
                                        .map(|capture| {
                                            128 + capture.name.len() + capture.path.len()
                                        })
                                        .sum::<usize>(),
                                ),
                        )?;
                    }
                    budget.output.reserve_vec(&mut matches, 1)?;
                    if !assignment.nodes.is_empty() || !assignment.edges.is_empty() {
                        budget.output.reserve(graph_bytes)?;
                    }
                    let (captures, interpretations) = if let Some(recipe) =
                        &self.definition.interpretation
                    {
                        self.interpreted_candidates(
                            recipe,
                            budget,
                            predicates,
                            tokens,
                            start..candidate.position,
                            |reading, budget| {
                                for (other_index, other) in accepted.iter().enumerate() {
                                    budget.interpretation(
                                        &self.definition.name,
                                        1 + reading.slots.len(),
                                        0,
                                    )?;
                                    if other.candidate.position == reading.position
                                        && other.candidate.slots == reading.slots
                                        && other.counts == *counts
                                        && measured.get(other_index).and_then(Option::as_ref)
                                            == measures
                                    {
                                        let comparisons = other.assignments.len().saturating_mul(
                                            1 + assignment.nodes.len() + assignment.edges.len(),
                                        );
                                        budget.interpretation(
                                            &self.definition.name,
                                            comparisons,
                                            0,
                                        )?;
                                        return Ok(other.assignments.contains(assignment));
                                    }
                                }
                                Ok(false)
                            },
                        )?
                    } else {
                        (
                            self.public_captures(candidate, &budget.output)?,
                            Box::default(),
                        )
                    };
                    let mut graph_bindings =
                        self.public_graph_bindings(assignment, tokens, enhanced_graph);
                    if !counts.is_empty() {
                        budget
                            .output
                            .reserve_buffer::<GraphBinding>(counts.len() + graph_bindings.len())?;
                        budget
                            .output
                            .reserve(counts.iter().map(|binding| binding.name().len()).sum())?;
                        let mut bindings = graph_bindings.into_vec();
                        bindings.extend(counts.iter().cloned());
                        graph_bindings = bindings.into_boxed_slice();
                    }
                    if let Some(measures) = measures {
                        budget.output.reserve_buffer::<GraphBinding>(
                            measures.len() + graph_bindings.len(),
                        )?;
                        budget
                            .output
                            .reserve(measures.iter().map(|m| m.name().len()).sum())?;
                        let mut bindings = graph_bindings.into_vec();
                        bindings.extend(measures.iter().cloned());
                        bindings.sort();
                        graph_bindings = bindings.into_boxed_slice();
                    }
                    matches.push(ProgramMatch {
                        interpretations,
                        start,
                        end: candidate.position,
                        captures,
                        graph_bindings,
                    });
                }
            }
        }
        Ok(matches)
    }

    /// Whether every `where` edge holds for one accepted candidate.
    ///
    /// `$head -[labels]-> $dependent` holds when the dependent token's
    /// `dep.rel` is matched by some member of the label set and its
    /// `dep.head` — CoNLL-U's one-based, sentence-local HEAD — designates the
    /// head token. A token carries exactly one `dep.rel`, so at most one
    /// member can match; the set is a disjunction with no ambiguity to
    /// resolve, and the matched label stays readable as the dependent
    /// capture's own `dep.rel`.
    ///
    /// The HEAD parse happens here, once per check, so a formatting oddity
    /// such as `"00"` cannot produce a string-inequality false negative. A
    /// token missing either attribute, or carrying an unparseable HEAD, fails
    /// the constraint: the existing three-valued attribute rule collapsed to
    /// a filter, never a match-time diagnostic.
    fn relations_hold<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        candidate: &Candidate,
        tokens: &S,
        basic_observation_usable: &mut Option<bool>,
        counts: &mut Vec<GraphBinding>,
    ) -> bool {
        self.relations.iter().all(|relation| {
            let Some((head_start, _)) = candidate.slots[relation.head.index()] else {
                return false;
            };
            match &relation.target {
                RuntimeRelationTarget::Captured {
                    capture: dependent,
                    polarity,
                } => {
                    let Some((dependent_start, _)) = candidate.slots[dependent.index()] else {
                        return false;
                    };
                    if *polarity == RelationPolarity::Negative
                        && !*basic_observation_usable.get_or_insert_with(|| {
                            dependency_observation_is_usable(relation, tokens)
                        })
                    {
                        return false;
                    }
                    let exists =
                        relation_matches_token(relation, tokens, dependent_start, head_start);
                    match polarity {
                        RelationPolarity::Positive => exists,
                        RelationPolarity::Negative => !exists,
                    }
                }
                RuntimeRelationTarget::Anonymous(polarity) => {
                    if !dependency_observation_is_usable(relation, tokens) {
                        return false;
                    }
                    let exists = (0..tokens.len()).any(|dependent_start| {
                        relation_matches_token(relation, tokens, dependent_start, head_start)
                    });
                    match polarity {
                        RelationPolarity::Positive => exists,
                        RelationPolarity::Negative => !exists,
                    }
                }
                RuntimeRelationTarget::Predicate {
                    requirement,
                    predicates: target_predicates,
                } => {
                    let mut unknown = || {
                        if let DependentRequirement::Measure(name) = requirement {
                            counts.push(GraphBinding::Count {
                                name: name.clone(),
                                value: None,
                            });
                            true
                        } else {
                            false
                        }
                    };
                    if !*basic_observation_usable
                        .get_or_insert_with(|| dependency_observation_is_usable(relation, tokens))
                    {
                        return unknown();
                    }
                    let mut count = 0;
                    for dependent_start in 0..tokens.len() {
                        if !relation_matches_token(relation, tokens, dependent_start, head_start) {
                            continue;
                        }
                        let witness = target_predicates.iter().any(|(predicate, _)| {
                            predicates.matches_at_site::<PREDICATE_SITE_EVALUATED_TOKEN, _>(
                                *predicate,
                                tokens,
                                dependent_start,
                            )
                        });
                        if !witness
                            && *requirement != DependentRequirement::Exists
                            && target_predicates
                                .iter()
                                .flat_map(|(_, scalars)| scalars)
                                .any(|attribute| {
                                    tokens.value_locator(dependent_start, attribute).is_none()
                                })
                        {
                            return unknown();
                        }
                        if witness {
                            match requirement {
                                DependentRequirement::Exists => return true,
                                DependentRequirement::Absent => return false,
                                DependentRequirement::Count { .. }
                                | DependentRequirement::Measure(_) => count += 1,
                            }
                        }
                    }
                    if let Some(name) = requirement.binding() {
                        counts.push(GraphBinding::Count {
                            name: name.clone(),
                            value: Some(count),
                        });
                    }
                    match requirement {
                        DependentRequirement::Exists => false,
                        DependentRequirement::Absent | DependentRequirement::Measure(_) => true,
                        DependentRequirement::Count { min, max, .. } => {
                            count >= *min && max.is_none_or(|upper| count <= upper)
                        }
                    }
                }
            }
        })
    }

    /// The measures of one candidate, read from its own captures.
    fn measure<'a, S: TokenSequence + ?Sized>(
        &'a self,
        candidate: &Candidate,
        start: usize,
        observations: &mut measure::Observations<'a>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        budget: &mut MatchBudget<'_>,
    ) -> Result<Vec<GraphBinding>, MatchResourceError> {
        budget.allocate(
            &self.definition.name,
            self.measures
                .iter()
                .map(|m| size_of::<GraphBinding>() + m.name.len())
                .sum(),
        )?;
        let mut bindings = Vec::with_capacity(self.measures.len());
        for m in &self.measures {
            observations.charge(
                m,
                candidate.position - start,
                tokens,
                budget,
                &self.definition.name,
            )?;
            let value = observations
                .observations(
                    m,
                    start..candidate.position,
                    |id| candidate.slots[id.index()].map(|slot| slot.0),
                    predicates,
                    tokens,
                )
                .map_or_else(|missing| missing, |values| values.reduce(m.reducer));
            bindings.push(GraphBinding::Measure {
                name: m.name.clone(),
                value,
            });
        }
        Ok(bindings)
    }

    fn enhanced_assignments<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        candidate: &Candidate,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Result<Vec<GraphAssignment>, MatchResourceError> {
        if self.enhanced_relations.is_empty() {
            let assignment = GraphAssignment::default();
            return Ok(self
                .node_constraints_hold(candidate, tokens, &assignment)
                .then_some(assignment)
                .into_iter()
                .collect());
        }
        if graph.is_none() && self.has_enhanced_relations() {
            return Ok(Vec::new());
        }
        let mut assignments = enhanced_assignments_from(
            budget,
            self.definition.name.as_str(),
            &self.enhanced_relations,
            0,
            candidate,
            tokens,
            graph,
            &GraphAssignment::default(),
            &|partial| {
                #[cfg(test)]
                if tests::UNPRUNED_ORACLE.get() {
                    return true;
                }
                basic_binders_are_distinct(&self.enhanced_relations, candidate, partial)
                    && self.partial_order_holds(candidate, tokens, partial)
            },
        )?;
        assignments.retain(|assignment| {
            basic_binders_are_distinct(&self.enhanced_relations, candidate, assignment)
                && self.node_constraints_hold(candidate, tokens, assignment)
        });
        let mut keyed = assignments
            .into_iter()
            .map(|assignment| {
                let graph_bytes = self.public_graph_bytes(budget, &assignment, tokens, graph)?;
                budget.allocate(self.definition.name.as_str(), graph_bytes)?;
                Ok((
                    self.public_graph_bindings(&assignment, tokens, graph),
                    assignment,
                ))
            })
            .collect::<Result<Vec<_>, MatchResourceError>>()?;
        keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
        keyed.dedup_by(|(left, _), (right, _)| left == right);
        Ok(keyed
            .into_iter()
            .map(|(_, assignment)| assignment)
            .collect())
    }

    fn node_constraints_hold<S: TokenSequence + ?Sized>(
        &self,
        candidate: &Candidate,
        tokens: &S,
        assignment: &GraphAssignment,
    ) -> bool {
        self.node_constraints.iter().all(|order| {
            if let NodeComparison::Attributes { keys, equal } = &order.comparison {
                let value = |index: usize| {
                    let EnhancedNodeId::Word { token_index } = enhanced_endpoint_assignment(
                        &order.operands[index],
                        candidate,
                        tokens,
                        assignment,
                    )?
                    else {
                        return None;
                    };
                    tokens.scalar_key(token_index, &keys[index])
                };
                return value(0)
                    .zip(value(1))
                    .is_some_and(|(left, right)| (left == right) == *equal);
            }
            let nodes = order
                .operands
                .iter()
                .map(|operand| {
                    enhanced_endpoint_assignment(operand, candidate, tokens, assignment)
                        .map(EnhancedNodeId::source_order_key)
                })
                .collect::<Option<Vec<_>>>();
            nodes.is_some_and(|nodes| nodes.is_sorted_by(|before, after| before < after))
        })
    }

    fn partial_order_holds<S: TokenSequence + ?Sized>(
        &self,
        candidate: &Candidate,
        tokens: &S,
        assignment: &GraphAssignment,
    ) -> bool {
        self.node_constraints.iter().all(|order| {
            if !matches!(order.comparison, NodeComparison::SourceOrder) {
                return true;
            }
            order
                .operands
                .iter()
                .filter_map(|operand| {
                    enhanced_endpoint_assignment(operand, candidate, tokens, assignment)
                        .map(EnhancedNodeId::source_order_key)
                })
                .is_sorted_by(|before, after| before < after)
        })
    }

    pub(crate) fn matches_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Vec<ProgramMatch>, MatchResourceError> {
        self.matches_with(budget, predicates, tokens, &mut NoAssertions, None)
    }

    pub(crate) fn matches_without_assertions_with_graph<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: &EnhancedGraph,
    ) -> Result<Vec<ProgramMatch>, MatchResourceError> {
        self.matches_with(budget, predicates, tokens, &mut NoAssertions, Some(graph))
    }

    pub(crate) fn compared_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        self.node_constraints
            .iter()
            .filter_map(|constraint| match &constraint.comparison {
                NodeComparison::Attributes { keys, .. } => Some(keys),
                NodeComparison::SourceOrder => None,
            })
            .flatten()
            .cloned()
    }

    pub(crate) fn required_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        let mut attributes = Vec::new();
        collect_attributes(&self.definition.expr, &mut attributes);
        for m in &self.definition.measures {
            use crate::fsm::compiler::model::{MeasureSource, TreeLayer};
            if matches!(
                m.source,
                MeasureSource::Links { .. }
                    | MeasureSource::Tree(TreeLayer::Word | TreeLayer::Bunsetsu, _)
                    | MeasureSource::Depth(TreeLayer::Word | TreeLayer::Bunsetsu, _)
            ) {
                attributes.extend([dependency_head_key(), dependency_relation_key()]);
            }
            if let Some(test) = m.source.test() {
                attributes.extend(test.iter().flatten().map(|a| a.attr.clone()));
            }
        }
        // The edge attributes join the projection exactly as lookaround-only
        // attributes do — and this is also the bind-time admission rule: a
        // relation-bearing catalog cannot bind against a schema that lacks
        // them, which `SchemaBindError::MissingKeys` reports by name before
        // any matching begins.
        if !self.relations.is_empty() {
            attributes.push(dependency_head_key());
            attributes.push(dependency_relation_key());
            for relation in &self.definition.relations {
                if let RelationTarget::Predicate { predicates, .. } = &relation.target {
                    for predicate in predicates {
                        attributes.extend(predicate.iter().map(|atom| atom.attr.clone()));
                    }
                }
            }
        }
        if self.has_enhanced_relations() {
            attributes.push(enhanced_graph_key());
        }
        if self.has_basic_graph_relations() {
            attributes.push(dependency_head_key());
            attributes.push(dependency_relation_key());
        }
        if !self.enhanced_relations.is_empty() {
            for relation in &self.definition.enhanced_relations {
                for endpoint in [&relation.head, &relation.dependent] {
                    match endpoint {
                        ExecutionEnhancedEndpoint::Graph { predicates, .. } => {
                            for predicate in predicates {
                                attributes.extend(
                                    predicate
                                        .iter()
                                        .filter(|atom| {
                                            !matches!(atom.attr.as_str(), "node.kind" | "node.id")
                                        })
                                        .map(|atom| atom.attr.clone()),
                                );
                            }
                        }
                        ExecutionEnhancedEndpoint::BasicHead(_) => {
                            attributes.push(dependency_head_key());
                        }
                        ExecutionEnhancedEndpoint::Surface(_) => {}
                    }
                }
            }
        }
        attributes.into_iter().chain(self.compared_attributes())
    }

    pub(crate) fn has_basic_head_endpoints(&self) -> bool {
        self.definition.enhanced_relations.iter().any(|relation| {
            [&relation.head, &relation.dependent]
                .into_iter()
                .any(|endpoint| matches!(endpoint, ExecutionEnhancedEndpoint::BasicHead(_)))
        })
    }

    pub(crate) fn comparison(&self) -> Option<(&str, AttributeKey)> {
        let (capture, projection) = first_same_as(&self.definition.expr)?;
        let descriptor = self
            .captures
            .iter()
            .find(|descriptor| descriptor.id == capture)?;
        Some((descriptor.path.as_str(), projection.attribute().clone()))
    }

    fn candidates<
        const TOKEN_SITE: usize,
        const PREVIOUS_SITE: usize,
        S: TokenSequence + ?Sized,
        A: AssertionLookup,
    >(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        assertions: &mut A,
    ) -> Vec<Candidate> {
        let initial = Candidate {
            position: start,
            slots: vec![None; self.captures.len()],
            any_order_branches: Vec::new(),
        };
        evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
            &self.runtime,
            predicates,
            tokens,
            initial,
            assertions,
        )
    }

    fn public_captures(
        &self,
        candidate: &Candidate,
        output: &OutputBudget,
    ) -> Result<Box<[Capture]>, MatchResourceError> {
        let count = self
            .captures
            .iter()
            .filter(|descriptor| candidate.slots[descriptor.id.index()].is_some())
            .count();
        output.reserve_buffer::<Capture>(count.saturating_mul(2))?;
        let heap_bytes = self
            .captures
            .iter()
            .filter(|descriptor| candidate.slots[descriptor.id.index()].is_some())
            .fold(0usize, |bytes, descriptor| {
                bytes
                    .saturating_add(compact_string_heap_bytes(&descriptor.name))
                    .saturating_add(compact_string_heap_bytes(&descriptor.path))
            });
        output.reserve(heap_bytes)?;
        let mut captures = Vec::with_capacity(count);
        for descriptor in &self.captures {
            if let Some((start, end)) = candidate.slots[descriptor.id.index()] {
                captures.push(Capture {
                    name: descriptor.name.clone(),
                    path: descriptor.path.clone(),
                    start,
                    end,
                });
            }
        }
        captures.sort_by_key(|capture| (capture.start, capture.end));
        Ok(captures.into_boxed_slice())
    }

    fn public_graph_bytes<S: TokenSequence + ?Sized>(
        &self,
        budget: &mut MatchBudget<'_>,
        assignment: &GraphAssignment,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Result<usize, MatchResourceError> {
        for node in assignment.nodes.values() {
            let probes = match node {
                EnhancedNodeId::Word { .. } => 10,
                EnhancedNodeId::Empty { .. } => graph
                    .map_or(0, |graph| graph.empty_nodes().len())
                    .saturating_mul(2)
                    .saturating_add(10),
            };
            budget.work(self.definition.name.as_str(), probes)?;
        }
        Ok(assignment
            .nodes
            .values()
            .fold(assignment.estimated_bytes(), |bytes, node| {
                let values = match node {
                    EnhancedNodeId::Word { token_index } => {
                        ["text", "lemma", "norm", "upos", "xpos"]
                            .into_iter()
                            .filter_map(|name| {
                                tokens.scalar_key(
                                    *token_index,
                                    &AttributeKey::new(name).expect("canonical graph key"),
                                )
                            })
                            .map(str::len)
                            .sum::<usize>()
                    }
                    EnhancedNodeId::Empty { .. } => graph
                        .and_then(|graph| {
                            graph.empty_nodes().iter().find(|empty| empty.id() == *node)
                        })
                        .map_or(0, |empty| {
                            [empty.form(), empty.lemma(), empty.upos(), empty.xpos()]
                                .into_iter()
                                .flatten()
                                .map(str::len)
                                .sum()
                        }),
                };
                bytes.saturating_add(1024).saturating_add(values)
            }))
    }

    fn public_graph_bindings<S: TokenSequence + ?Sized>(
        &self,
        assignment: &GraphAssignment,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Box<[GraphBinding]> {
        self.definition
            .graph_bindings
            .iter()
            .filter_map(|declaration| match declaration {
                ExecutionGraphBindingDecl::Node(name) => {
                    let node = *assignment.nodes.get(name)?;
                    Some(GraphBinding::Node {
                        name: name.clone(),
                        node,
                        attributes: graph_node_scalar_attributes(node, tokens, graph),
                    })
                }
                ExecutionGraphBindingDecl::Edge(name) => {
                    let (head, dependent, relation) = assignment.edges.get(name)?;
                    Some(GraphBinding::Edge {
                        name: name.clone(),
                        head: *head,
                        dependent: *dependent,
                        relation: relation.clone(),
                    })
                }
            })
            .collect()
    }
}

fn dependency_observation_is_usable<S: TokenSequence + ?Sized>(
    relation: &RuntimeRelation,
    tokens: &S,
) -> bool {
    (0..tokens.len()).all(|index| {
        let Some(AttributeValueRef::Scalar(label)) =
            tokens.value_locator(index, &relation.relation_attribute)
        else {
            return false;
        };
        if label.is_empty() || label == "_" {
            return false;
        }
        let Some(AttributeValueRef::Scalar(head)) =
            tokens.value_locator(index, &relation.head_attribute)
        else {
            return false;
        };
        if head == "_" {
            return false;
        }
        head.parse::<usize>().is_ok_and(|head| head <= tokens.len())
    })
}

fn relation_matches_token<S: TokenSequence + ?Sized>(
    relation: &RuntimeRelation,
    tokens: &S,
    dependent_start: usize,
    head_start: usize,
) -> bool {
    let Some(AttributeValueRef::Scalar(label)) =
        tokens.value_locator(dependent_start, &relation.relation_attribute)
    else {
        return false;
    };
    let label_matches = match &relation.labels {
        RelationLabels::Any => true,
        RelationLabels::Listed(labels) => labels.iter().any(|candidate| candidate.matches(label)),
    };
    if !label_matches {
        return false;
    }
    let Some(AttributeValueRef::Scalar(head)) =
        tokens.value_locator(dependent_start, &relation.head_attribute)
    else {
        return false;
    };
    head.parse::<usize>()
        .is_ok_and(|head| head == head_start + 1)
}

pub(crate) fn dependency_head_key() -> AttributeKey {
    AttributeKey::new("dep.head").expect("dep.head is a canonical attribute key")
}

pub(crate) fn dependency_relation_key() -> AttributeKey {
    AttributeKey::new("dep.rel").expect("dep.rel is a canonical attribute key")
}

pub(crate) fn enhanced_graph_key() -> AttributeKey {
    AttributeKey::new("dep.enhanced").expect("dep.enhanced is a canonical attribute key")
}

fn compile_enhanced_endpoint(endpoint: &ExecutionEnhancedEndpoint) -> RuntimeEnhancedEndpoint {
    match endpoint {
        ExecutionEnhancedEndpoint::Surface(capture) => RuntimeEnhancedEndpoint::Surface(*capture),
        ExecutionEnhancedEndpoint::BasicHead(dependent) => RuntimeEnhancedEndpoint::BasicHead {
            dependent: *dependent,
            head_attribute: AttributeLocator::symbolic(dependency_head_key()),
        },
        ExecutionEnhancedEndpoint::Graph {
            name,
            predicates: tests,
        } => RuntimeEnhancedEndpoint::Graph {
            name: name.clone(),
            symbolic: tests.clone(),
            members: tests
                .iter()
                .map(|predicate| {
                    predicate
                        .iter()
                        .map(|atom| match &atom.value {
                            AttrValueType::OneOf(values) | AttrValueType::NotIn(values) => {
                                Some(Members::new(values))
                            }
                            _ => None,
                        })
                        .collect()
                })
                .collect(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn enhanced_assignments_from<S: TokenSequence + ?Sized>(
    budget: &mut MatchBudget<'_>,
    pattern: &str,
    relations: &[RuntimeEnhancedRelation],
    index: usize,
    candidate: &Candidate,
    tokens: &S,
    graph: Option<&EnhancedGraph>,
    assignment: &GraphAssignment,
    admissible: &dyn Fn(&GraphAssignment) -> bool,
) -> Result<Vec<GraphAssignment>, MatchResourceError> {
    budget.output.check()?;
    if index > MAX_RELATION_DEPTH {
        return Err(MatchResourceError::Sentence {
            pattern: pattern.into(),
            resource: "graph relation depth",
            limit: MAX_RELATION_DEPTH,
        });
    }
    if index == relations.len() {
        if enhanced_negative_relations_hold(
            budget, pattern, relations, candidate, tokens, graph, assignment,
        )? {
            budget.assignment(pattern, assignment.estimated_bytes())?;
            return Ok(vec![assignment.clone()]);
        }
        return Ok(Vec::new());
    }
    let relation = &relations[index];
    if relation.polarity == RelationPolarity::Negative {
        return enhanced_assignments_from(
            budget,
            pattern,
            relations,
            index + 1,
            candidate,
            tokens,
            graph,
            assignment,
            admissible,
        );
    }
    let mut assignments = Vec::new();
    for edge in layer_edges(&relation.layer, tokens, graph) {
        budget.step(pattern)?;
        let Some((head, dependent, label)) = edge else {
            continue;
        };
        if !relation_label_matches(&relation.labels, label) {
            continue;
        }
        budget.allocate(pattern, assignment.estimated_bytes())?;
        let mut next = assignment.clone();
        if !enhanced_endpoint_accepts(
            budget,
            pattern,
            &relation.head,
            head,
            candidate,
            tokens,
            graph,
            &mut next.nodes,
        )? || !enhanced_endpoint_accepts(
            budget,
            pattern,
            &relation.dependent,
            dependent,
            candidate,
            tokens,
            graph,
            &mut next.nodes,
        )? {
            continue;
        }
        if let Some(name) = &relation.edge {
            let before = btree_storage_bytes::<
                compact_str::CompactString,
                (EnhancedNodeId, EnhancedNodeId, compact_str::CompactString),
            >(next.edges.len());
            let after =
                btree_storage_bytes::<
                    compact_str::CompactString,
                    (EnhancedNodeId, EnhancedNodeId, compact_str::CompactString),
                >(next.edges.len() + usize::from(!next.edges.contains_key(name)));
            budget.allocate(
                pattern,
                after
                    .saturating_sub(before)
                    .saturating_add(compact_string_heap_bytes(name))
                    .saturating_add(label.len()),
            )?;
            let value = (head, dependent, label.into());
            if next
                .edges
                .get(name)
                .is_some_and(|assigned| *assigned != value)
            {
                continue;
            }
            next.edges.insert(name.clone(), value);
        }
        if !admissible(&next) {
            continue;
        }
        assignments.extend(enhanced_assignments_from(
            budget,
            pattern,
            relations,
            index + 1,
            candidate,
            tokens,
            graph,
            &next,
            admissible,
        )?);
    }
    Ok(assignments)
}

fn basic_binders_are_distinct(
    relations: &[RuntimeEnhancedRelation],
    candidate: &Candidate,
    assignment: &GraphAssignment,
) -> bool {
    let mut binders = BTreeSet::new();
    let mut surface = BTreeSet::new();
    for relation in relations
        .iter()
        .filter(|relation| matches!(relation.layer, RuntimeLayer::Basic { .. }))
    {
        for endpoint in [&relation.head, &relation.dependent] {
            match endpoint {
                RuntimeEnhancedEndpoint::Graph { name, .. } => {
                    binders.insert(name);
                }
                RuntimeEnhancedEndpoint::Surface(capture) => {
                    surface.extend(candidate.slots[capture.index()].map(|(start, _)| start));
                }
                RuntimeEnhancedEndpoint::BasicHead { .. } => {}
            }
        }
    }
    let mut occupied = surface
        .into_iter()
        .map(|token_index| EnhancedNodeId::Word { token_index })
        .collect::<BTreeSet<_>>();
    binders
        .into_iter()
        .filter_map(|name| assignment.nodes.get(name))
        .all(|node| occupied.insert(*node))
}

/// Yields one item per source row so the caller also charges skipped and root edges.
fn layer_edges<'a, S: TokenSequence + ?Sized>(
    layer: &'a RuntimeLayer,
    tokens: &'a S,
    graph: Option<&'a EnhancedGraph>,
) -> Box<dyn Iterator<Item = Option<(EnhancedNodeId, EnhancedNodeId, &'a str)>> + 'a> {
    match layer {
        RuntimeLayer::Basic {
            head_attribute,
            relation_attribute,
        } => Box::new((0..tokens.len()).map(|token_index| {
            let Some(AttributeValueRef::Scalar(head)) =
                tokens.value_locator(token_index, head_attribute)
            else {
                return None;
            };
            let head = head.parse::<usize>().ok()?;
            if !(1..=tokens.len()).contains(&head) {
                return None;
            }
            let Some(AttributeValueRef::Scalar(label)) =
                tokens.value_locator(token_index, relation_attribute)
            else {
                return None;
            };
            Some((
                EnhancedNodeId::Word {
                    token_index: head - 1,
                },
                EnhancedNodeId::Word { token_index },
                label,
            ))
        })),
        RuntimeLayer::Enhanced => Box::new(graph.into_iter().flat_map(EnhancedGraph::edges).map(
            |edge| match edge.head() {
                EnhancedHead::Node(head) => Some((head, edge.dependent(), edge.relation())),
                EnhancedHead::Root => None,
            },
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn enhanced_negative_relations_hold<S: TokenSequence + ?Sized>(
    budget: &mut MatchBudget<'_>,
    pattern: &str,
    relations: &[RuntimeEnhancedRelation],
    candidate: &Candidate,
    tokens: &S,
    graph: Option<&EnhancedGraph>,
    assignment: &GraphAssignment,
) -> Result<bool, MatchResourceError> {
    for relation in relations
        .iter()
        .filter(|relation| relation.polarity == RelationPolarity::Negative)
    {
        let Some(head) =
            enhanced_endpoint_assignment(&relation.head, candidate, tokens, assignment)
        else {
            return Ok(false);
        };
        let Some(dependent) =
            enhanced_endpoint_assignment(&relation.dependent, candidate, tokens, assignment)
        else {
            return Ok(false);
        };
        for edge in layer_edges(&relation.layer, tokens, graph) {
            budget.step(pattern)?;
            let Some((actual_head, actual_dependent, label)) = edge else {
                continue;
            };
            if actual_head == head
                && actual_dependent == dependent
                && relation_label_matches(&relation.labels, label)
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn enhanced_endpoint_assignment<S: TokenSequence + ?Sized>(
    endpoint: &RuntimeEnhancedEndpoint,
    candidate: &Candidate,
    tokens: &S,
    assignment: &GraphAssignment,
) -> Option<EnhancedNodeId> {
    match endpoint {
        RuntimeEnhancedEndpoint::Surface(capture) => candidate.slots[capture.index()]
            .and_then(|(start, end)| (end == start + 1).then_some(start))
            .map(|token_index| EnhancedNodeId::Word { token_index }),
        RuntimeEnhancedEndpoint::BasicHead {
            dependent,
            head_attribute,
        } => derived_basic_head(*dependent, head_attribute, candidate, tokens),
        RuntimeEnhancedEndpoint::Graph { name, .. } => assignment.nodes.get(name).copied(),
    }
}

fn relation_label_matches(labels: &RelationLabels<RelationLabel>, actual: &str) -> bool {
    match labels {
        RelationLabels::Any => true,
        RelationLabels::Listed(labels) => labels.iter().any(|label| label.matches(actual)),
    }
}

#[allow(clippy::too_many_arguments)]
fn enhanced_endpoint_accepts<S: TokenSequence + ?Sized>(
    budget: &mut MatchBudget<'_>,
    pattern: &str,
    endpoint: &RuntimeEnhancedEndpoint,
    node: EnhancedNodeId,
    candidate: &Candidate,
    tokens: &S,
    graph: Option<&EnhancedGraph>,
    assignments: &mut BTreeMap<compact_str::CompactString, EnhancedNodeId>,
) -> Result<bool, MatchResourceError> {
    Ok(match endpoint {
        RuntimeEnhancedEndpoint::Surface(capture) => {
            candidate.slots[capture.index()].is_some_and(|(start, end)| {
                end == start + 1 && node == EnhancedNodeId::Word { token_index: start }
            })
        }
        RuntimeEnhancedEndpoint::BasicHead {
            dependent,
            head_attribute,
        } => derived_basic_head(*dependent, head_attribute, candidate, tokens) == Some(node),
        RuntimeEnhancedEndpoint::Graph {
            name,
            symbolic,
            members,
        } => {
            if assignments
                .get(name)
                .is_some_and(|assigned| *assigned != node)
            {
                return Ok(false);
            }
            let mut matches = false;
            for (predicate, members) in symbolic.iter().zip(members) {
                if matches!(node, EnhancedNodeId::Empty { .. }) {
                    budget.work(pattern, graph.map_or(0, |graph| graph.empty_nodes().len()))?;
                }
                if graph_node_predicate_matches(node, tokens, graph, predicate, members) {
                    matches = true;
                    break;
                }
            }
            if matches && !assignments.contains_key(name) {
                let before = btree_storage_bytes::<compact_str::CompactString, EnhancedNodeId>(
                    assignments.len(),
                );
                let after = btree_storage_bytes::<compact_str::CompactString, EnhancedNodeId>(
                    assignments.len() + 1,
                );
                budget.allocate(
                    pattern,
                    after
                        .saturating_sub(before)
                        .saturating_add(compact_string_heap_bytes(name)),
                )?;
                assignments.insert(name.clone(), node);
            }
            matches
        }
    })
}

fn derived_basic_head<S: TokenSequence + ?Sized>(
    dependent: CaptureId,
    head_attribute: &AttributeLocator,
    candidate: &Candidate,
    tokens: &S,
) -> Option<EnhancedNodeId> {
    let (dependent_start, dependent_end) = candidate.slots[dependent.index()]?;
    if dependent_end != dependent_start + 1 {
        return None;
    }
    let AttributeValueRef::Scalar(head) = tokens.value_locator(dependent_start, head_attribute)?
    else {
        return None;
    };
    let one_based = head.parse::<usize>().ok()?;
    (1..=tokens.len())
        .contains(&one_based)
        .then(|| EnhancedNodeId::Word {
            token_index: one_based - 1,
        })
}

fn graph_node_predicate_matches<S: TokenSequence + ?Sized>(
    node: EnhancedNodeId,
    tokens: &S,
    graph: Option<&EnhancedGraph>,
    predicate: &TokenPredicate,
    members: &[Option<Members>],
) -> bool {
    let empty = match node {
        EnhancedNodeId::Word { .. } => None,
        EnhancedNodeId::Empty { .. } => {
            graph.and_then(|graph| graph.empty_nodes().iter().find(|empty| empty.id() == node))
        }
    };
    predicate.iter().zip(members).all(|(atom, members)| {
        let scalar_value_matches =
            |actual: &str, expected: &AttrValueType| match (expected, members) {
                (AttrValueType::OneOf(_), Some(members)) => members.contains(actual),
                (AttrValueType::NotIn(_), Some(members)) => !members.contains(actual),
                _ => scalar_value_matches(actual, expected),
            };
        let held = match atom.attr.as_str() {
            "node.kind" => {
                scalar_value_matches(if empty.is_some() { "empty" } else { "word" }, &atom.value)
            }
            "node.id" => scalar_value_matches(&node.spelling(), &atom.value),
            key => match node {
                EnhancedNodeId::Word { token_index } => tokens
                    .value_key(token_index, &atom.attr)
                    .is_some_and(|actual| match actual {
                        AttributeValueRef::Scalar(actual) => {
                            scalar_value_matches(actual, &atom.value)
                        }
                        AttributeValueRef::StringSet(_) => {
                            attribute_value_matches(actual, &atom.value)
                        }
                    }),
                EnhancedNodeId::Empty { .. } => empty.is_some_and(|empty| match key {
                    "text" => empty.form().is_some_and(|actual| {
                        scalar_value_matches(&crate::analysis::text_key(actual), &atom.value)
                    }),
                    "surface" => empty
                        .form()
                        .is_some_and(|actual| scalar_value_matches(actual, &atom.value)),
                    "lemma" => empty
                        .lemma()
                        .is_some_and(|actual| scalar_value_matches(actual, &atom.value)),
                    "upos" => empty
                        .upos()
                        .is_some_and(|actual| scalar_value_matches(actual, &atom.value)),
                    "xpos" => empty
                        .xpos()
                        .is_some_and(|actual| scalar_value_matches(actual, &atom.value)),
                    "conllu.feats" => set_value_matches(empty.raw_features(), &atom.value),
                    "conllu.misc" => set_value_matches(empty.misc(), &atom.value),
                    key if key.starts_with("feat.") => empty
                        .feature_values(key.strip_prefix("feat.").expect("prefix checked"))
                        .is_some_and(|values| set_value_matches(values, &atom.value)),
                    _ => false,
                }),
            },
        };
        held != atom.complement
    })
}

fn attribute_value_matches(actual: AttributeValueRef<'_>, expected: &AttrValueType) -> bool {
    match actual {
        AttributeValueRef::Scalar(actual) => scalar_value_matches(actual, expected),
        AttributeValueRef::StringSet(actual) => set_value_matches(
            actual.iter().map(compact_str::CompactString::as_str),
            expected,
        ),
    }
}

fn scalar_value_matches(actual: &str, expected: &AttrValueType) -> bool {
    match expected {
        AttrValueType::Single(value) => actual == value,
        AttrValueType::OneOf(values) => values.iter().any(|value| actual == value),
        AttrValueType::NotIn(values) => values.iter().all(|value| actual != value),
        AttrValueType::Contains(value) => actual.contains(value.as_str()),
        AttrValueType::NotContains(value) => !actual.contains(value.as_str()),
        AttrValueType::Regex(regex) => regex.is_match(actual),
        AttrValueType::NotRegex(regex) => !regex.is_match(actual),
        AttrValueType::SetHas(_)
        | AttrValueType::SetHasAny(_)
        | AttrValueType::SetNotHas(_)
        | AttrValueType::SetNotHasAny(_) => false,
    }
}

fn set_value_matches<'a>(actual: impl Iterator<Item = &'a str>, expected: &AttrValueType) -> bool {
    let actual = actual.collect::<Vec<_>>();
    match expected {
        AttrValueType::SetHas(value) => actual.iter().any(|item| *item == value),
        AttrValueType::SetHasAny(values) => actual
            .iter()
            .any(|item| values.iter().any(|value| item == value)),
        AttrValueType::SetNotHas(value) => actual.iter().all(|item| *item != value),
        AttrValueType::SetNotHasAny(values) => actual
            .iter()
            .all(|item| values.iter().all(|value| item != value)),
        // Existential over entries, mirroring `has`/`not has` above: an
        // entry is a string, so a containment or regex test has an answer
        // about it.
        AttrValueType::Contains(needle) => actual.iter().any(|item| item.contains(needle.as_str())),
        AttrValueType::NotContains(needle) => {
            actual.iter().all(|item| !item.contains(needle.as_str()))
        }
        AttrValueType::Regex(regex) => actual.iter().any(|item| regex.is_match(item)),
        AttrValueType::NotRegex(regex) => actual.iter().all(|item| !regex.is_match(item)),
        AttrValueType::Single(_) | AttrValueType::OneOf(_) | AttrValueType::NotIn(_) => false,
    }
}

fn graph_node_scalar_attributes<S: TokenSequence + ?Sized>(
    node: EnhancedNodeId,
    tokens: &S,
    graph: Option<&EnhancedGraph>,
) -> Box<[GraphNodeAttribute]> {
    const SCALARS: [&str; 5] = ["text", "lemma", "norm", "upos", "xpos"];
    let mut attributes = Vec::new();
    attributes.push(GraphNodeAttribute {
        name: AttributeKey::new("node.kind").expect("node.kind is canonical"),
        value: match node {
            EnhancedNodeId::Word { .. } => "word".into(),
            EnhancedNodeId::Empty { .. } => "empty".into(),
        },
    });
    attributes.push(GraphNodeAttribute {
        name: AttributeKey::new("node.id").expect("node.id is canonical"),
        value: node.spelling().into(),
    });
    match node {
        EnhancedNodeId::Word { token_index } => {
            for spelling in SCALARS {
                let key = AttributeKey::new(spelling).expect("canonical graph scalar key");
                if let Some(value) = tokens.scalar_key(token_index, &key) {
                    attributes.push(GraphNodeAttribute {
                        name: key,
                        value: value.into(),
                    });
                }
            }
        }
        EnhancedNodeId::Empty { .. } => {
            let Some(empty) =
                graph.and_then(|graph| graph.empty_nodes().iter().find(|empty| empty.id() == node))
            else {
                return Box::default();
            };
            let text = empty.form().map(crate::analysis::text_key);
            for (spelling, value) in [
                ("text", text.as_deref()),
                ("lemma", empty.lemma()),
                ("upos", empty.upos()),
                ("xpos", empty.xpos()),
            ] {
                if let Some(value) = value {
                    attributes.push(GraphNodeAttribute {
                        name: AttributeKey::new(spelling).expect("canonical graph scalar key"),
                        value: value.into(),
                    });
                }
            }
        }
    }
    attributes.into_boxed_slice()
}

fn count_capture_assignments(
    expr: &ExecutionExpr,
    repeated: bool,
    counts: &mut BTreeMap<CaptureId, usize>,
) {
    match expr {
        ExecutionExpr::Capture { id, expr, .. } => {
            *counts.entry(*id).or_default() += if repeated { 2 } else { 1 };
            count_capture_assignments(expr, repeated, counts);
        }
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            for child in children {
                count_capture_assignments(child, repeated, counts);
            }
        }
        ExecutionExpr::Intersection { operands, .. } | ExecutionExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                count_capture_assignments(operand, repeated, counts);
            }
        }
        ExecutionExpr::Repeat { expr, max, .. } => {
            count_capture_assignments(expr, repeated || max.is_none_or(|max| max > 1), counts);
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
}

fn compile_runtime(
    expr: &ExecutionExpr,
    predicates: &mut PredicateCatalog,
    relations: &[RuntimeRelation],
    assignments: &BTreeMap<CaptureId, usize>,
) -> RuntimeExpr {
    match expr {
        ExecutionExpr::Token(predicate) => RuntimeExpr::Token(predicates.intern(predicate)),
        ExecutionExpr::Assertion(assertion) => {
            RuntimeExpr::Assertion(predicates.intern_position_assertion(assertion))
        }
        ExecutionExpr::Sequence(children) => RuntimeExpr::Sequence(
            children
                .iter()
                .map(|child| compile_runtime(child, predicates, relations, assignments))
                .collect(),
        ),
        ExecutionExpr::Alternation(children) => RuntimeExpr::Alternation(
            children
                .iter()
                .map(|child| compile_runtime(child, predicates, relations, assignments))
                .collect(),
        ),
        ExecutionExpr::Intersection { operands, .. } => RuntimeExpr::Intersection(
            operands
                .iter()
                .map(|operand| compile_runtime(operand, predicates, relations, assignments))
                .collect(),
        ),
        ExecutionExpr::AnyOrder { operands, .. } => RuntimeExpr::AnyOrder(
            operands
                .iter()
                .map(|operand| compile_runtime(operand, predicates, relations, assignments))
                .collect(),
        ),
        ExecutionExpr::Repeat { expr, min, max } => RuntimeExpr::Repeat {
            expr: Box::new(compile_runtime(expr, predicates, relations, assignments)),
            min: *min,
            max: *max,
        },
        ExecutionExpr::Capture { id, expr, .. } => RuntimeExpr::Capture {
            id: *id,
            expr: Box::new(compile_runtime(expr, predicates, relations, assignments)),
            relations: relations
                .iter()
                .filter(|relation| {
                    let RuntimeRelationTarget::Captured {
                        capture: dependent,
                        polarity: RelationPolarity::Positive,
                    } = &relation.target
                    else {
                        return false;
                    };
                    (relation.head == *id || *dependent == *id)
                        && assignments.get(&relation.head) == Some(&1)
                        && assignments.get(dependent) == Some(&1)
                })
                .cloned()
                .collect(),
        },
        ExecutionExpr::SameAs {
            capture,
            projection,
            ..
        } => RuntimeExpr::SameAs {
            capture: *capture,
            projection: AttributeLocator::symbolic(projection.attribute().clone()),
        },
    }
}

fn evaluate<
    const TOKEN_SITE: usize,
    const PREVIOUS_SITE: usize,
    S: TokenSequence + ?Sized,
    A: AssertionLookup,
>(
    expr: &RuntimeExpr,
    predicates: &PredicateSession<'_>,
    tokens: &S,
    candidate: Candidate,
    assertions: &mut A,
) -> Vec<Candidate> {
    let result = match expr {
        RuntimeExpr::Token(predicate) => (candidate.position < tokens.len()
            && predicates.matches_at_site::<TOKEN_SITE, _>(*predicate, tokens, candidate.position))
        .then(|| Candidate {
            position: candidate.position + 1,
            ..candidate
        })
        .into_iter()
        .collect(),
        RuntimeExpr::Assertion(assertion) => assertion_matches::<PREVIOUS_SITE, _, _>(
            assertion,
            predicates,
            tokens,
            candidate.position,
            assertions,
        )
        .then_some(candidate)
        .into_iter()
        .collect(),
        RuntimeExpr::Sequence(expressions) => {
            let mut candidates = vec![candidate];
            for expression in expressions {
                candidates = candidates
                    .into_iter()
                    .flat_map(|candidate| {
                        evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                            expression, predicates, tokens, candidate, assertions,
                        )
                    })
                    .collect();
                if candidates.is_empty() {
                    break;
                }
            }
            candidates
        }
        RuntimeExpr::Alternation(expressions) => expressions
            .iter()
            .flat_map(|expression| {
                evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                    expression,
                    predicates,
                    tokens,
                    candidate.clone(),
                    assertions,
                )
            })
            .collect(),
        RuntimeExpr::Intersection(operands) => {
            let (first, rest) = operands
                .split_first()
                .expect("the grammar gives an intersection at least two operands");
            let mut held = evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                first,
                predicates,
                tokens,
                candidate.clone(),
                assertions,
            );
            for operand in rest {
                if held.is_empty() {
                    break;
                }
                let reachable = evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                    operand,
                    predicates,
                    tokens,
                    candidate.clone(),
                    assertions,
                )
                .into_iter()
                .map(|other| other.position)
                .collect::<BTreeSet<_>>();
                held.retain(|next| reachable.contains(&next.position));
            }
            held
        }
        RuntimeExpr::AnyOrder(operands) => {
            let mut found = Vec::new();
            for first in 0..operands.len() {
                let remaining = (0..operands.len())
                    .filter(|index| *index != first)
                    .collect::<Vec<_>>();
                for next in evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                    &operands[first],
                    predicates,
                    tokens,
                    candidate.clone(),
                    assertions,
                ) {
                    let order = vec![first];
                    found.extend(evaluate_any_order_tail::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                        operands, &remaining, &order, predicates, tokens, next, assertions,
                    ));
                }
            }
            found
        }
        RuntimeExpr::Repeat { expr, min, max } => repeat::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
            expr, *min, *max, predicates, tokens, candidate, assertions,
        ),
        RuntimeExpr::Capture {
            id,
            expr,
            relations,
        } => {
            let start = candidate.position;
            evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                expr, predicates, tokens, candidate, assertions,
            )
            .into_iter()
            .map(|mut candidate| {
                candidate.slots[id.index()] = Some((start, candidate.position));
                candidate
            })
            .filter(|candidate| {
                relations.iter().all(|relation| {
                    let RuntimeRelationTarget::Captured {
                        capture: dependent,
                        polarity: RelationPolarity::Positive,
                    } = &relation.target
                    else {
                        return true;
                    };
                    match (
                        candidate.slots[relation.head.index()],
                        candidate.slots[dependent.index()],
                    ) {
                        (Some((head, _)), Some((dependent, _))) => {
                            relation_matches_token(relation, tokens, dependent, head)
                        }
                        _ => true,
                    }
                })
            })
            .collect()
        }
        RuntimeExpr::SameAs {
            capture,
            projection,
            ..
        } => {
            let Some((captured_start, captured_end)) = candidate.slots[capture.index()] else {
                return Vec::new();
            };
            let width = captured_end - captured_start;
            let Some(end) = candidate.position.checked_add(width) else {
                return Vec::new();
            };
            if end > tokens.len()
                || !spans_equal(
                    tokens,
                    captured_start,
                    candidate.position,
                    width,
                    projection,
                )
            {
                Vec::new()
            } else {
                vec![Candidate {
                    position: end,
                    ..candidate
                }]
            }
        }
    };
    #[cfg(feature = "bench-instrumentation")]
    let produced = result.len();
    let result = deduplicate(result);
    #[cfg(feature = "bench-instrumentation")]
    predicates.record_candidates(produced, result.len());
    result
}

/// Places each remaining node after the previously consumed node, allowing an
/// arbitrary token gap. The first node is handled by `evaluate` at the
/// caller's exact start boundary, so the complete match has no implicit outer
/// gap and spans precisely from its earliest node to its latest.
fn evaluate_any_order_tail<
    const TOKEN_SITE: usize,
    const PREVIOUS_SITE: usize,
    S: TokenSequence + ?Sized,
    A: AssertionLookup,
>(
    operands: &[RuntimeExpr],
    remaining: &[usize],
    order: &[usize],
    predicates: &PredicateSession<'_>,
    tokens: &S,
    candidate: Candidate,
    assertions: &mut A,
) -> Vec<Candidate> {
    if remaining.is_empty() {
        let mut candidate = candidate;
        candidate.any_order_branches.push(order.into());
        return vec![candidate];
    }

    let mut found = Vec::new();
    for (choice, operand_index) in remaining.iter().copied().enumerate() {
        let mut next_remaining = remaining.to_vec();
        next_remaining.remove(choice);
        let mut next_order = order.to_vec();
        next_order.push(operand_index);
        // Mirrors the explicit spelling `operand []* operand`: the gap is
        // greedy, so equal-end candidates prefer the latest viable next node.
        for position in (candidate.position..tokens.len()).rev() {
            let placed = Candidate {
                position,
                slots: candidate.slots.clone(),
                any_order_branches: candidate.any_order_branches.clone(),
            };
            for next in evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                &operands[operand_index],
                predicates,
                tokens,
                placed,
                assertions,
            ) {
                found.extend(evaluate_any_order_tail::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                    operands,
                    &next_remaining,
                    &next_order,
                    predicates,
                    tokens,
                    next,
                    assertions,
                ));
            }
        }
    }
    found
}

fn repeat<
    const TOKEN_SITE: usize,
    const PREVIOUS_SITE: usize,
    S: TokenSequence + ?Sized,
    A: AssertionLookup,
>(
    expr: &RuntimeExpr,
    min: usize,
    max: Option<usize>,
    predicates: &PredicateSession<'_>,
    tokens: &S,
    initial: Candidate,
    assertions: &mut A,
) -> Vec<Candidate> {
    let ceiling = max.unwrap_or(tokens.len().saturating_add(1));
    let mut levels = vec![vec![initial]];
    for _ in 0..ceiling {
        let next = levels
            .last()
            .expect("initial level exists")
            .iter()
            .cloned()
            .flat_map(|candidate| {
                let before = candidate.position;
                evaluate::<TOKEN_SITE, PREVIOUS_SITE, _, _>(
                    expr, predicates, tokens, candidate, assertions,
                )
                .into_iter()
                .filter(move |next| next.position > before)
            })
            .collect::<Vec<_>>();
        if next.is_empty() {
            break;
        }
        levels.push(next);
    }
    levels
        .into_iter()
        .enumerate()
        .rev()
        .filter(|(count, _)| *count >= min)
        .flat_map(|(_, candidates)| candidates)
        .collect()
}

/// Collapse candidates that are indistinguishable to everything downstream.
///
/// The key is the position **and the whole slot vector**, because a capture
/// assignment is observable: `same_as` reads one slot mid-expression, and a
/// `where` clause reads several after the walk. Keying on position alone
/// would discard a satisfying assignment whenever a failing one reached the
/// same position first — the defect `matches_with`'s comment guards against
/// one level up, at end position rather than at assignment.
///
/// Widening cannot move what a relation-free definition publishes, which is
/// why this is not a behaviour change outside `where`. This keeps the *first*
/// candidate of each key group and preserves order, so a wider key only ever
/// adds survivors; `matches_with` then takes `accepted.first()` after a
/// **stable** sort by descending position, and the first candidate at the
/// maximum position is the one the narrow key already kept. `extract_with`'s
/// `find` on an exact end position is the same argument. For a
/// relation-bearing definition the added survivors reach the filter, so its
/// published set can only grow.
///
/// The slot clone per surviving candidate is the cost, and it is
/// unmeasurable: five catalogs publishing 400k records are byte-identical and
/// inside run-to-run noise, and the counted-work screen does not move.
fn deduplicate(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert((candidate.position, candidate.slots.clone())))
        .collect()
}

fn assertion_matches<const PREVIOUS_SITE: usize, S: TokenSequence + ?Sized, A: AssertionLookup>(
    assertion: &CompiledPositionAssertion,
    predicates: &PredicateSession<'_>,
    tokens: &S,
    position: usize,
    assertions: &mut A,
) -> bool {
    match assertion {
        CompiledPositionAssertion::SentenceStart => position == 0,
        CompiledPositionAssertion::SentenceEnd => position == tokens.len(),
        CompiledPositionAssertion::PreviousToken(predicate) => {
            position.checked_sub(1).is_some_and(|index| {
                predicates.matches_at_site::<PREVIOUS_SITE, _>(*predicate, tokens, index)
            })
        }
        CompiledPositionAssertion::Lookaround(occurrence) => {
            assertions.holds(*occurrence, position)
        }
    }
}

fn spans_equal<S: TokenSequence + ?Sized>(
    tokens: &S,
    left: usize,
    right: usize,
    width: usize,
    projection: &AttributeLocator,
) -> bool {
    (0..width).all(|offset| {
        match (
            tokens.value_locator(left + offset, projection),
            tokens.value_locator(right + offset, projection),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    })
}

fn bind_runtime_attributes(expr: &mut RuntimeExpr, projection: &BoundProjection) {
    match expr {
        RuntimeExpr::Token(_) | RuntimeExpr::Assertion(_) => {}
        RuntimeExpr::Sequence(children)
        | RuntimeExpr::Alternation(children)
        | RuntimeExpr::Intersection(children)
        | RuntimeExpr::AnyOrder(children) => {
            for child in children {
                bind_runtime_attributes(child, projection);
            }
        }
        RuntimeExpr::Repeat { expr, .. } => bind_runtime_attributes(expr, projection),
        RuntimeExpr::Capture {
            expr, relations, ..
        } => {
            bind_runtime_attributes(expr, projection);
            for relation in relations {
                for attribute in [
                    &mut relation.head_attribute,
                    &mut relation.relation_attribute,
                ] {
                    attribute.bind(
                        projection
                            .ordinal_for_key(attribute.key())
                            .expect("schema binding projected every relation key"),
                    );
                }
            }
        }
        RuntimeExpr::SameAs {
            projection: attribute,
            ..
        } => {
            // An unavailable `by` key stays unbound; `spans_equal` reads it as
            // absent on both spans and the equality never holds.
            if let Some(ordinal) = projection.ordinal_for_key(attribute.key()) {
                attribute.bind(ordinal);
            }
        }
    }
}

fn collect_captures(expr: &ExecutionExpr, captures: &mut Vec<CaptureDescriptor>) {
    match expr {
        ExecutionExpr::Capture {
            id,
            name,
            path,
            expr,
            ..
        } => {
            captures.push(CaptureDescriptor {
                id: *id,
                name: name.clone(),
                path: path.clone(),
            });
            collect_captures(expr, captures);
        }
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            for child in children {
                collect_captures(child, captures);
            }
        }
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_captures(operand, captures);
            }
        }
        ExecutionExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_captures(operand, captures);
            }
        }
        ExecutionExpr::Repeat { expr, .. } => collect_captures(expr, captures),
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
}

fn collect_attributes(expr: &ExecutionExpr, attributes: &mut Vec<AttributeKey>) {
    match expr {
        ExecutionExpr::Token(predicate) => {
            attributes.extend(predicate.iter().map(|value| value.attr.clone()))
        }
        ExecutionExpr::Assertion(PositionAssertion::PreviousToken(predicate)) => {
            attributes.extend(predicate.iter().map(|value| value.attr.clone()))
        }
        ExecutionExpr::SameAs { projection, .. } => {
            attributes.push(projection.attribute().clone());
        }
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            for child in children {
                collect_attributes(child, attributes);
            }
        }
        // Load-bearing: this feeds the attribute projection, so an attribute
        // reachable only from inside an intersection operand and not collected
        // here would make a cache-backed caller silently lose the column and
        // then fail to match on it.
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_attributes(operand, attributes);
            }
        }
        ExecutionExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_attributes(operand, attributes);
            }
        }
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            collect_attributes(expr, attributes)
        }
        ExecutionExpr::Assertion(_) => {}
    }
}

fn first_same_as(expr: &ExecutionExpr) -> Option<(CaptureId, CaptureProjection)> {
    match expr {
        ExecutionExpr::SameAs {
            capture,
            projection,
            ..
        } => Some((*capture, projection.clone())),
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            children.iter().find_map(first_same_as)
        }
        ExecutionExpr::Intersection { operands, .. } => operands.iter().find_map(first_same_as),
        ExecutionExpr::AnyOrder { operands, .. } => operands.iter().find_map(first_same_as),
        ExecutionExpr::Repeat { expr, .. } | ExecutionExpr::Capture { expr, .. } => {
            first_same_as(expr)
        }
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::execution::{BoundSentenceInput, EnhancedGraphInput};
    use crate::fsm::{CompileLimits, MatchResourceError, PatternSource, compile_catalog};
    use hegel::generators;
    use std::cell::Cell;
    use std::fmt::Write as _;

    thread_local! {
        pub(super) static UNPRUNED_ORACLE: Cell<bool> = const { Cell::new(false) };
    }

    const UPOS: [&str; 3] = ["VERB", "NOUN", "NOUN"];
    const LABELS: [&str; 3] = ["obj", "obj", "nmod"];

    fn draw_sentence(tc: &hegel::TestCase) -> String {
        let words = tc.draw(generators::integers::<usize>().min_value(3).max_value(8));
        let order = tc.draw(generators::permutations((0..words).collect::<Vec<_>>()));
        let mut heads = vec![0; words];
        for (position, &word) in order.iter().enumerate().skip(1) {
            let parent = if tc.draw(generators::booleans()) {
                0
            } else {
                tc.draw(generators::integers::<usize>().max_value(position - 1))
            };
            heads[word] = order[parent] + 1;
        }
        let labels = heads
            .iter()
            .map(|head| {
                if *head == 0 {
                    "root"
                } else {
                    tc.draw(generators::sampled_from(LABELS.to_vec()))
                }
            })
            .collect::<Vec<_>>();
        let empty = tc.draw(generators::booleans()).then(|| {
            let at = tc.draw(
                generators::integers::<usize>()
                    .min_value(1)
                    .max_value(words),
            );
            let head = tc.draw(
                generators::integers::<usize>()
                    .min_value(1)
                    .max_value(words),
            );
            let dependent = tc.draw(
                generators::integers::<usize>()
                    .min_value(1)
                    .max_value(words),
            );
            (at, head, dependent)
        });
        let mut sentence = String::new();
        for word in 1..=words {
            let mut deps = vec![(heads[word - 1].to_string(), labels[word - 1].to_owned())];
            if let Some((at, _, dependent)) = empty
                && dependent == word
            {
                deps.push((format!("{at}.1"), "nmod".to_owned()));
            }
            deps.sort_by_key(|(head, _)| {
                let (whole, fraction) = head.split_once('.').unwrap_or((head, "0"));
                (
                    whole.parse::<usize>().unwrap(),
                    fraction.parse::<usize>().unwrap(),
                )
            });
            let deps = deps
                .iter()
                .map(|(head, label)| format!("{head}:{label}"))
                .collect::<Vec<_>>()
                .join("|");
            let upos = tc.draw(generators::sampled_from(UPOS.to_vec()));
            writeln!(
                sentence,
                "{word}\tw\tw\t{upos}\t_\t_\t{}\t{}\t{deps}\t_",
                heads[word - 1],
                labels[word - 1],
            )
            .unwrap();
            if let Some((at, head, _)) = empty
                && at == word
            {
                writeln!(sentence, "{word}.1\te\te\tNOUN\t_\t_\t_\t_\t{head}:obj\t_").unwrap();
            }
        }
        sentence
    }

    fn draw_pattern(tc: &hegel::TestCase) -> String {
        let node_test = || tc.draw(generators::sampled_from(vec!["[upos=NOUN]", "[]", "[]"]));
        let binders = tc.draw(generators::integers::<usize>().min_value(1).max_value(4));
        let mut nodes = vec!["v".to_owned()];
        let mut clauses = Vec::new();
        for binder in 1..=binders {
            let anchor = if tc.draw(generators::booleans()) {
                nodes[0].clone()
            } else {
                tc.draw(generators::sampled_from(nodes.clone()))
            };
            let name = format!("b{binder}");
            let test = node_test();
            let labels = tc.draw(generators::sampled_from(vec!["obj", "*", "*", "obj|nmod"]));
            let bracket = if tc.draw(generators::integers::<u8>().max_value(2)) == 0 {
                let edge = if tc.draw(generators::booleans()) {
                    format!(" as $e{binder}")
                } else {
                    String::new()
                };
                format!("enhanced {labels}{edge}")
            } else {
                labels.to_owned()
            };
            clauses.push(if tc.draw(generators::integers::<u8>().max_value(3)) > 0 {
                format!("${anchor} -[{bracket}]-> (?<{name}> {test})")
            } else {
                format!("(?<{name}> {test}) -[{bracket}]-> ${anchor}")
            });
            nodes.push(name);
        }
        if tc.draw(generators::integers::<u8>().max_value(3)) == 0 {
            let pair = tc.draw(generators::permutations(nodes.clone()));
            let labels = tc.draw(generators::sampled_from(vec!["obj", "nmod", "*"]));
            clauses.push(format!(
                "not ${} -[enhanced {labels}]-> ${}",
                pair[0], pair[1]
            ));
        }
        for _ in 0..tc.draw(generators::integers::<usize>().max_value(2)) {
            let chain = tc.draw(generators::permutations(nodes.clone()));
            let length = tc.draw(
                generators::integers::<usize>()
                    .min_value(2)
                    .max_value(chain.len().min(3)),
            );
            clauses.push(
                chain[..length]
                    .iter()
                    .map(|node| format!("${node}"))
                    .collect::<Vec<_>>()
                    .join(" < "),
            );
        }
        format!(
            "pattern p := (?<v> {}) where {}.",
            node_test(),
            clauses.join(", ")
        )
    }

    fn graph_fixture(
        words: usize,
        binders: usize,
    ) -> (crate::fsm::BoundCatalog, BoundSentenceInput) {
        let clauses = (0..binders)
            .map(|index| format!("$v -[*]-> (?<b{index}> [])"))
            .collect::<Vec<_>>()
            .join(", ");
        graph_fixture_with_clauses(words, &clauses)
    }

    fn graph_fixture_with_clauses(
        words: usize,
        clauses: &str,
    ) -> (crate::fsm::BoundCatalog, BoundSentenceInput) {
        let compiled = compile_catalog(
            PatternSource::named(
                "budget.lx",
                &format!("pattern p := (?<v> [upos=VERB]) where {clauses}."),
            ),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let bound = compiled
            .bind(&crate::corpus::conllu::token_schema())
            .unwrap();
        let mut sentence = String::from("1\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n");
        for index in 2..=words {
            writeln!(sentence, "{index}\tn\tn\tNOUN\t_\t_\t1\tobj\t_\t_").unwrap();
        }
        sentence.push('\n');
        let read = crate::corpus::conllu::Reader::new(sentence.as_bytes(), "budget")
            .next()
            .unwrap()
            .unwrap();
        let input = BoundSentenceInput::new(
            read.project(bound.projection()).unwrap(),
            EnhancedGraphInput::Observation(read.enhanced_graph().clone()),
        );
        (bound, input)
    }

    #[test]
    fn matching_batch_rejects_aggregate_output_from_individually_admitted_sentences() {
        let mut clauses = (0..7)
            .map(|index| format!("$v -[obj]-> (?<b{index}> [])"))
            .collect::<Vec<_>>();
        clauses.push(
            (0..7)
                .map(|index| format!("$b{index}"))
                .collect::<Vec<_>>()
                .join(" < "),
        );
        let (bound, input) = graph_fixture_with_clauses(13, &clauses.join(", "));
        let expected = bound.find_matches_input(&input).unwrap();
        assert_eq!(expected.len(), 792);
        for threads in [1, 4] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let small = crate::fsm::BoundSentenceMatcher::new(&bound)
                .match_all(&pool, &[input.clone(), input.clone()])
                .unwrap();
            assert_eq!(small, vec![expected.clone(), expected.clone()]);
            let large = crate::fsm::BoundSentenceMatcher::new(&bound)
                .match_all(&pool, &vec![input.clone(); 32]);
            assert!(
                matches!(
                    large,
                    Err(crate::fsm::BoundExecutionError::Resource(
                        MatchResourceError::BatchOutput { .. }
                    ))
                ),
                "aggregate output must exceed the matching batch budget"
            );
            let fresh = crate::fsm::BoundSentenceMatcher::new(&bound)
                .match_all(&pool, std::slice::from_ref(&input))
                .unwrap();
            assert_eq!(fresh, vec![expected.clone()]);
        }
    }

    #[test]
    fn execution_splits_output_limited_batches_and_preserves_complete_graph_matches() {
        use crate::execution::{
            AnalysisBinding, CancellationFlag, ExecutionBatch, ExecutionError, ExecutionLimits,
            ExecutionSentence, MatchSink, PolicySelection, SentenceAdmission, SentenceBatchSource,
            SentenceLocation, SinkControl, TokenizedBatchObservation, execute_analyzed,
        };
        struct Source(Option<ExecutionBatch>);
        impl SentenceBatchSource for Source {
            fn wants_lookahead(&self) -> bool {
                false
            }
            fn next_batch(
                &mut self,
                _: &ExecutionLimits,
            ) -> Result<Option<ExecutionBatch>, ExecutionError> {
                Ok(self.0.take())
            }
        }
        struct Sink {
            expected: Vec<crate::fsm::Match>,
            ordinal: u64,
        }
        impl MatchSink for Sink {
            fn push(
                &mut self,
                location: &SentenceLocation,
                _: &dyn crate::token_match::TokenSequence,
                matches: &[crate::fsm::Match],
            ) -> Result<SinkControl, ExecutionError> {
                if location.corpus_sentence_ordinal != self.ordinal || matches != self.expected {
                    return Err(ExecutionError::Sink(format!(
                        "sentence {} differs from complete sequential graph matches",
                        self.ordinal
                    )));
                }
                self.ordinal += 1;
                Ok(SinkControl::Continue {
                    records_emitted: matches.len(),
                })
            }
        }
        let mut clauses = (0..7)
            .map(|index| format!("$v -[obj]-> (?<b{index}> [])"))
            .collect::<Vec<_>>();
        clauses.push(
            (0..7)
                .map(|index| format!("$b{index}"))
                .collect::<Vec<_>>()
                .join(" < "),
        );
        let (bound, input) = graph_fixture_with_clauses(13, &clauses.join(", "));
        let expected = bound.find_matches_input(&input).unwrap();
        assert_eq!(expected.len(), 792);
        let charge = input.resident_bytes().unwrap();
        for threads in [1, 4] {
            let source = Source(Some(ExecutionBatch::bound_tokenized_with_charges(
                (0..32)
                    .map(|ordinal| ExecutionSentence {
                        location: SentenceLocation::run_local(ordinal),
                        value: input.clone(),
                    })
                    .collect(),
                vec![charge; 32],
            )));
            let mut sink = Sink {
                expected: expected.clone(),
                ordinal: 0,
            };
            type NoAnalyzer = crate::testing::execution::FixtureSession<
                fn(&str) -> Vec<crate::token_match::TokenInfo>,
            >;
            let outcome = execute_analyzed::<_, NoAnalyzer, _, _, _>(
                source,
                AnalysisBinding::Prepared(crate::corpus::conllu::token_schema()),
                &bound,
                &rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap(),
                &ExecutionLimits::default(),
                32,
                &CancellationFlag::new(),
                &mut sink,
                &mut |_, _: &Sink| {},
                &mut |_: TokenizedBatchObservation| Ok(()),
                PolicySelection::Apply,
                &SentenceAdmission::All,
            )
            .unwrap();
            assert_eq!(sink.ordinal, 32);
            let crate::execution::RunOutcome::Completed(report) = outcome else {
                panic!("unexpected cancellation")
            };
            assert_eq!(report.sentences_admitted, 32);
            assert_eq!(report.matches_found, 32 * 792);
            assert_eq!(report.records_emitted, 32 * 792);
        }
    }

    #[test]
    fn unordered_graph_binders_return_a_resource_error() {
        let (bound, input) = graph_fixture(32, 5);
        let error = bound.find_matches_input(&input).unwrap_err();
        assert!(
            matches!(error, crate::fsm::BoundExecutionError::Resource(_)),
            "{error}"
        );
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        assert!(matches!(
            crate::fsm::BoundSentenceMatcher::new(&bound).match_all(&pool, &[input]),
            Err(crate::fsm::BoundExecutionError::Resource(_))
        ));
    }

    #[test]
    fn graph_resource_failure_propagates_through_execution_without_publication() {
        use crate::execution::{
            AnalysisBinding, CancellationFlag, ExecutionBatch, ExecutionError, ExecutionLimits,
            ExecutionSentence, MatchSink, PolicySelection, SentenceAdmission, SentenceBatchSource,
            SentenceLocation, SinkControl, TokenizedBatchObservation, execute_analyzed,
        };
        struct Source(Option<ExecutionBatch>);
        impl SentenceBatchSource for Source {
            fn wants_lookahead(&self) -> bool {
                false
            }
            fn next_batch(
                &mut self,
                _: &ExecutionLimits,
            ) -> Result<Option<ExecutionBatch>, ExecutionError> {
                Ok(self.0.take())
            }
        }
        #[derive(Default)]
        struct Sink(usize);
        impl MatchSink for Sink {
            fn push(
                &mut self,
                _: &SentenceLocation,
                _: &dyn crate::token_match::TokenSequence,
                _: &[crate::fsm::Match],
            ) -> Result<SinkControl, ExecutionError> {
                self.0 += 1;
                Ok(SinkControl::Continue { records_emitted: 0 })
            }
        }
        let (bound, input) = graph_fixture(32, 5);
        let charge = input.resident_bytes().unwrap();
        let source = Source(Some(ExecutionBatch::bound_tokenized_with_charges(
            vec![ExecutionSentence {
                location: SentenceLocation::run_local(0),
                value: input,
            }],
            vec![charge],
        )));
        let mut sink = Sink::default();
        type NoAnalyzer = crate::testing::execution::FixtureSession<
            fn(&str) -> Vec<crate::token_match::TokenInfo>,
        >;
        let error = execute_analyzed::<_, NoAnalyzer, _, _, _>(
            source,
            AnalysisBinding::Prepared(crate::corpus::conllu::token_schema()),
            &bound,
            &rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
            &ExecutionLimits::default(),
            1,
            &CancellationFlag::new(),
            &mut sink,
            &mut |_, _: &Sink| {},
            &mut |_: TokenizedBatchObservation| Ok(()),
            PolicySelection::Apply,
            &SentenceAdmission::All,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ExecutionError::Matching(crate::fsm::BoundExecutionError::Resource(_))
            ),
            "{error}"
        );
        assert_eq!(sink.0, 0);
    }

    #[test]
    fn wide_single_relation_with_a_rare_node_predicate_completes() {
        let (bound, input) =
            graph_fixture_with_clauses(20_000, "$v -[*]-> (?<b0> [node.id=20000])");
        let matches = bound.find_matches_input(&input).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].graph_bindings.iter().any(|binding| matches!(
            binding,
            crate::fsm::GraphBinding::Node {
                node: crate::corpus::conllu::EnhancedNodeId::Word {
                    token_index: 19_999
                },
                ..
            }
        )));
    }

    #[test]
    fn root_only_graph_scans_exhaust_the_work_budget() {
        for layer in ["", "enhanced "] {
            let catalog = compile_catalog(
                PatternSource::named(
                    "roots.lx",
                    &format!("pattern p := (?<v> []) where $v -[{layer}*]-> (?<b> [])."),
                ),
                CompileLimits::default(),
            )
            .unwrap()
            .matcher;
            let bound = catalog
                .bind(&crate::corpus::conllu::token_schema())
                .unwrap();
            let mut sentence = String::new();
            for index in 1..=2000 {
                writeln!(sentence, "{index}\tn\tn\tNOUN\t_\t_\t0\troot\t0:root\t_").unwrap();
            }
            sentence.push('\n');
            let read = crate::corpus::conllu::Reader::new(sentence.as_bytes(), "roots")
                .next()
                .unwrap()
                .unwrap();
            let input = BoundSentenceInput::new(
                read.project(bound.projection()).unwrap(),
                EnhancedGraphInput::Observation(read.enhanced_graph().clone()),
            );
            let crate::fsm::BoundExecutionError::Resource(error) =
                bound.find_matches_input(&input).unwrap_err()
            else {
                panic!("graph work error expected");
            };
            let MatchResourceError::Sentence {
                resource, limit, ..
            } = error
            else {
                panic!("sentence graph error expected");
            };
            assert_eq!(resource, "graph work steps");
            assert_eq!(limit, 1_000_000);
        }
    }

    #[test]
    fn graph_chain_with_no_complete_assignment_returns_a_resource_error() {
        let (bound, input) =
            graph_fixture_with_clauses(1100, "$v -[*]-> (?<b0> []), $b0 -[*]-> (?<b1> [])");
        let error = bound.find_matches_input(&input).unwrap_err();
        assert!(
            matches!(error, crate::fsm::BoundExecutionError::Resource(_)),
            "{error}"
        );
    }

    #[test]
    fn bounded_graph_enumeration_preserves_complete_matches() {
        let (bound, input) = graph_fixture(5, 2);
        let bounded = bound.find_matches_input(&input).unwrap();
        UNPRUNED_ORACLE.set(true);
        let unpruned = bound.find_matches_input(&input);
        UNPRUNED_ORACLE.set(false);
        assert_eq!(bounded, unpruned.unwrap());
        assert_eq!(bounded.len(), 12);
        assert!(
            bounded
                .iter()
                .all(|matched| matched.graph_bindings.len() == 2
                    && matched.captures.len() == 1
                    && matched.lexical.is_none())
        );
    }

    #[hegel::test(test_cases = 400, derandomize = true)]
    fn pruned_binder_enumeration_publishes_the_unpruned_matches(tc: hegel::TestCase) {
        let sentence = draw_sentence(&tc);
        let source = draw_pattern(&tc);
        tc.note(&source);
        tc.note(&sentence);

        let compiled = match compile_catalog(
            PatternSource::named("pruning-oracle.lx", &source),
            CompileLimits::default(),
        ) {
            Ok(output) => output.matcher,
            Err(error) => {
                tc.assume(!error.diagnostics.iter().all(|diagnostic| {
                    diagnostic.code() == crate::fsm::DiagnosticCode::WhereContradictoryExistence
                }));
                panic!("{source} compiles: {:?}", error.diagnostics);
            }
        };
        let bound = compiled
            .bind(&crate::corpus::conllu::token_schema())
            .expect("the CoNLL-U schema carries dep.head, dep.rel and dep.enhanced");
        let read = crate::corpus::conllu::Reader::new(sentence.as_bytes(), "oracle")
            .next()
            .expect("one sentence")
            .expect("the generated sentence parses");
        let input = BoundSentenceInput::new(
            read.project(bound.projection())
                .expect("the generated sentence projects"),
            EnhancedGraphInput::Observation(read.enhanced_graph().clone()),
        );

        let pruned = bound.find_matches_input(&input).expect("schema matches");
        tc.target(pruned.len() as f64);
        UNPRUNED_ORACLE.set(true);
        let unpruned = bound.find_matches_input(&input);
        UNPRUNED_ORACLE.set(false);
        assert_eq!(pruned, unpruned.expect("schema matches"));
    }
}
