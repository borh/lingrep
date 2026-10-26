//! Compile-time spawn table and its per-position injection routine.
//!
//! See [`SpawnTable`] for the closure-prefiltering rationale and
//! [`inject_spawns`] for how the executor uses it at each token position.

use super::dense::DenseMachine;
use super::exec::{ExecScratch, Matcher};
use super::predicate::{
    CompiledPositionAssertion, PredicateCatalog, PredicateId, PredicateSession,
    SpawnPredicateDescriptor,
};
#[cfg(feature = "bench-instrumentation")]
use super::spawn_candidate_metrics::{
    SpawnCandidateCatalogMetrics, SpawnPredicateShapeMetric, SpawnSafeAtomMetric,
};
use super::spawn_dispatch::SpawnDispatch;
use crate::fsm::types::*;
use crate::token_match::{SequenceToken, TokenSequence};
use std::collections::HashMap;
#[cfg(feature = "bench-instrumentation")]
use std::collections::{BTreeMap, BTreeSet};

/// Compile-time image of the state-0 epsilon/assertion closure.
///
/// The executor injects a fresh match attempt at every token position. Doing
/// that by re-walking the epsilon closure of state 0 made injection cost
/// O(closure size) per position, with most injected states failing their first
/// predicate. This table computes that closure once: per position the
/// executor evaluates the (deduplicated) assertion conditions and each entry
/// state's consuming predicates, and materializes only the entry states that
/// can actually make progress. States on the closure with no consuming
/// transition are never materialized; they cannot affect any observable
/// output (they emit nothing and produce no successors).
#[derive(Clone, Default)]
pub(crate) struct SpawnTable {
    /// Deduplicated assertions guarding pre-consume paths (`^`,
    /// `preceded_by(...)`); indexes are bit positions in a `u64` mask.
    conditions: Vec<CompiledPositionAssertion>,
    /// Closure states owning at least one consuming transition, in
    /// deterministic (state id) order.
    entries: Vec<SpawnEntry>,
    /// Consuming entry states grouped by their deduplicated token predicate.
    /// One evaluation can therefore admit every condition-ready entry that
    /// shares the predicate.
    predicate_groups: Vec<SpawnPredicateGroup>,
    predicate_entry_indexes: Vec<usize>,
    #[cfg(feature = "bench-instrumentation")]
    shape_count: usize,
    /// Union of every entry's alternative masks; conditions outside it can
    /// never gate a spawn and are not evaluated at runtime.
    referenced_conditions: u64,
    /// Construction exceeded its bounds. The executor then uses per-position
    /// epsilon closure, preserving matching semantics at higher cost; the
    /// fallback is logged when the table is built.
    fallback: bool,
    /// Compile-time candidate index. `None` for a declined table or one with
    /// fewer than two groups, which uses the per-group scan.
    pub(crate) dispatch: Option<SpawnDispatch>,
    pub(crate) decline: Option<SpawnDecline>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnDecline {
    pub(crate) budget: &'static str,
    pub(crate) requested: usize,
    pub(crate) maximum: usize,
}

#[derive(Clone)]
struct SpawnEntry {
    state: usize,
    /// Alternative condition sets as bitmasks over `conditions`; the entry
    /// is reachable at a position when any alternative's conditions all
    /// hold. An empty mask means an unconditional epsilon path exists.
    condition_alts: Vec<u64>,
}

struct SpawnBuildEntry {
    state: usize,
    condition_alts: Vec<u64>,
    consume_transitions: Vec<usize>,
}

#[derive(Clone)]
struct SpawnPredicateGroup {
    predicate: PredicateId,
    entry_start: usize,
    entry_end: usize,
    /// Finite-positive classification of `predicate`, which the compile-time
    /// dispatcher indexes and the gated census reports.
    descriptor: SpawnPredicateDescriptor,
    #[cfg(feature = "bench-instrumentation")]
    shape_id: usize,
}

/// Bit width of a condition-set mask; more distinct pre-consume assertions
/// than this forces the fallback.
const MAX_SPAWN_CONDITIONS: usize = u64::BITS as usize;
/// Bound on explored (state, condition-set) pairs during table construction.
const MAX_SPAWN_SETS: usize = 4096;

impl SpawnTable {
    /// A declined table uses per-position epsilon closure. Differential tests
    /// treat that path as the reference semantics.
    fn declined(decline: SpawnDecline) -> Self {
        Self {
            conditions: Vec::new(),
            entries: Vec::new(),
            predicate_groups: Vec::new(),
            predicate_entry_indexes: Vec::new(),
            #[cfg(feature = "bench-instrumentation")]
            shape_count: 0,
            referenced_conditions: 0,
            fallback: true,
            dispatch: None,
            decline: Some(decline),
        }
    }

    pub(crate) fn build(
        transitions: &[Vec<(TransitionLabel, usize)>],
        predicates: &mut PredicateCatalog,
    ) -> Self {
        let fallback = |decline| {
            log::debug!(
                "spawn table construction declined; matcher falls back to \
                 per-position closure injection"
            );
            Self::declined(decline)
        };
        if transitions.is_empty() {
            return Self::default();
        }

        let mut conditions: Vec<CompiledPositionAssertion> = Vec::new();
        // Antichain of minimal condition masks per reached state.
        let mut reached: HashMap<usize, Vec<u64>> = HashMap::new();
        let mut work: Vec<(usize, u64)> = vec![(0, 0)];
        let mut explored_sets = 0usize;

        while let Some((state, mask)) = work.pop() {
            let alts = reached.entry(state).or_default();
            if alts.iter().any(|&known| known & !mask == 0) {
                // A subset of these conditions already reaches this state.
                continue;
            }
            alts.retain(|&known| mask & !known != 0);
            alts.push(mask);
            explored_sets += 1;
            if explored_sets > MAX_SPAWN_SETS {
                return fallback(SpawnDecline {
                    budget: "spawn condition sets",
                    requested: explored_sets,
                    maximum: MAX_SPAWN_SETS,
                });
            }
            for (label, next) in transitions.get(state).map_or(&[][..], Vec::as_slice) {
                match label.kind() {
                    TransitionKind::Consume => {}
                    TransitionKind::Epsilon => work.push((*next, mask)),
                    TransitionKind::Assert(assertion) => {
                        let assertion = predicates.intern_position_assertion(assertion);
                        let id = conditions
                            .iter()
                            .position(|known| known == &assertion)
                            .unwrap_or_else(|| {
                                conditions.push(assertion);
                                conditions.len() - 1
                            });
                        if id >= MAX_SPAWN_CONDITIONS {
                            return fallback(SpawnDecline {
                                budget: "spawn conditions",
                                requested: id + 1,
                                maximum: MAX_SPAWN_CONDITIONS,
                            });
                        }
                        work.push((*next, mask | (1 << id)));
                    }
                }
            }
        }

        let mut build_entries: Vec<SpawnBuildEntry> = reached
            .into_iter()
            .filter_map(|(state, condition_alts)| {
                let consume_transitions: Vec<usize> = transitions
                    .get(state)
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .enumerate()
                    .filter(|(_, (label, _))| matches!(label.kind(), TransitionKind::Consume))
                    .map(|(index, _)| index)
                    .collect();
                (!consume_transitions.is_empty()).then_some(SpawnBuildEntry {
                    state,
                    condition_alts,
                    consume_transitions,
                })
            })
            .collect();
        build_entries.sort_by_key(|entry| entry.state);
        let mut grouped_entries: Vec<(PredicateId, Vec<usize>)> = Vec::new();
        for (entry_index, entry) in build_entries.iter().enumerate() {
            for &transition_index in &entry.consume_transitions {
                let (label, _) = &transitions[entry.state][transition_index];
                let predicate = predicates.intern(&label.attrs);
                let group_index = grouped_entries
                    .iter()
                    .position(|(known, _)| *known == predicate)
                    .unwrap_or_else(|| {
                        grouped_entries.push((predicate, Vec::new()));
                        grouped_entries.len() - 1
                    });
                let entries = &mut grouped_entries[group_index].1;
                if entries.last() != Some(&entry_index) {
                    entries.push(entry_index);
                }
            }
        }
        let mut predicate_entry_indexes = Vec::new();
        let predicate_groups = grouped_entries
            .into_iter()
            .map(|(predicate, entries)| {
                let entry_start = predicate_entry_indexes.len();
                predicate_entry_indexes.extend(entries);
                SpawnPredicateGroup {
                    predicate,
                    entry_start,
                    entry_end: predicate_entry_indexes.len(),
                    descriptor: predicates.spawn_candidate_descriptor(predicate),
                    #[cfg(feature = "bench-instrumentation")]
                    shape_id: 0,
                }
            })
            .collect::<Vec<_>>();
        #[cfg(feature = "bench-instrumentation")]
        let (predicate_groups, shape_count) = {
            let mut predicate_groups = predicate_groups;
            let shape_ids = predicate_groups
                .iter()
                .map(|group| group.descriptor.shape_signature.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .enumerate()
                .map(|(id, signature)| (signature, id))
                .collect::<BTreeMap<_, _>>();
            for group in &mut predicate_groups {
                group.shape_id = shape_ids[&group.descriptor.shape_signature];
            }
            (predicate_groups, shape_ids.len())
        };
        let entries = build_entries
            .into_iter()
            .map(|entry| SpawnEntry {
                state: entry.state,
                condition_alts: entry.condition_alts,
            })
            .collect::<Vec<_>>();
        let referenced_conditions = entries
            .iter()
            .flat_map(|entry| entry.condition_alts.iter())
            .fold(0u64, |union, &alternative| union | alternative);

        let dispatch = SpawnDispatch::build(
            &predicate_groups
                .iter()
                .map(|group| group.descriptor.clone())
                .collect::<Vec<_>>(),
        );

        Self {
            conditions,
            entries,
            predicate_groups,
            predicate_entry_indexes,
            #[cfg(feature = "bench-instrumentation")]
            shape_count,
            referenced_conditions,
            fallback: false,
            dispatch,
            decline: None,
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn spawn_candidate_catalog_metrics(&self) -> SpawnCandidateCatalogMetrics {
        if self.fallback {
            return SpawnCandidateCatalogMetrics::default();
        }
        let mut metrics = SpawnCandidateCatalogMetrics {
            spawn_groups: self.predicate_groups.len(),
            ..SpawnCandidateCatalogMetrics::default()
        };
        let mut scheme_a_attributes = BTreeSet::new();
        let mut scheme_b_attributes = BTreeSet::new();
        let mut safe_atoms = BTreeMap::new();
        let mut shapes = BTreeMap::new();
        for group in &self.predicate_groups {
            match group.descriptor.finite_atoms.len() {
                0 => metrics.zero_safe_groups += 1,
                1 => metrics.one_safe_groups += 1,
                _ => metrics.multiple_safe_groups += 1,
            }
            if let Some(atom) = &group.descriptor.scheme_a {
                scheme_a_attributes.insert(atom.attribute.key().clone());
            } else {
                metrics.scheme_a_undiscriminated_groups += 1;
            }
            if group.descriptor.finite_atoms.is_empty() {
                metrics.scheme_b_undiscriminated_groups += 1;
            }
            for atom in &group.descriptor.finite_atoms {
                scheme_b_attributes.insert(atom.attribute.key().clone());
                *safe_atoms
                    .entry((atom.attribute.key().clone(), atom.operator))
                    .or_insert(0) += 1;
            }
            let shape = shapes
                .entry(group.shape_id)
                .or_insert_with(|| (group.descriptor.shape_signature.clone(), 0));
            shape.1 += 1;
        }
        metrics.scheme_a_indexed_attributes = scheme_a_attributes.len();
        metrics.scheme_b_indexed_attributes = scheme_b_attributes.len();
        metrics.safe_atoms = safe_atoms
            .into_iter()
            .map(|((attribute, operator), atoms)| SpawnSafeAtomMetric {
                attribute,
                operator,
                atoms,
            })
            .collect();
        metrics.shapes = shapes
            .into_iter()
            .map(|(id, (signature, groups))| SpawnPredicateShapeMetric {
                id,
                signature,
                groups,
            })
            .collect();
        metrics
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn spawn_candidate_dimensions(&self) -> (usize, usize) {
        (self.predicate_groups.len(), self.shape_count)
    }

    /// Each group's predicate and its finite-positive classification, in
    /// table order.
    #[cfg(test)]
    pub(crate) fn groups_for_test(&self) -> Vec<(PredicateId, SpawnPredicateDescriptor)> {
        self.predicate_groups
            .iter()
            .map(|group| (group.predicate, group.descriptor.clone()))
            .collect()
    }
}

/// Admit one spawn group at `index`: check readiness, evaluate the group's
/// predicate once, and materialize every entry whose conditions hold.
///
/// Shared by the dispatched and undispatched paths so the two cannot drift.
///
/// `inline(always)` rather than `inline`: extracting this body from
/// `inject_spawns` regressed the one-group path despite identical work; only
/// code layout changed. Sharing one body prevents path drift, and forcing the
/// inline keeps the shared helper out of the hot call path.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn inject_group<S: TokenSequence + ?Sized>(
    table: &SpawnTable,
    predicates: &PredicateSession<'_>,
    tokens: &S,
    index: usize,
    frontier: &mut Vec<StateInfo>,
    holds: u64,
    spawn: StateInfo,
    group: &SpawnPredicateGroup,
) {
    let group_entries = &table.predicate_entry_indexes[group.entry_start..group.entry_end];
    if !group_entries.iter().any(|&entry| {
        table.entries[entry]
            .condition_alts
            .iter()
            .any(|&alternative| alternative & !holds == 0)
    }) {
        return;
    }
    #[cfg(feature = "bench-instrumentation")]
    {
        let token =
            SequenceToken::new(tokens, index).expect("spawn index is inside the token sequence");
        predicates.record_spawn_candidate(&group.descriptor, group.shape_id, &token);
    }
    if predicates.matches_at_spawn(group.predicate, tokens, index) {
        for &entry in group_entries {
            let entry = &table.entries[entry];
            if entry
                .condition_alts
                .iter()
                .any(|&alternative| alternative & !holds == 0)
            {
                // Pushed without a membership test. Every spawn entry carries
                // `start_token == index`, while a carried frontier entry always
                // carries `start_token <= index - 1` --- `handle_state_transition`
                // sets `start_token = token_index` only out of state 0 --- so a
                // spawn can never duplicate a carried state. Two entries sharing
                // one `state` can duplicate each other, which is bounded by the
                // table's entry count and invisible in output: successors land in
                // a deduplicating `next_states` and matches in a `MatchSink` keyed
                // on `(pattern, start, end)`.
                frontier.push(StateInfo {
                    state: entry.state,
                    ..spawn
                });
            }
        }
    }
}

/// Inject the match attempts starting at `index` into the frontier.
///
/// Semantically this is the epsilon closure of state 0 validated at
/// `index`. The spawn table
/// answers the same question with one predicate evaluation per distinct
/// entry predicate: an entry state is materialized only when it is
/// reachable (its assertion conditions hold) and at least one of its
/// consuming transitions accepts the current token. Closure states left
/// out by the prefilter would emit nothing and produce no successors,
/// so the frontier difference is unobservable.
pub(crate) fn inject_spawns<S: TokenSequence + ?Sized>(
    dense: &DenseMachine,
    predicates: &PredicateSession<'_>,
    tokens: &S,
    index: usize,
    frontier: &mut Vec<StateInfo>,
    scratch: &mut ExecScratch,
    assertion_holds: &mut dyn FnMut(&CompiledPositionAssertion, usize) -> bool,
) {
    let spawn = StateInfo {
        state: 0,
        start_token: index,
        position: index,
        entry_position: index,
    };
    if dense.spawn_table.fallback {
        // Full epsilon closure for machines the bounded table construction
        // declined.
        Matcher::epsilon_closure_into(
            dense,
            std::iter::once(spawn),
            Some((tokens, index)),
            scratch,
            assertion_holds,
            frontier,
        );
        return;
    }

    let table = &dense.spawn_table;
    if table.entries.is_empty() {
        return;
    }
    #[cfg(feature = "bench-instrumentation")]
    predicates.begin_spawn_position();
    let mut holds = 0u64;
    for (bit, condition) in table.conditions.iter().enumerate() {
        if table.referenced_conditions & (1 << bit) == 0 {
            // No entry's alternatives mention this condition; its paths
            // all died in consume-less states.
            continue;
        }
        if assertion_holds(condition, index) {
            holds |= 1 << bit;
        }
    }

    // The census measures the pre-dispatch call population; see
    // `PredicateSession::measuring_spawn_candidates`.
    #[cfg(feature = "bench-instrumentation")]
    let dispatch = table
        .dispatch
        .as_ref()
        .filter(|_| !predicates.measuring_spawn_candidates());
    #[cfg(not(feature = "bench-instrumentation"))]
    let dispatch = table.dispatch.as_ref();

    match dispatch {
        // No index: every group is a candidate, exactly as before.
        None => {
            for group in &table.predicate_groups {
                inject_group(
                    table, predicates, tokens, index, frontier, holds, spawn, group,
                );
            }
        }
        // Indexed: consider only the groups whose canonical finite-positive
        // atom accepts this token. Ascending bit order is the table's own
        // group order, so the frontier sees the identical insertion sequence.
        Some(dispatch) => {
            let words = dispatch.words();
            let candidates = scratch.spawn_candidates(words);
            let token = SequenceToken::new(tokens, index)
                .expect("spawn index is inside the token sequence");
            dispatch.candidates(&token, candidates);
            for (word, mut bits) in candidates.iter().copied().enumerate() {
                while bits != 0 {
                    let group = word * 64 + bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    inject_group(
                        table,
                        predicates,
                        tokens,
                        index,
                        frontier,
                        holds,
                        spawn,
                        &table.predicate_groups[group],
                    );
                }
            }
        }
    }
    #[cfg(feature = "bench-instrumentation")]
    predicates.finish_spawn_position();
}

#[cfg(test)]
mod spawn_table_tests {
    use super::{CompiledPositionAssertion, DenseMachine, Matcher, SpawnDecline, SpawnTable};
    use crate::fsm::matcher::predicate::{PredicateCatalog, PredicateSession};
    use crate::fsm::types::{PositionAssertion, TransitionLabel};
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{TokenAttributeIndex, TokenInfo};
    use std::collections::{BTreeMap, HashMap};

    /// Differential patterns: anchors, previous-token assertions, nullable
    /// prefixes, quantifiers, alternation, set membership, and regex — the
    /// constructs whose spawn behavior the table has to reproduce.
    const DIFFERENTIAL_PATTERN: &str = "\
fragment ARU := [text in {ある, 有る}].
anchored := ^ [unidic.pos1=NOUN] [unidic.pos1=VERB]? $.
prev := preceded_by([unidic.pos1=ADV]) [unidic.pos1=NOUN].
sou := [text=そう] @ARU.
run := [unidic.pos1=NOUN]+ [unidic.pos1=VERB].
alt := ([text=a] | [text=b]) [text=c]+.
zero := [unidic.pos1=ADJ]? [unidic.pos1=NOUN] [unidic.pos1=VERB].
notin := [text not in {x, y}] [text=z].
re := [text~/^a+b$/] [text=c].
tail := [unidic.pos1=VERB] $.
";

    fn dense_for(source: &str) -> (DenseMachine, PredicateCatalog) {
        let compiled = compile_catalog(
            PatternSource::named("spawn-table.lx", source),
            CompileLimits::default(),
        )
        .unwrap();
        assert!(
            compiled.diagnostics.is_empty(),
            "{:?}",
            compiled.diagnostics
        );
        (
            compiled.matcher.dense_for_test().clone(),
            compiled.matcher.predicates_for_test().clone(),
        )
    }

    fn derived_unconditional_group_count(table: &SpawnTable) -> usize {
        table
            .predicate_groups
            .iter()
            .filter(|group| {
                table.predicate_entry_indexes[group.entry_start..group.entry_end]
                    .iter()
                    .all(|&entry| table.entries[entry].condition_alts.as_slice() == [0])
            })
            .count()
    }

    #[test]
    fn tracked_catalog_spawn_group_classification() {
        let mut expected = BTreeMap::from([
            ("patterns/alternation.lx", (0x0, 8, 8)),
            // The two れる/られる patterns share one lookaround body at
            // opposite polarity, so the catalog references two assertion
            // conditions and the third spawn group is the conditional one.
            ("patterns/collexeme.lx", (0x3, 3, 2)),
            ("patterns/concord.lx", (0x0, 6, 6)),
            ("patterns/counting.lx", (0x0, 1, 1)),
            // Both any-token definitions share one unconditional spawn group.
            ("patterns/distributions.lx", (0x0, 1, 1)),
            // Mixed catalog: the nine relation-free definitions classify into
            // spawn groups, the four `where` ones are tree-evaluated. Two
            // assertion conditions, both `dash_aside`'s, which asks for a
            // non-punctuation neighbour on each side; that is also why one
            // group fewer is unconditional than there are relation-free
            // definitions minus the shared ones.
            ("patterns/comment-register.lx", (0x3, 9, 5)),
            // Every pattern carries a `where` clause: whole-catalog tree
            // evaluation, the ud-dependency precedent.
            ("patterns/double-wo-dep.lx", (0x0, 0, 0)),
            ("patterns/double-wo.lx", (0x1f, 1, 0)),
            ("patterns/ngrams.lx", (0x0, 1, 1)),
            ("patterns/npv-collocation.lx", (0x0, 0, 0)),
            ("patterns/passive-agent-dep.lx", (0x0, 0, 0)),
            ("patterns/passive-oblique-realization.lx", (0x0, 0, 0)),
            ("patterns/portable-content-words.lx", (0x0, 2, 2)),
            // Both patterns carry `same_as`, so the whole catalog is
            // recognized by the tree evaluator and the dense machine spawns
            // nothing.
            ("patterns/reduplication.lx", (0x0, 0, 0)),
            ("patterns/sentence-final.lx", (0x1, 28, 27)),
            ("patterns/sf-experimental.lx", (0x1, 43, 42)),
            ("patterns/sfe.lx", (0x0, 23, 23)),
            ("patterns/sketch-grammar.lx", (0x0, 7, 7)),
            ("patterns/test-simple.lx", (0x0, 1, 1)),
            // Every pattern carries a `where` clause, so the whole catalog is
            // recognized by the tree evaluator — the reduplication precedent
            // with `Relations` in place of `SpanEquality`.
            ("patterns/ud-dependency.lx", (0x0, 0, 0)),
            ("patterns/ud-enhanced-argument.lx", (0x0, 0, 0)),
            ("patterns/ud-enhanced-consistency.lx", (0x0, 0, 0)),
            ("patterns/ud-enhanced-propagation.lx", (0x0, 0, 0)),
            ("patterns/ud-nonprojectivity.lx", (0x0, 0, 0)),
            ("patterns/ud-object-realization.lx", (0x0, 0, 0)),
            ("patterns/ud-subtree-profile.lx", (0x0, 0, 0)),
            ("patterns/ud-valency.lx", (0x0, 0, 0)),
            ("patterns/wo-predicate-dep.lx", (0x0, 1, 1)),
        ]);
        #[cfg(feature = "bench-instrumentation")]
        let mut expected_candidates = BTreeMap::from([
            // Scheme A indexes two attributes here and on sketch-grammar,
            // not one, since `cad29b50` gave a safe atom an `AttributeKey` in
            // place of a `TokenAttributeIndex`: `finite_atoms` sorts by key
            // string rather than by UniDic column ordinal, and `scheme_a` is
            // `finite_atoms.first()`. It moves a group only where the two
            // orders disagree, which is why eleven catalogs did not move.
            // `al_quotative_verb` is this catalog's one such group —
            // `[unidic.pos1=動詞 & lemma in {言う, …}]` sorted `Pos1, Lemma`
            // by column and sorts `lemma, unidic.pos1` by key — so its
            // discriminating atom moved to `lemma` while the other seven
            // groups kept `unidic.pos1`. Prefilter selection only; no
            // catalog's text changed.
            ("patterns/alternation.lx", (8, 0, 6, 2, 2, 3)),
            ("patterns/collexeme.lx", (3, 0, 3, 0, 1, 1)),
            ("patterns/concord.lx", (6, 0, 6, 0, 1, 1)),
            ("patterns/counting.lx", (1, 0, 0, 1, 1, 3)),
            ("patterns/distributions.lx", (1, 1, 0, 0, 0, 0)),
            ("patterns/double-wo-dep.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/double-wo.lx", (1, 0, 0, 1, 1, 3)),
            ("patterns/ngrams.lx", (1, 1, 0, 0, 0, 0)),
            ("patterns/npv-collocation.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/passive-agent-dep.lx", (0, 0, 0, 0, 0, 0)),
            (
                "patterns/passive-oblique-realization.lx",
                (0, 0, 0, 0, 0, 0),
            ),
            ("patterns/portable-content-words.lx", (2, 0, 2, 0, 1, 1)),
            ("patterns/sfe.lx", (23, 1, 19, 3, 7, 7)),
            ("patterns/reduplication.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/sentence-final.lx", (28, 1, 25, 2, 3, 5)),
            ("patterns/sf-experimental.lx", (43, 0, 38, 5, 4, 6)),
            // The second catalog the key-order sort moved, same cause as
            // alternation above: `[unidic.pos1 & unidic.cform]` sorted
            // `Pos1, CForm` by column — UniDic puts pos1 before cForm — and
            // sorts `unidic.cform, unidic.pos1` by key, so that group's
            // discriminating atom moved to `unidic.cform`.
            ("patterns/sketch-grammar.lx", (7, 0, 5, 2, 2, 3)),
            ("patterns/test-simple.lx", (1, 0, 1, 0, 1, 1)),
            // Counted candidate work for the mixed catalog: only the nine
            // relation-free definitions reach the spawn table at all.
            ("patterns/comment-register.lx", (9, 1, 6, 2, 3, 3)),
            ("patterns/ud-dependency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-argument.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-consistency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-propagation.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-nonprojectivity.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-object-realization.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-subtree-profile.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-valency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/wo-predicate-dep.lx", (1, 1, 0, 0, 0, 0)),
        ]);
        // Every moved catalog is reported, rather than the first stopping the
        // run: these tuples move together when an attribute-identity or
        // classification change lands, and repinning them one rerun at a time
        // hides how wide the move was. The counted-work pin in
        // `lingrep_perf::work` collects the same way, for the same reason.
        let mut moved: Vec<String> = Vec::new();
        for (name, source) in crate::testing::catalogs::tracked_catalog_sources()
            .into_iter()
            .filter(|(name, _)| name.starts_with("patterns/"))
        {
            let (dense, _) = dense_for(&source);
            let table = &dense.spawn_table;
            let actual = (
                table.referenced_conditions,
                table.predicate_groups.len(),
                derived_unconditional_group_count(table),
            );
            let pinned = expected.remove(name.as_str());
            if pinned != Some(actual) {
                moved.push(format!("  {name}: pinned {pinned:?}, measured {actual:?}"));
            }
            #[cfg(feature = "bench-instrumentation")]
            {
                let metrics = table.spawn_candidate_catalog_metrics();
                let actual_candidates = (
                    metrics.spawn_groups,
                    metrics.zero_safe_groups,
                    metrics.one_safe_groups,
                    metrics.multiple_safe_groups,
                    metrics.scheme_a_indexed_attributes,
                    metrics.scheme_b_indexed_attributes,
                );
                let pinned_candidates = expected_candidates.remove(name.as_str());
                if pinned_candidates != Some(actual_candidates) {
                    moved.push(format!(
                        "  {name} candidates: pinned {pinned_candidates:?}, measured \
                         {actual_candidates:?}"
                    ));
                }
                assert_eq!(
                    metrics.scheme_a_undiscriminated_groups, metrics.zero_safe_groups,
                    "{name} Scheme A"
                );
                assert_eq!(
                    metrics.scheme_b_undiscriminated_groups, metrics.zero_safe_groups,
                    "{name} Scheme B"
                );
                assert_eq!(
                    metrics
                        .shapes
                        .iter()
                        .map(|shape| shape.groups)
                        .sum::<usize>(),
                    metrics.spawn_groups,
                    "{name} shape total"
                );
                assert_eq!(
                    metrics
                        .shapes
                        .iter()
                        .map(|shape| shape.id)
                        .collect::<Vec<_>>(),
                    (0..metrics.shapes.len()).collect::<Vec<_>>(),
                    "{name} dense shape ids"
                );
            }
        }
        assert!(
            moved.is_empty(),
            "tracked catalog spawn classification moved:\n{}",
            moved.join("\n")
        );
        assert!(
            expected.is_empty(),
            "missing tracked catalogs: {expected:?}"
        );
        #[cfg(feature = "bench-instrumentation")]
        assert!(
            expected_candidates.is_empty(),
            "missing tracked candidate catalogs: {expected_candidates:?}"
        );
    }

    fn with_forced_fallback(dense: &DenseMachine) -> DenseMachine {
        let mut fallback = dense.clone();
        fallback.spawn_table = SpawnTable::declined(SpawnDecline {
            budget: "test",
            requested: 1,
            maximum: 0,
        });
        fallback
    }

    fn token(orth: &str, pos1: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token.set(TokenAttributeIndex::Pos1, pos1.into());
        token
    }

    fn mixed_catalog_pattern(assertion_definitions: usize) -> String {
        let mut source = String::from("pattern plain := [text=token].\n");
        for index in 0..assertion_definitions {
            source.push_str(&format!(
                "pattern probe{index} := preceded_by([text=prev{index}]) [text=token].\n"
            ));
        }
        source
    }

    fn sequential_assertion_antichain_pattern(levels: usize) -> String {
        let mut source = String::from("pattern antichain := ");
        for level in 0..levels {
            source.push_str(&format!(
                "(preceded_by([text=left{level}]) | preceded_by([text=right{level}])) "
            ));
        }
        source.push_str("[text=token].\n");
        source
    }

    /// The spawn table must be observationally identical to per-position
    /// epsilon-closure injection on
    /// every stream, complete `Match` values included.
    #[test]
    fn spawn_table_is_equivalent_to_closure_injection() {
        let (dense, predicates) = dense_for(DIFFERENTIAL_PATTERN);
        assert!(!dense.spawn_table.fallback);
        let table = &dense.spawn_table;
        let unconditional = derived_unconditional_group_count(table);
        assert!(unconditional > 0, "fixture needs an unconditional group");
        assert!(
            unconditional < table.predicate_groups.len(),
            "fixture needs a conditional group"
        );
        let fallback = with_forced_fallback(&dense);

        let orths = ["a", "b", "c", "z", "x", "ab", "aab", "そう", "ある", "y"];
        let tags = ["NOUN", "VERB", "ADV", "ADJ", "PART"];
        let mut seed = 0x9e37_79b9_u64;
        let mut random = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };

        let mut total_matches = 0usize;
        for _ in 0..400 {
            let len = random() % 17;
            let tokens: Vec<TokenInfo> = (0..len)
                .map(|_| token(orths[random() % orths.len()], tags[random() % tags.len()]))
                .collect();
            let expected_session = PredicateSession::new_unmemoized(&predicates);
            let expected = Matcher::find_matches_dense(&fallback, &expected_session, &tokens);
            drop(expected_session);
            let actual_session = PredicateSession::new_unmemoized(&predicates);
            let actual = Matcher::find_matches_dense(&dense, &actual_session, &tokens);
            assert_eq!(actual, expected, "tokens: {tokens:?}");
            total_matches += actual.len();
        }
        assert!(total_matches > 100, "vacuous differential: {total_matches}");
    }

    /// The trace observable: the merged pass changed `Trace.entries` from
    /// restart-major to position-major order, but the emitted entry multiset
    /// and `Trace.matches` must still equal the fallback path's.
    #[test]
    fn spawn_table_trace_matches_fallback_entry_multiset() {
        let (dense, predicates) = dense_for(DIFFERENTIAL_PATTERN);
        let fallback = with_forced_fallback(&dense);

        let orths = ["a", "b", "c", "z", "そう", "ある"];
        let tags = ["NOUN", "VERB", "ADV", "ADJ"];
        let mut seed = 0x51ed_2701_u64;
        let mut random = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };

        let mut total_entries = 0usize;
        for _ in 0..100 {
            let len = random() % 12;
            let tokens: Vec<TokenInfo> = (0..len)
                .map(|_| token(orths[random() % orths.len()], tags[random() % tags.len()]))
                .collect();
            let expected_session = PredicateSession::new_unmemoized(&predicates);
            let expected =
                Matcher::find_matches_with_trace_dense(&fallback, &expected_session, &tokens);
            drop(expected_session);
            let actual_session = PredicateSession::new_unmemoized(&predicates);
            let actual = Matcher::find_matches_with_trace_dense(&dense, &actual_session, &tokens);
            assert_eq!(actual.matches, expected.matches, "tokens: {tokens:?}");
            let sort = |mut entries: Vec<crate::fsm::types::TraceEntry>| {
                entries.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
                entries
            };
            let actual_entries = sort(actual.entries);
            assert_eq!(actual_entries, sort(expected.entries), "tokens: {tokens:?}");
            total_entries += actual_entries.len();
        }
        assert!(total_entries > 100, "vacuous trace differential");
    }

    /// `^`-anchored entries carry a SentenceStart condition: they spawn at
    /// position 0 and nowhere else, and `preceded_by` entries spawn only
    /// where the previous token satisfies the predicate.
    #[test]
    fn spawn_table_records_assertion_conditions() {
        let (dense, _) = dense_for(DIFFERENTIAL_PATTERN);
        let table = &dense.spawn_table;
        assert!(!table.fallback);
        assert!(
            table
                .conditions
                .contains(&CompiledPositionAssertion::SentenceStart),
            "expected a SentenceStart condition: {:?}",
            table.conditions
        );
        assert!(
            table
                .conditions
                .iter()
                .any(|condition| matches!(condition, CompiledPositionAssertion::PreviousToken(_))),
            "expected a PreviousToken condition: {:?}",
            table.conditions
        );
        // Entries are deterministic and every entry owns a consuming
        // transition; pure pass-through closure states are excluded.
        assert!(
            table
                .entries
                .windows(2)
                .all(|pair| pair[0].state < pair[1].state)
        );
        assert!(
            table
                .entries
                .iter()
                .enumerate()
                .all(|(entry, _)| table.predicate_entry_indexes.contains(&entry))
        );
    }

    /// More distinct pre-consume assertions than a condition mask can hold
    /// forces the fallback, which must still match correctly.
    #[test]
    fn oversized_condition_sets_fall_back_to_closure_injection() {
        let mut transitions: HashMap<usize, Vec<(TransitionLabel, usize)>> = HashMap::new();
        let mut entries = Vec::new();
        for index in 0..=super::MAX_SPAWN_CONDITIONS {
            let assertion =
                PositionAssertion::PreviousToken(Box::from([crate::fsm::types::AttrValue {
                    attr: TokenAttributeIndex::Orth.into(),
                    value: crate::fsm::types::AttrValueType::Single(format!("prev-{index}").into()),
                    complement: false,
                }]));
            entries.push((TransitionLabel::assertion(assertion), index + 1));
        }
        transitions.insert(0, entries);
        let mut predicates = PredicateCatalog::default();
        let table = SpawnTable::build(
            &(0..=super::MAX_SPAWN_CONDITIONS + 1)
                .map(|state| transitions.remove(&state).unwrap_or_default())
                .collect::<Vec<_>>(),
            &mut predicates,
        );
        assert!(table.fallback);
    }

    #[test]
    fn mixed_catalogs_build_through_the_supported_condition_limit() {
        for count in [1, 8, 32, 63, 64] {
            let (dense, _) = dense_for(&mixed_catalog_pattern(count));
            let table = &dense.spawn_table;
            assert!(!table.fallback, "{count} assertion branches");
            assert_eq!(table.conditions.len(), count, "{count} assertion branches");
        }
    }

    #[test]
    fn sixty_five_source_shaped_conditions_cross_the_spawn_condition_limit() {
        let (dense, _) = dense_for(&mixed_catalog_pattern(65));
        let table = &dense.spawn_table;
        assert!(
            table.fallback,
            "the sixty-fifth condition exceeds the u64 mask"
        );
    }

    // The raw binary antichain reaches 2^(levels + 1) - 1 accepted
    // (state, mask) pairs: 4,095 at level 11 and 8,191 at level 12. The
    // compiler's epsilon bookkeeping adds states, but subset pruning keeps
    // those intermediates from moving the first declining level below 12.
    const OBSERVED_FIRST_DECLINING_LEVEL: usize = 12;

    #[test]
    fn authored_sequential_assertion_antichain_records_the_spawn_set_cliff() {
        let first_declining_level = (1..=12)
            .find(|level| {
                dense_for(&sequential_assertion_antichain_pattern(*level))
                    .0
                    .spawn_table
                    .fallback
            })
            .expect("twelve binary assertion levels must cross the set bound");
        assert_eq!(
            first_declining_level, OBSERVED_FIRST_DECLINING_LEVEL,
            "pin the first compiler-path level whose spawn table declines"
        );
    }
}
