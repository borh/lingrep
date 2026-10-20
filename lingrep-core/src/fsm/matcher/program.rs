use super::assertion::AssertionSession;
use super::predicate::{
    CompiledPositionAssertion, PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN, PREDICATE_SITE_CAPTURE_TOKEN,
    PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN, PREDICATE_SITE_EVALUATED_TOKEN, PredicateCatalog,
    PredicateId, PredicateSession,
};
use crate::corpus::conllu::{EnhancedGraph, EnhancedHead, EnhancedNodeId};
use crate::fsm::compiler::model::{
    CaptureId, CaptureProjection, ExecutionDefinition, ExecutionEnhancedEndpoint, ExecutionExpr,
    ExecutionGraphBindingDecl, RelationLabel, RelationLabels, RelationPolarity, RelationTarget,
    TokenPredicate,
};
use crate::fsm::{AttrValueType, Capture, GraphBinding, GraphNodeAttribute, PositionAssertion};
use crate::token_match::{
    AttributeKey, AttributeLocator, AttributeValueRef, BoundProjection, TokenSequence,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub(crate) struct Program {
    definition: ExecutionDefinition,
    runtime: RuntimeExpr,
    captures: Box<[CaptureDescriptor]>,
    relations: Box<[RuntimeRelation]>,
    enhanced_relations: Box<[RuntimeEnhancedRelation]>,
}

/// One `where` edge constraint in executable form.
///
/// The locators start symbolic and are bound with the rest of the program's
/// attributes: `required_attributes` reports `dep.head` and `dep.rel` for any
/// relation-bearing program, so schema binding either projected both or
/// refused the catalog before execution could begin.
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
    Captured(CaptureId),
    Anonymous(RelationPolarity),
    Predicate {
        polarity: RelationPolarity,
        predicates: Box<[PredicateId]>,
        required_attributes: Box<[AttributeLocator]>,
    },
}

#[derive(Clone)]
struct RuntimeEnhancedRelation {
    polarity: RelationPolarity,
    head: RuntimeEnhancedEndpoint,
    labels: RelationLabels<RelationLabel>,
    edge: Option<compact_str::CompactString>,
    dependent: RuntimeEnhancedEndpoint,
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
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) captures: Box<[Capture]>,
    pub(crate) graph_bindings: Box<[GraphBinding]>,
}

#[derive(Clone, Default)]
struct GraphAssignment {
    nodes: BTreeMap<compact_str::CompactString, EnhancedNodeId>,
    edges: BTreeMap<
        compact_str::CompactString,
        (EnhancedNodeId, EnhancedNodeId, compact_str::CompactString),
    >,
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
                    RelationTarget::Captured { capture, .. } => {
                        RuntimeRelationTarget::Captured(*capture)
                    }
                    RelationTarget::Anonymous { polarity, .. } => {
                        RuntimeRelationTarget::Anonymous(*polarity)
                    }
                    RelationTarget::Predicate {
                        polarity,
                        predicates: tests,
                        ..
                    } => RuntimeRelationTarget::Predicate {
                        polarity: *polarity,
                        predicates: tests
                            .iter()
                            .map(|predicate| predicates.intern(predicate))
                            .collect(),
                        required_attributes: tests
                            .iter()
                            .flat_map(|predicate| predicate.iter())
                            .map(|atom| atom.attr.clone())
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .map(AttributeLocator::symbolic)
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
                polarity: relation.polarity,
                head: compile_enhanced_endpoint(&relation.head),
                labels: relation.labels.clone(),
                edge: relation.edge.clone(),
                dependent: compile_enhanced_endpoint(&relation.dependent),
            })
            .collect();
        Self {
            definition,
            runtime,
            captures: captures.into_boxed_slice(),
            relations,
            enhanced_relations,
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
        !self.enhanced_relations.is_empty()
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
            if let RuntimeRelationTarget::Predicate {
                required_attributes,
                ..
            } = &mut relation.target
            {
                for attribute in required_attributes {
                    let ordinal = projection
                        .ordinal_for_key(attribute.key())
                        .expect("schema binding projected every predicate-target key");
                    attribute.bind(ordinal);
                }
            }
        }
        for relation in &mut self.enhanced_relations {
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
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Option<Box<[Capture]>> {
        self.extract_with(predicates, tokens, start, end, session)
    }

    fn extract_with<S: TokenSequence + ?Sized, A: AssertionLookup>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
        assertions: &mut A,
    ) -> Option<Box<[Capture]>> {
        self.candidates::<
            PREDICATE_SITE_CAPTURE_TOKEN,
            PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN,
            _,
            _,
        >(predicates, tokens, start, assertions)
            .into_iter()
            .find(|candidate| candidate.position == end)
            .map(|candidate| self.public_captures(&candidate))
    }

    pub(crate) fn extract_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        start: usize,
        end: usize,
    ) -> Option<Box<[Capture]>> {
        self.extract_with(predicates, tokens, start, end, &mut NoAssertions)
    }

    pub(crate) fn matches<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
    ) -> Vec<ProgramMatch> {
        self.matches_with(predicates, tokens, session, None)
    }

    pub(crate) fn matches_with_graph<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut AssertionSession<'_, '_, S>,
        graph: &EnhancedGraph,
    ) -> Vec<ProgramMatch> {
        self.matches_with(predicates, tokens, session, Some(graph))
    }

    fn matches_with<S: TokenSequence + ?Sized, A: AssertionLookup>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        assertions: &mut A,
        enhanced_graph: Option<&EnhancedGraph>,
    ) -> Vec<ProgramMatch> {
        let mut matches = Vec::new();
        for start in 0..tokens.len() {
            let mut accepted = self
                .candidates::<
                    PREDICATE_SITE_EVALUATED_TOKEN,
                    PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN,
                    _,
                    _,
                >(predicates, tokens, start, assertions)
                .into_iter()
                .filter(|candidate| candidate.position > start)
                // Before the longest-candidate selection below, deliberately:
                // filtering after it would let a failing longer candidate
                // shadow a passing shorter one at the same start, and the
                // shadow would be unobservable.
                .filter(|candidate| self.relations_hold(predicates, candidate, tokens))
                .filter_map(|candidate| {
                    let assignments = self.enhanced_assignments(
                        &candidate,
                        tokens,
                        enhanced_graph,
                    );
                    (!assignments.is_empty()).then_some((candidate, assignments))
                })
                .collect::<Vec<_>>();
            accepted.sort_by_key(|(candidate, _)| std::cmp::Reverse(candidate.position));
            let mut seen_branches = BTreeSet::new();
            for (candidate, assignments) in accepted
                .into_iter()
                .filter(|(candidate, _)| seen_branches.insert(candidate.any_order_branches.clone()))
            {
                for assignment in assignments {
                    matches.push(ProgramMatch {
                        start,
                        end: candidate.position,
                        captures: self.public_captures(&candidate),
                        graph_bindings: self.public_graph_bindings(
                            &assignment,
                            tokens,
                            enhanced_graph,
                        ),
                    });
                }
            }
        }
        matches
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
    ) -> bool {
        self.relations.iter().all(|relation| {
            let Some((head_start, _)) = candidate.slots[relation.head.index()] else {
                return false;
            };
            match &relation.target {
                RuntimeRelationTarget::Captured(dependent) => {
                    let Some((dependent_start, _)) = candidate.slots[dependent.index()] else {
                        return false;
                    };
                    relation_matches_token(relation, tokens, dependent_start, head_start)
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
                    polarity,
                    predicates: target_predicates,
                    required_attributes,
                } => {
                    if !dependency_observation_is_usable(relation, tokens) {
                        return false;
                    }
                    for dependent_start in 0..tokens.len() {
                        if !relation_matches_token(relation, tokens, dependent_start, head_start) {
                            continue;
                        }
                        if *polarity == RelationPolarity::Negative
                            && required_attributes.iter().any(|attribute| {
                                tokens.value_locator(dependent_start, attribute).is_none()
                            })
                        {
                            return false;
                        }
                        let witness = target_predicates.iter().any(|predicate| {
                            predicates.matches_at_site::<PREDICATE_SITE_EVALUATED_TOKEN, _>(
                                *predicate,
                                tokens,
                                dependent_start,
                            )
                        });
                        if witness {
                            return *polarity == RelationPolarity::Positive;
                        }
                    }
                    *polarity == RelationPolarity::Negative
                }
            }
        })
    }

    fn enhanced_assignments<S: TokenSequence + ?Sized>(
        &self,
        candidate: &Candidate,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Vec<GraphAssignment> {
        if self.enhanced_relations.is_empty() {
            return vec![GraphAssignment::default()];
        }
        let Some(graph) = graph else {
            return Vec::new();
        };
        let mut assignments = enhanced_assignments_from(
            &self.enhanced_relations,
            0,
            candidate,
            tokens,
            graph,
            &GraphAssignment::default(),
        );
        assignments.sort_by(|left, right| {
            self.public_graph_bindings(left, tokens, Some(graph))
                .cmp(&self.public_graph_bindings(right, tokens, Some(graph)))
        });
        assignments.dedup_by(|left, right| {
            self.public_graph_bindings(left, tokens, Some(graph))
                == self.public_graph_bindings(right, tokens, Some(graph))
        });
        assignments
    }

    pub(crate) fn matches_without_assertions<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Vec<ProgramMatch> {
        self.matches_with(predicates, tokens, &mut NoAssertions, None)
    }

    pub(crate) fn matches_without_assertions_with_graph<S: TokenSequence + ?Sized>(
        &self,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: &EnhancedGraph,
    ) -> Vec<ProgramMatch> {
        self.matches_with(predicates, tokens, &mut NoAssertions, Some(graph))
    }

    pub(crate) fn required_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        let mut attributes = Vec::new();
        collect_attributes(&self.definition.expr, &mut attributes);
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
        if !self.enhanced_relations.is_empty() {
            attributes.push(enhanced_graph_key());
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
        attributes.into_iter()
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

    fn public_captures(&self, candidate: &Candidate) -> Box<[Capture]> {
        let mut captures = self
            .captures
            .iter()
            .filter_map(|descriptor| {
                candidate.slots[descriptor.id.index()].map(|(start, end)| Capture {
                    name: descriptor.name.clone(),
                    path: descriptor.path.clone(),
                    start,
                    end,
                })
            })
            .collect::<Vec<_>>();
        captures.sort_by_key(|capture| (capture.start, capture.end));
        captures.into_boxed_slice()
    }

    fn public_graph_bindings<S: TokenSequence + ?Sized>(
        &self,
        assignment: &GraphAssignment,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Box<[GraphBinding]> {
        let Some(graph) = graph else {
            return Box::default();
        };
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
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn enhanced_assignments_from<S: TokenSequence + ?Sized>(
    relations: &[RuntimeEnhancedRelation],
    index: usize,
    candidate: &Candidate,
    tokens: &S,
    graph: &EnhancedGraph,
    assignment: &GraphAssignment,
) -> Vec<GraphAssignment> {
    if index == relations.len() {
        return enhanced_negative_relations_hold(relations, candidate, tokens, graph, assignment)
            .then(|| assignment.clone())
            .into_iter()
            .collect();
    }
    let relation = &relations[index];
    if relation.polarity == RelationPolarity::Negative {
        return enhanced_assignments_from(
            relations,
            index + 1,
            candidate,
            tokens,
            graph,
            assignment,
        );
    }
    graph
        .edges()
        .iter()
        .flat_map(|edge| {
            let EnhancedHead::Node(head) = edge.head() else {
                return Vec::new();
            };
            if !relation_label_matches(&relation.labels, edge.relation()) {
                return Vec::new();
            }
            let mut next = assignment.clone();
            if !enhanced_endpoint_accepts(
                &relation.head,
                head,
                candidate,
                tokens,
                graph,
                &mut next.nodes,
            ) || !enhanced_endpoint_accepts(
                &relation.dependent,
                edge.dependent(),
                candidate,
                tokens,
                graph,
                &mut next.nodes,
            ) {
                return Vec::new();
            }
            if let Some(name) = &relation.edge {
                let value = (head, edge.dependent(), edge.relation().into());
                if next
                    .edges
                    .get(name)
                    .is_some_and(|assigned| *assigned != value)
                {
                    return Vec::new();
                }
                next.edges.insert(name.clone(), value);
            }
            enhanced_assignments_from(relations, index + 1, candidate, tokens, graph, &next)
        })
        .collect()
}

fn enhanced_negative_relations_hold<S: TokenSequence + ?Sized>(
    relations: &[RuntimeEnhancedRelation],
    candidate: &Candidate,
    tokens: &S,
    graph: &EnhancedGraph,
    assignment: &GraphAssignment,
) -> bool {
    relations
        .iter()
        .filter(|relation| relation.polarity == RelationPolarity::Negative)
        .all(|relation| {
            let Some(head) =
                enhanced_endpoint_assignment(&relation.head, candidate, tokens, assignment)
            else {
                return false;
            };
            let Some(dependent) =
                enhanced_endpoint_assignment(&relation.dependent, candidate, tokens, assignment)
            else {
                return false;
            };
            !graph.edges().iter().any(|edge| {
                matches!(edge.head(), EnhancedHead::Node(actual_head) if actual_head == head)
                    && edge.dependent() == dependent
                    && relation_label_matches(&relation.labels, edge.relation())
            })
        })
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
    endpoint: &RuntimeEnhancedEndpoint,
    node: EnhancedNodeId,
    candidate: &Candidate,
    tokens: &S,
    graph: &EnhancedGraph,
    assignments: &mut BTreeMap<compact_str::CompactString, EnhancedNodeId>,
) -> bool {
    match endpoint {
        RuntimeEnhancedEndpoint::Surface(capture) => {
            candidate.slots[capture.index()].is_some_and(|(start, end)| {
                end == start + 1 && node == EnhancedNodeId::Word { token_index: start }
            })
        }
        RuntimeEnhancedEndpoint::BasicHead {
            dependent,
            head_attribute,
        } => derived_basic_head(*dependent, head_attribute, candidate, tokens) == Some(node),
        RuntimeEnhancedEndpoint::Graph { name, symbolic } => {
            if assignments
                .get(name)
                .is_some_and(|assigned| *assigned != node)
            {
                return false;
            }
            let matches = symbolic
                .iter()
                .any(|predicate| graph_node_predicate_matches(node, tokens, graph, predicate));
            if matches {
                assignments.insert(name.clone(), node);
            }
            matches
        }
    }
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
    graph: &EnhancedGraph,
    predicate: &TokenPredicate,
) -> bool {
    let empty = match node {
        EnhancedNodeId::Word { .. } => None,
        EnhancedNodeId::Empty { .. } => graph.empty_nodes().iter().find(|empty| empty.id() == node),
    };
    predicate.iter().all(|atom| {
        let held = match atom.attr.as_str() {
            "node.kind" => {
                scalar_value_matches(if empty.is_some() { "empty" } else { "word" }, &atom.value)
            }
            "node.id" => scalar_value_matches(&node.spelling(), &atom.value),
            key => match node {
                EnhancedNodeId::Word { token_index } => tokens
                    .value_key(token_index, &atom.attr)
                    .is_some_and(|actual| attribute_value_matches(actual, &atom.value)),
                EnhancedNodeId::Empty { .. } => empty.is_some_and(|empty| match key {
                    "text" => empty
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
    graph: &EnhancedGraph,
) -> Box<[GraphNodeAttribute]> {
    const SCALARS: [&str; 4] = ["text", "lemma", "upos", "xpos"];
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
            let Some(empty) = graph.empty_nodes().iter().find(|empty| empty.id() == node) else {
                return Box::default();
            };
            for (spelling, value) in [
                ("text", empty.form()),
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
                    let RuntimeRelationTarget::Captured(dependent) = &relation.target else {
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
        // Same span, exactly rather than approximately: every operand is
        // evaluated from the *same* incoming `candidate.position`, so two
        // operands agreeing on an end position have agreed on the whole span.
        // No span join to get wrong, and no separate anchored-membership
        // primitive — living inside `evaluate` is also what makes `&` compose
        // at any nesting depth for free.
        //
        // Slot vectors survive from the first operand alone. That is sound
        // rather than lossy: `analyze::inspect_intersection_operands` refuses a
        // capture in *every* operand, so the retained candidates carry exactly
        // the slots the enclosing expression handed in, and every operand hands
        // back the same ones.
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
                    let RuntimeRelationTarget::Captured(dependent) = &relation.target else {
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
/// discarded a satisfying assignment whenever a failing one reached the same
/// position first — 82% of `patterns/double-wo-dep.lx`'s misses against a
/// head-sharing oracle, and the defect `matches_with`'s comment already
/// guards against one level up, at end position rather than at assignment.
/// See [the design round](../../../../docs/README.md#historical-evidence).
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
        // Unreachable from a definition that compiled, since `analyze` refuses
        // a capture inside `&`. Walked anyway, because the cost of being wrong
        // here is not a missing descriptor but an out-of-range `slots` index at
        // runtime — `Candidate::slots` is sized from exactly this list.
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
