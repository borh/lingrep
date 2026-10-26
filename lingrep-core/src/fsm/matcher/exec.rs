//! Advancement kernel: the per-token NFA simulation loop.
//!
//! Provides the `Matcher` struct with NFA simulation, state transition logic,
//! and backtracking for pattern matching. Emission (`MatchSink`) lives in
//! `emit.rs`; the compile-time spawn table lives in `spawn.rs`; the dense
//! machine view lives in `dense.rs`. This module owns advancing the
//! frontier: `find_matches_internal`, `handle_state_transition`,
//! `get_action`, and `epsilon_closure_with`.

use super::dense::DenseMachine;
use super::pool::{FastBuild, HASHSET_POOL, StateSet};
use super::predicate::{
    CompiledPositionAssertion, PREDICATE_SITE_DENSE_ADVANCE, PREDICATE_SITE_DENSE_MAIN,
    PREDICATE_SITE_DENSE_MATCH_CLOSURE, PREDICATE_SITE_DENSE_RETRY_ADVANCE,
    PREDICATE_SITE_DENSE_RETRY_EXTEND, PredicateSession,
};
use super::spawn;
use crate::fsm::types::*;
use crate::token_match::{AttributeKey, SequenceToken, TokenInfoTrait, TokenSequence};
use compact_str::CompactString;
use log::{debug, trace};

pub(crate) struct Matcher;

fn form_key() -> &'static AttributeKey {
    static FORM: std::sync::LazyLock<AttributeKey> =
        std::sync::LazyLock::new(|| AttributeKey::new("text").expect("form is canonical"));
    &FORM
}

#[derive(Default)]
pub(crate) struct ExecScratch {
    /// Generation-stamped DFS visited marks (state id -> generation).
    visited: Vec<u32>,
    visit_generation: u32,
    dfs_stack: Vec<usize>,
    /// Closure seeds, reordered in place so equal-provenance seeds are
    /// contiguous. Held in scratch so grouping allocates nothing per call.
    closure_seeds: Vec<StateInfo>,
    /// Generation-stamped marks indexed by `start_token`, used only to count
    /// how many distinct start tokens a seed set carries.
    token_marks: Vec<u32>,
    token_generation: u32,
    /// The two frontier buffers the position loop swaps between. Pooled for
    /// the same reason the state sets were: otherwise every sentence pays a
    /// fresh allocation for each.
    frontier: Vec<StateInfo>,
    frontier_next: Vec<StateInfo>,
    /// Per-position spawn candidate bitset, reused across positions so the
    /// dispatched injection path allocates nothing per token.
    spawn_candidates: Vec<u64>,
}

impl ExecScratch {
    /// The candidate bitset, grown once to `words` and reused thereafter.
    ///
    /// The contents are not cleared: `SpawnDispatch::candidates` overwrites
    /// every word it is given.
    pub(crate) fn spawn_candidates(&mut self, words: usize) -> &mut [u64] {
        if self.spawn_candidates.len() < words {
            self.spawn_candidates.resize(words, 0);
        }
        &mut self.spawn_candidates[..words]
    }
}

thread_local! {
    static EXEC_SCRATCH: std::cell::RefCell<ExecScratch> =
        std::cell::RefCell::new(ExecScratch::default());

}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExecScratchCapacities {
    pub visited: usize,
    pub dfs_stack: usize,
    pub closure_seeds: usize,
    pub token_marks: usize,
    pub frontier: usize,
    pub frontier_next: usize,
    pub spawn_candidates: usize,
}

#[cfg(test)]
pub(crate) fn exec_scratch_capacities() -> ExecScratchCapacities {
    EXEC_SCRATCH.with(|cell| {
        let scratch = cell.borrow();
        ExecScratchCapacities {
            visited: scratch.visited.capacity(),
            dfs_stack: scratch.dfs_stack.capacity(),
            closure_seeds: scratch.closure_seeds.capacity(),
            token_marks: scratch.token_marks.capacity(),
            frontier: scratch.frontier.capacity(),
            frontier_next: scratch.frontier_next.capacity(),
            spawn_candidates: scratch.spawn_candidates.capacity(),
        }
    })
}

/// Release the current thread's reusable execution scratch.
pub(crate) fn clear_exec_scratch() {
    EXEC_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        scratch.visited.clear();
        scratch.visited.shrink_to_fit();
        scratch.dfs_stack.clear();
        scratch.dfs_stack.shrink_to_fit();
        scratch.closure_seeds.clear();
        scratch.closure_seeds.shrink_to_fit();
        scratch.token_marks.clear();
        scratch.token_marks.shrink_to_fit();
        scratch.token_generation = 0;
        scratch.frontier.clear();
        scratch.frontier.shrink_to_fit();
        scratch.frontier_next.clear();
        scratch.frontier_next.shrink_to_fit();
        scratch.spawn_candidates.clear();
        scratch.spawn_candidates.shrink_to_fit();
        scratch.visit_generation = 0;
    });
}

impl ExecScratch {
    /// Take the thread's scratch, prepared for a fresh `find_matches` call.
    fn take_for_call(dense: &DenseMachine) -> Self {
        let mut scratch = EXEC_SCRATCH.with(|cell| std::mem::take(&mut *cell.borrow_mut()));

        if scratch.visited.len() < dense.state_count() {
            scratch.visited.resize(dense.state_count(), 0);
        }
        scratch
    }

    /// Return the scratch to the thread-local for the next call.
    fn recycle(self) {
        EXEC_SCRATCH.with(|cell| *cell.borrow_mut() = self);
    }

    /// Bump the generation so previous visited marks expire.
    #[inline(always)]
    fn begin_generation(&mut self) {
        self.visit_generation = self.visit_generation.wrapping_add(1);
        if self.visit_generation == 0 {
            self.visited.fill(0);
            self.visit_generation = 1;
        }
    }

    /// Begin a DFS: bump the generation so previous visited marks expire.
    #[inline(always)]
    fn begin_dfs(&mut self, start_state: usize) {
        self.begin_generation();
        self.dfs_stack.clear();
        self.dfs_stack.push(start_state);
    }

    /// Open a `start_token` counting generation over `token_count` tokens.
    fn begin_token_generation(&mut self, token_count: usize) {
        if self.token_marks.len() < token_count {
            self.token_marks.resize(token_count, 0);
        }
        self.token_generation = self.token_generation.wrapping_add(1);
        if self.token_generation == 0 {
            self.token_marks.fill(0);
            self.token_generation = 1;
        }
    }

    /// Whether `start_token` is the first of its value this generation.
    #[inline(always)]
    fn mark_start_token(&mut self, start_token: usize) -> bool {
        match self.token_marks.get_mut(start_token) {
            Some(mark) if *mark != self.token_generation => {
                *mark = self.token_generation;
                true
            }
            // Outside the sized range: counted as distinct, which can only
            // push the decision towards ordering. Ordering is always correct.
            None => true,
            Some(_) => false,
        }
    }

    #[inline(always)]
    fn mark_visited(&mut self, state: usize) -> bool {
        let seen = self.visited[state] == self.visit_generation;
        if !seen {
            self.visited[state] = self.visit_generation;
        }
        !seen
    }
}

impl Matcher {
    fn get_state_set() -> StateSet {
        HASHSET_POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if let Some(mut set) = pool.pop() {
                set.clear();
                set
            } else {
                StateSet::with_capacity_and_hasher(8, FastBuild::default())
            }
        })
    }

    pub(crate) fn recycle_state_set(set: StateSet) {
        HASHSET_POOL.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < 1000 {
                // Bound thread-local retention.
                pool.push(set);
            }
        });
    }

    pub(crate) fn find_matches_dense<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Vec<Match> {
        trace!(
            "=== NFA Matcher::find_matches called with {} tokens ===",
            tokens.len()
        );
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| {
            Self::assertion_holds_at(assertion, predicates, tokens, position)
        };
        Self::find_matches_internal(dense, predicates, tokens, None, &mut assertion_holds)
    }

    pub(crate) fn find_matches_dense_with_assertions<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut super::assertion::AssertionSession<'_, '_, S>,
    ) -> Vec<Match> {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| match assertion
        {
            CompiledPositionAssertion::Lookaround(occurrence) => {
                session.holds(*occurrence, position)
            }
            _ => Self::assertion_holds_at(assertion, predicates, tokens, position),
        };
        Self::find_matches_internal(dense, predicates, tokens, None, &mut assertion_holds)
    }

    pub(crate) fn find_matches_with_trace_dense<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Trace {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| {
            Self::assertion_holds_at(assertion, predicates, tokens, position)
        };
        Self::find_matches_with_trace_dense_by(dense, predicates, tokens, &mut assertion_holds)
    }

    pub(crate) fn find_matches_with_trace_dense_with_assertions<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut super::assertion::AssertionSession<'_, '_, S>,
    ) -> Trace {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| match assertion
        {
            CompiledPositionAssertion::Lookaround(occurrence) => {
                session.holds(*occurrence, position)
            }
            _ => Self::assertion_holds_at(assertion, predicates, tokens, position),
        };
        Self::find_matches_with_trace_dense_by(dense, predicates, tokens, &mut assertion_holds)
    }

    fn find_matches_with_trace_dense_by<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        assertion_holds: &mut dyn FnMut(&CompiledPositionAssertion, usize) -> bool,
    ) -> Trace {
        let mut trace = Trace {
            input: (0..tokens.len())
                .map(|index| {
                    tokens
                        .scalar_key(index, form_key())
                        .map(CompactString::new)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join("")
                .into(),
            entries: Vec::new(),
            events: Vec::new(),
            matches: Vec::new(),
        };

        let mut trace_collector = |state: usize,
                                   token_index: usize,
                                   matched: bool,
                                   next_state: usize,
                                   patterns: &[CompactString]| {
            trace.entries.push(TraceEntry {
                state,
                token: tokens
                    .scalar_key(token_index, form_key())
                    .map(CompactString::new)
                    .unwrap_or_default(),
                matched,
                next_state,
                possible_patterns: patterns.to_vec(),
            });
        };

        trace.matches = Self::find_matches_internal(
            dense,
            predicates,
            tokens,
            Some(&mut trace_collector),
            assertion_holds,
        );
        trace
    }

    fn find_matches_internal<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        mut trace_collector: Option<&mut dyn FnMut(usize, usize, bool, usize, &[CompactString])>,
        assertion_holds: &mut dyn FnMut(&CompiledPositionAssertion, usize) -> bool,
    ) -> Vec<Match> {
        let start_time = std::time::Instant::now();

        // Skip empty token sets
        if tokens.is_empty() {
            return Vec::new();
        }

        let mut sink = super::emit::MatchSink::new();

        let mut scratch = ExecScratch::take_for_call(dense);

        trace!(
            "Matching {} tokens: {}",
            tokens.len(),
            (0..tokens.len())
                .map(|index| tokens
                    .scalar_key(index, form_key())
                    .map(CompactString::new)
                    .unwrap_or_default())
                .collect::<Vec<_>>()
                .join(" | ")
        );

        #[cfg(test)]
        trace!("Starting matching process with {} tokens", tokens.len());

        #[cfg(test)]
        for index in 0..tokens.len() {
            trace!(
                "Token[{}]: {:?}",
                index,
                tokens.scalar_key(index, form_key())
            );
        }

        // One pass over the input: every position injects a fresh match
        // attempt (via the compile-time spawn table) into the same frontier
        // that carries attempts started at earlier positions. Each frontier
        // element keeps its own start_token, so the merged pass emits exactly
        // the per-start-position match set.
        // Taken from scratch so the position loop can hold them across calls
        // that also borrow `scratch`; returned before it is recycled.
        let mut current_states = std::mem::take(&mut scratch.frontier);
        let mut closed_buf = std::mem::take(&mut scratch.frontier_next);
        current_states.clear();
        closed_buf.clear();
        for index in 0..tokens.len() {
            spawn::inject_spawns(
                dense,
                predicates,
                tokens,
                index,
                &mut current_states,
                &mut scratch,
                assertion_holds,
            );
            if current_states.is_empty() {
                continue;
            }

            let token = SequenceToken::new(tokens, index)
                .expect("matching index is inside the token sequence");
            let mut next_states = Self::get_state_set();

            trace!(
                "Processing token[{}]: {:?} with {} active states",
                index,
                token.scalar_key(form_key()).unwrap_or("<no-text>"),
                current_states.len()
            );

            #[cfg(test)]
            trace!(
                "Processing token[{}]: {:?} with {} active states",
                index,
                token.scalar_key(form_key()),
                current_states.len()
            );

            for current_state in &current_states {
                trace!(
                    "  Current state: state={}, position={}, start={}",
                    current_state.state, current_state.position, current_state.start_token
                );

                // Epsilon-created states from an earlier token cannot consume this one.
                if current_state.position < index {
                    #[cfg(test)]
                    trace!(
                        "    SKIP: stale state (position {} < index {})",
                        current_state.position, index
                    );
                    continue;
                }

                if let Some(fs_name) = dense.final_pattern(current_state.state)
                    && index > current_state.start_token
                {
                    sink.emit(fs_name, current_state.start_token, index);
                }

                {
                    let transitions = dense.transitions(current_state.state);
                    for (transition_index, (token_spec, next_state_id)) in
                        transitions.iter().enumerate()
                    {
                        if !matches!(token_spec.kind(), TransitionKind::Consume) {
                            continue;
                        }
                        let matched = Self::spec_matches_at::<PREDICATE_SITE_DENSE_MAIN, _>(
                            dense,
                            predicates,
                            current_state.state,
                            transition_index,
                            token_spec,
                            tokens,
                            index,
                        );

                        trace!(
                            "    Transition: {}→{} {:?} matched={} attrs={:?}",
                            current_state.state,
                            next_state_id,
                            token_spec.op,
                            matched,
                            token_spec.attrs
                        );

                        // An empty predicate is the epsilon-shaped exception.
                        if !matched && !token_spec.attrs.is_empty() {
                            trace!("      SKIP: token doesn't match non-epsilon transition");
                            continue;
                        }

                        debug!(
                            "Transition: {}→{} {:?} {}",
                            current_state.state,
                            next_state_id,
                            token_spec.op,
                            if matched { "✓" } else { "✗" }
                        );

                        if let Some(collector) = trace_collector.as_mut() {
                            collector(
                                current_state.state,
                                index,
                                matched,
                                *next_state_id,
                                dense.patterns(current_state.state),
                            );
                        }

                        Self::handle_state_transition(
                            matched,
                            token_spec,
                            *next_state_id,
                            current_state,
                            index,
                            &mut next_states,
                            &mut sink,
                            dense,
                            predicates,
                            tokens,
                            &mut scratch,
                        );
                    }
                }
            }

            if next_states.is_empty() {
                #[cfg(test)]
                trace!("  No surviving states after position {index}");
                // Later positions still inject fresh attempts through the spawn table.
                current_states.clear();
                Self::recycle_state_set(next_states);
                continue;
            }

            #[cfg(test)]
            trace!("  Next states before epsilon closure: {next_states:?}");

            // Get epsilon closure for next states, validating against the next
            // token. The closure appends into a caller-owned buffer, so the
            // pooled seed set is retained and recycled rather than consumed.
            closed_buf.clear();
            Self::epsilon_closure_into(
                dense,
                next_states.iter().copied(),
                Some((tokens, index + 1)),
                &mut scratch,
                assertion_holds,
                &mut closed_buf,
            );
            Self::recycle_state_set(next_states);
            let closed = &closed_buf;

            #[cfg(test)]
            trace!("  Next states after epsilon closure: {closed:?}");

            for current_state in closed {
                if let Some(fs_name) = dense.final_pattern(current_state.state) {
                    let end_pos = index + 1;
                    sink.emit(fs_name, current_state.start_token, end_pos);
                }
            }

            // The processed frontier is replaced by its successors. Swapping
            // rather than moving keeps both buffers' capacity for the next
            // position.
            std::mem::swap(&mut current_states, &mut closed_buf);
        }
        scratch.frontier = current_states;
        scratch.frontier_next = closed_buf;

        scratch.recycle();

        let filtered = sink.finish();

        #[cfg(test)]
        trace!("Filtered matches: {filtered:?}");

        let elapsed = start_time.elapsed();
        if elapsed > std::time::Duration::from_millis(100) {
            debug!(
                "Slow match: {:?} for {} tokens, {} matches",
                elapsed,
                tokens.len(),
                filtered.len()
            );
        }

        debug!("Filtered matches: {filtered:?}");
        filtered
    }

    /// Zero-width position assertion, evaluated identically by the spawn
    /// table's condition mask and by `epsilon_closure` — the two paths the
    /// differential characterization holds equivalent.
    #[inline(always)]
    fn assertion_holds_at<S: TokenSequence + ?Sized>(
        assertion: &CompiledPositionAssertion,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        position: usize,
    ) -> bool {
        match assertion {
            CompiledPositionAssertion::SentenceStart => position == 0,
            CompiledPositionAssertion::SentenceEnd => position == tokens.len(),
            CompiledPositionAssertion::PreviousToken(predicate) => {
                position.checked_sub(1).is_some_and(|previous| {
                    predicates.matches_at_nfa_previous_token(*predicate, tokens, previous)
                })
            }
            CompiledPositionAssertion::Lookaround(_) => false,
        }
    }

    /// Memoized predicate evaluation for a transition identified by
    /// (state, local index) against the token at `position`.
    #[inline(always)]
    fn spec_matches_at<const SITE: usize, S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        state: usize,
        transition_index: usize,
        spec: &TokenSpec,
        tokens: &S,
        position: usize,
    ) -> bool {
        if spec.attrs.is_empty() {
            return true;
        }
        let predicate_id = dense.predicate_id[dense.transition_base[state] + transition_index];
        predicates.matches_at_site::<SITE, S>(predicate_id, tokens, position)
    }

    fn handle_state_transition<S: TokenSequence + ?Sized>(
        matched: bool,
        token_spec: &TokenSpec,
        next_state: usize,
        current_state: &StateInfo,
        token_index: usize,
        next_states: &mut StateSet,
        sink: &mut super::emit::MatchSink,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        scratch: &mut ExecScratch,
    ) {
        #[cfg(test)]
        trace!(
            "    handle_state_transition: matched={}, op={:?}, count={}, is_final={}",
            matched,
            token_spec.op,
            current_state.position - current_state.start_token,
            dense.final_pattern(current_state.state).is_some()
        );

        debug!(
            "get_action: matched={}, op={:?}, count={}, is_final={}",
            matched,
            token_spec.op,
            current_state.position - current_state.start_token,
            dense.final_pattern(current_state.state).is_some()
        );

        // If the token doesn't match and this isn't an epsilon transition, reject immediately
        if !matched && !token_spec.attrs.is_empty() {
            #[cfg(test)]
            trace!("    Rejecting transition - token doesn't match pattern");
            return;
        }

        let action = Self::get_action(matched, token_spec, current_state, dense);

        #[cfg(test)]
        trace!("    Action: {action:?}");

        match action {
            Action::Match | Action::MatchExtend => {
                if let Some(fs_name) = dense.final_pattern(next_state) {
                    // Epsilon transitions reach the final state without consuming the
                    // current token. Preserve the active state's exclusive consumed
                    // position so a nonmatching repetition terminator is not included.
                    let end_pos = if Self::is_epsilon_transition(token_spec) {
                        current_state.position
                    } else {
                        token_index + 1
                    };
                    sink.emit(fs_name, current_state.start_token, end_pos);
                }

                let start_token = if current_state.state == 0 {
                    token_index
                } else {
                    current_state.start_token
                };

                #[cfg(test)]
                trace!(
                    "      Setting start_token to {} (was {})",
                    start_token, current_state.start_token
                );

                // An epsilon advance survives only if its closure can consume this token.
                if token_spec.attrs.is_empty() && token_index < tokens.len() {
                    let mut can_match = false;
                    scratch.begin_dfs(next_state);

                    while let Some(check_state) = scratch.dfs_stack.pop() {
                        if !scratch.mark_visited(check_state) {
                            continue;
                        }

                        for (i, (spec, next)) in dense.transitions(check_state).iter().enumerate() {
                            if spec.attrs.is_empty() {
                                scratch.dfs_stack.push(*next);
                            } else if Self::spec_matches_at::<PREDICATE_SITE_DENSE_MATCH_CLOSURE, _>(
                                dense,
                                predicates,
                                check_state,
                                i,
                                spec,
                                tokens,
                                token_index,
                            ) {
                                can_match = true;
                                break;
                            }
                        }

                        if can_match {
                            break;
                        }
                    }

                    if !can_match && next_state != 0 {
                        #[cfg(test)]
                        trace!(
                            "      Skipping next state {next_state} - cannot match current token"
                        );
                        return;
                    }
                }

                // Epsilon transitions do not advance either consumed position.
                let (new_position, new_entry_position) = if Self::is_epsilon_transition(token_spec)
                {
                    (current_state.position, current_state.position)
                } else {
                    (token_index + 1, token_index + 1)
                };
                let new_state = StateInfo {
                    state: next_state,
                    start_token,
                    position: new_position,
                    entry_position: new_entry_position,
                };

                #[cfg(test)]
                trace!("      Adding next state: {new_state:?}");

                next_states.insert(new_state);

                if action == Action::MatchExtend {
                    let extend_state = StateInfo {
                        state: current_state.state,
                        start_token: current_state.start_token,
                        position: token_index + 1,
                        entry_position: current_state.entry_position,
                    };

                    #[cfg(test)]
                    trace!("      Also keeping current state: {extend_state:?}");

                    next_states.insert(extend_state);
                }
            }
            Action::Advance => {
                let start_token = if current_state.state == 0 {
                    if matched || token_spec.attrs.is_empty() {
                        token_index
                    } else {
                        current_state.start_token
                    }
                } else {
                    current_state.start_token
                };

                // An initial epsilon transition is viable only if its target can consume here.
                if current_state.state == 0
                    && token_spec.attrs.is_empty()
                    && token_index < tokens.len()
                {
                    let can_match =
                        dense
                            .transitions(next_state)
                            .iter()
                            .enumerate()
                            .any(|(i, (spec, _))| {
                                if spec.attrs.is_empty() {
                                    return false;
                                }

                                Self::spec_matches_at::<PREDICATE_SITE_DENSE_ADVANCE, _>(
                                    dense,
                                    predicates,
                                    next_state,
                                    i,
                                    spec,
                                    tokens,
                                    token_index,
                                )
                            });

                    if !can_match {
                        #[cfg(test)]
                        trace!(
                            "      Skipping next state {next_state} - cannot match current token"
                        );
                        return;
                    }
                }

                let new_state = StateInfo {
                    state: next_state,
                    start_token,
                    position: token_index + 1,
                    entry_position: token_index + 1,
                };
                next_states.insert(new_state);
            }
            Action::Retry => {
                let new_state = StateInfo {
                    state: current_state.state,
                    start_token: current_state.start_token,
                    position: token_index + 1,
                    entry_position: current_state.entry_position,
                };
                next_states.insert(new_state);
            }
            Action::RetryAdvance => {
                let retry_state = StateInfo {
                    state: current_state.state,
                    start_token: current_state.start_token,
                    position: token_index + 1,
                    entry_position: current_state.entry_position,
                };
                next_states.insert(retry_state);

                let start_token = if current_state.state == 0 {
                    if matched || token_spec.attrs.is_empty() {
                        token_index
                    } else {
                        #[cfg(test)]
                        trace!("      Skipping advance state - token doesn't match pattern");
                        return;
                    }
                } else {
                    current_state.start_token
                };

                // An initial epsilon transition is viable only if its target can consume here.
                if current_state.state == 0
                    && token_spec.attrs.is_empty()
                    && token_index < tokens.len()
                {
                    let can_match =
                        dense
                            .transitions(next_state)
                            .iter()
                            .enumerate()
                            .any(|(i, (spec, _))| {
                                if spec.attrs.is_empty() {
                                    return false;
                                }

                                Self::spec_matches_at::<PREDICATE_SITE_DENSE_RETRY_ADVANCE, _>(
                                    dense,
                                    predicates,
                                    next_state,
                                    i,
                                    spec,
                                    tokens,
                                    token_index,
                                )
                            });

                    if !can_match {
                        #[cfg(test)]
                        trace!(
                            "      Skipping next state {next_state} - cannot match current token"
                        );
                        return;
                    }
                }

                let advance_state = StateInfo {
                    state: next_state,
                    start_token,
                    position: token_index + 1,
                    entry_position: token_index + 1,
                };
                next_states.insert(advance_state);
            }
            Action::Reject => {}
            Action::RetryExtend => {
                let retry_state = StateInfo {
                    state: current_state.state,
                    start_token: current_state.start_token,
                    position: token_index + 1,
                    entry_position: current_state.entry_position,
                };
                next_states.insert(retry_state);

                let start_token = if current_state.state == 0 {
                    if matched || token_spec.attrs.is_empty() {
                        token_index
                    } else {
                        #[cfg(test)]
                        trace!("      Skipping advance state - token doesn't match pattern");

                        if let Some(fs_name) = dense.final_pattern(next_state) {
                            let end_pos = current_state.position;
                            sink.emit(fs_name, current_state.start_token, end_pos);
                        }
                        return;
                    }
                } else {
                    current_state.start_token
                };

                // An initial epsilon transition is viable only if its target can consume here.
                if current_state.state == 0
                    && token_spec.attrs.is_empty()
                    && token_index < tokens.len()
                {
                    let can_match =
                        dense
                            .transitions(next_state)
                            .iter()
                            .enumerate()
                            .any(|(i, (spec, _))| {
                                if spec.attrs.is_empty() {
                                    return false;
                                }

                                Self::spec_matches_at::<PREDICATE_SITE_DENSE_RETRY_EXTEND, _>(
                                    dense,
                                    predicates,
                                    next_state,
                                    i,
                                    spec,
                                    tokens,
                                    token_index,
                                )
                            });

                    if !can_match {
                        #[cfg(test)]
                        trace!(
                            "      Skipping next state {next_state} - cannot match current token"
                        );

                        if let Some(fs_name) = dense.final_pattern(next_state) {
                            let end_pos = current_state.position;
                            sink.emit(fs_name, current_state.start_token, end_pos);
                        }
                        return;
                    }
                }

                let advance_state = StateInfo {
                    state: next_state,
                    start_token,
                    position: token_index + 1,
                    entry_position: token_index + 1,
                };
                next_states.insert(advance_state);

                // Record match if transitioning to final state
                if let Some(fs_name) = dense.final_pattern(next_state) {
                    let end_pos = if !token_spec.attrs.is_empty() && matched {
                        token_index + 1
                    } else {
                        current_state.position
                    };
                    sink.emit(fs_name, current_state.start_token, end_pos);
                }
            }
            Action::MatchReject => {
                if let Some(fs_name) = dense.final_pattern(next_state) {
                    let end_pos = if !token_spec.attrs.is_empty() && matched {
                        token_index + 1
                    } else {
                        current_state.position
                    };
                    sink.emit(fs_name, current_state.start_token, end_pos);
                }
            }
            Action::MatchDouble => {
                if let Some(fs_name) = dense.final_pattern(next_state) {
                    let end_pos = if !token_spec.attrs.is_empty() && matched {
                        token_index + 1
                    } else {
                        current_state.position
                    };
                    sink.emit(fs_name, current_state.start_token, end_pos);
                }

                let current = StateInfo {
                    state: current_state.state,
                    start_token: current_state.start_token,
                    position: token_index + 1,
                    entry_position: current_state.entry_position,
                };
                next_states.insert(current);

                let next = StateInfo {
                    state: next_state,
                    start_token: current_state.start_token,
                    position: token_index + 1,
                    entry_position: token_index + 1,
                };
                next_states.insert(next);
            }
        }
    }

    /// Epsilon closure of `states`, appended to `closure`.
    ///
    /// [`Self::create_epsilon_state`] copies `start_token` and sets both
    /// `position` and `entry_position` to the source's `position`, so a state
    /// derived from a seed carries that seed's provenance and reaches a fixed
    /// point after one hop. Reachability is provenance-independent too: an
    /// `Assert` edge is evaluated at this call's single token position, not at
    /// anything the seed carries.
    ///
    /// Together those make the traversal deduplicable by **state id alone**,
    /// once per provenance group, against the generation-stamped `visited`
    /// array. That replaced a `HashSet<StateInfo>` keyed on the whole tuple
    /// which was measured rejecting nothing at all --- 99,976,726 inserts and
    /// 99,976,726 successes over `patterns/sfe.lx` --- so it was being paid
    /// for as a deduplicator and used as a container.
    ///
    /// The emitted multiset is unchanged. Within a group a seed already at
    /// `entry_position == position` is the same tuple an epsilon hop would
    /// derive, so it claims its id up front; a seed at `entry_position !=
    /// position` does not, because the derived tuple is genuinely distinct and
    /// both are kept. Order does change: seeds are grouped rather than
    /// visited in hash order. Frontier order is not observable --- successors
    /// land in a deduplicating `next_states`, and matches land in a
    /// `MatchSink` keyed on `(pattern, start, end)`.
    pub(crate) fn epsilon_closure_into<
        S: TokenSequence + ?Sized,
        I: IntoIterator<Item = StateInfo>,
    >(
        dense: &DenseMachine,
        states: I,
        tokens: Option<(&S, usize)>,
        scratch: &mut ExecScratch,
        assertion_holds: &mut dyn FnMut(&CompiledPositionAssertion, usize) -> bool,
        closure: &mut Vec<StateInfo>,
    ) {
        // Owned for the call so grouping can borrow the seeds while stamping
        // `visited`; returned to scratch at the end so capacity survives.
        let mut seeds = std::mem::take(&mut scratch.closure_seeds);
        seeds.clear();

        // Collecting the seeds and deciding whether to order them is one pass,
        // not two: `distinct` counts how many carry a start token no earlier
        // seed carried.
        //
        // Ordering only pays when two seeds share a provenance, and provenance
        // includes `start_token`, so an all-distinct seed set cannot be
        // coalesced at all and must not be charged for trying. That is not a
        // corner case --- `[text=a]+ [text=z]` keeps every attempt alive, so the
        // frontier at position *k* carries *k* distinct start tokens, and
        // ordering it turns an O(n^2) match into O(n^2 log n).
        let token_count = tokens.map_or(0, |(sequence, _)| sequence.len());
        let mut distinct = 0;
        if token_count > 0 {
            scratch.begin_token_generation(token_count);
            for seed in states {
                if scratch.mark_start_token(seed.start_token) {
                    distinct += 1;
                }
                seeds.push(seed);
            }
        } else {
            seeds.extend(states);
        }
        if seeds.is_empty() {
            scratch.closure_seeds = seeds;
            return;
        }

        // Grouping is by adjacent run, so unordered seeds stay correct --- a
        // fragmented group deduplicates less, which costs traversal but never
        // changes what is emitted.
        let provenance = |seed: &StateInfo| (seed.start_token, seed.position);
        if token_count == 0 || distinct < seeds.len() {
            seeds.sort_unstable_by_key(provenance);
        }

        let mut group_start = 0;
        while group_start < seeds.len() {
            let group = provenance(&seeds[group_start]);
            let mut group_end = group_start + 1;
            while group_end < seeds.len() && provenance(&seeds[group_end]) == group {
                group_end += 1;
            }

            scratch.begin_generation();
            // `closure` is its own work queue: every state the traversal would
            // enqueue is appended here anyway, in the same order, so a cursor
            // over the tail replaces the second copy a `VecDeque` held.
            let mut cursor = closure.len();
            for seed in &seeds[group_start..group_end] {
                closure.push(*seed);
                if seed.entry_position == seed.position {
                    scratch.mark_visited(seed.state);
                }
            }

            while cursor < closure.len() {
                let state_info = closure[cursor];
                cursor += 1;
                for (transition_index, (token_spec, next_state)) in
                    dense.transitions(state_info.state).iter().enumerate()
                {
                    let assertion_matches = match token_spec.kind() {
                        TransitionKind::Epsilon => true,
                        TransitionKind::Consume => false,
                        TransitionKind::Assert(_) => tokens.is_some_and(|(_, position)| {
                            assertion_holds(
                                dense.assertion(state_info.state, transition_index),
                                position,
                            )
                        }),
                    };
                    if !assertion_matches {
                        continue;
                    }
                    if !scratch.mark_visited(*next_state) {
                        continue;
                    }

                    #[cfg(test)]
                    trace!(
                        "  Epsilon transition: {}→{} for pattern(s) {:?}",
                        state_info.state,
                        next_state,
                        dense.patterns(*next_state)
                    );

                    closure.push(Self::create_epsilon_state(&state_info, *next_state));
                }
            }

            group_start = group_end;
        }

        scratch.closure_seeds = seeds;
    }

    fn get_action(
        matched: bool,
        token_spec: &TokenSpec,
        current_state: &StateInfo,
        dense: &DenseMachine,
    ) -> Action {
        let is_final = dense.final_pattern(current_state.state).is_some();
        // Count tokens consumed since entering this transition, not since match start.
        let count = current_state.position - current_state.entry_position;

        match token_spec.op {
            Operator::Exact(n) => {
                // `count` is local to this transition. At `n`, a matching
                // token completes it; a nonmatch cannot extend it.
                if count > n {
                    Action::Reject
                } else if count == n && matched {
                    Action::Match
                } else if count == n {
                    Action::Reject
                } else if matched {
                    Action::Advance
                } else {
                    Action::Reject
                }
            }
            Operator::Range(min, max) => {
                if count >= max {
                    Action::Match
                } else if count >= min && matched {
                    Action::MatchExtend
                } else if matched {
                    Action::Advance
                } else {
                    Action::Reject
                }
            }
            Operator::AtLeast(n) => {
                if count >= n && matched {
                    Action::MatchExtend
                } else if count >= n {
                    Action::Match
                } else if matched {
                    Action::Advance
                } else {
                    Action::Reject
                }
            }
            Operator::AtMost(n) => {
                if count >= n {
                    Action::Match
                } else if matched {
                    Action::MatchExtend
                } else {
                    Action::Advance
                }
            }
            Operator::ZeroOrMore => {
                if matched {
                    Action::MatchExtend
                } else if is_final {
                    Action::Match
                } else {
                    Action::Advance
                }
            }
            Operator::OneOrMore => {
                if matched && count >= 1 {
                    Action::MatchExtend
                } else if matched {
                    Action::Advance
                } else if count >= 1 {
                    Action::Match
                } else {
                    Action::Reject
                }
            }
            Operator::ZeroOrOne => {
                if matched || is_final {
                    Action::Match
                } else {
                    Action::Advance
                }
            }
            _ => {
                if matched {
                    Action::Advance
                } else {
                    Action::Reject
                }
            }
        }
    }

    fn is_epsilon_transition(token_spec: &TokenSpec) -> bool {
        token_spec.attrs.is_empty() && token_spec.op == Operator::Exact(0)
    }

    fn create_epsilon_state(current: &StateInfo, next_state: usize) -> StateInfo {
        // For epsilon transitions, entry_position = position (no token consumed)
        StateInfo {
            state: next_state,
            start_token: current.start_token,
            position: current.position,
            entry_position: current.position, // entry_position = position for epsilon
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use super::Matcher;
    use crate::fsm::types::Match;

    #[test]
    fn duplicate_final_state_ids_resolve_to_the_lexicographically_first_pattern() {
        use crate::fsm::types::{
            AttrValue, AttrValueType, FinalState, Operator, TokenSpec, TransitionLabel,
        };
        use crate::token_match::{TokenAttributeIndex, TokenInfo};
        use std::collections::{HashMap, HashSet};

        let spec = TokenSpec {
            op: Operator::Exact(1),
            attrs: vec![AttrValue {
                attr: TokenAttributeIndex::Orth.into(),
                value: AttrValueType::Single("a".into()),
                complement: false,
            }],
        };
        let state_machine =
            HashMap::from([(0usize, vec![(TransitionLabel::consume(spec), 1usize)])]);
        let final_states = HashSet::from([
            FinalState {
                state_id: 1,
                pattern_name: "zeta".into(),
            },
            FinalState {
                state_id: 1,
                pattern_name: "alpha".into(),
            },
        ]);
        let state_patterns = HashMap::from([(1usize, vec!["alpha".into(), "zeta".into()])]);
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "a".into());

        let mut predicates = super::super::predicate::PredicateCatalog::default();
        let dense = super::DenseMachine::build(
            &state_machine,
            &final_states,
            &state_patterns,
            &mut predicates,
        );
        let session = super::PredicateSession::new(&predicates);
        let matches = Matcher::find_matches_dense(&dense, &session, &[token]);

        assert_eq!(
            matches,
            vec![Match {
                pattern: "alpha".into(),
                start: 0,
                end: 1,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            }]
        );
    }

    fn matched(start: usize, end: usize) -> Match {
        Match {
            pattern: "p".into(),
            start,
            end,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        }
    }

    fn quadratic_reference(mut matches: Vec<Match>) -> Vec<Match> {
        matches.sort_by(|left, right| {
            let left_len = left.end - left.start;
            let right_len = right.end - right.start;
            right_len
                .cmp(&left_len)
                .then_with(|| left.start.cmp(&right.start))
        });

        let mut filtered = Vec::new();
        for candidate in matches {
            let contained = filtered.iter().any(|previous: &Match| {
                (previous.start == candidate.start && previous.end >= candidate.end)
                    || (previous.start < candidate.start && previous.end > candidate.end)
            });
            if !contained {
                filtered.push(candidate);
            }
        }
        filtered.sort_by_key(|matched| matched.start);
        filtered
    }

    #[test]
    fn interval_sweep_preserves_the_quadratic_filter_contract() {
        let candidates = (0..5)
            .flat_map(|start| ((start + 1)..=5).map(move |end| matched(start, end)))
            .collect::<Vec<_>>();

        for subset in 0..(1usize << candidates.len()) {
            let input = candidates
                .iter()
                .enumerate()
                .filter(|(index, _)| subset & (1 << index) != 0)
                .map(|(_, candidate)| candidate.clone())
                .collect::<Vec<_>>();
            assert_eq!(
                super::super::emit::filter_and_collect_matches(&input),
                quadratic_reference(input),
                "subset {subset:#x}"
            );
        }
    }
}
