//! Advances the NFA frontier and evaluates epsilon closures.

use super::dense::DenseMachine;
use super::pool::{FastBuild, HASHSET_POOL, StateSet};
use super::predicate::{CompiledPositionAssertion, PREDICATE_SITE_DENSE_MAIN, PredicateSession};
use super::resource::{MatchResourceError, OutputBudget};
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
    /// Generation-stamped visited marks (state id -> generation).
    visited: Vec<u32>,
    visit_generation: u32,
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

    #[cfg(test)]
    pub(crate) fn find_matches_dense<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Vec<Match> {
        Self::try_find_matches_dense(&OutputBudget::default(), dense, predicates, tokens)
            .expect("matching exceeded its resource budget")
    }

    pub(crate) fn try_find_matches_dense<S: TokenSequence + ?Sized>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Vec<Match>, MatchResourceError> {
        trace!(
            "=== NFA Matcher::find_matches called with {} tokens ===",
            tokens.len()
        );
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| {
            Self::assertion_holds_at(assertion, predicates, tokens, position)
        };
        Self::find_matches_internal(
            output,
            dense,
            predicates,
            tokens,
            None,
            &mut assertion_holds,
        )
    }

    pub(crate) fn try_find_matches_dense_with_assertions<S: TokenSequence + ?Sized>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut super::assertion::AssertionSession<'_, '_, S>,
    ) -> Result<Vec<Match>, MatchResourceError> {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| match assertion
        {
            CompiledPositionAssertion::Lookaround(occurrence) => {
                session.holds(*occurrence, position)
            }
            _ => Self::assertion_holds_at(assertion, predicates, tokens, position),
        };
        Self::find_matches_internal(
            output,
            dense,
            predicates,
            tokens,
            None,
            &mut assertion_holds,
        )
    }

    #[cfg(test)]
    pub(crate) fn find_matches_with_trace_dense<S: TokenSequence + ?Sized>(
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Trace {
        Self::try_find_matches_with_trace_dense(&OutputBudget::default(), dense, predicates, tokens)
            .expect("matching exceeded its resource budget")
    }

    pub(crate) fn try_find_matches_with_trace_dense<S: TokenSequence + ?Sized>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Trace, MatchResourceError> {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| {
            Self::assertion_holds_at(assertion, predicates, tokens, position)
        };
        Self::try_find_matches_with_trace_dense_by(
            output,
            dense,
            predicates,
            tokens,
            &mut assertion_holds,
        )
    }

    pub(crate) fn try_find_matches_with_trace_dense_with_assertions<S: TokenSequence + ?Sized>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        session: &mut super::assertion::AssertionSession<'_, '_, S>,
    ) -> Result<Trace, MatchResourceError> {
        let mut assertion_holds = |assertion: &CompiledPositionAssertion, position| match assertion
        {
            CompiledPositionAssertion::Lookaround(occurrence) => {
                session.holds(*occurrence, position)
            }
            _ => Self::assertion_holds_at(assertion, predicates, tokens, position),
        };
        Self::try_find_matches_with_trace_dense_by(
            output,
            dense,
            predicates,
            tokens,
            &mut assertion_holds,
        )
    }

    fn try_find_matches_with_trace_dense_by<S: TokenSequence + ?Sized, F>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        assertion_holds: &mut F,
    ) -> Result<Trace, MatchResourceError>
    where
        F: FnMut(&CompiledPositionAssertion, usize) -> bool,
    {
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
            output,
            dense,
            predicates,
            tokens,
            Some(&mut trace_collector),
            assertion_holds,
        )?;
        Ok(trace)
    }

    fn find_matches_internal<S: TokenSequence + ?Sized, F>(
        output: &OutputBudget,
        dense: &DenseMachine,
        predicates: &PredicateSession<'_>,
        tokens: &S,
        mut trace_collector: Option<&mut dyn FnMut(usize, usize, bool, usize, &[CompactString])>,
        assertion_holds: &mut F,
    ) -> Result<Vec<Match>, MatchResourceError>
    where
        F: FnMut(&CompiledPositionAssertion, usize) -> bool,
    {
        let start_time = log::log_enabled!(log::Level::Debug).then(std::time::Instant::now);

        // Skip empty token sets
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        output.check()?;
        let mut sink = super::emit::MatchSink::with_output(output);

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
                    sink.emit(fs_name, current_state.start_token, index)?;
                }

                {
                    let transitions = dense.transitions(current_state.state);
                    for (transition_index, (label, next_state_id)) in transitions.iter().enumerate()
                    {
                        let TransitionLabel::Consume(attrs) = label else {
                            continue;
                        };
                        let matched = Self::spec_matches_at::<PREDICATE_SITE_DENSE_MAIN, _>(
                            dense,
                            predicates,
                            current_state.state,
                            transition_index,
                            attrs,
                            tokens,
                            index,
                        );

                        trace!(
                            "    Transition: {}→{} matched={} attrs={:?}",
                            current_state.state, next_state_id, matched, attrs
                        );

                        if !matched {
                            trace!("      SKIP: token doesn't match transition");
                            continue;
                        }

                        debug!("Transition: {}→{} ✓", current_state.state, next_state_id);

                        if let Some(collector) = trace_collector.as_mut() {
                            collector(
                                current_state.state,
                                index,
                                matched,
                                *next_state_id,
                                dense.patterns(current_state.state),
                            );
                        }

                        let start_token = if current_state.state == 0 {
                            index
                        } else {
                            current_state.start_token
                        };
                        next_states.insert(StateInfo {
                            state: *next_state_id,
                            start_token,
                            position: index + 1,
                        });
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
                    sink.emit(fs_name, current_state.start_token, end_pos)?;
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

        let filtered = sink.finish()?;

        #[cfg(test)]
        trace!("Filtered matches: {filtered:?}");

        if let Some(start_time) = start_time {
            let elapsed = start_time.elapsed();
            if elapsed > std::time::Duration::from_millis(100) {
                debug!(
                    "Slow match: {:?} for {} tokens, {} matches",
                    elapsed,
                    tokens.len(),
                    filtered.len()
                );
            }
        }

        debug!("Filtered matches: {filtered:?}");
        Ok(filtered)
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
        attrs: &[SymbolicAttrValue],
        tokens: &S,
        position: usize,
    ) -> bool {
        if attrs.is_empty() {
            return true;
        }
        let predicate_id = dense.predicate_id[dense.transition_base[state] + transition_index];
        predicates.matches_at_site::<SITE, S>(predicate_id, tokens, position)
    }

    /// Appends the epsilon closure of `states` to `closure`.
    ///
    /// Epsilon transitions preserve each seed's start token and position.
    /// Assertions use the token position supplied for the call. Within each
    /// group of seeds sharing a start token and position, the traversal visits
    /// each state ID once.
    pub(crate) fn epsilon_closure_into<
        S: TokenSequence + ?Sized,
        I: IntoIterator<Item = StateInfo>,
        F: FnMut(&CompiledPositionAssertion, usize) -> bool,
    >(
        dense: &DenseMachine,
        states: I,
        tokens: Option<(&S, usize)>,
        scratch: &mut ExecScratch,
        assertion_holds: &mut F,
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
                scratch.mark_visited(seed.state);
            }

            while cursor < closure.len() {
                let state_info = closure[cursor];
                cursor += 1;
                for (transition_index, (label, next_state)) in
                    dense.transitions(state_info.state).iter().enumerate()
                {
                    let assertion_matches = match label {
                        TransitionLabel::Epsilon => true,
                        TransitionLabel::Consume(_) => false,
                        TransitionLabel::Assert(_) => tokens.is_some_and(|(_, position)| {
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

    fn create_epsilon_state(current: &StateInfo, next_state: usize) -> StateInfo {
        StateInfo {
            state: next_state,
            start_token: current.start_token,
            position: current.position,
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use super::Matcher;
    use crate::fsm::types::Match;

    #[test]
    fn duplicate_final_state_ids_resolve_to_the_lexicographically_first_pattern() {
        use crate::fsm::types::{AttrValue, AttrValueType, FinalState, TokenSpec, TransitionLabel};
        use crate::token_match::{TokenAttributeIndex, TokenInfo};
        use std::collections::{HashMap, HashSet};

        let spec = TokenSpec {
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
                interpretations: Box::default(),
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
            interpretations: Box::default(),
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
                super::super::emit::filter_and_collect_matches(
                    &input,
                    &super::OutputBudget::default()
                )
                .unwrap(),
                quadratic_reference(input),
                "subset {subset:#x}"
            );
        }
    }
}
