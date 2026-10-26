//! Dense, state-id-indexed view of an immutable NFA.

use super::predicate::{CompiledPositionAssertion, PredicateCatalog, PredicateId};
use super::spawn::{SpawnDecline, SpawnTable};
use crate::fsm::types::{FinalState, TransitionKind, TransitionLabel};
use crate::token_match::AttributeKey;
use compact_str::CompactString;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(crate) struct DenseMachine {
    transitions: Vec<Vec<(TransitionLabel, usize)>>,
    final_pattern: Vec<Option<CompactString>>,
    state_patterns: Vec<Vec<CompactString>>,
    /// Global id of a state's first transition; `transition_base[state] + i`
    /// identifies transition `i` of `state` for per-sentence memo tables.
    pub(crate) transition_base: Vec<usize>,
    /// Distinct-predicate id per transition (indexed by global transition
    /// id). Pattern references duplicate identical attribute tests across
    /// many transitions; deduplicating them lets one memoized evaluation
    /// serve every transition sharing the predicate.
    pub(crate) predicate_id: Vec<PredicateId>,
    compiled_assertion: Vec<Option<CompiledPositionAssertion>>,
    /// Whether any transition asserts a *general* lookaround.
    ///
    /// Stored rather than scanned, because
    /// [`super::compiled::CompiledCatalog::requires_assertion_session`] reads it
    /// once per sentence at each of the matcher's entry points to decide whether
    /// to build an assertion session at all, and once more at compile time for
    /// the spawn-limit gate.
    has_lookaround_assertion: bool,
    pub(crate) spawn_table: SpawnTable,
}

impl DenseMachine {
    pub(crate) fn build(
        state_machine: &HashMap<usize, Vec<(TransitionLabel, usize)>>,
        final_states: &HashSet<FinalState>,
        state_patterns: &HashMap<usize, Vec<CompactString>>,
        predicates: &mut PredicateCatalog,
    ) -> Self {
        let state_count = state_machine
            .iter()
            .flat_map(|(from, transitions)| {
                std::iter::once(*from).chain(transitions.iter().map(|(_, to)| *to))
            })
            .chain(final_states.iter().map(|fs| fs.state_id))
            .chain(state_patterns.keys().copied())
            .max()
            .map_or(0, |max_id| max_id + 1);

        let mut transitions = vec![Vec::new(); state_count];
        for (state, outgoing) in state_machine {
            transitions[*state] = outgoing.clone();
        }

        // Deterministic assignment: lowest pattern name wins when a state id
        // carries several FinalState entries. The previous per-call map kept
        // whichever HashSet iteration order produced first.
        let mut final_pattern = vec![None::<CompactString>; state_count];
        let mut ordered_finals: Vec<&FinalState> = final_states.iter().collect();
        ordered_finals.sort();
        for fs in ordered_finals {
            final_pattern[fs.state_id].get_or_insert_with(|| fs.pattern_name.clone());
        }

        let mut patterns = vec![Vec::new(); state_count];
        for (state, names) in state_patterns {
            patterns[*state] = names.clone();
        }

        let mut transition_base = Vec::with_capacity(state_count);
        let mut total_transitions = 0usize;
        for outgoing in &transitions {
            transition_base.push(total_transitions);
            total_transitions += outgoing.len();
        }

        let mut predicate_id = Vec::with_capacity(total_transitions);
        let mut compiled_assertion = Vec::with_capacity(total_transitions);
        for outgoing in &transitions {
            for (spec, _) in outgoing {
                predicate_id.push(predicates.intern(&spec.attrs));
                compiled_assertion.push(match spec.kind() {
                    TransitionKind::Assert(assertion) => {
                        Some(predicates.intern_position_assertion(assertion))
                    }
                    _ => None,
                });
            }
        }
        let spawn_table = SpawnTable::build(&transitions, predicates);
        let has_lookaround_assertion = compiled_assertion
            .iter()
            .flatten()
            .any(|assertion| matches!(assertion, CompiledPositionAssertion::Lookaround(_)));

        Self {
            transitions,
            final_pattern,
            state_patterns: patterns,
            transition_base,
            predicate_id,
            compiled_assertion,
            has_lookaround_assertion,
            spawn_table,
        }
    }

    /// Whether executing this machine ever consults a general lookaround.
    ///
    /// A `PreviousToken` assertion is deliberately excluded: the specialization
    /// answers it from the token slice, with no assertion session at all.
    pub(crate) fn queries_lookaround(&self) -> bool {
        self.has_lookaround_assertion
    }

    pub(crate) fn required_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        self.transitions.iter().flat_map(|transitions| {
            transitions
                .iter()
                .flat_map(|(spec, _)| spec.attrs.iter().map(|attribute| attribute.attr.clone()))
        })
    }

    #[inline(always)]
    pub(crate) fn state_count(&self) -> usize {
        self.transitions.len()
    }

    pub(crate) fn spawn_decline(&self) -> Option<&SpawnDecline> {
        self.spawn_table.decline.as_ref()
    }

    #[inline(always)]
    pub(crate) fn transitions(&self, state: usize) -> &[(TransitionLabel, usize)] {
        self.transitions.get(state).map_or(&[], Vec::as_slice)
    }

    #[inline(always)]
    pub(crate) fn final_pattern(&self, state: usize) -> Option<&CompactString> {
        self.final_pattern.get(state).and_then(Option::as_ref)
    }

    #[inline(always)]
    pub(crate) fn patterns(&self, state: usize) -> &[CompactString] {
        self.state_patterns.get(state).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn assertion(
        &self,
        state: usize,
        transition_index: usize,
    ) -> &CompiledPositionAssertion {
        self.compiled_assertion[self.transition_base[state] + transition_index]
            .as_ref()
            .expect("assertion transition has a compiled assertion")
    }
}
