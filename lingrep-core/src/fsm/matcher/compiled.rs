#[cfg(feature = "bench-instrumentation")]
use super::assertion::AssertionMetrics;
use super::assertion::{
    AssertionCatalog, AssertionProgramId, AssertionSession, CompiledAssertionCatalog,
};
use super::canonical_matches;
use super::capture::CapturePlan;
use super::evaluated::EvaluatedPlan;
use super::nfa_matcher::{DenseMachine, Matcher};
#[cfg(feature = "bench-instrumentation")]
use super::predicate::PredicateMetrics;
use super::predicate::{PredicateCatalog, PredicateDigest, PredicateSession};
use super::program::Program;
#[cfg(feature = "bench-instrumentation")]
use super::spawn_candidate_metrics::SpawnCandidateAccumulator;
use crate::corpus::conllu::{EnhancedGraph, EnhancedGraphObservation};
use crate::fsm::ProgramStats;
use crate::fsm::compiler::model::{ExecutionCapability, ExecutionDefinition};
use crate::fsm::explanation::{ExplainedMatch, SentenceExplanation};
use crate::fsm::policy::{
    AnchorPositions, AnchorSetDigest, NestingClaim, PolicyStats, apply_policy_records, is_hidden,
};
use crate::fsm::state_machine::StateMachineData;
use crate::fsm::types::{Match, Pattern, Patterns, Trace, TraceEvent};
use crate::token_match::{AttributeKey, AttributeKind, TokenSequence};
use compact_str::CompactString;
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
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
    pub dense_match_closure_predicate_calls: u64,
    pub dense_advance_validation_predicate_calls: u64,
    pub dense_retry_advance_validation_predicate_calls: u64,
    pub dense_retry_extend_validation_predicate_calls: u64,
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
        self.dense_match_closure_predicate_calls = metrics.dense_match_closure_predicate_calls;
        self.dense_advance_validation_predicate_calls =
            metrics.dense_advance_validation_predicate_calls;
        self.dense_retry_advance_validation_predicate_calls =
            metrics.dense_retry_advance_validation_predicate_calls;
        self.dense_retry_extend_validation_predicate_calls =
            metrics.dense_retry_extend_validation_predicate_calls;
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
            self.dense_match_closure_predicate_calls,
            self.dense_advance_validation_predicate_calls,
            self.dense_retry_advance_validation_predicate_calls,
            self.dense_retry_extend_validation_predicate_calls,
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

/// Immutable compiled lingrep patterns artifact produced by [`super::FsmBuilder`].
#[derive(Clone)]
pub struct CompiledCatalog {
    pub(crate) sentence_filter: Option<crate::fsm::SentenceFilter>,
    patterns: Patterns,
    lexicons: Option<super::CompiledLexicons>,
    dense: DenseMachine,
    predicates: PredicateCatalog,
    capture_plans: IndexMap<CompactString, CapturePlan>,
    evaluated_plans: Box<[EvaluatedPlan]>,
    assertion_catalog: CompiledAssertionCatalog,
    /// Whether **any** materialized execution definition consults a general
    /// lookaround.
    ///
    /// Every definition, not only the ones that become a capture or
    /// span-equality plan. Narrowing it to those would make the answer correct
    /// only because the recognition machine happens to cover the rest — exactly
    /// the cross-half argument [`CompiledCatalog::requires_assertion_session`]
    /// exists to avoid — so the scan is deliberately wider than the plans it is
    /// named for.
    ///
    /// Only half the answer; see that method for the other half.
    execution_queries_lookaround: bool,
    anchor_programs: BTreeMap<CompactString, AssertionProgramId>,
    required_attributes: Box<[AttributeKey]>,
    catalog_policy: crate::fsm::policy::CatalogPolicy,
    declared_dictionaries: Box<[CompactString]>,
    analyzer_constraints: Box<[CatalogAnalyzerConstraint]>,
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

fn merge_catalog_matches(ordinary: Vec<Match>, lexical: Vec<Match>) -> Vec<Match> {
    if lexical.is_empty() {
        return ordinary;
    }
    if ordinary.is_empty() {
        return lexical;
    }
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
    output
}

impl CompiledCatalog {
    pub(crate) fn new(patterns: Patterns, state_machine: StateMachineData) -> Self {
        let mut predicates = PredicateCatalog::default();
        let dense = DenseMachine::build(
            &state_machine.state_transitions,
            &state_machine.final_states,
            &state_machine.state_patterns,
            &mut predicates,
        );
        Self {
            patterns,
            sentence_filter: None,
            lexicons: None,
            dense,
            predicates,
            capture_plans: IndexMap::new(),
            evaluated_plans: Box::default(),
            assertion_catalog: CompiledAssertionCatalog::empty(),
            execution_queries_lookaround: false,
            anchor_programs: BTreeMap::new(),
            required_attributes: Box::default(),
            catalog_policy: crate::fsm::policy::CatalogPolicy::default(),
            declared_dictionaries: Box::default(),
            analyzer_constraints: Box::default(),
        }
    }

    pub(crate) fn install_assertions(&mut self, catalog: AssertionCatalog) {
        self.assertion_catalog = catalog.compile(&mut self.predicates);
    }

    /// Records which assertion program recognizes each declared anchor.
    ///
    /// A pure name-to-id record. The ids index the catalog
    /// [`Self::install_assertions`] installs, and that catalog is also what
    /// carries an `exclude` fragment's attributes into the projection — so this
    /// map is not on the path to [`Self::finalize_required_attributes`] and
    /// imposes no ordering against it. Installing it after the projection is
    /// frozen would change nothing; it sits beside `install_assertions` because
    /// that is what its values mean, not because it must.
    pub(crate) fn install_anchor_programs(
        &mut self,
        programs: BTreeMap<CompactString, AssertionProgramId>,
    ) {
        self.anchor_programs = programs;
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
        &self.anchor_programs
    }

    /// The compiled assertion catalog every [`AssertionProgramId`] indexes.
    #[cfg(test)]
    pub(crate) fn assertion_catalog_for_test(&self) -> &CompiledAssertionCatalog {
        &self.assertion_catalog
    }

    pub(crate) fn install_catalog_policy(&mut self, policy: crate::fsm::policy::CatalogPolicy) {
        self.catalog_policy = policy;
    }

    pub(crate) fn install_declared_dictionaries(&mut self, dictionaries: Vec<CompactString>) {
        self.declared_dictionaries = dictionaries.into_boxed_slice();
    }

    pub(crate) fn install_analyzer_constraints(
        &mut self,
        constraints: Vec<CatalogAnalyzerConstraint>,
    ) {
        self.analyzer_constraints = constraints.into_boxed_slice();
    }

    pub fn analyzer_constraints(&self) -> &[CatalogAnalyzerConstraint] {
        &self.analyzer_constraints
    }

    pub fn analyzer_compatibility(
        &self,
        family: &str,
        resource: Option<&str>,
        split: Option<&str>,
        segmentation: &str,
    ) -> AnalyzerCompatibility {
        if self.analyzer_constraints.is_empty() {
            AnalyzerCompatibility::Unspecified
        } else if self
            .analyzer_constraints
            .iter()
            .any(|constraint| constraint.matches(family, resource, split, segmentation))
        {
            AnalyzerCompatibility::Compatible
        } else {
            AnalyzerCompatibility::Mismatch
        }
    }

    /// The dictionaries this catalog states its tagset is valid for.
    ///
    /// A **set**, because compatibility is not one-to-one: `unidic-cwj` and
    /// `unidic-csj` differ only in weights and share a vocabulary and tagset,
    /// so a catalog is genuinely valid for both, while `unidic-kinsei-edo`
    /// differs in vocabulary and in closed-class morphology and is a different
    /// claim. The set is authored rather than derived from a compatibility
    /// table here: which dictionaries a catalog's predicates survive is a
    /// linguistic judgement belonging to the researcher, and a table in this
    /// crate would be an unmaintainable claim about dictionaries it does not
    /// ship.
    ///
    /// Empty for a catalog that declares none, including inline `--pattern`
    /// input, which has no file-level declaration.
    /// Absence is not reported: a notice that always fires is a notice nobody
    /// reads.
    ///
    /// This is a claim by the catalog about itself, never a constraint on
    /// matching. Comparing it against the dictionary a run actually loaded is
    /// the caller's job, and the mismatch is a notice — divergence is legal,
    /// because running a catalog against a second dictionary is a deliberate
    /// experiment as often as it is a mistake.
    pub fn declared_dictionaries(&self) -> &[CompactString] {
        &self.declared_dictionaries
    }

    /// Removes synthesized policy emitters from the *public* catalog.
    ///
    /// They must keep matching — the engine needs their records as evidence for
    /// a fragment operand — so only [`Self::patterns`] is filtered, and
    /// the state machine is untouched. Everything matching depends on is
    /// derived before this runs: `dense` carries its own state-to-name map
    /// (`exec.rs` emits from it, never from this map), and the attribute
    /// projection is already frozen. Callers of `patterns()` — check
    /// descriptors, projections, example verification — therefore never see a
    /// definition no researcher wrote.
    pub(crate) fn hide_synthesized_patterns(&mut self) {
        self.patterns
            .0
            .retain(|name, _| !crate::fsm::policy::is_hidden(name));
    }

    /// The result policy this catalog declares.
    ///
    /// Empty for a catalog that declares none, which is what
    /// [`crate::fsm::policy::CatalogPolicy::declared`] reports. Policy is not
    /// consulted during matching: it is applied at the publication seam, over
    /// the canonical matches this matcher produces.
    pub fn catalog_policy(&self) -> &crate::fsm::policy::CatalogPolicy {
        &self.catalog_policy
    }

    /// Structural identity of the anchor set this catalog declares.
    ///
    /// What a reader of an [`AnchorPositions`] must present to
    /// [`AnchorPositions::get`]. [`AnchorSetDigest::default`] for a catalog
    /// declaring no anchor.
    pub fn anchor_digest(&self) -> &AnchorSetDigest {
        &self.catalog_policy.anchor_digest
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
        &self.catalog_policy.nesting_claims
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
            programs: &self.anchor_programs,
            assertions: &self.assertion_catalog,
            predicates: &self.predicates,
            digest: self.catalog_policy.anchor_digest,
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

    /// The matches this catalog *publishes* for one sentence.
    ///
    /// [`Self::find_matches`] returns the canonical stream, which for a
    /// policy-declaring catalog contains synthesized `__policy__` evidence
    /// records that no researcher wrote and that no published stream may
    /// contain. Every caller that displays or exports results therefore has to
    /// apply policy, and the ones running through
    /// [`crate::execution::execute`] get that for free from the seam it owns.
    ///
    /// Direct sentence consumers such as previews and embedders use this method
    /// so publication semantics remain centralized.
    ///
    /// Statistics are discarded. Use [`crate::execution::execute`] when the run
    /// reports what policy suppressed.
    ///
    /// # Errors
    ///
    /// `AnchorResolutionError` when the anchor an `admit` measures against could
    /// not be resolved. This pairs a catalog's policy with *its own* positions,
    /// and the compiler refuses an `admit` naming an undeclared anchor, so valid
    /// compiled catalogs cannot produce this error. The signature retains it
    /// because the type system does not encode that invariant.
    ///
    /// [`AnchorResolutionError`]: crate::fsm::policy::AnchorResolutionError
    pub fn find_published_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<Vec<Match>, crate::fsm::policy::AnchorResolutionError> {
        let canonical = self.find_matches(tokens);
        if self.catalog_policy.declared() {
            let anchors = self.anchor_positions(tokens);
            let mut stats = crate::fsm::policy::PolicyStats::default();
            crate::fsm::policy::apply_policy(
                &self.catalog_policy,
                &canonical,
                tokens,
                &anchors,
                &mut stats,
            )
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
    pub fn explain_sentence<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> Result<SentenceExplanation, crate::fsm::policy::AnchorResolutionError> {
        let canonical = self.find_matches(tokens);
        let policy_declared = self.catalog_policy.declared();
        let mut published_by_raw = std::collections::HashMap::<Match, Vec<Match>>::new();

        if policy_declared {
            let anchors = self.anchor_positions(tokens);
            let mut stats = PolicyStats::default();
            let mut program = ProgramStats::for_program(self.catalog_policy.program());
            for record in apply_policy_records(
                &self.catalog_policy,
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

    pub(crate) fn spawn_decline(&self) -> Option<&super::nfa_matcher::SpawnDecline> {
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
    /// Each executor answers for itself, so no ordering between
    /// [`Self::install_execution`] and [`Self::new`] is load-bearing and neither
    /// half has to be argued to cover the other. That matters: the execution
    /// oracle in `compiler/oracle.rs` builds a matcher from a lowered NFA with
    /// no execution at all.
    ///
    /// [`crate::fsm::compiler`] reads this too, for the `spawn_decline` gate, so
    /// that the compile-time limit and the per-sentence path answer one question
    /// rather than two that can drift apart.
    pub(crate) fn requires_assertion_session(&self) -> bool {
        self.dense.queries_lookaround() || self.execution_queries_lookaround
    }

    pub(crate) fn install_execution(&mut self, definitions: Vec<ExecutionDefinition>) {
        self.sentence_filter = crate::fsm::compiler::derive_sentence_filter(&definitions);
        self.execution_queries_lookaround = definitions
            .iter()
            .any(|definition| definition.expr.queries_lookaround());
        let mut captures = IndexMap::new();
        let mut evaluated = Vec::new();
        for definition in definitions {
            match definition.capability {
                ExecutionCapability::Plain => {}
                ExecutionCapability::Captures => {
                    captures.insert(
                        definition.name.clone(),
                        CapturePlan(Program::new(definition, &mut self.predicates)),
                    );
                }
                ExecutionCapability::SpanEquality
                | ExecutionCapability::Intersection
                | ExecutionCapability::AnyOrder
                | ExecutionCapability::Relations => {
                    evaluated.push(EvaluatedPlan(Program::new(
                        definition,
                        &mut self.predicates,
                    )));
                }
            }
        }
        self.capture_plans = captures;
        self.evaluated_plans = evaluated.into_boxed_slice();
    }

    /// Freezes the attribute projection after every execution context is installed.
    ///
    /// This deliberately runs after both assertion and execution installation: either
    /// can introduce attributes that do not occur on the recognition NFA.
    pub(crate) fn finalize_required_attributes(&mut self) {
        self.required_attributes = self
            .dense
            .required_attributes()
            .chain(
                self.capture_plans
                    .values()
                    .flat_map(|plan| plan.0.required_attributes()),
            )
            .chain(
                self.evaluated_plans
                    .iter()
                    .flat_map(|plan| plan.0.required_attributes()),
            )
            .chain(self.assertion_catalog.required_attributes())
            .chain(
                self.lexicons()
                    .iter()
                    .flat_map(|lexicon| lexicon.by.iter().cloned()),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }

    #[cfg(test)]
    pub(crate) fn dense_for_test(&self) -> &DenseMachine {
        &self.dense
    }

    #[cfg(test)]
    pub(crate) fn predicates_for_test(&self) -> &PredicateCatalog {
        &self.predicates
    }

    pub fn find_matches<S: TokenSequence + ?Sized>(&self, tokens: &S) -> Vec<Match> {
        let predicates = PredicateSession::new(&self.predicates);
        if !self.requires_assertion_session() {
            let mut matches = Matcher::find_matches_dense(&self.dense, &predicates, tokens);
            for matched in &mut matches {
                if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                    matched.captures = plan
                        .extract_without_assertions(&predicates, tokens, matched.start, matched.end)
                        .expect("NFA-accepted capture span must have a prioritized extraction");
                }
            }
            for plan in &self.evaluated_plans {
                matches.extend(plan.find_matches_without_assertions(&predicates, tokens));
            }
            return self.finish_catalog_matches(tokens, canonical_matches(matches));
        }

        let mut session = AssertionSession::new(&self.assertion_catalog, &predicates, tokens);
        let mut matches = Matcher::find_matches_dense_with_assertions(
            &self.dense,
            &predicates,
            tokens,
            &mut session,
        );
        for matched in &mut matches {
            if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                matched.captures = plan
                    .extract(
                        &predicates,
                        tokens,
                        matched.start,
                        matched.end,
                        &mut session,
                    )
                    .expect("NFA-accepted capture span must have a prioritized extraction");
            }
        }
        for plan in &self.evaluated_plans {
            matches.extend(plan.find_matches(&predicates, tokens, &mut session));
        }
        self.finish_catalog_matches(tokens, canonical_matches(matches))
    }

    /// Matches with an explicit enhanced-graph observation.
    ///
    /// `Unavailable` and `Incomplete` cannot satisfy an enhanced constraint,
    /// but ordinary definitions in a mixed catalog continue to match. The
    /// plain [`Self::find_matches`] method has those same unavailable-graph
    /// semantics; bound execution additionally refuses when its source forgot
    /// to request a graph required by the catalog.
    pub fn find_matches_with_enhanced_graph<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        observation: &EnhancedGraphObservation,
    ) -> Result<Vec<Match>, crate::fsm::EnhancedGraphExecutionError> {
        let Some(graph) = observation.complete() else {
            return Ok(self.find_matches(tokens));
        };
        if graph.word_count() != tokens.len() {
            return Err(crate::fsm::EnhancedGraphExecutionError {
                expected_words: tokens.len(),
                actual_words: graph.word_count(),
            });
        }
        Ok(self.find_matches_with_complete_graph(tokens, graph))
    }

    pub(crate) fn find_matches_with_complete_graph<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        graph: &EnhancedGraph,
    ) -> Vec<Match> {
        let predicates = PredicateSession::new(&self.predicates);
        if !self.requires_assertion_session() {
            let mut matches = Matcher::find_matches_dense(&self.dense, &predicates, tokens);
            for matched in &mut matches {
                if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                    matched.captures = plan
                        .extract_without_assertions(&predicates, tokens, matched.start, matched.end)
                        .expect("NFA-accepted capture span must have a prioritized extraction");
                }
            }
            for plan in &self.evaluated_plans {
                matches.extend(plan.find_matches_without_assertions_with_graph(
                    &predicates,
                    tokens,
                    graph,
                ));
            }
            return self.finish_catalog_matches(tokens, canonical_matches(matches));
        }

        let mut session = AssertionSession::new(&self.assertion_catalog, &predicates, tokens);
        let mut matches = Matcher::find_matches_dense_with_assertions(
            &self.dense,
            &predicates,
            tokens,
            &mut session,
        );
        for matched in &mut matches {
            if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                matched.captures = plan
                    .extract(
                        &predicates,
                        tokens,
                        matched.start,
                        matched.end,
                        &mut session,
                    )
                    .expect("NFA-accepted capture span must have a prioritized extraction");
            }
        }
        for plan in &self.evaluated_plans {
            matches.extend(plan.find_matches_with_graph(&predicates, tokens, &mut session, graph));
        }
        self.finish_catalog_matches(tokens, canonical_matches(matches))
    }

    /// Runs normal matching and reports assertion-evaluator work.
    ///
    /// This measurement-only entry point is compiled solely with the
    /// `bench-instrumentation` feature. Default builds contain neither this
    /// method nor the counters it reads.
    #[cfg(feature = "bench-instrumentation")]
    pub fn find_matches_with_assertion_metrics<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
    ) -> (Vec<Match>, AssertionMetrics) {
        if !self.requires_assertion_session() {
            return (self.find_matches(tokens), AssertionMetrics::default());
        }

        let predicates = PredicateSession::new(&self.predicates);
        let mut session = AssertionSession::new(&self.assertion_catalog, &predicates, tokens);
        let mut matches = Matcher::find_matches_dense_with_assertions(
            &self.dense,
            &predicates,
            tokens,
            &mut session,
        );
        for matched in &mut matches {
            if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                matched.captures = plan
                    .extract(
                        &predicates,
                        tokens,
                        matched.start,
                        matched.end,
                        &mut session,
                    )
                    .expect("NFA-accepted capture span must have a prioritized extraction");
            }
        }
        for plan in &self.evaluated_plans {
            matches.extend(plan.find_matches(&predicates, tokens, &mut session));
        }
        let metrics = session.metrics();
        (
            self.finish_catalog_matches(tokens, canonical_matches(matches)),
            metrics,
        )
    }

    /// Runs normal matching and reports coarse execution-phase work.
    ///
    /// This follows the same production plans and ordering as
    /// [`Self::find_matches`]. The timers and counters are compiled only for
    /// benchmark-instrumentation builds.
    #[cfg(feature = "bench-instrumentation")]
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
        let predicates = PredicateSession::new(&self.predicates);
        if spawn.is_some() {
            let (group_count, shape_count) = self.dense.spawn_table.spawn_candidate_dimensions();
            predicates.begin_spawn_candidate_measurement(group_count, shape_count);
        }
        let mut metrics = ExecutionMetrics {
            active_capture_plans: self.capture_plans.len(),
            ..ExecutionMetrics::default()
        };

        if !self.requires_assertion_session() {
            let nfa_started = Instant::now();
            let mut matches = Matcher::find_matches_dense(&self.dense, &predicates, tokens);
            metrics.nfa_duration = nfa_started.elapsed();
            metrics.nfa_matches = matches.len();

            let capture_started = Instant::now();
            for matched in &mut matches {
                if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                    metrics.capture_plan_invocations += 1;
                    let captures = plan
                        .extract_without_assertions(&predicates, tokens, matched.start, matched.end)
                        .expect("NFA-accepted capture span must have a prioritized extraction");
                    metrics.extracted_captures += captures.len();
                    metrics.extracted_capture_tokens += captures
                        .iter()
                        .map(|capture| capture.end.saturating_sub(capture.start))
                        .sum::<usize>();
                    matched.captures = captures;
                }
            }
            metrics.capture_duration = capture_started.elapsed();

            let evaluated_started = Instant::now();
            for plan in &self.evaluated_plans {
                metrics.evaluated_plan_invocations += 1;
                let found = plan.find_matches_without_assertions(&predicates, tokens);
                metrics.evaluated_matches += found.len();
                matches.extend(found);
            }
            metrics.evaluated_duration = evaluated_started.elapsed();
            metrics.record_predicate_metrics(predicates.metrics());
            if let Some(accumulator) = spawn {
                predicates.merge_spawn_candidate_metrics(accumulator);
            }
            return (
                self.finish_catalog_matches(tokens, canonical_matches(matches)),
                metrics,
            );
        }

        let mut session = AssertionSession::new(&self.assertion_catalog, &predicates, tokens);
        let nfa_started = Instant::now();
        let mut matches = Matcher::find_matches_dense_with_assertions(
            &self.dense,
            &predicates,
            tokens,
            &mut session,
        );
        metrics.nfa_duration = nfa_started.elapsed();
        metrics.nfa_matches = matches.len();

        let capture_started = Instant::now();
        for matched in &mut matches {
            if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                metrics.capture_plan_invocations += 1;
                let captures = plan
                    .extract(
                        &predicates,
                        tokens,
                        matched.start,
                        matched.end,
                        &mut session,
                    )
                    .expect("NFA-accepted capture span must have a prioritized extraction");
                metrics.extracted_captures += captures.len();
                metrics.extracted_capture_tokens += captures
                    .iter()
                    .map(|capture| capture.end.saturating_sub(capture.start))
                    .sum::<usize>();
                matched.captures = captures;
            }
        }
        metrics.capture_duration = capture_started.elapsed();

        let evaluated_started = Instant::now();
        for plan in &self.evaluated_plans {
            metrics.evaluated_plan_invocations += 1;
            let found = plan.find_matches(&predicates, tokens, &mut session);
            metrics.evaluated_matches += found.len();
            matches.extend(found);
        }
        metrics.evaluated_duration = evaluated_started.elapsed();
        metrics.record_predicate_metrics(predicates.metrics());
        if let Some(accumulator) = spawn {
            predicates.merge_spawn_candidate_metrics(accumulator);
        }
        (
            self.finish_catalog_matches(tokens, canonical_matches(matches)),
            metrics,
        )
    }

    pub fn find_matches_with_trace<S: TokenSequence + ?Sized>(&self, tokens: &S) -> Trace {
        let predicates = PredicateSession::new(&self.predicates);
        let mut trace = if self.requires_assertion_session() {
            let mut session = AssertionSession::new(&self.assertion_catalog, &predicates, tokens);
            let mut trace = Matcher::find_matches_with_trace_dense_with_assertions(
                &self.dense,
                &predicates,
                tokens,
                &mut session,
            );
            for matched in &mut trace.matches {
                if let Some(plan) = self.capture_plans.get(&matched.pattern) {
                    matched.captures = plan
                        .extract(
                            &predicates,
                            tokens,
                            matched.start,
                            matched.end,
                            &mut session,
                        )
                        .expect("NFA-accepted capture span must have a prioritized extraction");
                }
            }
            for plan in &self.evaluated_plans {
                trace
                    .matches
                    .extend(plan.find_matches(&predicates, tokens, &mut session));
            }
            trace.matches = self.finish_catalog_matches(tokens, canonical_matches(trace.matches));
            trace
        } else {
            let mut trace =
                Matcher::find_matches_with_trace_dense(&self.dense, &predicates, tokens);
            // The session-free trace walk does not canonicalize or run the
            // capture and span-equality plans, so the matches come from the
            // shared entry point instead. Folded into this branch rather than
            // repeated as `if !requires_assertion_session()` below: the two are
            // one decision, and reading them as one is what stops a later change
            // moving only one of them.
            trace.matches = self.find_matches(tokens);
            trace
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
        trace
    }

    pub fn patterns(&self) -> &IndexMap<CompactString, Pattern> {
        &self.patterns.0
    }

    /// Inline lexical definitions in catalog order.
    #[must_use]
    pub fn lexicons(&self) -> &[CompiledLexiconDefinition] {
        self.lexicons
            .as_ref()
            .map_or(&[], super::CompiledLexicons::descriptors)
    }

    fn finish_catalog_matches<S: TokenSequence + ?Sized>(
        &self,
        tokens: &S,
        ordinary: Vec<Match>,
    ) -> Vec<Match> {
        let Some(lexicons) = &self.lexicons else {
            return ordinary;
        };
        merge_catalog_matches(ordinary, lexicons.find_matches(tokens))
    }

    pub(crate) fn install_lexicons(&mut self, lexicons: Option<super::CompiledLexicons>) {
        self.lexicons = lexicons;
    }

    /// Attributes that can affect this compiled matcher's results.
    /// Cache-backed callers can use this to avoid rebuilding unrelated columns.
    pub fn required_attributes(&self) -> Vec<AttributeKey> {
        self.required_attributes.to_vec()
    }

    /// Attributes that matching or compiled catalog policy can read.
    #[must_use]
    pub fn execution_attributes(&self) -> Vec<AttributeKey> {
        self.required_attributes
            .iter()
            .cloned()
            .chain(self.catalog_policy.projected_attributes())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The dependency edge attributes this catalog's `where` clauses require.
    ///
    /// Empty when no definition declares a clause. Unlike an ordinarily
    /// referenced attribute — which binds as universally absent when a schema
    /// lacks it — these are a bind-time refusal, because a dependency
    /// constraint without its edge attributes is meaningless rather than
    /// unsatisfied.
    #[must_use]
    pub fn relation_attributes(&self) -> Vec<AttributeKey> {
        let mut programs = self
            .capture_plans
            .values()
            .map(|plan| &plan.0)
            .chain(self.evaluated_plans.iter().map(|plan| &plan.0));
        let basic = programs.clone().any(Program::has_relations);
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
            .chain(self.catalog_policy.required_attribute_kinds())
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

    pub(crate) fn bind_attributes(&mut self, projection: &crate::token_match::BoundProjection) {
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
    /// Every declared anchor's position for one sentence.
    ///
    /// One pre-pass over the raw tokens, before any policy runs. An anchor whose
    /// `exclude` matches no suffix falls back to the raw sentence end, which is
    /// an `admit`'s shipped meaning — an unpunctuated sentence's propositional
    /// end is its end. That is a computed answer and not the fallback
    /// [`AnchorPositions`] refuses: a *missing* anchor stays an error there.
    ///
    /// The session is built unconditionally, and deliberately without consulting
    /// `CompiledCatalog::requires_assertion_session`. That predicate answers a
    /// different question — whether any *pattern* queries a lookaround — and an
    /// anchor's `exclude` is reachable from no pattern, so it is `false` for the
    /// common anchored catalog, `PUNCT_ANCHOR` included. Gating on it would skip
    /// every anchor. This pass needs the session because that is where the
    /// programs live, not because a pattern asked for one.
    ///
    /// A catalog declaring no anchor returns before building either session, so
    /// it pays nothing per sentence. That guard is not an optimization needing a
    /// profile — the answer is known to be the empty map — and it is load-bearing
    /// rather than tidy: this runs from the publication seam under
    /// `PolicyMode::Apply`, which is selected by `CatalogPolicy::declared()`,
    /// i.e. by declaring *any* policy and not by declaring an anchor. Without it
    /// every anchor-free catalog would pay one `Rc` allocation and one zeroed
    /// `rows` vector per sentence for an empty answer.
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
    /// them — so widening the deduplication key on 2026-08-29 moved no counter
    /// on any catalog. These two do see it
    /// (docs/design/2026-08-29-where-capture-assignment.md).
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
        assert_eq!(metrics.dense_match_closure_predicate_calls, 0);
        assert_eq!(metrics.dense_advance_validation_predicate_calls, 0);
        assert_eq!(metrics.dense_retry_advance_validation_predicate_calls, 0);
        assert_eq!(metrics.dense_retry_extend_validation_predicate_calls, 0);
        assert_eq!(metrics.dense_predicate_calls, 1);
        assert_eq!(metrics.spawn_predicate_calls, 1);
    }
}
