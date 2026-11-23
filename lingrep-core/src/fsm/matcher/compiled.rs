#[cfg(feature = "bench-instrumentation")]
use super::assertion::AssertionMetrics;
use super::assertion::{
    AssertionCatalog, AssertionProgramId, AssertionSession, CompiledAssertionCatalog,
};
use super::canonical_matches;
use super::capture::CapturePlan;
use super::dense::DenseMachine;
use super::evaluated::EvaluatedPlan;
use super::exec::Matcher;
#[cfg(feature = "bench-instrumentation")]
use super::predicate::PredicateMetrics;
use super::predicate::{PredicateCatalog, PredicateDigest, PredicateSession};
use super::program::Program;
use super::resource::{MatchBudget, MatchResourceError, OutputBudget};
#[cfg(feature = "bench-instrumentation")]
use super::spawn_candidate_metrics::SpawnCandidateAccumulator;
use crate::corpus::conllu::{EnhancedGraph, EnhancedGraphObservation};
use crate::fsm::ProgramStats;
use crate::fsm::compiler::model::{ExecutionCapability, ExecutionDefinition, ExecutionExpr};
use crate::fsm::explanation::{ExplainedMatch, SentenceExplanation};
use crate::fsm::policy::{
    AnchorPositions, AnchorSetDigest, KeyTarget, NestingClaim, PatternPolicy, PolicyStats,
    apply_policy_records, is_hidden,
};
use crate::fsm::state_machine::StateMachineData;
use crate::fsm::types::{Match, Pattern, Patterns, Trace, TraceEvent};
use crate::token_match::{AttributeKey, AttributeKind, TokenSequence};
use compact_str::CompactString;
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
#[cfg(feature = "bench-instrumentation")]
use std::time::{Duration, Instant};

/// Measurement-only phase totals for one [`CompiledCatalog::find_matches`] call.
#[cfg(feature = "bench-instrumentation")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionMetrics {
    pub nfa_duration: Duration,
    pub capture_duration: Duration,
    pub evaluated_duration: Duration,
    pub nfa_matches: usize,
    pub active_capture_plans: usize,
    pub capture_plan_invocations: usize,
    pub extracted_captures: usize,
    pub extracted_capture_tokens: usize,
    pub evaluated_plan_invocations: usize,
    pub evaluated_matches: usize,
    pub predicate_calls: u64,
    pub predicate_memo_hits: u64,
    pub predicate_evaluations: u64,
    pub predicate_distinct_pairs: u64,
    pub predicate_recomputations: u64,
    /// Candidates the tree evaluator constructed, before deduplication.
    pub candidates_produced: u64,
    /// Those surviving deduplication.
    pub candidates_retained: u64,
    pub dense_predicate_calls: u64,
    pub dense_predicate_memo_hits: u64,
    pub dense_predicate_evaluations: u64,
    pub dense_main_transition_predicate_calls: u64,
    pub spawn_predicate_calls: u64,
    pub nfa_previous_token_predicate_calls: u64,
    pub assertion_consume_predicate_calls: u64,
    pub assertion_previous_token_predicate_calls: u64,
    pub capture_token_predicate_calls: u64,
    pub capture_previous_token_predicate_calls: u64,
    pub evaluated_token_predicate_calls: u64,
    pub evaluated_previous_token_predicate_calls: u64,
    pub unclassified_predicate_calls: u64,
}

#[cfg(feature = "bench-instrumentation")]
impl ExecutionMetrics {
    fn record_predicate_metrics(&mut self, metrics: PredicateMetrics) {
        self.predicate_calls = metrics.predicate_calls;
        self.predicate_memo_hits = metrics.predicate_memo_hits;
        self.predicate_evaluations = metrics.predicate_evaluations;
        self.predicate_distinct_pairs = metrics.predicate_distinct_pairs;
        self.predicate_recomputations = metrics.predicate_recomputations;
        self.candidates_produced = metrics.candidates_produced;
        self.candidates_retained = metrics.candidates_retained;
        self.dense_predicate_calls = metrics.dense_predicate_calls;
        self.dense_predicate_memo_hits = metrics.dense_predicate_memo_hits;
        self.dense_predicate_evaluations = metrics.dense_predicate_evaluations;
        self.dense_main_transition_predicate_calls = metrics.dense_main_transition_predicate_calls;
        self.spawn_predicate_calls = metrics.spawn_predicate_calls;
        self.nfa_previous_token_predicate_calls = metrics.nfa_previous_token_predicate_calls;
        self.assertion_consume_predicate_calls = metrics.assertion_consume_predicate_calls;
        self.assertion_previous_token_predicate_calls =
            metrics.assertion_previous_token_predicate_calls;
        self.capture_token_predicate_calls = metrics.capture_token_predicate_calls;
        self.capture_previous_token_predicate_calls =
            metrics.capture_previous_token_predicate_calls;
        self.evaluated_token_predicate_calls = metrics.evaluated_token_predicate_calls;
        self.evaluated_previous_token_predicate_calls =
            metrics.evaluated_previous_token_predicate_calls;
        self.unclassified_predicate_calls = metrics.unclassified_predicate_calls;
        debug_assert_eq!(self.predicate_calls, self.call_site_total());
    }

    fn call_site_total(&self) -> u64 {
        [
            self.dense_main_transition_predicate_calls,
            self.spawn_predicate_calls,
            self.nfa_previous_token_predicate_calls,
            self.assertion_consume_predicate_calls,
            self.assertion_previous_token_predicate_calls,
            self.capture_token_predicate_calls,
            self.capture_previous_token_predicate_calls,
            self.evaluated_token_predicate_calls,
            self.evaluated_previous_token_predicate_calls,
            self.unclassified_predicate_calls,
        ]
        .into_iter()
        .try_fold(0u64, u64::checked_add)
        .expect("predicate call-site total fits in u64")
    }
}

/// Schema-independent immutable catalog data shared across schema bindings.
#[derive(Clone)]
struct CatalogStatic {
    patterns: Patterns,
    lexicons: Option<super::CompiledLexicons>,
    /// Per definition, the capture name at each token position; `None` unless
    /// every execution definition is a run of unconstrained tokens.
    window_captures: Option<IndexMap<CompactString, Box<[Option<CompactString>]>>>,
    assertion_catalog: CompiledAssertionCatalog,
    /// Whether **any** materialized execution definition consults a general
    /// lookaround.
    ///
    /// Every definition, not only the ones that become a capture or
    /// span-equality plan. Narrowing it to those would make the answer correct
    /// only because the recognition machine happens to cover the rest — exactly
    /// the cross-half argument [`CompiledCatalog::requires_assertion_session`]
    /// exists to avoid — so the scan covers more plans than its name suggests.
    ///
    /// Only half the answer; see that method for the other half.
    execution_queries_lookaround: bool,
    anchor_programs: BTreeMap<CompactString, AssertionProgramId>,
    required_attributes: Box<[AttributeKey]>,
    catalog_policy: crate::fsm::policy::CatalogPolicy,
    analyzer_constraints: Box<[CatalogAnalyzerConstraint]>,
    vectors: Box<[crate::fsm::CatalogVectors]>,
    near_tests: Box<[crate::fsm::NearTest]>,
    label_declarations: Box<[crate::fsm::LabelDeclaration]>,
    span_constraint_kinds: BTreeMap<String, Vec<CompactString>>,
}

/// Complete inputs for constructing an immutable [`CompiledCatalog`] in one step.
pub(crate) struct CompiledCatalogParts {
    pub(crate) patterns: Patterns,
    pub(crate) state_machine: StateMachineData,
    pub(crate) assertions: AssertionCatalog,
    pub(crate) anchor_programs: BTreeMap<CompactString, AssertionProgramId>,
    pub(crate) execution: Vec<ExecutionDefinition>,
    pub(crate) lexicons: Option<super::CompiledLexicons>,
    pub(crate) catalog_policy: crate::fsm::policy::CatalogPolicy,
    pub(crate) analyzer_constraints: Vec<CatalogAnalyzerConstraint>,
    pub(crate) vectors: Vec<crate::fsm::CatalogVectors>,
    pub(crate) near_tests: Vec<crate::fsm::NearTest>,
    pub(crate) label_declarations: Vec<crate::fsm::LabelDeclaration>,
}

/// Immutable compiled lingrep patterns artifact produced by [`super::FsmBuilder`].
#[derive(Clone)]
pub struct CompiledCatalog {
    pub(crate) sentence_filter: Option<crate::fsm::SentenceFilter>,
    dense: DenseMachine,
    predicates: PredicateCatalog,
    capture_plans: IndexMap<CompactString, CapturePlan>,
    evaluated_plans: Box<[EvaluatedPlan]>,
    shared: Arc<CatalogStatic>,
}

/// One definition of a catalog [`CompiledCatalog::token_windows`] describes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenWindow {
    pub name: CompactString,
    /// Tokens per match; at least one.
    pub width: usize,
    /// The declared key, in authored order.
    pub key: Vec<WindowKeyField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowKeyField {
    pub name: CompactString,
    /// Offset of the token read, from the window start.
    pub position: usize,
    pub attribute: AttributeKey,
}

fn window_captures(definition: &ExecutionDefinition) -> Option<Box<[Option<CompactString>]>> {
    if !(definition.measures.is_empty()
        && definition.interpretation.is_none()
        && definition.span_constraints.is_empty()
        && definition.relations.is_empty()
        && definition.enhanced_relations.is_empty()
        && definition.node_constraints.is_empty()
        && definition.graph_bindings.is_empty())
    {
        return None;
    }
    let tokens = match &definition.expr {
        ExecutionExpr::Sequence(tokens) => &tokens[..],
        token => std::slice::from_ref(token),
    };
    if tokens.is_empty() {
        return None;
    }
    tokens
        .iter()
        .map(|token| match token {
            ExecutionExpr::Token(predicate) if predicate.is_empty() => Some(None),
            ExecutionExpr::Capture { name, expr, .. } => match &**expr {
                ExecutionExpr::Token(predicate) if predicate.is_empty() => Some(Some(name.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogAnalyzerConstraint {
    family: CompactString,
    resource: Option<CompactString>,
    split: Option<CompactString>,
    segmentation: Box<[CompactString]>,
}

impl CatalogAnalyzerConstraint {
    pub(crate) fn new(
        family: CompactString,
        resource: Option<CompactString>,
        split: Option<CompactString>,
        segmentation: Box<[CompactString]>,
    ) -> Self {
        Self {
            family,
            resource,
            split,
            segmentation,
        }
    }

    pub fn family(&self) -> &str {
        &self.family
    }
    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
    }
    pub fn split(&self) -> Option<&str> {
        self.split.as_deref()
    }
    pub fn segmentation(&self) -> &[CompactString] {
        &self.segmentation
    }

    fn matches(
        &self,
        family: &str,
        resource: Option<&str>,
        split: Option<&str>,
        segmentation: &str,
    ) -> bool {
        self.family == family
            && self
                .resource
                .as_deref()
                .is_none_or(|expected| Some(expected) == resource)
            && self
                .split
                .as_deref()
                .is_none_or(|expected| Some(expected) == split)
            && (self.segmentation.is_empty()
                || self.segmentation.iter().any(|mode| mode == segmentation))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnalyzerCompatibility {
    Unspecified,
    Compatible,
    Mismatch,
}

/// A validated inline lexicon declaration retained by the compiled catalog.
#[derive(Clone, Debug)]
pub struct CompiledLexiconDefinition {
    pub(crate) name: CompactString,
    pub(crate) by: Box<[AttributeKey]>,
    pub(crate) members: Box<[CompactString]>,
    pub(crate) metadata: std::collections::HashMap<CompactString, serde_json::Value>,
}

impl CompiledLexiconDefinition {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn by(&self) -> &[AttributeKey] {
        &self.by
    }

    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }

    #[must_use]
    pub fn metadata(&self) -> &std::collections::HashMap<CompactString, serde_json::Value> {
        &self.metadata
    }
}

trait MatchAssertionMode<S: TokenSequence + ?Sized> {
    fn run_dense(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Vec<Match>, MatchResourceError>;

    fn run_dense_with_trace(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Trace, MatchResourceError>;

    fn extract_capture(
        &mut self,
        plan: &CapturePlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        matched: &mut Match,
    ) -> Result<(), MatchResourceError>;

    fn run_evaluated(
        &mut self,
        plan: &EvaluatedPlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Result<Vec<Match>, MatchResourceError>;
}

struct NoAssertions;

impl<S: TokenSequence + ?Sized> MatchAssertionMode<S> for NoAssertions {
    #[inline(always)]
    fn run_dense(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Vec<Match>, MatchResourceError> {
        Matcher::try_find_matches_dense(output, dense, predicates, tokens)
    }

    #[inline(always)]
    fn run_dense_with_trace(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Trace, MatchResourceError> {
        Matcher::try_find_matches_with_trace_dense(output, dense, predicates, tokens)
    }

    #[inline(always)]
    fn extract_capture(
        &mut self,
        plan: &CapturePlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        matched: &mut Match,
    ) -> Result<(), MatchResourceError> {
        plan.extract_without_assertions(budget, predicates, tokens, matched)
    }

    #[inline(always)]
    fn run_evaluated(
        &mut self,
        plan: &EvaluatedPlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        match graph {
            Some(graph) => plan
                .try_find_matches_without_assertions_with_graph(budget, predicates, tokens, graph),
            None => plan.try_find_matches_without_assertions(budget, predicates, tokens),
        }
    }
}

struct WithAssertions<'a, 't, S: TokenSequence + ?Sized>(AssertionSession<'a, 't, S>);

impl<S: TokenSequence + ?Sized> MatchAssertionMode<S> for WithAssertions<'_, '_, S> {
    #[inline(always)]
    fn run_dense(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Vec<Match>, MatchResourceError> {
        Matcher::try_find_matches_dense_with_assertions(
            output,
            dense,
            predicates,
            tokens,
            &mut self.0,
        )
    }

    #[inline(always)]
    fn run_dense_with_trace(
        &mut self,
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Trace, MatchResourceError> {
        Matcher::try_find_matches_with_trace_dense_with_assertions(
            output,
            dense,
            predicates,
            tokens,
            &mut self.0,
        )
    }

    #[inline(always)]
    fn extract_capture(
        &mut self,
        plan: &CapturePlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        matched: &mut Match,
    ) -> Result<(), MatchResourceError> {
        plan.extract(budget, predicates, tokens, matched, &mut self.0)
    }

    #[inline(always)]
    fn run_evaluated(
        &mut self,
        plan: &EvaluatedPlan,
        budget: &mut MatchBudget<'_>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        graph: Option<&EnhancedGraph>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        match graph {
            Some(graph) => {
                plan.try_find_matches_with_graph(budget, predicates, tokens, &mut self.0, graph)
            }
            None => plan.try_find_matches(budget, predicates, tokens, &mut self.0),
        }
    }
}

trait MatchObserver {
    type Timer;

    fn begin_phase(&mut self) -> Self::Timer;
    fn finish_nfa(&mut self, _timer: Self::Timer, _nfa_matches: usize) {}
    fn on_capture(&mut self, _matched: &Match) {}
    fn finish_captures(&mut self, _timer: Self::Timer) {}
    fn on_evaluated_plan(&mut self, _found_len: usize) {}
    fn finish_evaluated(&mut self, _timer: Self::Timer) {}
}

struct NoObserver;

impl MatchObserver for NoObserver {
    type Timer = ();

    #[inline(always)]
    fn begin_phase(&mut self) {}
}

#[cfg(feature = "bench-instrumentation")]
struct ExecutionMetricsObserver(ExecutionMetrics);

#[cfg(feature = "bench-instrumentation")]
impl MatchObserver for ExecutionMetricsObserver {
    type Timer = Instant;

    #[inline(always)]
    fn begin_phase(&mut self) -> Instant {
        Instant::now()
    }

    #[inline(always)]
    fn finish_nfa(&mut self, started: Instant, nfa_matches: usize) {
        self.0.nfa_duration = started.elapsed();
        self.0.nfa_matches = nfa_matches;
    }

    #[inline(always)]
    fn on_capture(&mut self, matched: &Match) {
        self.0.capture_plan_invocations += 1;
        self.0.extracted_captures += matched.captures.len();
        self.0.extracted_capture_tokens += matched
            .captures
            .iter()
            .map(|capture| capture.end.saturating_sub(capture.start))
            .sum::<usize>();
    }

    #[inline(always)]
    fn finish_captures(&mut self, started: Instant) {
        self.0.capture_duration = started.elapsed();
    }

    #[inline(always)]
    fn on_evaluated_plan(&mut self, found_len: usize) {
        self.0.evaluated_plan_invocations += 1;
        self.0.evaluated_matches += found_len;
    }

    #[inline(always)]
    fn finish_evaluated(&mut self, started: Instant) {
        self.0.evaluated_duration = started.elapsed();
    }
}

struct MatchRun<'a, 'b, 'p, O> {
    budget: &'a mut MatchBudget<'b>,
    predicates: &'a PredicateSession<'p>,
    graph: Option<&'a EnhancedGraph>,
    observer: &'a mut O,
}

fn catalog_match_key(matched: &Match) -> (usize, usize, &str, Option<&str>) {
    (
        matched.start,
        matched.end,
        &matched.pattern,
        matched
            .lexical
            .as_ref()
            .map(|lexical| lexical.term.as_str()),
    )
}

fn merge_catalog_matches(
    ordinary: Vec<Match>,
    lexical: Vec<Match>,
    output_budget: &OutputBudget,
) -> Result<Vec<Match>, MatchResourceError> {
    if lexical.is_empty() {
        return Ok(ordinary);
    }
    if ordinary.is_empty() {
        return Ok(lexical);
    }
    output_budget.reserve_buffer::<Match>(ordinary.len().saturating_add(lexical.len()))?;
    let mut output = Vec::with_capacity(ordinary.len() + lexical.len());
    let mut ordinary = ordinary.into_iter().peekable();
    let mut lexical = lexical.into_iter().peekable();
    while let (Some(left), Some(right)) = (ordinary.peek(), lexical.peek()) {
        if catalog_match_key(left) <= catalog_match_key(right) {
            output.push(ordinary.next().expect("peeked ordinary match"));
        } else {
            output.push(lexical.next().expect("peeked lexical match"));
        }
    }
    output.extend(ordinary);
    output.extend(lexical);
    Ok(output)
}

impl CompiledCatalog {
    /// Constructs a complete, immutable [`CompiledCatalog`] in one step.
    ///
    /// Ordering invariants enforced here:
    /// 1. Predicate interning runs in deterministic order: recognition NFA,
    ///    assertion catalog, then execution plans (preserving [`Self::predicate_digest`]).
    /// 2. `required_attributes` unions every execution context (recognition NFA,
    ///    capture plans, evaluated plans, assertions, and lexicons).
    /// 3. Synthesized `__policy__*` emitters are removed from the public
    ///    [`Self::patterns`] map after all matching structures are derived.
    pub(crate) fn from_parts(parts: CompiledCatalogParts) -> Self {
        let mut predicates = PredicateCatalog::default();
        let dense = DenseMachine::build(
            &parts.state_machine.state_transitions,
            &parts.state_machine.final_states,
            &parts.state_machine.state_patterns,
            &mut predicates,
        );
        let assertion_catalog = parts.assertions.compile(&mut predicates);

        let sentence_filter = crate::fsm::compiler::derive_sentence_filter(&parts.execution);
        let execution_queries_lookaround = parts
            .execution
            .iter()
            .any(|definition| definition.expr.queries_lookaround());
        let window_captures = parts
            .execution
            .iter()
            .map(|definition| Some((definition.name.clone(), window_captures(definition)?)))
            .collect();
        let mut span_constraint_kinds: BTreeMap<String, Vec<CompactString>> = BTreeMap::new();
        for definition in &parts.execution {
            for constraint in &definition.span_constraints {
                let names = span_constraint_kinds
                    .entry(constraint.kind.clone())
                    .or_default();
                if names.last() != Some(&definition.name) {
                    names.push(definition.name.clone());
                }
            }
        }
        let mut capture_plans = IndexMap::new();
        let mut evaluated = Vec::new();
        for definition in parts.execution {
            match definition.capability {
                ExecutionCapability::Plain => {}
                ExecutionCapability::Captures => {
                    capture_plans.insert(
                        definition.name.clone(),
                        CapturePlan(Program::new(definition, &mut predicates)),
                    );
                }
                ExecutionCapability::SpanEquality
                | ExecutionCapability::Intersection
                | ExecutionCapability::AnyOrder
                | ExecutionCapability::Relations => {
                    evaluated.push(EvaluatedPlan(Program::new(definition, &mut predicates)));
                }
            }
        }
        let evaluated_plans = evaluated.into_boxed_slice();

        let lexicon_descriptors = parts
            .lexicons
            .as_ref()
            .map_or(&[][..], super::CompiledLexicons::descriptors);
        let required_attributes = dense
            .required_attributes()
            .chain(
                capture_plans
                    .values()
                    .flat_map(|plan| plan.0.required_attributes()),
            )
            .chain(
                evaluated_plans
                    .iter()
                    .flat_map(|plan| plan.0.required_attributes()),
            )
            .chain(assertion_catalog.required_attributes())
            .chain(
                lexicon_descriptors
                    .iter()
                    .flat_map(|lexicon| lexicon.by.iter().cloned()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        // Removes synthesized policy emitters from the *public* catalog.
        // They must keep matching — the engine needs their records as evidence
        // for a fragment operand — so only `patterns` is filtered, and the
        // state machine is untouched.
        let mut patterns = parts.patterns;
        patterns
            .0
            .retain(|name, _| !crate::fsm::policy::is_hidden(name));

        let shared = Arc::new(CatalogStatic {
            patterns,
            lexicons: parts.lexicons,
            window_captures,
            assertion_catalog,
            execution_queries_lookaround,
            anchor_programs: parts.anchor_programs,
            required_attributes,
            catalog_policy: parts.catalog_policy,
            analyzer_constraints: parts.analyzer_constraints.into_boxed_slice(),
            vectors: parts.vectors.into_boxed_slice(),
            near_tests: parts.near_tests.into_boxed_slice(),
            label_declarations: parts.label_declarations.into_boxed_slice(),
            span_constraint_kinds,
        });

        Self {
            sentence_filter,
            dense,
            predicates,
            capture_plans,
            evaluated_plans,
            shared,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_nfa_for_test(
        patterns: Patterns,
        state_machine: StateMachineData,
        assertions: AssertionCatalog,
    ) -> Self {
        Self::from_parts(CompiledCatalogParts {
            patterns,
            state_machine,
            assertions,
            anchor_programs: BTreeMap::new(),
            execution: Vec::new(),
            lexicons: None,
            catalog_policy: crate::fsm::policy::CatalogPolicy::default(),
            analyzer_constraints: Vec::new(),
            vectors: Vec::new(),
            near_tests: Vec::new(),
            label_declarations: Vec::new(),
        })
    }

    /// The assertion program recognizing each declared anchor's `exclude`, by
    /// anchor name.
    ///
    /// Empty for a catalog that declares no anchor.
    ///
    /// `#[cfg(test)]`, and named for it, because the anchor evaluator reads the
    /// field directly — it is built here, in this module. The only caller an
    /// accessor can ever have is a test in a sibling module, which is what
    /// [`Self::predicates_for_test`] and [`Self::dense_for_test`] already say
    /// about themselves.
    #[cfg(test)]
    pub(crate) fn anchor_programs_for_test(&self) -> &BTreeMap<CompactString, AssertionProgramId> {
        &self.shared.anchor_programs
    }

    /// The compiled assertion catalog every [`AssertionProgramId`] indexes.
    #[cfg(test)]
    pub(crate) fn assertion_catalog_for_test(&self) -> &CompiledAssertionCatalog {
        &self.shared.assertion_catalog
    }

    /// The catalog's distinct `near` tests in first-appearance order, each
    /// with the values it was resolved to. Empty when the catalog has none.
    #[must_use]
    pub fn near_tests(&self) -> &[crate::fsm::NearTest] {
        &self.shared.near_tests
    }

    /// The catalog's `vectors:` entries, in source order. Empty when the
    /// catalog declares none.
    #[must_use]
    pub fn vectors(&self) -> &[crate::fsm::CatalogVectors] {
        &self.shared.vectors
    }

    /// The sentence-span kinds that `where span(…)`, `within_span` and
    /// `span_dependency` constraints name, each with the definitions that
    /// carry such a constraint, in catalog order. A definition matches nothing
    /// in a sentence without a span of a kind it names.
    pub fn span_constraint_kinds(&self) -> &BTreeMap<String, Vec<CompactString>> {
        &self.shared.span_constraint_kinds
    }

    /// The catalog's `labels:` fields, in source order.
    pub fn label_declarations(&self) -> &[crate::fsm::LabelDeclaration] {
        &self.shared.label_declarations
    }

    pub fn analyzer_constraints(&self) -> &[CatalogAnalyzerConstraint] {
        &self.shared.analyzer_constraints
    }

    pub fn analyzer_compatibility(
        &self,
        family: &str,
        resource: Option<&str>,
        split: Option<&str>,
        segmentation: &str,
    ) -> AnalyzerCompatibility {
        if self.shared.analyzer_constraints.is_empty() {
            AnalyzerCompatibility::Unspecified
        } else if self
            .shared
            .analyzer_constraints
            .iter()
            .any(|constraint| constraint.matches(family, resource, split, segmentation))
        {
            AnalyzerCompatibility::Compatible
        } else {
            AnalyzerCompatibility::Mismatch
        }
    }

    /// Adjusts publication and policy explanations for `overlaps` without
    /// changing the candidates matched by each definition.
    #[must_use]
    pub fn with_overlaps(mut self, overlaps: crate::fsm::policy_program::Overlaps) -> Self {
        let shared = Arc::make_mut(&mut self.shared);
        let policy = std::mem::take(&mut shared.catalog_policy);
        shared.catalog_policy = policy.with_overlaps(overlaps);
        self
    }

    /// The effective publication policy, including any [`Self::with_overlaps`]
    /// adjustment; [`crate::fsm::policy::CatalogPolicy::declared`] reports
    /// whether it contains statements.
    pub fn catalog_policy(&self) -> &crate::fsm::policy::CatalogPolicy {
        &self.shared.catalog_policy
    }

    /// Structural identity of the anchor set this catalog declares.
    ///
    /// What a reader of an [`AnchorPositions`] must present to
    /// [`AnchorPositions::get`]. [`AnchorSetDigest::default`] for a catalog
    /// declaring no anchor.
    pub fn anchor_digest(&self) -> &AnchorSetDigest {
        &self.shared.catalog_policy.anchor_digest
    }

    /// Each declared anchor's `max_nesting` claim, by anchor name.
    ///
    /// Empty for a catalog declaring no anchor, and otherwise carrying exactly
    /// the keys of [`CatalogPolicy::anchors`]. Claims are mandatory, so an
    /// `exclude` without one does not compile. What executes
    /// them is [`crate::fsm::verify_anchor_claims`], which needs a dictionary.
    ///
    /// [`CatalogPolicy::anchors`]: crate::fsm::CatalogPolicy::anchors
    pub fn nesting_claims(&self) -> &BTreeMap<CompactString, NestingClaim> {
        &self.shared.catalog_policy.nesting_claims
    }

    /// A borrowed view of everything the anchor pre-pass needs.
    ///
    /// Not a performance affordance — constructing one is three pointer copies
    /// and a 32-byte digest copy, so hoisting it out of a sentence loop saves
    /// nothing measurable and all the per-sentence cost is inside
    /// [`AnchorEvaluator::positions`]. It exists so that a long-lived consumer
    /// can *hold* the borrow: a sink that evaluates anchors inside its per-
    /// sentence method cannot reach a `&CompiledCatalog` from there without taking a
    /// lifetime parameter, and storing this instead is what lets it.
    ///
    /// [`Self::anchor_positions`] is the one-shot spelling for everyone else.
    pub fn anchor_evaluator(&self) -> AnchorEvaluator<'_> {
        AnchorEvaluator {
            programs: &self.shared.anchor_programs,
            assertions: &self.shared.assertion_catalog,
            predicates: &self.predicates,
            digest: self.shared.catalog_policy.anchor_digest,
        }
    }

    /// Every declared anchor's position in one sentence.
    ///
    /// The result is opaque and digest-checked: read it with
    /// [`AnchorPositions::get`], passing [`Self::anchor_digest`]. An anchor whose
    /// `exclude` matches no suffix lands on the raw sentence end; an anchor this
    /// catalog does not declare is an error rather than a position. See
    /// [`AnchorEvaluator::positions`], which this delegates to.
    pub fn anchor_positions<S: TokenSequence + ?Sized>(&self, tokens: &S) -> AnchorPositions {
        self.anchor_evaluator().positions(tokens)
    }

    /// Returns policy-filtered matches, excluding synthesized evidence records.
    ///
    /// Policy statistics are discarded; use [`crate::execution::execute_analyzed`] to retain them.
    ///
    /// # Errors
    ///
    /// Returns an anchor-resolution error if the catalog's anchor cannot be resolved.
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_published_matches`] to handle this error.
    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "The legacy error type cannot represent resource exhaustion; the try_* replacement returns it."
    )]
    pub fn find_published_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<Vec<Match>, crate::fsm::policy::AnchorResolutionError> {
        match self.try_find_published_matches(tokens) {
            Ok(value) => Ok(value),
            Err(crate::fsm::MatchPublicationError::Policy(error)) => Err(error),
            Err(crate::fsm::MatchPublicationError::Resource(error)) => panic!("{error}"),
        }
    }

    /// As [`Self::find_published_matches`], with matching resource failures returned as errors.
    pub fn try_find_published_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<Vec<Match>, crate::fsm::MatchPublicationError> {
        let canonical = self.try_find_matches(tokens)?;
        if self.shared.catalog_policy.declared() {
            let anchors = self.anchor_positions(tokens);
            let mut stats = crate::fsm::policy::PolicyStats::default();
            crate::fsm::policy::apply_policy(
                &self.shared.catalog_policy,
                &canonical,
                tokens,
                &anchors,
                &mut stats,
            )
            .map_err(Into::into)
        } else {
            // Nothing declared means nothing synthesized, so the canonical
            // stream is already the published one — no copy, and no anchor
            // pre-pass either.
            Ok(canonical)
        }
    }

    /// Explains how each visible raw candidate in one sentence reached the
    /// publication boundary.
    ///
    /// Unlike [`Self::find_published_matches`], this retains candidates the
    /// catalog policy suppressed and pairs a surviving candidate with its
    /// possibly transformed published match. Synthesized `__policy__` evidence
    /// is never included. This is intentionally opt-in for interactive
    /// inspection; normal corpus execution should keep using its streaming
    /// publication seam.
    ///
    /// # Errors
    ///
    /// Returns [`crate::fsm::policy::AnchorResolutionError`] if the declared
    /// result policy cannot resolve an anchor. This is the same failure mode as
    /// [`Self::find_published_matches`].
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_explain_sentence`] to handle this error.
    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "The legacy error type cannot represent resource exhaustion; the try_* replacement returns it."
    )]
    pub fn explain_sentence<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<SentenceExplanation, crate::fsm::policy::AnchorResolutionError> {
        match self.try_explain_sentence(tokens) {
            Ok(value) => Ok(value),
            Err(crate::fsm::MatchPublicationError::Policy(error)) => Err(error),
            Err(crate::fsm::MatchPublicationError::Resource(error)) => panic!("{error}"),
        }
    }

    /// As [`Self::explain_sentence`], with matching resource failures returned as errors.
    pub fn try_explain_sentence<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<SentenceExplanation, crate::fsm::MatchPublicationError> {
        self.try_explain_sentence_with_context(tokens, None)
    }

    /// [`Self::explain_sentence`] with `within=` lookarounds reading the
    /// windows in `context`, as in [`Self::find_matches_in_context`].
    ///
    /// # Errors
    ///
    /// As [`Self::explain_sentence`].
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_explain_sentence_in_context`] to handle this error.
    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "The legacy error type cannot represent resource exhaustion; the try_* replacement returns it."
    )]
    pub fn explain_sentence_in_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: &'t super::LookaroundContext<'t, S>,
    ) -> Result<SentenceExplanation, crate::fsm::policy::AnchorResolutionError> {
        match self.try_explain_sentence_in_context(tokens, context) {
            Ok(value) => Ok(value),
            Err(crate::fsm::MatchPublicationError::Policy(error)) => Err(error),
            Err(crate::fsm::MatchPublicationError::Resource(error)) => panic!("{error}"),
        }
    }

    /// As [`Self::explain_sentence_in_context`], with matching resource failures returned as errors.
    pub fn try_explain_sentence_in_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: &'t super::LookaroundContext<'t, S>,
    ) -> Result<SentenceExplanation, crate::fsm::MatchPublicationError> {
        self.try_explain_sentence_with_context(tokens, Some(context))
    }

    fn try_explain_sentence_with_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: Option<&'t super::LookaroundContext<'t, S>>,
    ) -> Result<SentenceExplanation, crate::fsm::MatchPublicationError> {
        let canonical = self.try_find_matches_with_context(tokens, context)?;
        let policy = &self.shared.catalog_policy;
        let policy_declared = policy.declared();
        let mut published_by_raw = std::collections::HashMap::<Match, Vec<Match>>::new();

        if policy_declared {
            let anchors = self.anchor_positions(tokens);
            let mut stats = PolicyStats::default();
            let mut program = ProgramStats::for_program(policy.program());
            for record in apply_policy_records(
                policy,
                &canonical,
                tokens,
                &anchors,
                &mut stats,
                &mut program,
                None,
            )? {
                let raw = record.raw.clone();
                published_by_raw
                    .entry(raw)
                    .or_default()
                    .push(record.into_match());
            }
        }

        let candidates = canonical
            .into_iter()
            .filter(|raw| !is_hidden(&raw.pattern))
            .map(|raw| {
                let published = if policy_declared {
                    published_by_raw.get_mut(&raw).and_then(Vec::pop)
                } else {
                    Some(raw.clone())
                };
                ExplainedMatch { raw, published }
            })
            .collect();

        Ok(SentenceExplanation { candidates })
    }

    pub(crate) fn spawn_decline(&self) -> Option<&super::spawn::SpawnDecline> {
        self.dense.spawn_decline()
    }

    /// Whether matching this catalog needs a per-sentence [`AssertionSession`].
    ///
    /// Asked of the two things that can consult one — the recognition machine
    /// and the capture/span-equality plans — rather than of the assertion
    /// catalog's occurrence count. The difference is the point: an anchor's
    /// `exclude` registers an occurrence whenever a lookaround appears inside
    /// it, and no *pattern* can reach that occurrence, so counting occurrences
    /// moved a whole catalog onto the session path on the strength of a
    /// declaration that changes no result.
    ///
    /// Each executor answers for itself, so neither half has to be argued to
    /// cover the other. That matters: the execution oracle in
    /// `compiler/oracle.rs` builds a matcher from a lowered NFA with no
    /// execution plans at all.
    ///
    /// [`crate::fsm::compiler`] reads this too, for the `spawn_decline` gate, so
    /// that the compile-time limit and the per-sentence path answer one question
    /// rather than two that can drift apart.
    pub(crate) fn requires_assertion_session(&self) -> bool {
        self.dense.queries_lookaround() || self.shared.execution_queries_lookaround
    }

    #[cfg(test)]
    pub(crate) fn dense_for_test(&self) -> &DenseMachine {
        &self.dense
    }

    #[cfg(test)]
    pub(crate) fn predicates_for_test(&self) -> &PredicateCatalog {
        &self.predicates
    }

    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches`] for untrusted patterns.
    pub fn find_matches<S: TokenSequence + ?Sized>(&self, tokens: &S) -> Vec<Match> {
        self.try_find_matches(tokens)
            .expect("matching exceeded its resource budget")
    }

    /// Returns complete canonical matches, or an error when matching exceeds a graph budget or the memory budget for results.
    pub fn try_find_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_with_context(tokens, None)
    }

    /// Matches one sentence. Each `within=` lookaround reads the window that
    /// `context` supplies for its scope.
    ///
    /// Matching reads at most [`Self::lookaround_reach`] tokens of a window on
    /// each side of the sentence.
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches_in_context`] to handle this error.
    pub fn find_matches_in_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: &'t super::LookaroundContext<'t, S>,
    ) -> Vec<Match> {
        self.try_find_matches_with_context(tokens, Some(context))
            .expect("matching exceeded its resource budget")
    }

    /// As [`Self::find_matches_in_context`], with matching resource failures returned as errors.
    pub fn try_find_matches_in_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: &'t super::LookaroundContext<'t, S>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_with_context(tokens, Some(context))
    }

    /// The context each scope's `within=` lookarounds can read.
    pub fn lookaround_reach(
        &self,
    ) -> BTreeMap<crate::fsm::LookaroundScope, super::LookaroundReach> {
        self.shared.assertion_catalog.lookaround_reach()
    }

    /// The division kinds that `within=` lookarounds name, in
    /// [`Self::lookaround_reach`] order.
    pub fn lookaround_division_kinds(&self) -> Vec<compact_str::CompactString> {
        self.lookaround_reach()
            .into_keys()
            .filter_map(|scope| match scope {
                crate::fsm::LookaroundScope::Document => None,
                crate::fsm::LookaroundScope::Division(kind) => Some(kind),
            })
            .collect()
    }

    pub(crate) fn try_find_matches_with_context<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: Option<&'t super::LookaroundContext<'t, S>>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_with_context_and_output(tokens, context, &OutputBudget::default())
    }

    #[inline(always)]
    fn run_match_pipeline_with_mode<S, M, O>(
        &self,
        tokens: &S,
        run: &mut MatchRun<'_, '_, '_, O>,
        mode: &mut M,
    ) -> Result<Vec<Match>, MatchResourceError>
    where
        S: TokenSequence + ?Sized,
        M: MatchAssertionMode<S>,
        O: MatchObserver,
    {
        let nfa_timer = run.observer.begin_phase();
        let matches = mode.run_dense(run.budget.output, &self.dense, run.predicates, tokens)?;
        run.observer.finish_nfa(nfa_timer, matches.len());
        self.complete_dense_matches(tokens, run, mode, matches)
    }

    #[inline(always)]
    fn complete_dense_matches<S, M, O>(
        &self,
        tokens: &S,
        run: &mut MatchRun<'_, '_, '_, O>,
        mode: &mut M,
        mut matches: Vec<Match>,
    ) -> Result<Vec<Match>, MatchResourceError>
    where
        S: TokenSequence + ?Sized,
        M: MatchAssertionMode<S>,
        O: MatchObserver,
    {
        let capture_timer = run.observer.begin_phase();
        for matched in &mut matches {
            if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                mode.extract_capture(plan, run.budget, run.predicates, tokens, matched)?;
                run.observer.on_capture(matched);
            }
        }
        run.observer.finish_captures(capture_timer);

        let evaluated_timer = run.observer.begin_phase();
        for plan in &self.evaluated_plans {
            let found = mode.run_evaluated(plan, run.budget, run.predicates, tokens, run.graph)?;
            run.observer.on_evaluated_plan(found.len());
            run.budget.output.reserve_vec(&mut matches, found.len())?;
            matches.extend(found);
        }
        run.observer.finish_evaluated(evaluated_timer);

        self.finish_catalog_matches(tokens, canonical_matches(matches), run.budget.output)
    }

    #[inline(always)]
    fn try_find_matches_core<'t, S, O>(
        &self,
        tokens: &'t S,
        graph: Option<&EnhancedGraph>,
        context: Option<&'t super::LookaroundContext<'t, S>>,
        output: &OutputBudget,
        observer: &mut O,
    ) -> Result<Vec<Match>, MatchResourceError>
    where
        S: TokenSequence + ?Sized,
        O: MatchObserver,
    {
        output.check()?;
        let mut budget = MatchBudget::with_output(output);
        let predicates = PredicateSession::new(&self.predicates);
        let mut run = MatchRun {
            budget: &mut budget,
            predicates: &predicates,
            graph,
            observer,
        };
        if self.requires_assertion_session() {
            let mut mode = WithAssertions(
                AssertionSession::new(&self.shared.assertion_catalog, &predicates, tokens)
                    .with_context(context),
            );
            self.run_match_pipeline_with_mode(tokens, &mut run, &mut mode)
        } else {
            self.run_match_pipeline_with_mode(tokens, &mut run, &mut NoAssertions)
        }
    }

    fn run_trace_with_mode<S, M>(
        &self,
        tokens: &S,
        run: &mut MatchRun<'_, '_, '_, NoObserver>,
        mode: &mut M,
    ) -> Result<Trace, MatchResourceError>
    where
        S: TokenSequence + ?Sized,
        M: MatchAssertionMode<S>,
    {
        let mut trace =
            mode.run_dense_with_trace(run.budget.output, &self.dense, run.predicates, tokens)?;
        let matches = std::mem::take(&mut trace.matches);
        trace.matches = self.complete_dense_matches(tokens, run, mode, matches)?;
        Ok(trace)
    }

    pub(crate) fn try_find_matches_with_context_and_output<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        context: Option<&'t super::LookaroundContext<'t, S>>,
        output: &OutputBudget,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_core(tokens, None, context, output, &mut NoObserver)
    }

    /// Matches with an explicit enhanced-graph observation.
    ///
    /// `Unavailable` and `Incomplete` cannot satisfy an enhanced constraint,
    /// but ordinary definitions in a mixed catalog continue to match. The
    /// plain [`Self::find_matches`] method has those same unavailable-graph
    /// semantics; bound execution additionally fails when its source did not
    /// request a graph required by the catalog.
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches_with_enhanced_graph`] to handle this error.
    #[expect(
        clippy::panic,
        clippy::panic_in_result_fn,
        reason = "The legacy error type cannot represent resource exhaustion; the try_* replacement returns it."
    )]
    pub fn find_matches_with_enhanced_graph<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        observation: &EnhancedGraphObservation,
    ) -> Result<Vec<Match>, crate::fsm::EnhancedGraphExecutionError> {
        match self.try_find_matches_with_enhanced_graph(tokens, observation) {
            Ok(value) => Ok(value),
            Err(crate::fsm::EnhancedMatchError::Graph(error)) => Err(error),
            Err(crate::fsm::EnhancedMatchError::Resource(error)) => panic!("{error}"),
        }
    }

    /// As [`Self::find_matches_with_enhanced_graph`], with matching resource failures returned as errors.
    pub fn try_find_matches_with_enhanced_graph<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        observation: &EnhancedGraphObservation,
    ) -> Result<Vec<Match>, crate::fsm::EnhancedMatchError> {
        let Some(graph) = observation.complete() else {
            return self.try_find_matches(tokens).map_err(Into::into);
        };
        if graph.word_count() != tokens.len() {
            return Err(crate::fsm::EnhancedGraphExecutionError {
                expected_words: tokens.len(),
                actual_words: graph.word_count(),
            }
            .into());
        }
        self.try_find_matches_with_complete_graph(tokens, graph, None)
            .map_err(Into::into)
    }

    pub(crate) fn try_find_matches_with_complete_graph<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        graph: &EnhancedGraph,
        context: Option<&'t super::LookaroundContext<'t, S>>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_with_complete_graph_and_output(
            tokens,
            graph,
            context,
            &OutputBudget::default(),
        )
    }

    pub(crate) fn try_find_matches_with_complete_graph_and_output<'t, S: TokenSequence + ?Sized>(
        &self,
        tokens: &'t S,
        graph: &EnhancedGraph,
        context: Option<&'t super::LookaroundContext<'t, S>>,
        output: &OutputBudget,
    ) -> Result<Vec<Match>, MatchResourceError> {
        self.try_find_matches_core(tokens, Some(graph), context, output, &mut NoObserver)
    }

    /// Runs normal matching and reports assertion-evaluator work.
    ///
    /// This measurement-only entry point is compiled solely with the
    /// `bench-instrumentation` feature. Default builds contain neither this
    /// method nor the counters it reads.
    #[cfg(feature = "bench-instrumentation")]
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches`] for untrusted patterns.
    pub fn find_matches_with_assertion_metrics<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> (Vec<Match>, AssertionMetrics) {
        if !self.requires_assertion_session() {
            return (self.find_matches(tokens), AssertionMetrics::default());
        }

        let output = OutputBudget::default();
        let mut budget = MatchBudget::with_output(&output);
        let predicates = PredicateSession::new(&self.predicates);
        let mut mode = WithAssertions(AssertionSession::new(
            &self.shared.assertion_catalog,
            &predicates,
            tokens,
        ));
        let mut observer = NoObserver;
        let mut run = MatchRun {
            budget: &mut budget,
            predicates: &predicates,
            graph: None,
            observer: &mut observer,
        };
        let matches = self
            .run_match_pipeline_with_mode(tokens, &mut run, &mut mode)
            .expect("matching exceeded its resource budget");
        let metrics = mode.0.metrics();
        (matches, metrics)
    }

    /// Runs normal matching and reports coarse execution-phase work.
    ///
    /// This follows the same production plans and ordering as
    /// [`Self::find_matches`]. The timers and counters are compiled only for
    /// benchmark-instrumentation builds.
    #[cfg(feature = "bench-instrumentation")]
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches`] for untrusted patterns.
    pub fn find_matches_with_execution_metrics<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> (Vec<Match>, ExecutionMetrics) {
        self.find_matches_with_instrumentation(tokens, None)
    }

    /// Runs normal matching and accumulates post-readiness spawn candidates.
    ///
    /// This measurement-only entry point preserves the production call order
    /// and returns the same [`ExecutionMetrics`] as
    /// [`Self::find_matches_with_execution_metrics`].
    #[cfg(feature = "bench-instrumentation")]
    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches`] for untrusted patterns.
    pub fn find_matches_with_spawn_candidate_metrics<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        accumulator: &mut SpawnCandidateAccumulator,
    ) -> (Vec<Match>, ExecutionMetrics) {
        self.find_matches_with_instrumentation(tokens, Some(accumulator))
    }

    #[cfg(feature = "bench-instrumentation")]
    fn find_matches_with_instrumentation<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        spawn: Option<&mut SpawnCandidateAccumulator>,
    ) -> (Vec<Match>, ExecutionMetrics) {
        let output = OutputBudget::default();
        let mut budget = MatchBudget::with_output(&output);
        let predicates = PredicateSession::new(&self.predicates);
        if spawn.is_some() {
            let (group_count, shape_count) = self.dense.spawn_table.spawn_candidate_dimensions();
            predicates.begin_spawn_candidate_measurement(group_count, shape_count);
        }
        let mut observer = ExecutionMetricsObserver(ExecutionMetrics {
            active_capture_plans: self.capture_plans.len(),
            ..ExecutionMetrics::default()
        });
        let mut run = MatchRun {
            budget: &mut budget,
            predicates: &predicates,
            graph: None,
            observer: &mut observer,
        };
        let matches = if self.requires_assertion_session() {
            let mut mode = WithAssertions(AssertionSession::new(
                &self.shared.assertion_catalog,
                &predicates,
                tokens,
            ));
            self.run_match_pipeline_with_mode(tokens, &mut run, &mut mode)
        } else {
            self.run_match_pipeline_with_mode(tokens, &mut run, &mut NoAssertions)
        }
        .expect("matching exceeded its resource budget");
        observer.0.record_predicate_metrics(predicates.metrics());
        if let Some(accumulator) = spawn {
            predicates.merge_spawn_candidate_metrics(accumulator);
        }
        (matches, observer.0)
    }

    /// # Panics
    /// Panics when matching exceeds a resource budget; use [`Self::try_find_matches_with_trace`] to handle this error.
    pub fn find_matches_with_trace<S: TokenSequence + ?Sized>(&self, tokens: &S) -> Trace {
        self.try_find_matches_with_trace(tokens)
            .expect("matching exceeded its resource budget")
    }

    /// As [`Self::find_matches_with_trace`], with matching resource failures returned as errors.
    pub fn try_find_matches_with_trace<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<Trace, MatchResourceError> {
        let output = OutputBudget::default();
        let mut budget = MatchBudget::with_output(&output);
        let predicates = PredicateSession::new(&self.predicates);
        let mut observer = NoObserver;
        let mut run = MatchRun {
            budget: &mut budget,
            predicates: &predicates,
            graph: None,
            observer: &mut observer,
        };
        let mut trace = if self.requires_assertion_session() {
            let mut mode = WithAssertions(AssertionSession::new(
                &self.shared.assertion_catalog,
                &predicates,
                tokens,
            ));
            self.run_trace_with_mode(tokens, &mut run, &mut mode)?
        } else {
            self.run_trace_with_mode(tokens, &mut run, &mut NoAssertions)?
        };
        trace
            .events
            .extend(trace.entries.iter().cloned().map(TraceEvent::Transition));
        for matched in &trace.matches {
            for capture in &matched.captures {
                trace.events.push(TraceEvent::CaptureOpen {
                    path: capture.path.clone(),
                    position: capture.start,
                });
                trace.events.push(TraceEvent::CaptureClose {
                    path: capture.path.clone(),
                    start: capture.start,
                    end: capture.end,
                });
            }
            if let Some(plan) = self
                .evaluated_plans
                .iter()
                .find(|plan| plan.0.definition().name == matched.pattern)
                && let Some((path, projection)) = plan.0.comparison()
                && let Some(capture) = matched.captures.iter().find(|capture| capture.path == path)
            {
                trace.events.push(TraceEvent::SpanComparison {
                    path: capture.path.clone(),
                    projection,
                    start: capture.start,
                    end: capture.end,
                    matched: true,
                });
            }
        }
        Ok(trace)
    }

    pub fn patterns(&self) -> &IndexMap<CompactString, Pattern> {
        &self.shared.patterns.0
    }

    /// The catalog as token windows, when its published records are exactly
    /// every `width`-token window of every sentence, once per definition, each
    /// keyed by attributes of the window's own tokens.
    ///
    /// `None` for any catalog that constrains a token, declares policy other
    /// than `key:`, or publishes a definition without a key.
    #[must_use]
    pub fn token_windows(&self) -> Option<Vec<TokenWindow>> {
        let policy = &self.shared.catalog_policy;
        if self.shared.lexicons.is_some()
            || self.sentence_filter.is_some()
            || !policy.hidden.is_empty()
            || !policy.program().ops.is_empty()
            || self.shared.patterns.0.is_empty()
        {
            return None;
        }
        let captures = self.shared.window_captures.as_ref()?;
        self.shared
            .patterns
            .0
            .keys()
            .map(|name| {
                let positions = captures.get(name)?;
                let declared = policy.get(name)?;
                let key_only = PatternPolicy {
                    key: declared.key.clone(),
                    ..PatternPolicy::default()
                };
                if declared.key.is_empty() || *declared != key_only {
                    return None;
                }
                let key = declared
                    .key
                    .iter()
                    .map(|field| {
                        let position = match &field.target {
                            KeyTarget::Match if positions.len() == 1 => 0,
                            KeyTarget::Capture { name, .. } => positions
                                .iter()
                                .position(|capture| capture.as_ref() == Some(name))?,
                            _ => return None,
                        };
                        Some(WindowKeyField {
                            name: field.name(),
                            position,
                            attribute: field.attribute.clone(),
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(TokenWindow {
                    name: name.clone(),
                    width: positions.len(),
                    key,
                })
            })
            .collect()
    }

    /// Inline lexical definitions in catalog order.
    #[must_use]
    pub fn lexicons(&self) -> &[CompiledLexiconDefinition] {
        self.shared
            .lexicons
            .as_ref()
            .map_or(&[], super::CompiledLexicons::descriptors)
    }

    fn finish_catalog_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        ordinary: Vec<Match>,
        output: &OutputBudget,
    ) -> Result<Vec<Match>, MatchResourceError> {
        let Some(lexicons) = &self.shared.lexicons else {
            return Ok(ordinary);
        };
        merge_catalog_matches(ordinary, lexicons.try_find_matches(output, tokens)?, output)
    }

    /// Attributes that can affect this compiled matcher's results.
    /// Cache-backed callers can use this to avoid rebuilding unrelated columns.
    pub fn required_attributes(&self) -> Vec<AttributeKey> {
        self.shared.required_attributes.to_vec()
    }

    /// Attributes that matching, compiled catalog policy, or the label-set count
    /// can read.
    #[must_use]
    pub fn execution_attributes(&self) -> Vec<AttributeKey> {
        self.shared
            .required_attributes
            .iter()
            .cloned()
            .chain(self.shared.catalog_policy.projected_attributes())
            .chain(
                self.shared
                    .label_declarations
                    .iter()
                    .map(|declaration| declaration.attribute().clone()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Returns dependency-edge attributes required by `where` clauses; missing keys are binding errors.
    #[must_use]
    pub fn relation_attributes(&self) -> Vec<AttributeKey> {
        let mut programs = self
            .capture_plans
            .values()
            .map(|plan| &plan.0)
            .chain(self.evaluated_plans.iter().map(|plan| &plan.0));
        let basic = programs
            .clone()
            .any(|program| program.has_relations() || program.has_basic_graph_relations());
        let enhanced = programs.clone().any(Program::has_enhanced_relations);
        let derived_basic_head = programs.any(Program::has_basic_head_endpoints);
        let mut attributes = Vec::new();
        if basic {
            attributes.extend([
                super::program::dependency_head_key(),
                super::program::dependency_relation_key(),
            ]);
        }
        if enhanced {
            attributes.push(super::program::enhanced_graph_key());
        }
        if derived_basic_head && !basic {
            attributes.push(super::program::dependency_head_key());
        }
        attributes
    }

    pub(crate) fn requires_enhanced_graph(&self) -> bool {
        self.capture_plans
            .values()
            .map(|plan| &plan.0)
            .chain(self.evaluated_plans.iter().map(|plan| &plan.0))
            .any(Program::has_enhanced_relations)
    }

    /// A structural digest of every distinct token predicate this catalog
    /// compiled, in interning order.
    ///
    /// For parity guards: it moves when the compiled predicates change and
    /// stays put otherwise, including for the changes that alter no match and
    /// no count. See [`PredicateDigest`].
    pub fn predicate_digest(&self) -> PredicateDigest {
        self.predicates.digest()
    }

    pub(crate) fn required_attribute_kinds(
        &self,
    ) -> Vec<(AttributeKey, crate::token_match::AttributeKind)> {
        self.predicates
            .required_kinds()
            .into_iter()
            .chain(self.relation_attributes().into_iter().map(|key| {
                let kind = if key == super::program::enhanced_graph_key() {
                    AttributeKind::StringSet
                } else {
                    AttributeKind::Scalar
                };
                (key, kind)
            }))
            .chain(
                self.evaluated_plans
                    .iter()
                    .flat_map(|plan| plan.0.compared_attributes())
                    .map(|key| (key, AttributeKind::Scalar)),
            )
            .chain(
                self.evaluated_plans
                    .iter()
                    .filter(|plan| {
                        plan.0.definition().measures.iter().any(|m| {
                            !matches!(
                                m.source,
                                crate::fsm::compiler::model::MeasureSource::Words(_)
                            )
                        })
                    })
                    .flat_map(|_| {
                        [
                            super::program::dependency_head_key(),
                            super::program::dependency_relation_key(),
                        ]
                    })
                    .map(|key| (key, AttributeKind::Scalar)),
            )
            .chain(self.shared.catalog_policy.required_attribute_kinds())
            .chain(self.lexicons().iter().flat_map(|lexicon| {
                lexicon
                    .by
                    .iter()
                    .cloned()
                    .map(|key| (key, crate::token_match::AttributeKind::Scalar))
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn bind_for_schema(
        &self,
        projection: &crate::token_match::BoundProjection,
        keep_sentence_filter: bool,
    ) -> Self {
        let mut compiled = Self {
            sentence_filter: keep_sentence_filter
                .then(|| self.sentence_filter.clone())
                .flatten(),
            dense: self.dense.clone(),
            predicates: self.predicates.clone(),
            capture_plans: self.capture_plans.clone(),
            evaluated_plans: self.evaluated_plans.clone(),
            shared: Arc::clone(&self.shared),
        };
        compiled.bind_attributes(projection);
        compiled
    }

    fn bind_attributes(&mut self, projection: &crate::token_match::BoundProjection) {
        self.predicates.bind_attributes(projection);
        for plan in self.capture_plans.values_mut() {
            plan.0.bind_attributes(projection);
        }
        for plan in &mut self.evaluated_plans {
            plan.0.bind_attributes(projection);
        }
        if let Some(dispatch) = &mut self.dense.spawn_table.dispatch {
            dispatch.bind_attributes(projection);
        }
    }

    /// Static finite-positive classification of this catalog's spawn groups.
    #[cfg(feature = "bench-instrumentation")]
    #[must_use]
    pub fn spawn_candidate_catalog_metrics(
        &self,
    ) -> super::spawn_candidate_metrics::SpawnCandidateCatalogMetrics {
        self.dense.spawn_table.spawn_candidate_catalog_metrics()
    }
}

/// Borrowed view of everything the derived-anchor pre-pass needs.
///
/// Obtained from [`CompiledCatalog::anchor_evaluator`]. It lives in this module
/// rather than beside [`AnchorPositions`] in `policy.rs` because three of its
/// four fields borrow `CompiledCatalog`'s private ones, which nothing outside this
/// file can read. The fourth, `digest`, has a public accessor and is no part of
/// that reason.
///
/// `Copy`, like the crate's other public borrowed view [`RecordView`]: every
/// field is. Not `Debug` — `PredicateCatalog` has no `Debug` impl.
///
/// [`RecordView`]: crate::fsm::RecordView
#[derive(Clone, Copy)]
pub struct AnchorEvaluator<'a> {
    programs: &'a BTreeMap<CompactString, AssertionProgramId>,
    assertions: &'a CompiledAssertionCatalog,
    predicates: &'a PredicateCatalog,
    digest: AnchorSetDigest,
}

impl AnchorEvaluator<'_> {
    /// Evaluates each declared anchor; an exclusion that matches no suffix places the anchor at the sentence end.
    pub fn positions<S: TokenSequence + ?Sized>(&self, tokens: &S) -> AnchorPositions {
        if self.programs.is_empty() {
            return AnchorPositions::new(self.digest, BTreeMap::new());
        }
        let predicates = PredicateSession::new(self.predicates);
        let mut session = AssertionSession::new(self.assertions, &predicates, tokens);
        let positions = self
            .programs
            .iter()
            .map(|(name, id)| {
                (
                    name.clone(),
                    session.suffix_start(*id).unwrap_or(tokens.len()),
                )
            })
            .collect();
        AnchorPositions::new(self.digest, positions)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "bench-instrumentation")]
    use crate::fsm::SpawnCandidateAccumulator;
    use crate::fsm::matcher::{PUNCT_ANCHOR, compile_for_test};
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{AttributeKey, TokenAttributeIndex, TokenInfo};

    #[test]
    fn dense_and_lexical_outputs_cannot_bypass_the_shared_quota() {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "a".into());
        for source in ["p := [].", "lexicon p by [text] := {\"a\"}."] {
            let matcher = compile_catalog(
                PatternSource::named("output.lx", source),
                CompileLimits::default(),
            )
            .unwrap()
            .matcher;
            let output = super::OutputBudget::new(0);
            assert!(matches!(
                matcher.try_find_matches_with_context_and_output(&[token.clone()], None, &output),
                Err(super::MatchResourceError::BatchOutput { limit: 0 })
            ));
            assert_eq!(matcher.try_find_matches(&[token.clone()]).unwrap().len(), 1);
        }
    }

    #[test]
    fn interpreted_records_observe_cancellation_and_charge_roles_before_publication() {
        use crate::execution::CancellationFlag;
        let body = "key: [$x.text]\np := (?<x> []).";
        let build = |source: &str| {
            compile_catalog(
                PatternSource::named("role.lx", source),
                CompileLimits::default(),
            )
            .unwrap()
            .matcher
        };
        let plain = build(body);
        let interpreted = build(&format!("interpret $x as part {{flag: true}}\n{body}"));
        assert!(plain.token_windows().is_some());
        assert!(interpreted.token_windows().is_none());
        let input = [TokenInfo::default()];
        let output = super::OutputBudget::new(1_000_000);
        plain
            .try_find_matches_with_context_and_output(&input, None, &output)
            .unwrap();
        let small = super::OutputBudget::new(output.allocated_bytes());
        assert!(matches!(
            interpreted.try_find_matches_with_context_and_output(&input, None, &small),
            Err(super::MatchResourceError::BatchOutput { .. })
        ));
        let cancel = CancellationFlag::new();
        let cancellable = super::OutputBudget::default().with_cancellation(Some(&cancel));
        cancel.cancel();
        assert_eq!(
            plain.try_find_matches_with_context_and_output(&input, None, &cancellable),
            Err(super::MatchResourceError::Cancelled)
        );
        assert_eq!(
            interpreted.try_find_matches_with_context_and_output(&input, None, &cancellable),
            Err(super::MatchResourceError::Cancelled)
        );
    }

    #[test]
    fn captured_payloads_consume_output_quota_beyond_dense_spans() {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "a".into());
        let plain = compile_catalog(
            PatternSource::named("plain.lx", "p := []."),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let output = super::OutputBudget::new(1_000_000);
        plain
            .try_find_matches_with_context_and_output(&[token.clone()], None, &output)
            .unwrap();
        let plain_bytes = output.allocated_bytes();
        let captured = compile_catalog(
            PatternSource::named("capture.lx", "p := (?<x> [])."),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let output = super::OutputBudget::new(plain_bytes);
        assert!(matches!(
            captured.try_find_matches_with_context_and_output(&[token.clone()], None, &output),
            Err(super::MatchResourceError::BatchOutput { .. })
        ));
        let expected = captured.try_find_matches(&[token]).unwrap();
        assert_eq!(expected[0].captures[0].name, "x");
    }

    /// Only a *pattern* that queries a lookaround needs a per-sentence session.
    ///
    /// Anchor-only assertions are unreachable from emitting patterns and must
    /// not force the matcher onto the per-sentence assertion-session path. The
    /// cases below independently exercise recognition-machine and execution-plan
    /// reachability.
    #[test]
    fn only_a_pattern_that_queries_a_lookaround_needs_an_assertion_session() {
        let lookaround_in_exclude = PUNCT_ANCHOR.replace(
            "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
            "fragment TRAILING_APPARATUS := preceded_by([lemma=引用]) [upos=PUNCT]+.",
        );
        let lookaround_in_pattern = PUNCT_ANCHOR.replace(
            "pattern toiu := [text=という].",
            "pattern toiu := [text=という] followed_by([upos=PUNCT]).",
        );
        // `nfa.rs`'s lowering skips every span-equality definition, so this
        // `followed_by` reaches the execution plan and the recognition machine
        // never sees it. It is the only shape the plans half of the predicate
        // answers alone.
        let lookaround_in_a_span_equality_pattern = PUNCT_ANCHOR.replace(
            "pattern toiu := [text=という].",
            "pattern toiu := (?<unit> [text=という] followed_by([upos=PUNCT])) same_as($unit).",
        );

        assert!(
            !compile_for_test("session-gate.lx", PUNCT_ANCHOR).requires_assertion_session(),
            "an anchor whose exclude uses no lookaround needs no session"
        );
        assert!(
            !compile_for_test("session-gate.lx", &lookaround_in_exclude)
                .requires_assertion_session(),
            "a lookaround reachable only from an anchor's exclude is not a \
             pattern's lookaround, and must not move matching onto the session \
             path"
        );
        assert!(
            compile_for_test("session-gate.lx", &lookaround_in_pattern)
                .requires_assertion_session(),
            "a pattern that queries a lookaround does need a session"
        );
        assert!(
            compile_for_test("session-gate.lx", &lookaround_in_a_span_equality_pattern)
                .requires_assertion_session(),
            "a lookaround that only the span-equality plan can see still needs a \
             session"
        );
    }

    /// An anchor's `exclude` and an identical lookaround body are one program.
    ///
    /// The assertion compiler keys its interning on `(direction, body)` rather
    /// than on the span the body was written at, so a catalog that both excludes
    /// `[upos=PUNCT]+` from an anchor and asserts `followed_by([upos=PUNCT]+)`
    /// compiles one automaton, not two — and evaluates it once per sentence
    /// rather than twice, since a session memoizes per program.
    ///
    /// Two assertions, because they pin different things. The bare count pins
    /// that the `exclude` is interned *at all*, which no comparison can show.
    /// The comparison against the same catalog without the lookaround pins the
    /// sharing, which no count can show.
    #[test]
    fn an_anchor_exclude_shares_a_program_with_an_identical_lookaround_body() {
        fn assertion_programs(source: &str) -> usize {
            compile_catalog(
                PatternSource::named("share.lx", source),
                CompileLimits::default(),
            )
            .unwrap_or_else(|error| panic!("fixture must compile: {:?}", error.diagnostics))
            .matcher
            .shared
            .assertion_catalog
            .program_count_for_test()
        }

        let anchor_only = PUNCT_ANCHOR;
        let with_lookaround = anchor_only.replace(
            "pattern toiu := [text=という].",
            "pattern toiu := [text=という] followed_by([upos=PUNCT]+).",
        );

        assert_eq!(
            assertion_programs(anchor_only),
            1,
            "the exclude fragment must be interned even with no lookaround anywhere"
        );
        assert_eq!(
            assertion_programs(&with_lookaround),
            assertion_programs(anchor_only),
            "adding a lookaround whose body means what the exclude means must not \
             add a second program"
        );
    }

    fn token(orth: &str, pos1: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token.set(TokenAttributeIndex::Pos1, pos1.into());
        token
    }

    #[cfg(feature = "bench-instrumentation")]
    fn spawn_token(orth: Option<&str>, lemma: Option<&str>) -> TokenInfo {
        let mut token = TokenInfo::default();
        if let Some(orth) = orth {
            token.set(TokenAttributeIndex::Orth, orth.into());
        }
        if let Some(lemma) = lemma {
            token.set(TokenAttributeIndex::Lemma, lemma.into());
        }
        token
    }

    /// The measured effect of one predicate shape, addressed by its signature.
    ///
    /// A signature spells each atom's **canonical key**, so the surface reads
    /// `text:`, not `orth:` — `TokenAttributeIndex::Orth` keeps the UniDic
    /// column's own name while the portable key it publishes became `text` in
    /// the 2026-08-24 rename. The atom order is unaffected: `lemma` sorts
    /// before both spellings.
    #[cfg(feature = "bench-instrumentation")]
    fn shape_effect(
        matcher: &super::CompiledCatalog,
        metrics: &SpawnCandidateAccumulator,
        signature: &str,
    ) -> (u64, u64, u64) {
        let shape_id = matcher
            .spawn_candidate_catalog_metrics()
            .shapes
            .into_iter()
            .find(|shape| shape.signature == signature)
            .unwrap_or_else(|| panic!("missing fixture shape {signature}"))
            .id;
        let effect = metrics
            .shape_effects
            .iter()
            .find(|effect| effect.shape_id == shape_id)
            .unwrap_or_else(|| panic!("missing fixture effect {signature}"));
        (
            effect.actual_calls,
            effect.scheme_a_avoided_calls,
            effect.scheme_b_avoided_calls,
        )
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn spawn_candidate_metrics_pin_discrimination_widths_and_shapes() {
        let matcher = compile_for_test(
            "spawn-candidates.lx",
            concat!(
                "one := [text=a].\n",
                "multi := [lemma=x & text in {a, b}].\n",
                "regex := [text~/a/].\n",
            ),
        );
        let tokens = vec![
            spawn_token(Some("a"), Some("x")),
            spawn_token(Some("c"), Some("x")),
            spawn_token(None, None),
        ];
        let expected = matcher.find_matches(&tokens);
        let mut metrics = SpawnCandidateAccumulator::default();
        let (actual, execution) =
            matcher.find_matches_with_spawn_candidate_metrics(&tokens, &mut metrics);
        assert_eq!(actual, expected);
        assert_eq!(metrics.spawn_positions, 3);
        assert_eq!(metrics.actual_spawn_calls, 9);
        assert_eq!(metrics.scheme_a_candidate_calls, 6);
        assert_eq!(metrics.scheme_b_candidate_calls, 5);
        assert_eq!(metrics.scheme_a_avoided_calls, 3);
        assert_eq!(metrics.scheme_b_avoided_calls, 4);
        assert_eq!(metrics.scheme_a_undiscriminated_calls, 3);
        assert_eq!(metrics.scheme_b_undiscriminated_calls, 3);
        assert_eq!(metrics.scheme_a_finite_atom_checks, 6);
        assert_eq!(metrics.scheme_b_finite_atom_checks, 8);
        assert_eq!(metrics.actual_spawn_calls, execution.spawn_predicate_calls);
        assert_eq!(
            shape_effect(&matcher, &metrics, "text:single:positive"),
            (3, 2, 2)
        );
        assert_eq!(
            shape_effect(
                &matcher,
                &metrics,
                "lemma:single:positive+text:one_of:positive"
            ),
            (3, 1, 2)
        );
        assert_eq!(
            shape_effect(&matcher, &metrics, "text:regex:positive"),
            (3, 0, 0)
        );

        metrics.reset();
        let (actual, execution) =
            matcher.find_matches_with_spawn_candidate_metrics(&tokens[..2], &mut metrics);
        assert_eq!(actual, matcher.find_matches(&tokens[..2]));
        assert_eq!(metrics.spawn_positions, 2);
        assert_eq!(metrics.actual_spawn_calls, 6);
        assert_eq!(metrics.scheme_a_candidate_calls, 5);
        assert_eq!(metrics.scheme_b_candidate_calls, 4);
        assert_eq!(metrics.scheme_a_avoided_calls, 1);
        assert_eq!(metrics.scheme_b_avoided_calls, 2);
        assert_eq!(metrics.scheme_a_width_sum, 5);
        assert_eq!(metrics.scheme_b_width_sum, 4);
        assert_eq!(metrics.scheme_a_width_histogram[2], 1);
        assert_eq!(metrics.scheme_a_width_histogram[3], 1);
        assert_eq!(metrics.scheme_b_width_histogram[1], 1);
        assert_eq!(metrics.scheme_b_width_histogram[3], 1);
        assert_eq!(metrics.scheme_a_width_histogram.iter().sum::<u64>(), 2);
        assert_eq!(metrics.scheme_b_width_histogram.iter().sum::<u64>(), 2);
        assert_eq!(metrics.scheme_a_undiscriminated_calls, 2);
        assert_eq!(metrics.scheme_b_undiscriminated_calls, 2);
        assert_eq!(metrics.scheme_a_finite_atom_checks, 4);
        assert_eq!(metrics.scheme_b_finite_atom_checks, 6);
        assert_eq!(metrics.scheme_a_max_width, 3);
        assert_eq!(metrics.scheme_b_max_width, 3);
        assert_eq!(metrics.actual_spawn_calls, execution.spawn_predicate_calls);
        assert_eq!(
            shape_effect(&matcher, &metrics, "text:single:positive"),
            (2, 1, 1)
        );
        assert_eq!(
            shape_effect(
                &matcher,
                &metrics,
                "lemma:single:positive+text:one_of:positive"
            ),
            (2, 0, 1)
        );
        assert_eq!(
            shape_effect(&matcher, &metrics, "text:regex:positive"),
            (2, 0, 0)
        );
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn spawn_candidate_metrics_exclude_condition_rejected_groups() {
        let matcher = compile_for_test(
            "spawn-readiness.lx",
            concat!(
                "conditional := preceded_by([text=p]) [lemma=a].\n",
                "plain := [text=z].",
            ),
        );
        let tokens = [spawn_token(Some("x"), None), spawn_token(Some("z"), None)];
        let mut metrics = SpawnCandidateAccumulator::default();
        let (actual, execution) =
            matcher.find_matches_with_spawn_candidate_metrics(&tokens, &mut metrics);
        assert_eq!(actual, matcher.find_matches(&tokens));
        assert_eq!(metrics.actual_spawn_calls, 2);
        assert_eq!(metrics.actual_spawn_calls, execution.spawn_predicate_calls);
        assert_eq!(
            shape_effect(&matcher, &metrics, "lemma:single:positive"),
            (0, 0, 0)
        );
        assert_eq!(
            shape_effect(&matcher, &metrics, "text:single:positive"),
            (2, 1, 1)
        );
    }

    #[cfg(feature = "bench-instrumentation")]
    fn metrics_for(source: &str, tokens: Vec<TokenInfo>) -> super::ExecutionMetrics {
        let matcher = compile_catalog(
            PatternSource::named("predicate-sites.lx", source),
            CompileLimits::default(),
        )
        .expect("predicate-site fixture compiles")
        .matcher;
        let expected = matcher.find_matches(&tokens);
        let (actual, metrics) = matcher.find_matches_with_execution_metrics(&tokens);
        assert_eq!(
            actual, expected,
            "instrumentation preserves complete matches"
        );
        assert_eq!(
            metrics.predicate_calls,
            metrics.call_site_total(),
            "named call fields copy the complete predicate total",
        );
        metrics
    }

    fn expected(pattern: &str, span: (usize, usize)) -> crate::fsm::types::Match {
        crate::fsm::types::Match {
            pattern: pattern.into(),
            start: span.0,
            end: span.1,
            captures: Box::default(),
            graph_bindings: Box::default(),
            interpretations: Box::default(),
            lexical: None,
        }
    }

    #[test]
    fn compiled_matcher_canonicalizes_the_reference_nfa_matches() {
        let matcher = compile_catalog(
            PatternSource::named("particle-run.lx", "particle_run := [unidic.pos1=助詞]+."),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let tokens = vec![
            token("私", "代名詞"),
            token("は", "助詞"),
            token("と", "助詞"),
            token("も", "助詞"),
            token("行く", "動詞"),
        ];

        // Golden characterization: pinned complete Match values (contexts
        // included), independent of the executor, so a defect in the dense
        // view cannot cancel out of both sides of the comparison.
        let golden = vec![
            expected("particle_run", (1, 4)),
            expected("particle_run", (2, 4)),
            expected("particle_run", (3, 4)),
        ];
        assert_eq!(matcher.find_matches(&tokens), golden);
    }

    #[test]
    fn compiled_matcher_matches_reference_for_empty_and_referenced_patterns() {
        let empty = compile_catalog(
            PatternSource::named("empty.lx", ""),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        assert_eq!(empty.find_matches(&Vec::<TokenInfo>::new()), Vec::new());

        let matcher = compile_catalog(
            PatternSource::named(
                "references.lx",
                "x := [text=x].\nxy := @x [text=y].\nb := [text=a].\na := [text=a].",
            ),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let tokens = vec![token("x", "X"), token("y", "X"), token("a", "X")];

        // Golden characterization (see above): both identically-spanned
        // patterns `a` and `b` are reported, in canonical order.
        let golden = vec![
            expected("x", (0, 1)),
            expected("xy", (0, 2)),
            expected("a", (2, 3)),
            expected("b", (2, 3)),
        ];
        assert_eq!(matcher.find_matches(&tokens), golden);
    }

    #[test]
    fn required_attributes_are_the_sorted_union_of_every_execution_context() {
        let matcher = compile_catalog(
            PatternSource::named(
                "required-attributes.lx",
                concat!(
                    "pattern transition := [text=x].\n",
                    "pattern previous := preceded_by([unidic.pos1=ADV]) [lemma=x].\n",
                    "pattern general := [text=x] followed_by([unidic.cform=X] [unidic.ctype=Y]).\n",
                    "pattern capture := (?<cap> [unidic.kana=x]).\n",
                    "pattern equality := (?<u> [upos=NOUN]) same_as($u, by=unidic.pron).",
                ),
            ),
            CompileLimits::default(),
        )
        .expect("all-context attribute fixture compiles")
        .matcher;

        assert_eq!(
            matcher.required_attributes(),
            [
                AttributeKey::new("lemma").unwrap(),
                AttributeKey::new("text").unwrap(),
                AttributeKey::new("unidic.cform").unwrap(),
                AttributeKey::new("unidic.ctype").unwrap(),
                AttributeKey::new("unidic.kana").unwrap(),
                AttributeKey::new("unidic.pos1").unwrap(),
                AttributeKey::new("unidic.pron").unwrap(),
                AttributeKey::new("upos").unwrap(),
            ]
        );
    }

    #[test]
    fn execution_attributes_are_the_sorted_matcher_policy_union() {
        let matcher = compile_catalog(
            PatternSource::named(
                "execution-attributes.lx",
                "key: [$item.norm]\npattern p := (?<item> [text=x]).",
            ),
            CompileLimits::default(),
        )
        .expect("fixture compiles")
        .matcher;

        assert_eq!(
            matcher.execution_attributes(),
            // Sorted by key: `norm` precedes `text`, where it followed `form`.
            [TokenAttributeIndex::Norm, TokenAttributeIndex::Orth]
        );
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn benchmark_entry_preserves_matches_and_reports_assertion_work() {
        let matcher = compile_catalog(
            PatternSource::named(
                "assertion-metrics.lx",
                "p := [text=a] followed_by(([text=b] | [text=c])).",
            ),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let tokens = vec![token("a", "X"), token("b", "X")];

        let expected = matcher.find_matches(&tokens);
        let (actual, metrics) = matcher.find_matches_with_assertion_metrics(&tokens);

        assert_eq!(actual, expected);
        assert!(metrics.row_queries > 0);
        assert_eq!(metrics.row_materializations, 3);
        assert!(metrics.state_visits > 0);
        assert_eq!(metrics.max_frontier_states, 2);
        assert!(metrics.state_visits >= metrics.max_frontier_states);
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn execution_metrics_preserve_matches_and_count_capture_work() {
        let matcher = compile_catalog(
            PatternSource::named(
                "execution-metrics.lx",
                concat!(
                    "pattern captured := (?<x> a).\n",
                    "pattern equal := (?<u> a) same_as($u).",
                ),
            ),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let tokens = vec![token("a", "X"), token("a", "X")];

        let expected = matcher.find_matches(&tokens);
        let (actual, metrics) = matcher.find_matches_with_execution_metrics(&tokens);

        assert_eq!(actual, expected);
        assert_eq!(metrics.active_capture_plans, 1);
        assert_eq!(metrics.capture_plan_invocations, 2);
        assert_eq!(metrics.extracted_captures, 2);
        assert_eq!(metrics.evaluated_plan_invocations, 1);
        assert!(metrics.predicate_calls > 0);
        assert_eq!(
            metrics.predicate_calls,
            metrics.predicate_memo_hits + metrics.predicate_evaluations
        );
        assert!(metrics.predicate_distinct_pairs <= metrics.predicate_evaluations);
        assert_eq!(
            metrics.predicate_recomputations,
            metrics.predicate_evaluations - metrics.predicate_distinct_pairs
        );
        assert!(metrics.dense_predicate_calls > 0);
        assert_eq!(
            metrics.dense_predicate_calls,
            metrics.dense_predicate_memo_hits + metrics.dense_predicate_evaluations
        );
        assert!(metrics.dense_predicate_calls < metrics.predicate_calls);
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn execution_metrics_isolate_capture_and_evaluated_sites() {
        let capture = metrics_for(
            "pattern captured := (?<x> [text=a]).",
            vec![token("a", "X")],
        );
        assert!(capture.capture_token_predicate_calls > 0);
        assert_eq!(capture.evaluated_token_predicate_calls, 0);
        assert_eq!(capture.evaluated_previous_token_predicate_calls, 0);

        let evaluated = metrics_for(
            "pattern equal := (?<u> [text=a]) same_as($u).",
            vec![token("a", "X"), token("a", "X")],
        );
        assert!(evaluated.evaluated_token_predicate_calls > 0);
        assert_eq!(evaluated.capture_token_predicate_calls, 0);
        assert_eq!(evaluated.capture_previous_token_predicate_calls, 0);
    }

    /// Candidate volume, the quantity the counted-work screen was blind to.
    ///
    /// The screen counts predicate calls, assertion work and publication, none
    /// of which sees how many candidates the tree evaluator carries between
    /// them. These two do see it.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn execution_metrics_count_tree_evaluator_candidate_volume() {
        // The dense machine carries no candidates, so a catalog it executes
        // whole leaves the pair at zero rather than reporting matcher work as
        // candidate work.
        let dense = metrics_for("pattern plain := [text=a].", vec![token("a", "X")]);
        assert_eq!(dense.candidates_produced, 0);
        assert_eq!(dense.candidates_retained, 0);

        let captured = metrics_for(
            "pattern captured := (?<x> [text=a]).",
            vec![token("a", "X")],
        );
        assert!(captured.candidates_produced > 0);
        assert_eq!(captured.candidates_retained, captured.candidates_produced);

        // Two branches reaching one position under one capture assignment:
        // the alternation node produces both and deduplication keeps one.
        // `retained < produced` is the divergence the pair exists to expose,
        // and it is what a change to the key moves.
        let collapsing = metrics_for(
            "pattern ambiguous := (?<x> [text=a] | [text=a]) [text=b].",
            vec![token("a", "X"), token("b", "X")],
        );
        assert!(
            collapsing.candidates_retained < collapsing.candidates_produced,
            "deduplication must be visible: produced {}, retained {}",
            collapsing.candidates_produced,
            collapsing.candidates_retained,
        );
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn execution_metrics_zero_absent_sites() {
        let metrics = metrics_for("pattern plain := [text=a].", vec![token("a", "X")]);

        assert_eq!(metrics.nfa_previous_token_predicate_calls, 0);
        assert_eq!(metrics.assertion_previous_token_predicate_calls, 0);
        assert_eq!(metrics.capture_previous_token_predicate_calls, 0);
        assert_eq!(metrics.evaluated_previous_token_predicate_calls, 0);
        assert_eq!(metrics.assertion_consume_predicate_calls, 0);
        assert_eq!(metrics.unclassified_predicate_calls, 0);
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn execution_metrics_dense_single_token_signature() {
        let metrics = metrics_for("pattern plain := [text=a].", vec![token("a", "X")]);

        assert_eq!(metrics.dense_main_transition_predicate_calls, 1);
        assert_eq!(metrics.dense_predicate_calls, 1);
        assert_eq!(metrics.spawn_predicate_calls, 1);
    }

    #[test]
    fn schema_binding_shares_static_catalog_data_and_with_overlaps_copies_on_write() {
        use crate::fsm::policy_program::Overlaps;
        use crate::token_match::TokenSchema;

        let compiled = compile_for_test(
            "shared-static.lx",
            concat!(
                "pattern a := [text=a].\n",
                "pattern ab := [text=a] [text=b].\n",
                "policy\n",
                "  resolve all rank: [longest, start, end, pattern].\n",
                "end\n",
            ),
        );
        let bound = compiled
            .bind(&TokenSchema::unidic_morphology())
            .expect("schema binds");
        assert!(
            std::sync::Arc::ptr_eq(&compiled.shared, &bound.compiled.shared),
            "schema binding must share immutable catalog data without deep-cloning"
        );

        let relaxed = compiled.clone().with_overlaps(Overlaps::Keep);
        assert!(
            !std::sync::Arc::ptr_eq(&relaxed.shared, &bound.compiled.shared),
            "with_overlaps must copy-on-write when shared with a bound catalog"
        );
        assert_ne!(relaxed.catalog_policy(), bound.catalog_policy());
    }

    #[test]
    fn trace_matches_and_events_agree_with_find_matches_with_and_without_assertions() {
        use crate::fsm::types::TraceEvent;

        let tokens = vec![token("a", "X"), token("a", "X"), token("b", "X")];
        for (source, expect_assertions) in [
            (
                concat!(
                    "pattern plain := [text=a].\n",
                    "pattern captured := (?<x> [text=a]) [text=b].\n",
                    "pattern equal := (?<u> [text=a]) same_as($u).\n",
                    "lexicon lex by [text] := {\"b\"}.\n",
                ),
                false,
            ),
            (
                concat!(
                    "pattern plain := [text=a] followed_by([text=a]).\n",
                    "pattern captured := (?<x> [text=a]) followed_by([text=b]).\n",
                    "pattern equal := (?<u> [text=a]) same_as($u) followed_by([text=b]).\n",
                    "lexicon lex by [text] := {\"b\"}.\n",
                ),
                true,
            ),
        ] {
            let matcher = compile_for_test("trace-parity.lx", source);
            assert_eq!(matcher.requires_assertion_session(), expect_assertions);

            let expected = matcher.find_matches(&tokens);
            let trace = matcher.find_matches_with_trace(&tokens);
            assert_eq!(trace.matches, expected);
            assert!(!trace.entries.is_empty());
            assert!(
                trace
                    .events
                    .iter()
                    .any(|event| matches!(event, TraceEvent::Transition(_)))
            );
            assert!(
                trace
                    .events
                    .iter()
                    .any(|event| matches!(event, TraceEvent::CaptureOpen { .. }))
            );
            assert!(
                trace
                    .events
                    .iter()
                    .any(|event| matches!(event, TraceEvent::CaptureClose { .. }))
            );
            assert!(
                trace
                    .events
                    .iter()
                    .any(|event| matches!(event, TraceEvent::SpanComparison { matched: true, .. }))
            );
        }
    }
}
