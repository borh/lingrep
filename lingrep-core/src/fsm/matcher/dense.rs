//! Dense, state-id-indexed view of an immutable NFA.

use super::predicate::{CompiledPositionAssertion, PredicateCatalog, PredicateId};
use super::spawn::{SpawnDecline, SpawnTable};
use crate::fsm::types::{FinalState, TransitionLabel};
use crate::token_match::AttributeKey;
use compact_str::CompactString;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub(crate) struct DenseMachine {
    /// Contiguous CSR transition table indexed by
    /// `transition_base[state]..transition_base[state + 1]`.
    transitions: Vec<(TransitionLabel, usize)>,
    final_pattern: Vec<Option<CompactString>>,
    state_patterns: Vec<Vec<CompactString>>,
    /// Global id of a state's first transition, with a trailing sentinel equal
    /// to `transitions.len()`; `transition_base[state] + i` identifies
    /// transition `i` of `state` for per-sentence memo tables.
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

        let mut state_transitions = vec![Vec::new(); state_count];
        for (state, outgoing) in state_machine {
            state_transitions[*state] = outgoing.clone();
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

        let mut transition_base = Vec::with_capacity(state_count + 1);
        let mut total_transitions = 0usize;
        for outgoing in &state_transitions {
            transition_base.push(total_transitions);
            total_transitions += outgoing.len();
        }
        transition_base.push(total_transitions);

        let mut predicate_id = Vec::with_capacity(total_transitions);
        let mut compiled_assertion = Vec::with_capacity(total_transitions);
        for outgoing in &state_transitions {
            for (label, _) in outgoing {
                predicate_id.push(predicates.intern(label.attrs()));
                compiled_assertion.push(match label {
                    TransitionLabel::Assert(assertion) => {
                        Some(predicates.intern_position_assertion(assertion))
                    }
                    TransitionLabel::Epsilon | TransitionLabel::Consume(_) => None,
                });
            }
        }
        let spawn_table = SpawnTable::build(&state_transitions, predicates);
        let has_lookaround_assertion = compiled_assertion
            .iter()
            .flatten()
            .any(|assertion| matches!(assertion, CompiledPositionAssertion::Lookaround(_)));

        let mut transitions = Vec::with_capacity(total_transitions);
        for outgoing in state_transitions {
            transitions.extend(outgoing);
        }

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
    /// A `PreviousToken` assertion is excluded: the specialization evaluates
    /// it directly from the token slice without an assertion session.
    pub(crate) fn queries_lookaround(&self) -> bool {
        self.has_lookaround_assertion
    }

    pub(crate) fn required_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        self.transitions
            .iter()
            .flat_map(|(label, _)| label.attrs().iter().map(|attribute| attribute.attr.clone()))
    }

    #[inline(always)]
    pub(crate) fn state_count(&self) -> usize {
        self.final_pattern.len()
    }

    pub(crate) fn spawn_decline(&self) -> Option<&SpawnDecline> {
        self.spawn_table.decline()
    }

    #[inline(always)]
    pub(crate) fn transitions(&self, state: usize) -> &[(TransitionLabel, usize)] {
        let Some(&start) = self.transition_base.get(state) else {
            return &[];
        };
        let Some(&end) = self.transition_base.get(state + 1) else {
            return &[];
        };
        &self.transitions[start..end]
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
