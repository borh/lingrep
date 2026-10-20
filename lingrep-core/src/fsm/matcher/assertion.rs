use super::predicate::{
    CompiledPositionAssertion, PredicateCatalog, PredicateId, PredicateSession,
};
use crate::fsm::compiler::model::{LookDirection, LookPolarity, TokenPredicate};
use crate::fsm::{AssertionOccurrenceId, PositionAssertion};
#[cfg(test)]
use crate::token_match::TokenAttributeIndex;
use crate::token_match::{AttributeKey, TokenSequence};
#[cfg(feature = "bench-instrumentation")]
use std::cell::Cell;
use std::collections::BTreeSet;

/// Assertion-evaluator work observed by opt-in benchmark builds.
///
/// This type and all counter updates are absent unless the
/// `bench-instrumentation` feature is enabled.
#[cfg(feature = "bench-instrumentation")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssertionMetrics {
    pub row_queries: u64,
    pub row_materializations: u64,
    pub state_visits: u64,
    /// Runtime closure work units: inspected edges on the general worklist or
    /// reachable-state activations on the transitive epsilon-only path.
    ///
    /// `state_visits` samples how full the frontier is; this counter is the
    /// path-specific work performed after the frontier has been seeded.
    pub edge_inspections: u64,
    /// Largest number of simultaneously active states observed in an
    /// assertion frontier.
    pub max_frontier_states: u64,
}

#[cfg(feature = "bench-instrumentation")]
#[derive(Default)]
struct AssertionMetricCells {
    row_queries: Cell<u64>,
    row_materializations: Cell<u64>,
    state_visits: Cell<u64>,
    max_frontier_states: Cell<u64>,
    closure_calls: Cell<u64>,
    frontier_seed_slots_scanned: Cell<u64>,
    edge_inspections: Cell<u64>,
    epsilon_edge_inspections: Cell<u64>,
    boundary_edge_inspections: Cell<u64>,
    fact_edge_inspections: Cell<u64>,
    consume_edge_inspections: Cell<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct AssertionProgramId(u32);

impl AssertionProgramId {
    pub(crate) fn new(value: usize) -> Self {
        Self(u32::try_from(value).expect("assertion program count is compiler-bounded"))
    }

    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 assertion IDs fit usize")
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AssertionCatalog {
    pub(crate) programs: Box<[AssertionProgram]>,
    pub(crate) occurrences: Box<[AssertionOccurrence]>,
}

impl AssertionCatalog {
    pub(crate) fn compile(self, predicates: &mut PredicateCatalog) -> CompiledAssertionCatalog {
        let mut epsilon_reachable_entries =
            CompiledEpsilonClosureIndex::MAX_CATALOG_REACHABLE_ENTRIES;
        CompiledAssertionCatalog {
            programs: self
                .programs
                .into_vec()
                .into_iter()
                .map(|program| CompiledAssertionProgram {
                    direction: program.direction,
                    automaton: compile_automaton(
                        program.automaton,
                        predicates,
                        &mut epsilon_reachable_entries,
                    ),
                    dependencies: program.dependencies,
                    required_attributes: program.required_attributes,
                    union_members: program.union_members,
                })
                .collect(),
            occurrences: self.occurrences,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AssertionOccurrence {
    pub(crate) program: AssertionProgramId,
    pub(crate) polarity: LookPolarity,
}

#[derive(Clone, Debug)]
pub(crate) struct AssertionProgram {
    pub(crate) direction: LookDirection,
    pub(crate) automaton: AssertionAutomaton,
    pub(crate) dependencies: Box<[AssertionProgramId]>,
    pub(crate) required_attributes: Box<[AttributeKey]>,
    /// The programs this one is the union of, when it is one.
    ///
    /// Non-empty exactly for a program compiled from a top-level alternation.
    /// A row is a set of boundaries and alternation is union, so the answer is
    /// the bitwise `or` of the members' rows and no walk over the sentence can
    /// tell us anything the members have not already recorded. Empty means the
    /// automaton is swept, which is every other program.
    pub(crate) union_members: Box<[AssertionProgramId]>,
}

#[derive(Clone, Debug)]
pub(crate) struct AssertionAutomaton {
    pub(crate) states: usize,
    pub(crate) start: usize,
    pub(crate) accept: usize,
    pub(crate) edges: Box<[AssertionEdge]>,
}

#[derive(Clone, Debug)]
pub(crate) struct AssertionEdge {
    pub(crate) from: usize,
    pub(crate) to: usize,
    pub(crate) kind: AssertionEdgeKind,
}

#[derive(Clone, Debug)]
pub(crate) enum AssertionEdgeKind {
    Epsilon,
    Consume(TokenPredicate),
    Boundary(PositionAssertion),
    Fact(AssertionOccurrenceId),
}

#[derive(Clone, Debug)]
pub(crate) struct CompiledAssertionCatalog {
    programs: Box<[CompiledAssertionProgram]>,
    occurrences: Box<[AssertionOccurrence]>,
}

impl CompiledAssertionCatalog {
    pub(crate) fn empty() -> Self {
        Self {
            programs: Box::default(),
            occurrences: Box::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn program_count_for_test(&self) -> usize {
        self.programs.len()
    }

    /// One program's forward consume index as `(groups, edges)`.
    #[cfg(test)]
    fn consume_shape_for_test(&self, program: usize) -> (usize, usize) {
        let index = &self.programs[program].automaton.forward_consume;
        (index.groups.len(), index.edges.len())
    }

    pub(crate) fn required_attributes(&self) -> impl Iterator<Item = AttributeKey> + '_ {
        self.programs
            .iter()
            .flat_map(|program| program.required_attributes.iter().cloned())
    }
}

#[derive(Clone, Debug)]
struct CompiledAssertionProgram {
    direction: LookDirection,
    automaton: CompiledAssertionAutomaton,
    dependencies: Box<[AssertionProgramId]>,
    required_attributes: Box<[AttributeKey]>,
    union_members: Box<[AssertionProgramId]>,
}

#[derive(Clone, Debug)]
struct CompiledAssertionAutomaton {
    states: usize,
    start: usize,
    accept: usize,
    forward_consume: CompiledConsumeIndex,
    reverse_consume: CompiledConsumeIndex,
    forward_closure: CompiledClosureIndex,
    reverse_closure: CompiledClosureIndex,
}

#[derive(Clone, Debug)]
struct CompiledAssertionEdge {
    from: usize,
    to: usize,
    kind: CompiledAssertionEdgeKind,
}

#[derive(Clone, Debug)]
enum CompiledAssertionEdgeKind {
    Epsilon,
    Consume(PredicateId),
    Boundary(CompiledPositionAssertion),
    Fact(AssertionOccurrenceId),
}

/// One consume edge, as its predicate's group records it.
#[derive(Clone, Copy, Debug)]
struct CompiledConsumeEdge {
    from: u32,
    to: u32,
}

/// The consume edges sharing one predicate, as a contiguous run of
/// [`CompiledConsumeIndex::edges`].
#[derive(Clone, Copy, Debug)]
struct CompiledConsumeGroup {
    predicate: PredicateId,
    start: u32,
    end: u32,
}

/// Consume edges grouped by predicate, for one walk direction.
///
/// [`AssertionSession::consume`] runs at every token of every sentence, and a
/// flat edge list made it ask the predicate memo once per *edge* there. The
/// catalog interns predicates, so a lookaround body that repeats a token test —
/// which is what a bounded gap compiles to, and why `double-wo`'s largest
/// program carries sixty consume edges — repeats a `PredicateId` too. Grouping
/// asks once per distinct predicate instead.
///
/// `sources` is `groups.len() * words` and holds, per group, the set of states
/// its edges leave. A group whose sources are all dead at this boundary is
/// skipped on a word-wise intersection, so the predicate is never consulted:
/// the frequent case for a lexically specific test is that nothing is live to
/// use it.
///
/// Two indexes rather than one plus a swap, because a reverse pass walks these
/// edges backwards and deciding per edge which end is the source — the
/// `if reversed { (edge.to, edge.from) }` this replaces — is exactly the
/// per-edge work being removed.
#[derive(Clone, Debug)]
struct CompiledConsumeIndex {
    groups: Box<[CompiledConsumeGroup]>,
    edges: Box<[CompiledConsumeEdge]>,
    sources: Box<[u64]>,
    words: usize,
}

impl CompiledConsumeIndex {
    fn build(states: usize, edges: &[CompiledAssertionEdge], reversed: bool) -> Self {
        // Grouped in order of first appearance, by linear search: an assertion
        // body carries a handful of distinct predicates, and first-appearance
        // order keeps the compiled index a readable image of the source.
        let mut grouped: Vec<(PredicateId, Vec<CompiledConsumeEdge>)> = Vec::new();
        for edge in edges {
            let CompiledAssertionEdgeKind::Consume(predicate) = edge.kind else {
                continue;
            };
            let (from, to) = if reversed {
                (edge.to, edge.from)
            } else {
                (edge.from, edge.to)
            };
            let slot = if let Some(slot) = grouped.iter().position(|(id, _)| *id == predicate) {
                slot
            } else {
                grouped.push((predicate, Vec::new()));
                grouped.len() - 1
            };
            grouped[slot].1.push(CompiledConsumeEdge {
                from: u32::try_from(from).expect("assertion state count is compiler-bounded"),
                to: u32::try_from(to).expect("assertion state count is compiler-bounded"),
            });
        }

        let words = states.div_ceil(u64::BITS as usize);
        let mut groups = Vec::with_capacity(grouped.len());
        let mut flat: Vec<CompiledConsumeEdge> = Vec::new();
        let mut sources = vec![0_u64; grouped.len() * words];
        for (slot, (predicate, group_edges)) in grouped.into_iter().enumerate() {
            let start =
                u32::try_from(flat.len()).expect("assertion edge count is compiler-bounded");
            for edge in &group_edges {
                let from = edge.from as usize;
                sources[slot * words + from / u64::BITS as usize] |=
                    1_u64 << (from % u64::BITS as usize);
            }
            flat.extend(group_edges);
            groups.push(CompiledConsumeGroup {
                predicate,
                start,
                end: u32::try_from(flat.len()).expect("assertion edge count is compiler-bounded"),
            });
        }

        Self {
            groups: groups.into_boxed_slice(),
            edges: flat.into_boxed_slice(),
            sources: sources.into_boxed_slice(),
            words,
        }
    }

    #[inline]
    fn sources(&self, slot: usize) -> &[u64] {
        &self.sources[slot * self.words..(slot + 1) * self.words]
    }

    #[inline]
    fn edges(&self, group: &CompiledConsumeGroup) -> &[CompiledConsumeEdge] {
        &self.edges[group.start as usize..group.end as usize]
    }
}

#[derive(Clone, Debug)]
struct CompiledClosureEdge {
    to: usize,
    kind: CompiledClosureEdgeKind,
}

#[derive(Clone, Debug)]
enum CompiledClosureEdgeKind {
    Epsilon,
    Boundary(CompiledPositionAssertion),
    Fact(AssertionOccurrenceId),
}

#[derive(Clone, Debug)]
struct CompiledClosureIndex {
    offsets: Box<[usize]>,
    edges: Box<[CompiledClosureEdge]>,
    epsilon_transitive: Option<CompiledEpsilonClosureIndex>,
}

#[derive(Clone, Debug)]
struct CompiledEpsilonClosureIndex {
    offsets: Box<[usize]>,
    states: Box<[usize]>,
}

impl CompiledEpsilonClosureIndex {
    /// Aggregate retained reachability storage across one compiled catalog.
    const MAX_CATALOG_REACHABLE_ENTRIES: usize = 8 * 1024 * 1024 / size_of::<usize>();

    /// Maximum retained reachable-state entries per direction.
    ///
    /// At eight bytes per `usize` on the production platform this caps each
    /// optional table at 512 KiB. Larger graphs keep the linear-space worklist.
    const MAX_REACHABLE_STATES: usize = 1 << 16;

    fn build(index: &CompiledClosureIndex, remaining_entries: &mut usize) -> Option<Self> {
        if *remaining_entries == 0 {
            return None;
        }
        let state_count = index.offsets.len() - 1;
        let mut offsets = Vec::with_capacity(state_count + 1);
        let mut states = Vec::new();
        let mut visited = vec![0_u32; state_count];
        let mut work = Vec::new();
        let mut reached = Vec::new();
        offsets.push(0);

        for start in 0..state_count {
            let generation =
                u32::try_from(start + 1).expect("assertion state count is bounded below u32::MAX");
            visited[start] = generation;
            work.clear();
            reached.clear();
            work.extend(index.from(start).iter().map(|edge| edge.to));
            while let Some(state) = work.pop() {
                if visited[state] == generation {
                    continue;
                }
                visited[state] = generation;
                reached.push(state);
                work.extend(index.from(state).iter().map(|edge| edge.to));
            }
            let next_len = states.len().checked_add(reached.len())?;
            if next_len > Self::MAX_REACHABLE_STATES || next_len > *remaining_entries {
                return None;
            }
            reached.sort_unstable();
            states.extend(reached.iter().copied());
            offsets.push(states.len());
        }

        *remaining_entries -= states.len();
        Some(Self {
            offsets: offsets.into_boxed_slice(),
            states: states.into_boxed_slice(),
        })
    }

    fn from(&self, state: usize) -> &[usize] {
        &self.states[self.offsets[state]..self.offsets[state + 1]]
    }
}

impl CompiledClosureIndex {
    fn build(
        states: usize,
        edges: &[CompiledAssertionEdge],
        reversed: bool,
        epsilon_reachable_entries: &mut usize,
    ) -> Self {
        let mut grouped = vec![Vec::new(); states];
        for edge in edges {
            let kind = match &edge.kind {
                CompiledAssertionEdgeKind::Epsilon => CompiledClosureEdgeKind::Epsilon,
                CompiledAssertionEdgeKind::Consume(_) => continue,
                CompiledAssertionEdgeKind::Boundary(assertion) => {
                    CompiledClosureEdgeKind::Boundary(assertion.clone())
                }
                CompiledAssertionEdgeKind::Fact(occurrence) => {
                    CompiledClosureEdgeKind::Fact(*occurrence)
                }
            };
            let (from, to) = if reversed {
                (edge.to, edge.from)
            } else {
                (edge.from, edge.to)
            };
            grouped[from].push(CompiledClosureEdge { to, kind });
        }

        let mut offsets = Vec::with_capacity(states + 1);
        let mut flat = Vec::new();
        offsets.push(0);
        for state_edges in grouped {
            flat.extend(state_edges);
            offsets.push(flat.len());
        }
        let mut index = Self {
            offsets: offsets.into_boxed_slice(),
            edges: flat.into_boxed_slice(),
            epsilon_transitive: None,
        };
        if !index.edges.is_empty()
            && index
                .edges
                .iter()
                .all(|edge| matches!(edge.kind, CompiledClosureEdgeKind::Epsilon))
        {
            index.epsilon_transitive =
                CompiledEpsilonClosureIndex::build(&index, epsilon_reachable_entries);
        }
        index
    }

    #[inline]
    fn from(&self, state: usize) -> &[CompiledClosureEdge] {
        &self.edges[self.offsets[state]..self.offsets[state + 1]]
    }
}

fn compile_automaton(
    automaton: AssertionAutomaton,
    predicates: &mut PredicateCatalog,
    epsilon_reachable_entries: &mut usize,
) -> CompiledAssertionAutomaton {
    let states = automaton.states;
    let edges: Box<[CompiledAssertionEdge]> = automaton
        .edges
        .into_vec()
        .into_iter()
        .map(|edge| CompiledAssertionEdge {
            from: edge.from,
            to: edge.to,
            kind: match edge.kind {
                AssertionEdgeKind::Epsilon => CompiledAssertionEdgeKind::Epsilon,
                AssertionEdgeKind::Consume(predicate) => {
                    CompiledAssertionEdgeKind::Consume(predicates.intern(&predicate))
                }
                AssertionEdgeKind::Boundary(assertion) => CompiledAssertionEdgeKind::Boundary(
                    predicates.intern_position_assertion(&assertion),
                ),
                AssertionEdgeKind::Fact(occurrence) => CompiledAssertionEdgeKind::Fact(occurrence),
            },
        })
        .collect();
    CompiledAssertionAutomaton {
        states,
        start: automaton.start,
        accept: automaton.accept,
        forward_consume: CompiledConsumeIndex::build(states, &edges, false),
        reverse_consume: CompiledConsumeIndex::build(states, &edges, true),
        forward_closure: CompiledClosureIndex::build(
            states,
            &edges,
            false,
            epsilon_reachable_entries,
        ),
        reverse_closure: CompiledClosureIndex::build(
            states,
            &edges,
            true,
            epsilon_reachable_entries,
        ),
    }
}

#[derive(Clone, Debug)]
struct BoundaryBits {
    words: Box<[u64]>,
    len: usize,
}

impl BoundaryBits {
    /// Boundaries per word.
    ///
    /// One symbol for all four uses. `new` divides by it, `set` and `get`
    /// address with it, and `first_set` multiplies by it to turn a word index
    /// back into a boundary — so a literal `64` in any one of them is a silent
    /// disagreement with the allocation, which nothing about the type would
    /// report.
    const BITS: usize = u64::BITS as usize;

    fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(Self::BITS)].into_boxed_slice(),
            len,
        }
    }

    fn set(&mut self, index: usize) {
        assert!(index < self.len);
        self.words[index / Self::BITS] |= 1_u64 << (index % Self::BITS);
    }

    fn get(&self, index: usize) -> bool {
        index < self.len && self.words[index / Self::BITS] & (1_u64 << (index % Self::BITS)) != 0
    }

    /// Sets every boundary `other` sets.
    ///
    /// Both rows are allocated from the same sentence length, so the word
    /// counts agree and `zip` drops nothing.
    fn union_with(&mut self, other: &Self) {
        debug_assert_eq!(self.len, other.len);
        for (word, source) in self.words.iter_mut().zip(&other.words) {
            *word |= *source;
        }
    }

    /// The lowest set boundary, or `None` when none is set.
    ///
    /// Scanning whole words is safe because [`Self::set`] asserts `index < len`,
    /// so no bit above `len` is ever set and none can be reported.
    fn first_set(&self) -> Option<usize> {
        self.words.iter().enumerate().find_map(|(word, bits)| {
            (*bits != 0).then(|| {
                word * Self::BITS
                    + usize::try_from(bits.trailing_zeros()).expect("a u32 bit index fits usize")
            })
        })
    }
}

/// Where a reverse pass admits the automaton's accepting state.
///
/// [`ReverseSeed::EveryBoundary`] asks "does this match *starting* here, ending
/// anywhere", which is what `followed_by` means. [`ReverseSeed::SentenceEndOnly`]
/// asks "does this match starting here and ending at the sentence end", which is
/// what a derived anchor means: the earliest `b` whose `b..n` the `exclude`
/// fragment recognizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReverseSeed {
    EveryBoundary,
    SentenceEndOnly,
}

/// The set of automaton states a pass has reached at one boundary: dense to
/// test, sparse to walk.
///
/// A pass touches its frontier at every boundary of every sentence, and the
/// automata compiled here are small while the set live at any one boundary is
/// smaller still. A plain `Vec<bool>` made both hot loops cost the automaton's
/// *size* at each position — [`AssertionSession::close`] seeded its worklist by
/// scanning every slot, and [`AssertionSession::consume`] cleared every slot and
/// then inspected every consume edge to find the ones leaving a live state.
///
/// So the bool array stays, because `close` needs an O(1) "already reached?"
/// and the passes read `start` and `accept` by index, and an explicit list of
/// the live states is kept beside it so every *walk* is over that set instead.
///
/// A bitset, because every operation a pass performs on the frontier is a set
/// operation over an automaton small enough to hold in a few words: emptying it
/// is a `fill(0)` of at most three words for the largest program any tracked
/// catalog compiles, and asking whether a predicate's sources are live at all is
/// a word-wise intersection instead of a walk.
///
/// A bool array made both hot loops cost the automaton's *size* at each
/// position, since [`AssertionSession::close`] seeded its worklist by scanning
/// every slot and [`AssertionSession::consume`] cleared every slot and then
/// inspected every consume edge. Neither cost survives here: the seed reads set
/// bits, and the clear is one store per word.
#[derive(Debug, Default)]
struct Frontier {
    words: Vec<u64>,
}

impl Frontier {
    const BITS: usize = u64::BITS as usize;

    /// Empties the frontier and sizes it for `states`.
    fn reset(&mut self, states: usize) {
        let words = states.div_ceil(Self::BITS);
        if self.words.len() == words {
            self.words.fill(0);
        } else {
            self.words.clear();
            self.words.resize(words, 0);
        }
    }

    #[inline]
    fn contains(&self, state: usize) -> bool {
        self.words[state / Self::BITS] & (1_u64 << (state % Self::BITS)) != 0
    }

    #[inline]
    fn insert(&mut self, state: usize) {
        self.words[state / Self::BITS] |= 1_u64 << (state % Self::BITS);
    }

    /// Whether any state in `other` is live.
    ///
    /// `other` is one group's source mask and is built with the same word count,
    /// so `zip` never truncates a live word away.
    #[inline]
    fn intersects(&self, other: &[u64]) -> bool {
        debug_assert_eq!(self.words.len(), other.len());
        self.words
            .iter()
            .zip(other)
            .any(|(live, source)| live & source != 0)
    }

    /// Appends every live state to `work`, lowest first.
    fn seed(&self, work: &mut Vec<usize>) {
        for (index, word) in self.words.iter().enumerate() {
            let mut bits = *word;
            while bits != 0 {
                let bit =
                    usize::try_from(bits.trailing_zeros()).expect("a u32 bit index fits usize");
                work.push(index * Self::BITS + bit);
                bits &= bits - 1;
            }
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    fn len(&self) -> u64 {
        self.words
            .iter()
            .map(|word| u64::from(word.count_ones()))
            .sum()
    }
}

pub(crate) struct AssertionSession<'catalog, 'tokens, S: TokenSequence + ?Sized> {
    catalog: &'catalog CompiledAssertionCatalog,
    predicates: PredicateSession<'catalog>,
    tokens: &'tokens S,
    rows: Vec<Option<BoundaryBits>>,
    work: Vec<usize>,
    /// Pooled pass buffers.
    ///
    /// Checked out with [`std::mem::take`] and returned at the end of the pass.
    /// That is sound only because no pass nests inside another: `ensure`
    /// materializes every dependency row *before* calling `evaluate`, so a
    /// `Fact` edge in `close` reads `self.rows` and never re-enters a pass.
    frontier: Frontier,
    next_frontier: Frontier,
    #[cfg(feature = "bench-instrumentation")]
    metrics: AssertionMetricCells,
}

impl<'catalog, 'tokens, S: TokenSequence + ?Sized> AssertionSession<'catalog, 'tokens, S> {
    pub(crate) fn new(
        catalog: &'catalog CompiledAssertionCatalog,
        predicates: &PredicateSession<'catalog>,
        tokens: &'tokens S,
    ) -> Self {
        Self {
            catalog,
            predicates: predicates.clone(),
            tokens,
            rows: vec![None; catalog.programs.len()],
            work: Vec::new(),
            frontier: Frontier::default(),
            next_frontier: Frontier::default(),
            #[cfg(feature = "bench-instrumentation")]
            metrics: AssertionMetricCells::default(),
        }
    }

    pub(crate) fn holds(&mut self, occurrence: AssertionOccurrenceId, boundary: usize) -> bool {
        #[cfg(feature = "bench-instrumentation")]
        self.metrics
            .row_queries
            .set(self.metrics.row_queries.get().saturating_add(1));
        let occurrence = self.catalog.occurrences[occurrence.index()];
        self.ensure(occurrence.program);
        let exists = self.rows[occurrence.program.index()]
            .as_ref()
            .expect("ensured assertion row exists")
            .get(boundary);
        match occurrence.polarity {
            LookPolarity::Positive => exists,
            LookPolarity::Negative => !exists,
        }
    }

    /// The earliest boundary from which `id` matches exactly to the sentence
    /// end, or `None` when it matches no suffix.
    ///
    /// One [`ReverseSeed::SentenceEndOnly`] pass, read for its lowest set
    /// boundary. The seed is the whole difference from `followed_by`, and it is
    /// a named argument rather than a copied loop precisely because the wrong
    /// seed is invisible in the answer for most inputs: passing
    /// [`ReverseSeed::EveryBoundary`] here fails only the two
    /// `suffix_start_ignores_…` tests below, and the other six still pass.
    ///
    /// The answer is deliberately **not** memoized into `self.rows`. That table
    /// holds one row per program answering `evaluate`'s question, and a program
    /// is shared by body: an anchor's `exclude` and a `followed_by` over the same
    /// fragment intern to one id. Writing a suffix answer there would hand
    /// `holds` a row meaning something else.
    ///
    /// Dependencies are materialized first, because [`Self::close`] reads
    /// `self.rows[occurrence.program]` for every `Fact` edge and requires a row
    /// to be there — the `debug_assert!` in its `Fact` arm is what a test build
    /// trips first when they are not. An anchor's `exclude` has a dependency
    /// whenever a lookaround appears inside it.
    pub(crate) fn suffix_start(&mut self, id: AssertionProgramId) -> Option<usize> {
        self.ensure_dependencies(id);

        // Not a claim about how this pass runs — a claim about what was
        // interned. `compile_assertions` interns every anchor `exclude` as
        // `FollowedBy`, because "matches forward from `b` to the end" is a
        // `followed_by` fact, and the direction is part of the interning key.
        //
        // `assert!`, not `debug_assert!`: it runs once per anchor per sentence
        // rather than per edge per convergence iteration, so it is unmeasurable
        // beside the pass, and what it buys is that a release build panics
        // instead of publishing a position computed from the wrong automaton.
        assert_eq!(
            self.catalog.programs[id.index()].direction,
            LookDirection::FollowedBy,
            "an anchor exclude is interned as a followed_by program"
        );
        // `self.catalog` is a `&'catalog CompiledAssertionCatalog`, so reading it
        // out yields a `'catalog` borrow rather than one of `self` — which is
        // why this `&mut self` method can hold `automaton` across `&self` calls.
        // Do not "fix" a borrow error here by taking `&mut self` in the pass.
        let automaton = &self.catalog.programs[id.index()].automaton;
        self.reverse_pass(automaton, ReverseSeed::SentenceEndOnly)
            .first_set()
    }

    /// Materializes every row `id`'s automaton reads through a `Fact` edge.
    fn ensure_dependencies(&mut self, id: AssertionProgramId) {
        for dependency in self.catalog.programs[id.index()]
            .dependencies
            .iter()
            .copied()
        {
            self.ensure(dependency);
        }
    }

    fn ensure(&mut self, id: AssertionProgramId) {
        if self.rows[id.index()].is_some() {
            return;
        }
        self.ensure_dependencies(id);
        #[cfg(feature = "bench-instrumentation")]
        self.metrics
            .row_materializations
            .set(self.metrics.row_materializations.get().saturating_add(1));
        let row = self.evaluate(id);
        self.rows[id.index()] = Some(row);
    }

    fn evaluate(&mut self, id: AssertionProgramId) -> BoundaryBits {
        let program = &self.catalog.programs[id.index()];
        let automaton = &program.automaton;

        // A union is read, not walked. `ensure` has already materialized every
        // member — they are this program's dependencies — so the answer is
        // their rows `or`ed together, and sweeping the sentence again to
        // rediscover it would be the whole cost of a pass for none of the
        // information.
        if !program.union_members.is_empty() {
            let mut row = BoundaryBits::new(self.tokens.len() + 1);
            for member in &program.union_members {
                let source = self.rows[member.index()]
                    .as_ref()
                    .expect("union members are materialized as dependencies");
                row.union_with(source);
            }
            return row;
        }

        match program.direction {
            LookDirection::PrecededBy => {
                let mut row = BoundaryBits::new(self.tokens.len() + 1);
                let mut frontier = std::mem::take(&mut self.frontier);
                frontier.reset(automaton.states);
                for boundary in 0..=self.tokens.len() {
                    frontier.insert(automaton.start);
                    self.close(automaton, &mut frontier, boundary, false);
                    if frontier.contains(automaton.accept) {
                        row.set(boundary);
                    }
                    if boundary < self.tokens.len() {
                        self.consume(automaton, &mut frontier, boundary, false);
                    }
                }
                self.frontier = frontier;
                row
            }
            LookDirection::FollowedBy => self.reverse_pass(automaton, ReverseSeed::EveryBoundary),
        }
    }

    /// Walks the sentence right-to-left, recording every boundary the automaton
    /// can start at.
    ///
    /// The sole reverse walk. `followed_by` and a derived anchor differ in
    /// exactly one thing — where `accept` is seeded — so that difference is the
    /// argument and everything else is shared. Two copies of this loop would let
    /// a fix to the `close`/`consume` ordering, the `boundary > 0` guard, or the
    /// instrumentation land in one and miss the other with no compiler or test
    /// signal, since both copies compile and most inputs agree.
    fn reverse_pass(
        &mut self,
        automaton: &CompiledAssertionAutomaton,
        seed: ReverseSeed,
    ) -> BoundaryBits {
        let mut row = BoundaryBits::new(self.tokens.len() + 1);
        let mut frontier = std::mem::take(&mut self.frontier);
        frontier.reset(automaton.states);

        if seed == ReverseSeed::SentenceEndOnly {
            frontier.insert(automaton.accept);
        }
        for boundary in (0..=self.tokens.len()).rev() {
            if seed == ReverseSeed::EveryBoundary {
                frontier.insert(automaton.accept);
            }
            self.close(automaton, &mut frontier, boundary, true);
            if frontier.contains(automaton.start) {
                row.set(boundary);
            }
            if boundary > 0 {
                self.consume(automaton, &mut frontier, boundary - 1, true);
            }
        }
        self.frontier = frontier;
        row
    }

    fn close(
        &mut self,
        automaton: &CompiledAssertionAutomaton,
        frontier: &mut Frontier,
        boundary: usize,
        reversed: bool,
    ) {
        let index = if reversed {
            &automaton.reverse_closure
        } else {
            &automaton.forward_closure
        };
        #[cfg(feature = "bench-instrumentation")]
        {
            self.metrics
                .closure_calls
                .set(self.metrics.closure_calls.get().saturating_add(1));
            self.metrics.frontier_seed_slots_scanned.set(
                self.metrics
                    .frontier_seed_slots_scanned
                    .get()
                    .saturating_add(frontier.len()),
            );
        }
        let mut work = std::mem::take(&mut self.work);
        work.clear();
        frontier.seed(&mut work);
        #[cfg(feature = "bench-instrumentation")]
        self.record_state_visits(frontier);
        if let Some(epsilon) = &index.epsilon_transitive {
            while let Some(from) = work.pop() {
                for to in epsilon.from(from) {
                    #[cfg(feature = "bench-instrumentation")]
                    {
                        self.metrics
                            .edge_inspections
                            .set(self.metrics.edge_inspections.get().saturating_add(1));
                        self.metrics.epsilon_edge_inspections.set(
                            self.metrics
                                .epsilon_edge_inspections
                                .get()
                                .saturating_add(1),
                        );
                    }
                    frontier.insert(*to);
                }
            }
            self.work = work;
            return;
        }
        while let Some(from) = work.pop() {
            for edge in index.from(from) {
                #[cfg(feature = "bench-instrumentation")]
                self.metrics
                    .edge_inspections
                    .set(self.metrics.edge_inspections.get().saturating_add(1));
                #[cfg(feature = "bench-instrumentation")]
                match &edge.kind {
                    CompiledClosureEdgeKind::Epsilon => self.metrics.epsilon_edge_inspections.set(
                        self.metrics
                            .epsilon_edge_inspections
                            .get()
                            .saturating_add(1),
                    ),
                    CompiledClosureEdgeKind::Boundary(_) => {
                        self.metrics.boundary_edge_inspections.set(
                            self.metrics
                                .boundary_edge_inspections
                                .get()
                                .saturating_add(1),
                        )
                    }
                    CompiledClosureEdgeKind::Fact(_) => self
                        .metrics
                        .fact_edge_inspections
                        .set(self.metrics.fact_edge_inspections.get().saturating_add(1)),
                }
                if frontier.contains(edge.to) {
                    continue;
                }
                let holds = match &edge.kind {
                    CompiledClosureEdgeKind::Epsilon => true,
                    CompiledClosureEdgeKind::Boundary(assertion) => {
                        self.position_assertion_holds(assertion, boundary)
                    }
                    CompiledClosureEdgeKind::Fact(occurrence) => {
                        let occurrence = self.catalog.occurrences[occurrence.index()];
                        debug_assert!(self.rows[occurrence.program.index()].is_some());
                        let exists = self.rows[occurrence.program.index()]
                            .as_ref()
                            .expect("fact dependencies are materialized")
                            .get(boundary);
                        match occurrence.polarity {
                            LookPolarity::Positive => exists,
                            LookPolarity::Negative => !exists,
                        }
                    }
                };
                if holds {
                    frontier.insert(edge.to);
                    work.push(edge.to);
                }
            }
        }
        self.work = work;
    }

    fn consume(
        &mut self,
        automaton: &CompiledAssertionAutomaton,
        frontier: &mut Frontier,
        token_index: usize,
        reversed: bool,
    ) {
        #[cfg(feature = "bench-instrumentation")]
        self.record_state_visits(frontier);
        let index = if reversed {
            &automaton.reverse_consume
        } else {
            &automaton.forward_consume
        };
        let mut next = std::mem::take(&mut self.next_frontier);
        next.reset(automaton.states);
        for (slot, group) in index.groups.iter().enumerate() {
            if !frontier.intersects(index.sources(slot)) {
                continue;
            }
            if !self.predicates.matches_at_assertion_consume(
                group.predicate,
                self.tokens,
                token_index,
            ) {
                continue;
            }
            for edge in index.edges(group) {
                #[cfg(feature = "bench-instrumentation")]
                self.metrics.consume_edge_inspections.set(
                    self.metrics
                        .consume_edge_inspections
                        .get()
                        .saturating_add(1),
                );
                if frontier.contains(edge.from as usize) {
                    next.insert(edge.to as usize);
                }
            }
        }
        std::mem::swap(frontier, &mut next);
        self.next_frontier = next;
    }

    fn position_assertion_holds(
        &self,
        assertion: &CompiledPositionAssertion,
        boundary: usize,
    ) -> bool {
        match assertion {
            CompiledPositionAssertion::SentenceStart => boundary == 0,
            CompiledPositionAssertion::SentenceEnd => boundary == self.tokens.len(),
            CompiledPositionAssertion::PreviousToken(predicate) => {
                boundary.checked_sub(1).is_some_and(|index| {
                    self.predicates.matches_at_assertion_previous_token(
                        *predicate,
                        self.tokens,
                        index,
                    )
                })
            }
            CompiledPositionAssertion::Lookaround(occurrence) => {
                let occurrence = self.catalog.occurrences[occurrence.index()];
                debug_assert!(self.rows[occurrence.program.index()].is_some());
                let exists = self.rows[occurrence.program.index()]
                    .as_ref()
                    .expect("lookaround dependencies are materialized")
                    .get(boundary);
                match occurrence.polarity {
                    LookPolarity::Positive => exists,
                    LookPolarity::Negative => !exists,
                }
            }
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn metrics(&self) -> AssertionMetrics {
        AssertionMetrics {
            row_queries: self.metrics.row_queries.get(),
            row_materializations: self.metrics.row_materializations.get(),
            state_visits: self.metrics.state_visits.get(),
            edge_inspections: self.metrics.edge_inspections.get(),
            max_frontier_states: self.metrics.max_frontier_states.get(),
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    fn record_state_visits(&self, frontier: &Frontier) {
        let active = frontier.len();
        self.metrics
            .state_visits
            .set(self.metrics.state_visits.get().saturating_add(active));
        self.metrics
            .max_frontier_states
            .set(self.metrics.max_frontier_states.get().max(active));
    }
}

pub(crate) fn required_attributes(automaton: &AssertionAutomaton) -> Box<[AttributeKey]> {
    let mut attributes = BTreeSet::new();
    for edge in &automaton.edges {
        match &edge.kind {
            AssertionEdgeKind::Consume(predicate) => {
                attributes.extend(predicate.iter().map(|attribute| attribute.attr.clone()));
            }
            AssertionEdgeKind::Boundary(PositionAssertion::PreviousToken(predicate)) => {
                attributes.extend(predicate.iter().map(|attribute| attribute.attr.clone()));
            }
            _ => {}
        }
    }
    attributes.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::matcher::{PUNCT_ANCHOR, compile_for_test};
    use crate::fsm::{AttrValue, AttrValueType};
    use crate::token_match::TokenInfo;

    fn token_with_pos(tag: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Pos, tag.into());
        token
    }

    /// Compiles [`PUNCT_ANCHOR`] over `tags` and hands the session to `body`.
    ///
    /// A closure fixture rather than a constructor: an [`AssertionSession`]
    /// borrows the compiled catalog, the predicate session and the tokens, all
    /// three of which are locals here, so it cannot be returned.
    fn with_punct_session(
        tags: &[&str],
        body: impl FnOnce(&mut AssertionSession<'_, '_, Vec<TokenInfo>>, AssertionProgramId),
    ) {
        with_anchor_session(PUNCT_ANCHOR, tags, body);
    }

    /// [`with_punct_session`], for a catalog other than [`PUNCT_ANCHOR`].
    fn with_anchor_session(
        source: &str,
        tags: &[&str],
        body: impl FnOnce(&mut AssertionSession<'_, '_, Vec<TokenInfo>>, AssertionProgramId),
    ) {
        let matcher = compile_for_test("suffix-start.lx", source);
        let tokens: Vec<TokenInfo> = tags.iter().map(|tag| token_with_pos(tag)).collect();
        let program = matcher.anchor_programs_for_test()["propositional_end"];
        let predicates = PredicateSession::new(matcher.predicates_for_test());
        let mut session =
            AssertionSession::new(matcher.assertion_catalog_for_test(), &predicates, &tokens);
        body(&mut session, program);
    }

    #[test]
    fn a_frontier_holds_only_what_was_inserted_and_empties_on_reset() {
        let mut frontier = Frontier::default();
        frontier.reset(70);
        assert!(!frontier.contains(0));
        frontier.insert(0);
        frontier.insert(69);
        assert!(frontier.contains(0));
        assert!(frontier.contains(69));
        assert!(!frontier.contains(64));

        let mut seeded = Vec::new();
        frontier.seed(&mut seeded);
        assert_eq!(seeded, vec![0, 69], "seeded lowest state first");

        // Reset at the same width keeps the allocation and must still clear.
        frontier.reset(70);
        assert!(!frontier.contains(0));
        assert!(!frontier.contains(69));
        let mut after = Vec::new();
        frontier.seed(&mut after);
        assert!(after.is_empty());
    }

    #[test]
    fn a_frontier_intersects_only_a_mask_sharing_a_live_state() {
        let mut frontier = Frontier::default();
        frontier.reset(128);
        frontier.insert(100);
        assert!(
            frontier.intersects(&[0, 1 << 36]),
            "state 100 is word 1 bit 36"
        );
        assert!(!frontier.intersects(&[u64::MAX, 0]));
        assert!(!frontier.intersects(&[0, 0]));
    }

    /// The invariant the consume loop's cost rests on: a body that repeats a
    /// token test interns one predicate, so it asks the memo once per token
    /// rather than once per edge. Losing the grouping is invisible in output —
    /// `find_matches` is unchanged either way — and this is what would report it.
    #[test]
    fn a_repeated_token_test_compiles_to_one_consume_group() {
        let matcher = compile_for_test(
            "consume-grouping.lx",
            "pattern p := [upos=X] followed_by([upos=A] [upos=A] [upos=A]).",
        );
        let catalog = matcher.assertion_catalog_for_test();
        assert_eq!(catalog.program_count_for_test(), 1);
        assert_eq!(
            catalog.consume_shape_for_test(0),
            (1, 3),
            "three edges over one interned predicate is one group"
        );
    }

    #[test]
    fn distinct_token_tests_compile_to_distinct_consume_groups() {
        let matcher = compile_for_test(
            "consume-grouping.lx",
            "pattern p := [upos=X] followed_by([upos=A] [upos=B] [upos=A]).",
        );
        assert_eq!(
            matcher
                .assertion_catalog_for_test()
                .consume_shape_for_test(0),
            (2, 3),
            "two interned predicates over three edges is two groups"
        );
    }

    #[test]
    fn suffix_start_finds_the_earliest_boundary_reaching_the_sentence_end() {
        // Boundaries 1 and 2 both reach the end; the earliest is the longest
        // apparatus run, which is the anchor.
        with_punct_session(&["NOUN", "PUNCT", "PUNCT"], |session, program| {
            assert_eq!(session.suffix_start(program), Some(1));
        });
    }

    #[test]
    fn suffix_start_is_none_when_nothing_reaches_the_end() {
        with_punct_session(&["NOUN", "NOUN"], |session, program| {
            assert_eq!(session.suffix_start(program), None);
        });
    }

    /// The degenerate sentence, where the seed boundary and boundary 0 coincide.
    ///
    /// The one input exercising that arithmetic edge: the loop body runs once,
    /// and `if boundary > 0` is the only thing standing between it and an
    /// underflowing `consume`. The caller maps `None` to `tokens.len()`, which
    /// is also 0 here.
    #[test]
    fn suffix_start_is_none_for_an_empty_sentence() {
        with_punct_session(&[], |session, program| {
            assert_eq!(session.suffix_start(program), None);
        });
    }

    /// The seed choice, stated as behaviour.
    ///
    /// [`ReverseSeed::EveryBoundary`] — what `evaluate`'s `FollowedBy` arm
    /// passes — asks "does the program match *starting* here", and the leading
    /// `PUNCT` does. [`ReverseSeed::SentenceEndOnly`] asks "does it match
    /// starting here and *ending at the end*", and nothing does. One of the two
    /// tests the wrong seed fails; see also
    /// [`suffix_start_ignores_an_earlier_run_that_stops_short`].
    #[test]
    fn suffix_start_ignores_a_match_that_does_not_end_at_the_sentence_end() {
        with_punct_session(&["PUNCT", "NOUN"], |session, program| {
            assert_eq!(session.suffix_start(program), None);
        });
    }

    /// The same property where both readings return a position.
    ///
    /// [`suffix_start_ignores_a_match_that_does_not_end_at_the_sentence_end`]
    /// separates the two readings by `Some`/`None`, which a pass that never
    /// reached the end at all would also satisfy. Here a suffix genuinely
    /// exists at 2, and [`ReverseSeed::EveryBoundary`] reports 0 instead — so
    /// the assertion pins the seed rather than the emptiness.
    #[test]
    fn suffix_start_ignores_an_earlier_run_that_stops_short() {
        with_punct_session(&["PUNCT", "NOUN", "PUNCT"], |session, program| {
            assert_eq!(session.suffix_start(program), Some(2));
        });
    }

    /// An `exclude` containing a lookaround, whose row `suffix_start` must
    /// materialize before its own pass reads it.
    ///
    /// `preceded_by` is part of the supported anchor-refinement shape, so this
    /// is not merely a corner case; it
    /// is the only one that reaches `suffix_start`'s dependency loop and
    /// `close`'s `Fact` arm at all.
    ///
    /// The guard makes the answer differ from [`PUNCT_ANCHOR`]'s, so the test
    /// cannot pass by ignoring it: over `NOUN PUNCT PUNCT` the bare
    /// `[upos=PUNCT]+` anchors at 1, while requiring a preceding `PUNCT` rules
    /// that boundary out and leaves 2.
    #[test]
    fn suffix_start_materializes_a_lookaround_its_exclude_depends_on() {
        let guarded = PUNCT_ANCHOR.replace(
            "fragment TRAILING_APPARATUS := [upos=PUNCT]+.",
            "fragment TRAILING_APPARATUS := preceded_by([upos=PUNCT]) [upos=PUNCT]+.",
        );
        with_anchor_session(&guarded, &["NOUN", "PUNCT", "PUNCT"], |session, program| {
            assert_eq!(session.suffix_start(program), Some(2));
        });
    }

    #[test]
    fn suffix_start_covers_the_whole_sentence_when_every_token_qualifies() {
        with_punct_session(&["PUNCT", "PUNCT"], |session, program| {
            assert_eq!(session.suffix_start(program), Some(0));
        });
    }

    /// A boundary in the second bit word, which no other case reaches.
    ///
    /// [`BoundaryBits::first_set`] scans words and adds `word * BITS`. Every
    /// other test's answer lands in word 0 — including the 201-token one, whose
    /// answer is 1 — so the multiplier is unexercised by them. It is not a
    /// corner: for a contiguous run of qualifying boundaries the first set bit
    /// *is* the answer, so every sentence anchoring past token 64 depends on it,
    /// and a wrong multiplier is a silent off-by-N on exactly the long sentences
    /// a derived anchor exists to serve.
    #[test]
    fn suffix_start_reports_a_boundary_beyond_the_first_bit_word() {
        let mut tags = vec!["NOUN"; 70];
        tags.extend(["PUNCT", "PUNCT"]);
        with_punct_session(&tags, |session, program| {
            assert_eq!(session.suffix_start(program), Some(70));
        });
    }

    /// Exactness at length: no window, so an apparatus run longer than any
    /// plausible cap still resolves to the true earliest boundary.
    ///
    /// This is the test a rejected 64-token scan window would have failed — it
    /// would have reported 137 rather than 1. This guards against an
    /// engine-authored maximum: greediness belongs to the authored expression.
    #[test]
    fn suffix_start_is_exact_beyond_any_plausible_window() {
        let mut tags = vec!["NOUN"];
        tags.extend(std::iter::repeat_n("PUNCT", 200));
        with_punct_session(&tags, |session, program| {
            assert_eq!(session.suffix_start(program), Some(1));
        });
    }

    /// Frontier work grows linearly in sentence length for a fixed automaton.
    ///
    /// Counted, not timed: a timing test cannot establish a complexity claim.
    /// This varies `n` for one recognizer, so it establishes scaling **in
    /// sentence length** and claims nothing about scaling in the automaton.
    ///
    /// All-`PUNCT` under [`PUNCT_ANCHOR`] is the adversarial shape:
    /// `[upos=PUNCT]+` reaches the sentence end from every boundary. Under the
    /// rejected per-start `Program` scan this was quadratic, so a quadratic
    /// regression would show here as a ratio near 16 rather than 4.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn anchor_frontier_work_is_linear_in_sentence_length() {
        /// The compiled `[upos=PUNCT]+` closure has two seed-boundary edges and
        /// three reachable closure edges per consumed token. Asserted rather
        /// than merely divided out: the ratio below is blind to a constant
        /// factor regression.
        const SEED_EDGES: u64 = 2;
        const EDGES_PER_TOKEN: u64 = 3;

        let short = anchor_edge_work(64);
        let long = anchor_edge_work(256);
        assert_eq!(short, SEED_EDGES + 64 * EDGES_PER_TOKEN);
        assert_eq!(long, SEED_EDGES + 256 * EDGES_PER_TOKEN);

        let ratio = long as f64 / short as f64;
        assert!(
            (3.0..5.0).contains(&ratio),
            "4x the tokens should cost about 4x the edge inspections, got {ratio:.2} \
             ({short} then {long})"
        );
    }

    /// Edge inspections performed by one anchor pass over `len` PUNCT tokens.
    ///
    /// No counter reset: [`with_punct_session`] compiles a fresh catalog and
    /// builds a fresh session per call, so the counters start at zero and this
    /// one `suffix_start` is all they see.
    #[cfg(feature = "bench-instrumentation")]
    fn anchor_edge_work(len: usize) -> u64 {
        let tags = vec!["PUNCT"; len];
        let mut counted = 0;
        with_punct_session(&tags, |session, program| {
            assert_eq!(session.suffix_start(program), Some(0));
            counted = session.metrics().edge_inspections;
        });
        counted
    }

    fn token(orth: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token
    }

    fn orth(value: &str) -> TokenPredicate {
        Box::from([AttrValue {
            attr: TokenAttributeIndex::Orth.into(),
            value: AttrValueType::Single(value.into()),
            complement: false,
        }])
    }

    fn ab_star(direction: LookDirection) -> AssertionCatalog {
        let automaton = AssertionAutomaton {
            states: 4,
            start: 0,
            accept: 3,
            edges: Box::from([
                AssertionEdge {
                    from: 0,
                    to: 1,
                    kind: AssertionEdgeKind::Consume(orth("a")),
                },
                AssertionEdge {
                    from: 1,
                    to: 3,
                    kind: AssertionEdgeKind::Epsilon,
                },
                AssertionEdge {
                    from: 1,
                    to: 2,
                    kind: AssertionEdgeKind::Consume(orth("b")),
                },
                AssertionEdge {
                    from: 2,
                    to: 1,
                    kind: AssertionEdgeKind::Epsilon,
                },
            ]),
        };
        AssertionCatalog {
            programs: Box::from([AssertionProgram {
                direction,
                required_attributes: required_attributes(&automaton),
                automaton,
                dependencies: Box::default(),
                union_members: Box::default(),
            }]),
            occurrences: Box::from([AssertionOccurrence {
                program: AssertionProgramId::new(0),
                polarity: LookPolarity::Positive,
            }]),
        }
    }

    #[test]
    fn direction_specialized_rows_match_exact_boundary_vectors() {
        let tokens = [token("a"), token("b"), token("b"), token("x"), token("a")];
        for (direction, expected) in [
            (
                LookDirection::PrecededBy,
                [false, true, true, true, false, true],
            ),
            (
                LookDirection::FollowedBy,
                [true, false, false, false, true, false],
            ),
        ] {
            let mut predicates = PredicateCatalog::default();
            let catalog = ab_star(direction).compile(&mut predicates);
            let predicates = PredicateSession::new(&predicates);
            let mut session = AssertionSession::new(&catalog, &predicates, &tokens);
            let actual = (0..=tokens.len())
                .map(|boundary| session.holds(AssertionOccurrenceId::new(0), boundary))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "direction: {direction:?}");
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn benchmark_metrics_count_queries_and_materialize_each_row_once() {
        let tokens = [token("a"), token("b")];
        let mut predicates = PredicateCatalog::default();
        let catalog = ab_star(LookDirection::PrecededBy).compile(&mut predicates);
        let predicates = PredicateSession::new(&predicates);
        let mut session = AssertionSession::new(&catalog, &predicates, &tokens);

        assert!(session.holds(AssertionOccurrenceId::new(0), 1));
        assert!(session.holds(AssertionOccurrenceId::new(0), 2));
        assert!(session.holds(AssertionOccurrenceId::new(0), 2));

        let metrics = session.metrics();
        assert_eq!(metrics.row_queries, 3);
        assert_eq!(metrics.row_materializations, 1);
        assert!(metrics.state_visits > 0);
    }

    /// Closure follows only closure-capable edges leaving newly active states.
    ///
    /// For `ab_star` over `a b`, boundary 0 has no reachable closure edge,
    /// boundary 1 follows `1 -> 3`, and boundary 2 follows `2 -> 1 -> 3`:
    /// exactly three inspections. Rescanning all four automaton edges until
    /// convergence performs 24 inspections on the same row.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn closure_inspects_only_edges_reachable_from_new_frontier_states() {
        let tokens = [token("a"), token("b")];
        let mut predicates = PredicateCatalog::default();
        let catalog = ab_star(LookDirection::PrecededBy).compile(&mut predicates);
        let predicates = PredicateSession::new(&predicates);
        let mut session = AssertionSession::new(&catalog, &predicates, &tokens);

        assert!(session.holds(AssertionOccurrenceId::new(0), 2));
        assert_eq!(session.metrics().edge_inspections, 3);
    }

    /// A static epsilon diamond activates each reachable state once instead of
    /// revisiting the convergent edge at every boundary.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn epsilon_only_closure_activates_each_reachable_state_once() {
        let automaton = AssertionAutomaton {
            states: 4,
            start: 0,
            accept: 3,
            edges: Box::from([
                AssertionEdge {
                    from: 0,
                    to: 1,
                    kind: AssertionEdgeKind::Epsilon,
                },
                AssertionEdge {
                    from: 0,
                    to: 2,
                    kind: AssertionEdgeKind::Epsilon,
                },
                AssertionEdge {
                    from: 1,
                    to: 3,
                    kind: AssertionEdgeKind::Epsilon,
                },
                AssertionEdge {
                    from: 2,
                    to: 3,
                    kind: AssertionEdgeKind::Epsilon,
                },
            ]),
        };
        let catalog = AssertionCatalog {
            programs: Box::from([AssertionProgram {
                direction: LookDirection::PrecededBy,
                required_attributes: required_attributes(&automaton),
                automaton,
                dependencies: Box::default(),
                union_members: Box::default(),
            }]),
            occurrences: Box::from([AssertionOccurrence {
                program: AssertionProgramId::new(0),
                polarity: LookPolarity::Positive,
            }]),
        };
        let mut predicates = PredicateCatalog::default();
        let catalog = catalog.compile(&mut predicates);
        let predicates = PredicateSession::new(&predicates);
        let mut session = AssertionSession::new(&catalog, &predicates, &[] as &[TokenInfo]);

        assert!(session.holds(AssertionOccurrenceId::new(0), 0));
        assert_eq!(session.metrics().edge_inspections, 3);
    }

    /// Transitive closure is an optimization, not permission to turn the
    /// compiler's linear state/transition budgets into quadratic storage.
    #[test]
    fn large_epsilon_closure_falls_back_to_the_bounded_worklist() {
        let states = 514;
        let automaton = AssertionAutomaton {
            states,
            start: 0,
            accept: states - 1,
            edges: (0..states - 1)
                .map(|from| AssertionEdge {
                    from,
                    to: from + 1,
                    kind: AssertionEdgeKind::Epsilon,
                })
                .collect(),
        };
        let mut predicates = PredicateCatalog::default();
        let mut epsilon_reachable_entries =
            CompiledEpsilonClosureIndex::MAX_CATALOG_REACHABLE_ENTRIES;
        let compiled =
            compile_automaton(automaton, &mut predicates, &mut epsilon_reachable_entries);

        assert!(compiled.forward_closure.epsilon_transitive.is_none());
        assert!(compiled.reverse_closure.epsilon_transitive.is_none());
    }

    /// The derived closure cache is bounded across the whole catalog, not once
    /// per assertion program. Otherwise many individually-small programs can
    /// bypass the compiler's aggregate generated-byte trust limit.
    #[test]
    fn epsilon_closure_storage_is_bounded_across_the_catalog() {
        const PROGRAMS: usize = 17;
        const STATES_PER_PROGRAM: usize = 258;

        let programs = (0..PROGRAMS)
            .map(|_| {
                let automaton = AssertionAutomaton {
                    states: STATES_PER_PROGRAM,
                    start: 0,
                    accept: STATES_PER_PROGRAM - 1,
                    edges: (0..STATES_PER_PROGRAM - 1)
                        .map(|from| AssertionEdge {
                            from,
                            to: from + 1,
                            kind: AssertionEdgeKind::Epsilon,
                        })
                        .collect(),
                };
                AssertionProgram {
                    direction: LookDirection::PrecededBy,
                    required_attributes: required_attributes(&automaton),
                    automaton,
                    dependencies: Box::default(),
                    union_members: Box::default(),
                }
            })
            .collect();
        let mut predicates = PredicateCatalog::default();
        let catalog = AssertionCatalog {
            programs,
            occurrences: Box::default(),
        }
        .compile(&mut predicates);

        let retained_entries = catalog
            .programs
            .iter()
            .flat_map(|program| {
                [
                    &program.automaton.forward_closure,
                    &program.automaton.reverse_closure,
                ]
            })
            .filter_map(|closure| closure.epsilon_transitive.as_ref())
            .map(|closure| closure.states.len())
            .sum::<usize>();
        assert!(retained_entries <= CompiledEpsilonClosureIndex::MAX_CATALOG_REACHABLE_ENTRIES);
        assert!(catalog.programs.iter().any(|program| {
            program
                .automaton
                .reverse_closure
                .epsilon_transitive
                .is_none()
        }));
    }

    /// Consume follows the dense consume-edge list without inspecting closure
    /// edges. `ab_star` has two consume edges and consumes two input tokens.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn consume_inspects_only_consume_edges() {
        let tokens = [token("a"), token("b")];
        let mut predicates = PredicateCatalog::default();
        let catalog = ab_star(LookDirection::PrecededBy).compile(&mut predicates);
        let predicates = PredicateSession::new(&predicates);
        let mut session = AssertionSession::new(&catalog, &predicates, &tokens);

        assert!(session.holds(AssertionOccurrenceId::new(0), 2));
        assert_eq!(session.metrics.consume_edge_inspections.get(), 2);
    }

    /// The decision probe distinguishes static epsilon work from dynamic
    /// boundary/fact work and from the frontier scan that seeds each closure.
    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn closure_metrics_attribute_edge_kinds_and_frontier_seed_scans() {
        let automaton = AssertionAutomaton {
            states: 3,
            start: 0,
            accept: 2,
            edges: Box::from([
                AssertionEdge {
                    from: 0,
                    to: 1,
                    kind: AssertionEdgeKind::Epsilon,
                },
                AssertionEdge {
                    from: 1,
                    to: 2,
                    kind: AssertionEdgeKind::Boundary(PositionAssertion::SentenceEnd),
                },
            ]),
        };
        let catalog = AssertionCatalog {
            programs: Box::from([AssertionProgram {
                direction: LookDirection::PrecededBy,
                required_attributes: required_attributes(&automaton),
                automaton,
                dependencies: Box::default(),
                union_members: Box::default(),
            }]),
            occurrences: Box::from([AssertionOccurrence {
                program: AssertionProgramId::new(0),
                polarity: LookPolarity::Positive,
            }]),
        };
        let mut predicates = PredicateCatalog::default();
        let catalog = catalog.compile(&mut predicates);
        let predicates = PredicateSession::new(&predicates);
        let mut session = AssertionSession::new(&catalog, &predicates, &[] as &[TokenInfo]);

        assert!(session.holds(AssertionOccurrenceId::new(0), 0));
        let metrics = session.metrics();
        assert_eq!(session.metrics.closure_calls.get(), 1);
        assert_eq!(session.metrics.frontier_seed_slots_scanned.get(), 1);
        assert_eq!(session.metrics.epsilon_edge_inspections.get(), 1);
        assert_eq!(session.metrics.boundary_edge_inspections.get(), 1);
        assert_eq!(session.metrics.fact_edge_inspections.get(), 0);
        assert_eq!(
            metrics.edge_inspections,
            session.metrics.epsilon_edge_inspections.get()
                + session.metrics.boundary_edge_inspections.get()
                + session.metrics.fact_edge_inspections.get()
        );
    }
}
