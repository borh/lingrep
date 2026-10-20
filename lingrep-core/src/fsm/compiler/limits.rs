use std::error::Error;
use std::fmt::{self, Display, Formatter};

/// The reason a caller's requested limits cannot be applied safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileLimitError {
    budget: &'static str,
    requested: usize,
    maximum: usize,
}

impl CompileLimitError {
    fn raised(budget: &'static str, requested: usize, maximum: usize) -> Self {
        Self {
            budget,
            requested,
            maximum,
        }
    }

    /// Returns the budget whose value was raised.
    pub fn budget(&self) -> &'static str {
        self.budget
    }

    /// Returns the requested value that was rejected.
    pub fn requested(&self) -> usize {
        self.requested
    }

    /// Returns the maximum allowed value.
    pub fn maximum(&self) -> usize {
        self.maximum
    }
}

impl Display for CompileLimitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} budget cannot be raised above {} (requested {})",
            self.budget, self.maximum, self.requested
        )
    }
}

impl Error for CompileLimitError {}

#[derive(Clone, Copy, Debug)]
struct LimitValues {
    source_bytes: usize,
    definitions: usize,
    lexicon_members: usize,
    expression_nodes: usize,
    nesting_depth: usize,
    metadata_bytes: usize,
    regex_bytes: usize,
    reference_edges: usize,
    expansion_work: usize,
    generated_owned_bytes: usize,
    nfa_states: usize,
    transitions: usize,
    token_test_branches: usize,
    resolved_test_nodes: usize,
}

// Supported untrusted-input policy. The closest
// stress probes retain 1.33x depth, 4x expansion-work, 7.94x generated-owned-
// byte, 8x state, and 16x transition headroom; the retained sfe.lx usage has
// substantially more.
//
// `token_test_branches` and `resolved_test_nodes` are not part of that
// measurement set — they are the Boolean token-test compile budgets, carried
// over unchanged from the fixed constants `token_test::MAX_TOKEN_TEST_BRANCHES`
// and `bind::MAX_RESOLVED_TEST_NODES` that declared them before this field
// existed. Every tracked catalog's widest `[…]` holds ten attributes and no
// `test` chain nests more than a handful of levels, so both defaults are
// already generous by orders of magnitude over real usage.
const SUPPORTED_DEFAULTS: LimitValues = LimitValues {
    source_bytes: 1 << 20,
    definitions: 4_096,
    lexicon_members: 65_536,
    expression_nodes: 65_536,
    nesting_depth: 256,
    metadata_bytes: 1 << 18,
    regex_bytes: 1 << 18,
    reference_edges: 65_536,
    expansion_work: 1 << 18,
    generated_owned_bytes: 1 << 26,
    nfa_states: 1 << 17,
    transitions: 1 << 18,
    token_test_branches: 64,
    resolved_test_nodes: 4_096,
};

// A separately reviewed implementation ceiling. Most budgets are fixed at 4x
// their supported defaults. Nesting depth is intentionally only 2x (256 ->
// 512): unlike the size/work counters, depth consumes parser/compiler stack.
// The 192-depth probe leaves only 1.33x default headroom and does not justify
// extrapolating to a 4x (1,024) ceiling. Do not raise the ceiling without
// dedicated parser and compiler stack evidence.
// No public constructor raises limits: callers can only tighten the supported
// defaults. Keeping the ceiling distinct documents the absolute bound for any
// future internal policy without turning it into a CLI/web knob.
const HARD_CEILING: LimitValues = LimitValues {
    source_bytes: 1 << 22,
    definitions: 16_384,
    lexicon_members: 1 << 20,
    expression_nodes: 1 << 18,
    nesting_depth: 512,
    metadata_bytes: 1 << 20,
    regex_bytes: 1 << 20,
    reference_edges: 1 << 18,
    expansion_work: 1 << 20,
    generated_owned_bytes: 1 << 28,
    nfa_states: 1 << 19,
    transitions: 1 << 20,
    token_test_branches: 256,
    resolved_test_nodes: 16_384,
};

/// Deterministic ceilings for source-derived lingrep patterns compilation work.
///
/// Callers begin with [`Default`] and use [`Self::tightened`] to select a
/// smaller trust-domain budget. Returned values expose read-only accessors;
/// only a [`TightenedCompileLimits`] draft may be mutated. Source bytes are
/// the checked sum of lingrep patterns text bytes and UTF-8 diagnostic-label bytes; a
/// multi-call builder applies that budget cumulatively. Generated owned bytes
/// conservatively charge cloned compatibility predicates, transition payload
/// fan-out, catalog names, and final catalog metadata before allocation.
#[derive(Clone, Copy, Debug)]
pub struct CompileLimits {
    values: LimitValues,
    hard_ceiling: LimitValues,
}

/// Mutable draft supplied only to [`CompileLimits::tightened`].
///
/// Its public fields let callers choose smaller budgets without making an
/// already-validated [`CompileLimits`] value mutable.
#[derive(Clone, Copy, Debug)]
pub struct TightenedCompileLimits {
    /// Maximum checked sum of lingrep patterns text and UTF-8 diagnostic-label bytes.
    ///
    /// [`crate::fsm::FsmBuilder`] applies this budget cumulatively across all
    /// registered definitions.
    pub max_source_bytes: usize,
    pub max_definitions: usize,
    pub max_lexicon_members: usize,
    pub max_expression_nodes: usize,
    pub max_nesting_depth: usize,
    pub max_metadata_bytes: usize,
    pub max_regex_bytes: usize,
    pub max_reference_edges: usize,
    /// Maximum expression/reference visits performed by NFA lowering.
    ///
    /// It seeds a second budget it is not named for: the catalog-wide count of
    /// resolved nodes a document's token tests may expand to once their `test`
    /// references are substituted. Substitution is expression-visit work of the
    /// kind this budget already bounds, and giving it a budget of its own would
    /// let a caller tighten one path while leaving the other wide. Tightening
    /// this therefore tightens `test` substitution too.
    pub max_expansion_work: usize,
    /// Maximum source-derived bytes owned by generated compatibility values,
    /// transition payloads, catalog names, and catalog metadata clones.
    pub max_generated_owned_bytes: usize,
    pub max_nfa_states: usize,
    pub max_transitions: usize,
    /// Maximum branches a token test's disjunctive normal form may distribute
    /// to.
    pub max_token_test_branches: usize,
    /// Maximum resolved-tree nodes a token test may expand to once its `test`
    /// references are substituted, checked before distribution runs.
    pub max_resolved_test_nodes: usize,
}

impl CompileLimits {
    /// Returns a copy with a caller-supplied subset of budgets tightened.
    ///
    /// # Errors
    ///
    /// Returns [`CompileLimitError`] when the closure raises any value above
    /// the current limits or the compiler's hard ceiling.
    pub fn tightened(
        self,
        configure: impl FnOnce(&mut TightenedCompileLimits),
    ) -> Result<Self, CompileLimitError> {
        let mut tightened = TightenedCompileLimits::from(self.values);
        configure(&mut tightened);
        let values = LimitValues::from(tightened);

        Self::ensure_not_above_values(values, self.values)?;
        Self::ensure_not_above_values(values, self.hard_ceiling)?;

        Ok(Self {
            values,
            hard_ceiling: self.hard_ceiling,
        })
    }

    /// Returns the maximum checked sum of lingrep patterns text and diagnostic-label bytes.
    pub(crate) fn max_source_bytes(&self) -> usize {
        self.values.source_bytes
    }

    /// Returns the maximum number of source definitions.
    pub(crate) fn max_definitions(&self) -> usize {
        self.values.definitions
    }

    pub(crate) fn max_lexicon_members(&self) -> usize {
        self.values.lexicon_members
    }

    /// Returns the maximum number of parsed expression nodes.
    pub(crate) fn max_expression_nodes(&self) -> usize {
        self.values.expression_nodes
    }

    /// Returns the compatibility-expansion work ceiling.
    ///
    /// Every expanded expression or reference visit consumes one unit,
    /// including repeated visits through a reference DAG.
    pub(crate) fn max_expansion_work(&self) -> usize {
        self.values.expansion_work
    }

    /// Returns the generated owned-payload byte ceiling.
    pub(crate) fn max_generated_owned_bytes(&self) -> usize {
        self.values.generated_owned_bytes
    }

    /// Returns the maximum allowed expression nesting depth.
    pub(crate) fn max_nesting_depth(&self) -> usize {
        self.values.nesting_depth
    }

    /// Returns the maximum total metadata bytes.
    pub(crate) fn max_metadata_bytes(&self) -> usize {
        self.values.metadata_bytes
    }

    /// Returns the maximum total regex pattern bytes.
    pub(crate) fn max_regex_bytes(&self) -> usize {
        self.values.regex_bytes
    }

    /// Returns the maximum number of bound reference edges.
    pub(crate) fn max_reference_edges(&self) -> usize {
        self.values.reference_edges
    }

    /// Returns the maximum number of generated NFA states.
    pub(crate) fn max_nfa_states(&self) -> usize {
        self.values.nfa_states
    }

    /// Returns the maximum number of generated NFA transitions.
    pub(crate) fn max_transitions(&self) -> usize {
        self.values.transitions
    }

    /// Returns the maximum branches a token test's disjunctive normal form
    /// may distribute to.
    pub(crate) fn max_token_test_branches(&self) -> usize {
        self.values.token_test_branches
    }

    /// Returns the maximum resolved-tree nodes a token test may expand to
    /// once its `test` references are substituted.
    ///
    /// Checked before distribution runs, and bounds a different blow-up than
    /// [`Self::max_token_test_branches`]: a `test` chain that names another
    /// test twice doubles this count at every level without ever widening a
    /// branch.
    pub(crate) fn max_resolved_test_nodes(&self) -> usize {
        self.values.resolved_test_nodes
    }

    fn ensure_not_above_values(
        requested: LimitValues,
        maximum: LimitValues,
    ) -> Result<(), CompileLimitError> {
        check_limit("source bytes", requested.source_bytes, maximum.source_bytes)?;
        check_limit("definitions", requested.definitions, maximum.definitions)?;
        check_limit(
            "lexicon members",
            requested.lexicon_members,
            maximum.lexicon_members,
        )?;
        check_limit(
            "expression nodes",
            requested.expression_nodes,
            maximum.expression_nodes,
        )?;
        check_limit(
            "nesting depth",
            requested.nesting_depth,
            maximum.nesting_depth,
        )?;
        check_limit(
            "metadata bytes",
            requested.metadata_bytes,
            maximum.metadata_bytes,
        )?;
        check_limit("regex bytes", requested.regex_bytes, maximum.regex_bytes)?;
        check_limit(
            "reference edges",
            requested.reference_edges,
            maximum.reference_edges,
        )?;
        check_limit(
            "NFA expansion work",
            requested.expansion_work,
            maximum.expansion_work,
        )?;
        check_limit(
            "generated owned bytes",
            requested.generated_owned_bytes,
            maximum.generated_owned_bytes,
        )?;
        check_limit("NFA states", requested.nfa_states, maximum.nfa_states)?;
        check_limit("transitions", requested.transitions, maximum.transitions)?;
        check_limit(
            "token test branches",
            requested.token_test_branches,
            maximum.token_test_branches,
        )?;
        check_limit(
            "resolved test nodes",
            requested.resolved_test_nodes,
            maximum.resolved_test_nodes,
        )
    }
}

impl Default for CompileLimits {
    fn default() -> Self {
        Self {
            values: SUPPORTED_DEFAULTS,
            hard_ceiling: HARD_CEILING,
        }
    }
}

impl From<LimitValues> for TightenedCompileLimits {
    fn from(values: LimitValues) -> Self {
        Self {
            max_source_bytes: values.source_bytes,
            max_definitions: values.definitions,
            max_lexicon_members: values.lexicon_members,
            max_expression_nodes: values.expression_nodes,
            max_nesting_depth: values.nesting_depth,
            max_metadata_bytes: values.metadata_bytes,
            max_regex_bytes: values.regex_bytes,
            max_reference_edges: values.reference_edges,
            max_expansion_work: values.expansion_work,
            max_generated_owned_bytes: values.generated_owned_bytes,
            max_nfa_states: values.nfa_states,
            max_transitions: values.transitions,
            max_token_test_branches: values.token_test_branches,
            max_resolved_test_nodes: values.resolved_test_nodes,
        }
    }
}

impl From<TightenedCompileLimits> for LimitValues {
    fn from(limits: TightenedCompileLimits) -> Self {
        Self {
            source_bytes: limits.max_source_bytes,
            definitions: limits.max_definitions,
            lexicon_members: limits.max_lexicon_members,
            expression_nodes: limits.max_expression_nodes,
            nesting_depth: limits.max_nesting_depth,
            metadata_bytes: limits.max_metadata_bytes,
            regex_bytes: limits.max_regex_bytes,
            reference_edges: limits.max_reference_edges,
            expansion_work: limits.max_expansion_work,
            generated_owned_bytes: limits.max_generated_owned_bytes,
            nfa_states: limits.max_nfa_states,
            transitions: limits.max_transitions,
            token_test_branches: limits.max_token_test_branches,
            resolved_test_nodes: limits.max_resolved_test_nodes,
        }
    }
}

fn check_limit(
    budget: &'static str,
    requested: usize,
    maximum: usize,
) -> Result<(), CompileLimitError> {
    if requested > maximum {
        return Err(CompileLimitError::raised(budget, requested, maximum));
    }
    Ok(())
}

/// Measurements gathered through compiler phases for selecting final limits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompilerStats {
    pub(crate) source_bytes: usize,
    pub(crate) definitions: usize,
    pub(crate) lexicon_members: usize,
    pub(crate) expression_nodes: usize,
    pub(crate) max_depth: usize,
    pub(crate) metadata_bytes: usize,
    pub(crate) regex_bytes: usize,
    pub(crate) reference_edges: usize,
    pub(crate) expansion_work: usize,
    pub(crate) generated_owned_bytes: usize,
    pub(crate) nfa_states: usize,
    pub(crate) transitions: usize,
}

impl CompilerStats {
    /// Returns the checked sum of accepted lingrep patterns text and UTF-8 label bytes.
    ///
    /// A source label is charged once regardless of its definition count;
    /// builder output reports the cumulative total across registrations.
    pub fn source_bytes(&self) -> usize {
        self.source_bytes
    }

    /// Returns the number of named definitions.
    pub fn definitions(&self) -> usize {
        self.definitions
    }

    /// Returns the number of decoded members across inline lexicons.
    pub fn lexicon_members(&self) -> usize {
        self.lexicon_members
    }

    /// Returns the number of typed expression nodes.
    pub fn expression_nodes(&self) -> usize {
        self.expression_nodes
    }

    /// Returns the deepest typed expression level.
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Returns the metadata source bytes charged by the compiler.
    pub fn metadata_bytes(&self) -> usize {
        self.metadata_bytes
    }

    /// Returns the number of regex pattern bytes.
    pub fn regex_bytes(&self) -> usize {
        self.regex_bytes
    }

    /// Returns the number of bound definition-reference edges.
    pub fn reference_edges(&self) -> usize {
        self.reference_edges
    }

    /// Returns the expression/reference visits performed by NFA lowering.
    pub fn expansion_work(&self) -> usize {
        self.expansion_work
    }

    /// Returns the conservative source-derived owned-byte charge generated by
    /// compatibility lowering, transition payload fan-out, and catalog clones.
    ///
    /// This is a deterministic compiler budget, not an allocator or RSS
    /// measurement. Fixed state/transition structure is bounded separately by
    /// the NFA count budgets.
    pub fn generated_owned_bytes(&self) -> usize {
        self.generated_owned_bytes
    }

    /// Returns the number of generated NFA states, excluding global state zero.
    pub fn nfa_states(&self) -> usize {
        self.nfa_states
    }

    /// Returns the number of generated NFA transitions.
    pub fn transitions(&self) -> usize {
        self.transitions
    }
}

impl std::fmt::Display for CompilerStats {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "definitions={} lexicon-members={} expressions={} references={} depth={} regex-bytes={} metadata-bytes={} expansion-work={} nfa-states={} transitions={} generated-bytes={} source-bytes={}",
            self.definitions,
            self.lexicon_members,
            self.expression_nodes,
            self.reference_edges,
            self.max_depth,
            self.regex_bytes,
            self.metadata_bytes,
            self.expansion_work,
            self.nfa_states,
            self.transitions,
            self.generated_owned_bytes,
            self.source_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_can_tighten_but_not_raise_the_hard_ceiling() {
        assert!(
            CompileLimits::default()
                .tightened(|limits| limits.max_definitions = 1)
                .is_ok()
        );
        assert!(
            CompileLimits::default()
                .tightened(|limits| limits.max_definitions = usize::MAX)
                .is_err()
        );
    }

    #[test]
    fn tightened_limits_cannot_relax_an_existing_limit() {
        let tightened = CompileLimits::default()
            .tightened(|limits| limits.max_source_bytes = 128)
            .expect("tightening a default limit must succeed");

        assert!(
            tightened
                .tightened(|limits| limits.max_source_bytes = 129)
                .is_err()
        );
    }

    #[test]
    fn tightened_limits_expose_configured_values_without_mutability() {
        let tightened = CompileLimits::default()
            .tightened(|limits| limits.max_definitions = 1)
            .expect("tightening a default limit must succeed");

        assert_eq!(tightened.max_definitions(), 1);
    }

    #[test]
    fn all_limit_accessors_report_tightened_values() {
        let tightened = CompileLimits::default()
            .tightened(|limits| {
                limits.max_source_bytes = 101;
                limits.max_definitions = 102;
                limits.max_expression_nodes = 103;
                limits.max_nesting_depth = 104;
                limits.max_metadata_bytes = 105;
                limits.max_regex_bytes = 106;
                limits.max_reference_edges = 107;
                limits.max_expansion_work = 108;
                limits.max_generated_owned_bytes = 109;
                limits.max_nfa_states = 110;
                limits.max_transitions = 111;
                limits.max_token_test_branches = 63;
                limits.max_resolved_test_nodes = 112;
            })
            .expect("tightening every limit must succeed");

        assert_eq!(tightened.max_source_bytes(), 101);
        assert_eq!(tightened.max_definitions(), 102);
        assert_eq!(tightened.max_expression_nodes(), 103);
        assert_eq!(tightened.max_nesting_depth(), 104);
        assert_eq!(tightened.max_metadata_bytes(), 105);
        assert_eq!(tightened.max_regex_bytes(), 106);
        assert_eq!(tightened.max_reference_edges(), 107);
        assert_eq!(tightened.max_expansion_work(), 108);
        assert_eq!(tightened.max_generated_owned_bytes(), 109);
        assert_eq!(tightened.max_nfa_states(), 110);
        assert_eq!(tightened.max_transitions(), 111);
        assert_eq!(tightened.max_token_test_branches(), 63);
        assert_eq!(tightened.max_resolved_test_nodes(), 112);
    }

    #[test]
    fn compiler_stats_start_with_all_budget_measurements_at_zero() {
        assert_eq!(
            CompilerStats::default(),
            CompilerStats {
                source_bytes: 0,
                definitions: 0,
                lexicon_members: 0,
                expression_nodes: 0,
                max_depth: 0,
                metadata_bytes: 0,
                regex_bytes: 0,
                reference_edges: 0,
                expansion_work: 0,
                generated_owned_bytes: 0,
                nfa_states: 0,
                transitions: 0,
            }
        );
    }

    #[test]
    fn compiler_stats_display_is_compact_and_machine_scannable() {
        let stats = CompilerStats {
            definitions: 2,
            nfa_states: 3,
            transitions: 4,
            ..CompilerStats::default()
        };

        let rendered = stats.to_string();
        assert!(rendered.contains("definitions=2"));
        assert!(rendered.contains("nfa-states=3"));
        assert!(rendered.contains("transitions=4"));
        assert!(!rendered.contains('\n'));
    }

    #[test]
    fn supported_defaults_and_hard_ceilings_match_the_measured_policy() {
        let limits = CompileLimits::default();

        assert_eq!(limits.values.source_bytes, 1 << 20);
        assert_eq!(limits.values.definitions, 4_096);
        assert_eq!(limits.values.expression_nodes, 65_536);
        assert_eq!(limits.values.nesting_depth, 256);
        assert_eq!(limits.values.metadata_bytes, 1 << 18);
        assert_eq!(limits.values.regex_bytes, 1 << 18);
        assert_eq!(limits.values.reference_edges, 65_536);
        assert_eq!(limits.values.expansion_work, 1 << 18);
        assert_eq!(limits.values.generated_owned_bytes, 1 << 26);
        assert_eq!(limits.values.nfa_states, 1 << 17);
        assert_eq!(limits.values.transitions, 1 << 18);
        assert_eq!(limits.values.token_test_branches, 64);
        assert_eq!(limits.values.resolved_test_nodes, 4_096);

        assert_eq!(limits.hard_ceiling.source_bytes, 1 << 22);
        assert_eq!(limits.hard_ceiling.definitions, 16_384);
        assert_eq!(limits.hard_ceiling.expression_nodes, 1 << 18);
        assert_eq!(limits.hard_ceiling.nesting_depth, 512);
        assert_eq!(limits.hard_ceiling.metadata_bytes, 1 << 20);
        assert_eq!(limits.hard_ceiling.regex_bytes, 1 << 20);
        assert_eq!(limits.hard_ceiling.reference_edges, 1 << 18);
        assert_eq!(limits.hard_ceiling.expansion_work, 1 << 20);
        assert_eq!(limits.hard_ceiling.generated_owned_bytes, 1 << 28);
        assert_eq!(limits.hard_ceiling.nfa_states, 1 << 19);
        assert_eq!(limits.hard_ceiling.transitions, 1 << 20);
        assert_eq!(limits.hard_ceiling.token_test_branches, 256);
        assert_eq!(limits.hard_ceiling.resolved_test_nodes, 16_384);
    }
}
