use crate::fsm::{AssertionOccurrenceId, AttrValueType, PositionAssertion, SymbolicAttrValue};
use crate::token_match::{
    AttributeKey, AttributeKind, AttributeLocator, BoundProjection, SequenceToken, TokenInfoTrait,
    TokenSequence,
};
use compact_str::CompactString;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

#[cfg(feature = "bench-instrumentation")]
use super::spawn_candidate_metrics::{SpawnCandidateAccumulator, SpawnCandidateShapeEffect};
use super::spawn_dispatch::SpawnAtomOperator;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PredicateId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RegexId(u32);

#[derive(Clone)]
pub(crate) struct CompiledAttribute {
    attr: AttributeLocator,
    value: CompiledValue,
    complement: bool,
}

#[derive(Clone)]
pub(crate) enum CompiledValue {
    Single(CompactString),
    OneOf(Box<[CompactString]>),
    NotIn(Box<[CompactString]>),
    SetHas(CompactString),
    SetHasAny(Box<[CompactString]>),
    SetNotHas(CompactString),
    SetNotHasAny(Box<[CompactString]>),
    Contains(CompactString),
    NotContains(CompactString),
    Regex(RegexId),
    NotRegex(RegexId),
}

#[derive(Clone)]
pub(crate) enum CompiledPredicate {
    Empty,
    Single(CompiledAttribute),
    Multiple(Box<[CompiledAttribute]>),
}

/// One uncomplemented, finite, positive atom of a compiled spawn predicate.
///
/// `values` is sorted and deduplicated by [`PredicateCatalog::spawn_candidate_descriptor`],
/// which is what lets [`finite_atom_matches`] binary-search it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FinitePositiveAtom {
    pub(crate) attribute: AttributeLocator,
    pub(crate) operator: SpawnAtomOperator,
    pub(crate) values: Box<[CompactString]>,
}

/// A compiled spawn predicate's finite-positive classification.
///
/// `scheme_a` is the canonical discriminator: the first atom under a total
/// order on (attribute, values, operator), so it does not depend on the order
/// the source wrote the attributes or the `OneOf` operands in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpawnPredicateDescriptor {
    pub(crate) scheme_a: Option<FinitePositiveAtom>,
    pub(crate) finite_atoms: Box<[FinitePositiveAtom]>,
    pub(crate) shape_signature: String,
}

/// A structural digest of every distinct token predicate a catalog compiled.
///
/// Ordered by interning — the order the dense machine, and then the assertion
/// catalog, first reach each distinct predicate. That order is a function of
/// the source, so the digest is too.
///
/// It exists because this is the one part of a compiled catalog that no count
/// reaches. Attribute conjunction is commutative, so reordering a `[…]`'s
/// attributes changes no match, and no state, transition, expression-node or
/// depth total either — a catalog can be recompiled into a different program
/// while every number the repository's parity guard carried stays put. The
/// guard is on the program, not on the behavior, so the difference has to be
/// visible somewhere.
///
/// Two catalogs sharing a digest agree on every attribute, operator, operand
/// and complement flag their predicates test, and on the order they appear in.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PredicateDigest([u8; 32]);

impl std::fmt::Display for PredicateDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for PredicateDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PredicateDigest")
            .field(&self.to_string())
            .finish()
    }
}

#[derive(Clone, Default)]
pub(crate) struct PredicateCatalog {
    predicates: Vec<CompiledPredicate>,
    regexes: Vec<regex::Regex>,
    owner: Arc<()>,
    /// Each predicate's definition, chained in as it is first interned; see
    /// [`PredicateDigest`].
    ///
    /// A chained 32-byte state and not a live `blake3::Hasher`, which is around
    /// two kilobytes: this catalog is cloned with every [`CompiledCatalog`], and it
    /// reaches [`crate::fsm::policy::TransformOp`] through a compiled transform
    /// guard, where the hasher made one enum variant dwarf its siblings.
    ///
    /// [`CompiledCatalog`]: super::CompiledCatalog
    digest: [u8; 32],
}

/// Chains one predicate's definition into a catalog digest.
///
/// The source [`AttrValue`]s rather than the compiled text, because a
/// [`RegexId`] is assigned by first appearance and so says nothing about the
/// pattern it stands for.
fn absorb_predicate(state: &mut [u8; 32], attrs: &[SymbolicAttrValue]) {
    let digest = &mut blake3::Hasher::new();
    digest.update(state);
    absorb_len(digest, attrs.len());
    for attr in attrs {
        absorb_len(digest, attr.attr.as_bytes().len());
        digest.update(attr.attr.as_bytes());
        digest.update(&[u8::from(attr.complement)]);
        // Exhaustive on purpose: a new operator must choose a tag here or fail
        // to compile, rather than digest identically to its neighbour.
        match &attr.value {
            AttrValueType::Single(value) => absorb_operands(digest, 0, [value.as_str()]),
            AttrValueType::OneOf(values) => {
                absorb_operands(digest, 1, values.iter().map(CompactString::as_str));
            }
            AttrValueType::NotIn(values) => {
                absorb_operands(digest, 2, values.iter().map(CompactString::as_str));
            }
            AttrValueType::SetHas(value) => absorb_operands(digest, 7, [value.as_str()]),
            AttrValueType::SetHasAny(values) => {
                absorb_operands(digest, 8, values.iter().map(CompactString::as_str));
            }
            AttrValueType::SetNotHas(value) => absorb_operands(digest, 9, [value.as_str()]),
            AttrValueType::SetNotHasAny(values) => {
                absorb_operands(digest, 10, values.iter().map(CompactString::as_str));
            }
            AttrValueType::Contains(value) => absorb_operands(digest, 3, [value.as_str()]),
            AttrValueType::NotContains(value) => absorb_operands(digest, 4, [value.as_str()]),
            AttrValueType::Regex(regex) => absorb_operands(digest, 5, [regex.as_str()]),
            AttrValueType::NotRegex(regex) => absorb_operands(digest, 6, [regex.as_str()]),
        }
    }
    *state = *digest.finalize().as_bytes();
}

/// Absorbs one operator tag and its operands, length-framed.
///
/// Every variable-length part is preceded by its length, so no two distinct
/// operand lists share an encoding — `{ab, c}` and `{a, bc}` do not collide.
fn absorb_operands<'a, I>(digest: &mut blake3::Hasher, tag: u8, operands: I)
where
    I: IntoIterator<Item = &'a str>,
    I::IntoIter: ExactSizeIterator,
{
    let operands = operands.into_iter();
    digest.update(&[tag]);
    absorb_len(digest, operands.len());
    for operand in operands {
        absorb_len(digest, operand.len());
        digest.update(operand.as_bytes());
    }
}

fn absorb_len(digest: &mut blake3::Hasher, len: usize) {
    digest.update(
        &u64::try_from(len)
            .expect("digested length fits in u64")
            .to_le_bytes(),
    );
}

pub(crate) struct PredicateEvaluator<'catalog> {
    catalog: &'catalog PredicateCatalog,
}

#[derive(Clone, Copy, Default)]
struct MemoCell(u64);

const MATCHED_BIT: u64 = 1 << 63;
const MAX_CALL_GENERATION: u32 = u32::MAX >> 1;

#[cfg(feature = "bench-instrumentation")]
#[derive(Default)]
struct PredicateCounters {
    calls: Cell<u64>,
    memo_hits: Cell<u64>,
    evaluations: Cell<u64>,
}

pub(super) const PREDICATE_SITE_DENSE_MAIN: usize = 0;
pub(super) const PREDICATE_SITE_DENSE_MATCH_CLOSURE: usize = 1;
pub(super) const PREDICATE_SITE_DENSE_ADVANCE: usize = 2;
pub(super) const PREDICATE_SITE_DENSE_RETRY_ADVANCE: usize = 3;
pub(super) const PREDICATE_SITE_DENSE_RETRY_EXTEND: usize = 4;
const PREDICATE_SITE_SPAWN: usize = 5;
const PREDICATE_SITE_NFA_PREVIOUS_TOKEN: usize = 6;
const PREDICATE_SITE_ASSERTION_CONSUME: usize = 7;
const PREDICATE_SITE_ASSERTION_PREVIOUS_TOKEN: usize = 8;
pub(super) const PREDICATE_SITE_CAPTURE_TOKEN: usize = 9;
pub(super) const PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN: usize = 10;
pub(super) const PREDICATE_SITE_EVALUATED_TOKEN: usize = 11;
pub(super) const PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN: usize = 12;
const PREDICATE_SITE_UNCLASSIFIED: usize = 13;
const PREDICATE_SITE_COUNT: usize = 14;
const _: [(); PREDICATE_SITE_COUNT] = [(); PREDICATE_SITE_UNCLASSIFIED + 1];

#[cfg(feature = "bench-instrumentation")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PredicateMetrics {
    pub(crate) predicate_calls: u64,
    pub(crate) predicate_memo_hits: u64,
    pub(crate) predicate_evaluations: u64,
    pub(crate) predicate_distinct_pairs: u64,
    pub(crate) predicate_recomputations: u64,
    pub(crate) candidates_produced: u64,
    pub(crate) candidates_retained: u64,
    pub(crate) dense_predicate_calls: u64,
    pub(crate) dense_predicate_memo_hits: u64,
    pub(crate) dense_predicate_evaluations: u64,
    pub(crate) dense_main_transition_predicate_calls: u64,
    pub(crate) dense_match_closure_predicate_calls: u64,
    pub(crate) dense_advance_validation_predicate_calls: u64,
    pub(crate) dense_retry_advance_validation_predicate_calls: u64,
    pub(crate) dense_retry_extend_validation_predicate_calls: u64,
    pub(crate) spawn_predicate_calls: u64,
    pub(crate) nfa_previous_token_predicate_calls: u64,
    pub(crate) assertion_consume_predicate_calls: u64,
    pub(crate) assertion_previous_token_predicate_calls: u64,
    pub(crate) capture_token_predicate_calls: u64,
    pub(crate) capture_previous_token_predicate_calls: u64,
    pub(crate) evaluated_token_predicate_calls: u64,
    pub(crate) evaluated_previous_token_predicate_calls: u64,
    pub(crate) unclassified_predicate_calls: u64,
}

#[derive(Default)]
struct PredicateScratch {
    cells: Vec<Cell<MemoCell>>,
    call_generation: u32,
    #[cfg(feature = "bench-instrumentation")]
    predicate_counters: [PredicateCounters; PREDICATE_SITE_COUNT],
    #[cfg(feature = "bench-instrumentation")]
    seen_generations: RefCell<Vec<Vec<u32>>>,
    #[cfg(feature = "bench-instrumentation")]
    distinct_pairs: Cell<u64>,
    /// Candidates constructed by the tree evaluator, before deduplication.
    ///
    /// Lives here rather than in `program.rs` because the session is the one
    /// thing already threaded through every `evaluate` frame, and a counter
    /// the walk has to carry is a counter the walk's signature has to change
    /// for.
    #[cfg(feature = "bench-instrumentation")]
    candidates_produced: Cell<u64>,
    #[cfg(feature = "bench-instrumentation")]
    candidates_retained: Cell<u64>,
    #[cfg(feature = "bench-instrumentation")]
    spawn_candidates: RefCell<SpawnCandidateScratch>,
}

#[cfg(feature = "bench-instrumentation")]
#[derive(Default)]
struct SpawnCandidateScratch {
    enabled: bool,
    spawn_positions: u64,
    actual_spawn_calls: u64,
    scheme_a_candidate_calls: u64,
    scheme_b_candidate_calls: u64,
    scheme_a_avoided_calls: u64,
    scheme_b_avoided_calls: u64,
    scheme_a_width_sum: u64,
    scheme_b_width_sum: u64,
    scheme_a_max_width: usize,
    scheme_b_max_width: usize,
    scheme_a_undiscriminated_calls: u64,
    scheme_b_undiscriminated_calls: u64,
    scheme_a_finite_atom_checks: u64,
    scheme_b_finite_atom_checks: u64,
    current_a_width: usize,
    current_b_width: usize,
    scheme_a_width_histogram: Vec<u64>,
    scheme_b_width_histogram: Vec<u64>,
    shape_effects: Vec<SpawnCandidateShapeEffect>,
}

/// One-call view of the shared predicate catalog and its reusable memo.
///
/// Each predicate owns one cell stamped with both call generation and token
/// position. Looking up a different position is a normal miss, so callers may
/// move arbitrarily through the token slice without relying on a scheduler
/// convention for correctness.
pub(crate) struct PredicateSession<'catalog> {
    catalog: &'catalog PredicateCatalog,
    scratch: Rc<PredicateScratch>,
    memoize: bool,
}

impl Clone for PredicateSession<'_> {
    fn clone(&self) -> Self {
        Self {
            catalog: self.catalog,
            scratch: Rc::clone(&self.scratch),
            memoize: self.memoize,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CompiledPositionAssertion {
    SentenceStart,
    SentenceEnd,
    PreviousToken(PredicateId),
    Lookaround(AssertionOccurrenceId),
}

#[derive(Default)]
struct LocalRegexes {
    owner: Option<Arc<()>>,
    regexes: Vec<Option<regex::Regex>>,
}

thread_local! {
    static LOCAL_REGEXES: RefCell<LocalRegexes> = RefCell::new(LocalRegexes::default());
    static PREDICATE_SCRATCH: RefCell<PredicateScratch> =
        RefCell::new(PredicateScratch::default());
}

impl PredicateScratch {
    fn begin_call(&mut self, predicate_count: usize) {
        self.call_generation = self.call_generation.wrapping_add(1);
        if self.call_generation == 0 || self.call_generation > MAX_CALL_GENERATION {
            for cell in &self.cells {
                cell.set(MemoCell::default());
            }
            #[cfg(feature = "bench-instrumentation")]
            for positions in self.seen_generations.get_mut() {
                positions.fill(0);
            }
            self.call_generation = 1;
        }
        if self.cells.len() < predicate_count {
            self.cells
                .resize_with(predicate_count, || Cell::new(MemoCell::default()));
        }
        #[cfg(feature = "bench-instrumentation")]
        {
            for counters in &self.predicate_counters {
                counters.calls.set(0);
                counters.memo_hits.set(0);
                counters.evaluations.set(0);
            }
            self.distinct_pairs.set(0);
            self.candidates_produced.set(0);
            self.candidates_retained.set(0);
            self.seen_generations
                .get_mut()
                .resize_with(predicate_count, Vec::new);
            self.spawn_candidates.get_mut().enabled = false;
        }
    }
}

impl<'catalog> PredicateSession<'catalog> {
    pub(crate) fn new(catalog: &'catalog PredicateCatalog) -> Self {
        let mut scratch = PREDICATE_SCRATCH.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        scratch.begin_call(catalog.len());
        Self {
            catalog,
            scratch: Rc::new(scratch),
            memoize: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_unmemoized(catalog: &'catalog PredicateCatalog) -> Self {
        let mut session = Self::new(catalog);
        session.memoize = false;
        session
    }

    #[inline]
    #[cfg(test)]
    pub(crate) fn matches_at<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_impl::<PREDICATE_SITE_UNCLASSIFIED, S>(predicate, tokens, position)
    }

    #[inline]
    #[cfg(all(test, feature = "bench-instrumentation"))]
    pub(crate) fn matches_at_dense_main<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_DENSE_MAIN, S>(predicate, tokens, position)
    }

    #[inline]
    #[cfg(all(test, feature = "bench-instrumentation"))]
    pub(crate) fn matches_at_dense_match_closure<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_DENSE_MATCH_CLOSURE, S>(predicate, tokens, position)
    }

    #[inline]
    #[cfg(all(test, feature = "bench-instrumentation"))]
    pub(crate) fn matches_at_dense_advance<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_DENSE_ADVANCE, S>(predicate, tokens, position)
    }

    #[inline]
    #[cfg(all(test, feature = "bench-instrumentation"))]
    pub(crate) fn matches_at_dense_retry_advance<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_DENSE_RETRY_ADVANCE, S>(predicate, tokens, position)
    }

    #[inline]
    #[cfg(all(test, feature = "bench-instrumentation"))]
    pub(crate) fn matches_at_dense_retry_extend<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_DENSE_RETRY_EXTEND, S>(predicate, tokens, position)
    }

    #[inline]
    pub(crate) fn matches_at_spawn<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_SPAWN, S>(predicate, tokens, position)
    }

    #[inline]
    pub(crate) fn matches_at_nfa_previous_token<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_NFA_PREVIOUS_TOKEN, S>(predicate, tokens, position)
    }

    /// Whether the spawn candidate census is accumulating on this session.
    ///
    /// The census asks how many of the calls the executor *would* make a
    /// candidate index could avoid, so it must observe the undispatched call
    /// population. `inject_spawns` reads this to stand the dispatcher down for
    /// the duration; matches are identical either way, and only counts differ.
    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn measuring_spawn_candidates(&self) -> bool {
        self.scratch.spawn_candidates.borrow().enabled
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn begin_spawn_candidate_measurement(&self, group_count: usize, shape_count: usize) {
        let mut scratch = self.scratch.spawn_candidates.borrow_mut();
        scratch.enabled = true;
        scratch.spawn_positions = 0;
        scratch.actual_spawn_calls = 0;
        scratch.scheme_a_candidate_calls = 0;
        scratch.scheme_b_candidate_calls = 0;
        scratch.scheme_a_avoided_calls = 0;
        scratch.scheme_b_avoided_calls = 0;
        scratch.scheme_a_width_sum = 0;
        scratch.scheme_b_width_sum = 0;
        scratch.scheme_a_max_width = 0;
        scratch.scheme_b_max_width = 0;
        scratch.scheme_a_undiscriminated_calls = 0;
        scratch.scheme_b_undiscriminated_calls = 0;
        scratch.scheme_a_finite_atom_checks = 0;
        scratch.scheme_b_finite_atom_checks = 0;
        scratch.current_a_width = 0;
        scratch.current_b_width = 0;
        let width_count = group_count
            .checked_add(1)
            .expect("spawn group histogram length fits in usize");
        scratch.scheme_a_width_histogram.resize(width_count, 0);
        scratch.scheme_a_width_histogram.truncate(width_count);
        scratch.scheme_a_width_histogram.fill(0);
        scratch.scheme_b_width_histogram.resize(width_count, 0);
        scratch.scheme_b_width_histogram.truncate(width_count);
        scratch.scheme_b_width_histogram.fill(0);
        scratch
            .shape_effects
            .resize_with(shape_count, SpawnCandidateShapeEffect::default);
        scratch.shape_effects.truncate(shape_count);
        for (shape_id, effect) in scratch.shape_effects.iter_mut().enumerate() {
            *effect = SpawnCandidateShapeEffect {
                shape_id,
                ..SpawnCandidateShapeEffect::default()
            };
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn begin_spawn_position(&self) {
        let mut scratch = self.scratch.spawn_candidates.borrow_mut();
        if !scratch.enabled {
            return;
        }
        checked_increment(&mut scratch.spawn_positions, "spawn positions");
        scratch.current_a_width = 0;
        scratch.current_b_width = 0;
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn record_spawn_candidate<T: TokenInfoTrait>(
        &self,
        descriptor: &SpawnPredicateDescriptor,
        shape_id: usize,
        token: &T,
    ) {
        let mut scratch = self.scratch.spawn_candidates.borrow_mut();
        if !scratch.enabled {
            return;
        }
        checked_increment(&mut scratch.actual_spawn_calls, "actual spawn calls");
        checked_increment(
            &mut scratch.shape_effects[shape_id].actual_calls,
            "shape actual calls",
        );

        let scheme_a_matches = descriptor.scheme_a.as_ref().is_none_or(|atom| {
            checked_increment(
                &mut scratch.scheme_a_finite_atom_checks,
                "Scheme A finite-atom checks",
            );
            finite_atom_matches(atom, token)
        });
        if descriptor.scheme_a.is_none() {
            checked_increment(
                &mut scratch.scheme_a_undiscriminated_calls,
                "Scheme A undiscriminated calls",
            );
        }
        if scheme_a_matches {
            checked_increment(
                &mut scratch.scheme_a_candidate_calls,
                "Scheme A candidate calls",
            );
            scratch.current_a_width = scratch
                .current_a_width
                .checked_add(1)
                .expect("Scheme A width fits in usize");
        } else {
            checked_increment(
                &mut scratch.scheme_a_avoided_calls,
                "Scheme A avoided calls",
            );
            checked_increment(
                &mut scratch.shape_effects[shape_id].scheme_a_avoided_calls,
                "shape Scheme A avoided calls",
            );
        }

        let mut scheme_b_matches = true;
        if descriptor.finite_atoms.is_empty() {
            checked_increment(
                &mut scratch.scheme_b_undiscriminated_calls,
                "Scheme B undiscriminated calls",
            );
        } else {
            for atom in &descriptor.finite_atoms {
                checked_increment(
                    &mut scratch.scheme_b_finite_atom_checks,
                    "Scheme B finite-atom checks",
                );
                if !finite_atom_matches(atom, token) {
                    scheme_b_matches = false;
                    break;
                }
            }
        }
        if scheme_b_matches {
            checked_increment(
                &mut scratch.scheme_b_candidate_calls,
                "Scheme B candidate calls",
            );
            scratch.current_b_width = scratch
                .current_b_width
                .checked_add(1)
                .expect("Scheme B width fits in usize");
        } else {
            checked_increment(
                &mut scratch.scheme_b_avoided_calls,
                "Scheme B avoided calls",
            );
            checked_increment(
                &mut scratch.shape_effects[shape_id].scheme_b_avoided_calls,
                "shape Scheme B avoided calls",
            );
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn finish_spawn_position(&self) {
        let mut scratch = self.scratch.spawn_candidates.borrow_mut();
        if !scratch.enabled {
            return;
        }
        let current_a_width = scratch.current_a_width;
        let current_b_width = scratch.current_b_width;
        checked_add_metric(
            &mut scratch.scheme_a_width_sum,
            u64::try_from(current_a_width).expect("Scheme A width fits in u64"),
            "Scheme A width sum",
        );
        checked_add_metric(
            &mut scratch.scheme_b_width_sum,
            u64::try_from(current_b_width).expect("Scheme B width fits in u64"),
            "Scheme B width sum",
        );
        scratch.scheme_a_max_width = scratch.scheme_a_max_width.max(current_a_width);
        scratch.scheme_b_max_width = scratch.scheme_b_max_width.max(current_b_width);
        checked_increment(
            &mut scratch.scheme_a_width_histogram[current_a_width],
            "Scheme A width histogram",
        );
        checked_increment(
            &mut scratch.scheme_b_width_histogram[current_b_width],
            "Scheme B width histogram",
        );
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn merge_spawn_candidate_metrics(
        &self,
        accumulator: &mut SpawnCandidateAccumulator,
    ) {
        let mut scratch = self.scratch.spawn_candidates.borrow_mut();
        if !scratch.enabled {
            return;
        }
        accumulator.prepare_dimensions(
            scratch.scheme_a_width_histogram.len(),
            scratch.shape_effects.len(),
        );
        checked_add_metric(
            &mut accumulator.spawn_positions,
            scratch.spawn_positions,
            "accumulated spawn positions",
        );
        checked_add_metric(
            &mut accumulator.actual_spawn_calls,
            scratch.actual_spawn_calls,
            "accumulated actual spawn calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_a_candidate_calls,
            scratch.scheme_a_candidate_calls,
            "accumulated Scheme A candidate calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_b_candidate_calls,
            scratch.scheme_b_candidate_calls,
            "accumulated Scheme B candidate calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_a_avoided_calls,
            scratch.scheme_a_avoided_calls,
            "accumulated Scheme A avoided calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_b_avoided_calls,
            scratch.scheme_b_avoided_calls,
            "accumulated Scheme B avoided calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_a_width_sum,
            scratch.scheme_a_width_sum,
            "accumulated Scheme A width sum",
        );
        checked_add_metric(
            &mut accumulator.scheme_b_width_sum,
            scratch.scheme_b_width_sum,
            "accumulated Scheme B width sum",
        );
        accumulator.scheme_a_max_width = accumulator
            .scheme_a_max_width
            .max(scratch.scheme_a_max_width);
        accumulator.scheme_b_max_width = accumulator
            .scheme_b_max_width
            .max(scratch.scheme_b_max_width);
        checked_add_metric(
            &mut accumulator.scheme_a_undiscriminated_calls,
            scratch.scheme_a_undiscriminated_calls,
            "accumulated Scheme A undiscriminated calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_b_undiscriminated_calls,
            scratch.scheme_b_undiscriminated_calls,
            "accumulated Scheme B undiscriminated calls",
        );
        checked_add_metric(
            &mut accumulator.scheme_a_finite_atom_checks,
            scratch.scheme_a_finite_atom_checks,
            "accumulated Scheme A finite-atom checks",
        );
        checked_add_metric(
            &mut accumulator.scheme_b_finite_atom_checks,
            scratch.scheme_b_finite_atom_checks,
            "accumulated Scheme B finite-atom checks",
        );
        for (total, sentence) in accumulator
            .scheme_a_width_histogram
            .iter_mut()
            .zip(&scratch.scheme_a_width_histogram)
        {
            checked_add_metric(total, *sentence, "accumulated Scheme A histogram");
        }
        for (total, sentence) in accumulator
            .scheme_b_width_histogram
            .iter_mut()
            .zip(&scratch.scheme_b_width_histogram)
        {
            checked_add_metric(total, *sentence, "accumulated Scheme B histogram");
        }
        for (total, sentence) in accumulator
            .shape_effects
            .iter_mut()
            .zip(&scratch.shape_effects)
        {
            checked_add_metric(
                &mut total.actual_calls,
                sentence.actual_calls,
                "accumulated shape actual calls",
            );
            checked_add_metric(
                &mut total.scheme_a_avoided_calls,
                sentence.scheme_a_avoided_calls,
                "accumulated shape Scheme A avoided calls",
            );
            checked_add_metric(
                &mut total.scheme_b_avoided_calls,
                sentence.scheme_b_avoided_calls,
                "accumulated shape Scheme B avoided calls",
            );
        }
        scratch.enabled = false;
    }

    #[inline]
    pub(crate) fn matches_at_assertion_consume<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_ASSERTION_CONSUME, S>(predicate, tokens, position)
    }

    #[inline]
    pub(crate) fn matches_at_assertion_previous_token<S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_site::<PREDICATE_SITE_ASSERTION_PREVIOUS_TOKEN, S>(
            predicate, tokens, position,
        )
    }

    #[inline]
    pub(super) fn matches_at_site<const SITE: usize, S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        self.matches_at_impl::<SITE, S>(predicate, tokens, position)
    }

    #[inline]
    fn matches_at_impl<const SITE: usize, S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        #[cfg(feature = "bench-instrumentation")]
        self.increment(&self.counters::<SITE>().calls);
        if !self.memoize {
            return self.evaluate::<SITE, S>(predicate, tokens, position);
        }
        let Some(stamp) = u32::try_from(position)
            .ok()
            .map(|position| (u64::from(self.scratch.call_generation) << 32) | u64::from(position))
        else {
            return self.evaluate::<SITE, S>(predicate, tokens, position);
        };
        let index = predicate.index();
        let cell = self.scratch.cells[index].get().0;
        if cell & !MATCHED_BIT == stamp {
            #[cfg(feature = "bench-instrumentation")]
            self.increment(&self.counters::<SITE>().memo_hits);
            return cell & MATCHED_BIT != 0;
        }
        let matched = self.evaluate::<SITE, S>(predicate, tokens, position);
        self.scratch.cells[index].set(MemoCell(stamp | if matched { MATCHED_BIT } else { 0 }));
        matched
    }

    #[inline]
    fn evaluate<const SITE: usize, S: TokenSequence + ?Sized>(
        &self,
        predicate: PredicateId,
        tokens: &S,
        position: usize,
    ) -> bool {
        #[cfg(feature = "bench-instrumentation")]
        self.record_evaluation::<SITE>(predicate, position);
        let token = SequenceToken::new(tokens, position)
            .expect("predicate position is inside the token sequence");
        self.catalog.matches(predicate, &token)
    }

    #[cfg(feature = "bench-instrumentation")]
    fn counters<const SITE: usize>(&self) -> &PredicateCounters {
        &self.scratch.predicate_counters[SITE]
    }

    #[cfg(feature = "bench-instrumentation")]
    fn increment(&self, counter: &Cell<u64>) {
        counter.set(
            counter
                .get()
                .checked_add(1)
                .expect("predicate metric fits in u64"),
        );
    }

    /// Record one tree-evaluator node's candidate volume.
    ///
    /// `produced` is what the node constructed and `retained` what survived
    /// deduplication, which is the pair the deduplication key controls: a
    /// wider key raises `retained` toward `produced`, and `produced` is also
    /// the slot-vector clone count the wide key pays. Counted per `evaluate`
    /// frame rather than per match, so a node visited under a repetition
    /// contributes once per visit — the quantity a candidate-growth regression
    /// would move.
    #[cfg(feature = "bench-instrumentation")]
    pub(super) fn record_candidates(&self, produced: usize, retained: usize) {
        for (counter, amount) in [
            (&self.scratch.candidates_produced, produced),
            (&self.scratch.candidates_retained, retained),
        ] {
            counter.set(
                counter
                    .get()
                    .checked_add(u64::try_from(amount).expect("a candidate count fits in u64"))
                    .expect("candidate volume fits in u64"),
            );
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    fn record_evaluation<const SITE: usize>(&self, predicate: PredicateId, position: usize) {
        self.increment(&self.counters::<SITE>().evaluations);
        let required_len = position
            .checked_add(1)
            .expect("a valid token index has a successor");
        let mut seen_generations = self.scratch.seen_generations.borrow_mut();
        let positions = &mut seen_generations[predicate.index()];
        if positions.len() < required_len {
            positions.resize(required_len, 0);
        }
        if positions[position] != self.scratch.call_generation {
            positions[position] = self.scratch.call_generation;
            self.increment(&self.scratch.distinct_pairs);
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    pub(crate) fn metrics(&self) -> PredicateMetrics {
        let mut calls = 0u64;
        let mut memo_hits = 0u64;
        let mut evaluations = 0u64;
        for counters in &self.scratch.predicate_counters {
            calls = calls
                .checked_add(counters.calls.get())
                .expect("predicate call total fits in u64");
            memo_hits = memo_hits
                .checked_add(counters.memo_hits.get())
                .expect("predicate memo-hit total fits in u64");
            evaluations = evaluations
                .checked_add(counters.evaluations.get())
                .expect("predicate evaluation total fits in u64");
        }
        let mut dense_calls = 0u64;
        let mut dense_memo_hits = 0u64;
        let mut dense_evaluations = 0u64;
        for counters in &self.scratch.predicate_counters[..=PREDICATE_SITE_DENSE_RETRY_EXTEND] {
            dense_calls = dense_calls
                .checked_add(counters.calls.get())
                .expect("dense predicate call total fits in u64");
            dense_memo_hits = dense_memo_hits
                .checked_add(counters.memo_hits.get())
                .expect("dense predicate memo-hit total fits in u64");
            dense_evaluations = dense_evaluations
                .checked_add(counters.evaluations.get())
                .expect("dense predicate evaluation total fits in u64");
        }
        let site_calls = |site: usize| self.scratch.predicate_counters[site].calls.get();
        let distinct = self.scratch.distinct_pairs.get();
        PredicateMetrics {
            predicate_calls: calls,
            predicate_memo_hits: memo_hits,
            predicate_evaluations: evaluations,
            predicate_distinct_pairs: distinct,
            predicate_recomputations: evaluations
                .checked_sub(distinct)
                .expect("distinct pairs are a subset of evaluations"),
            candidates_produced: self.scratch.candidates_produced.get(),
            candidates_retained: self.scratch.candidates_retained.get(),
            dense_predicate_calls: dense_calls,
            dense_predicate_memo_hits: dense_memo_hits,
            dense_predicate_evaluations: dense_evaluations,
            dense_main_transition_predicate_calls: site_calls(PREDICATE_SITE_DENSE_MAIN),
            dense_match_closure_predicate_calls: site_calls(PREDICATE_SITE_DENSE_MATCH_CLOSURE),
            dense_advance_validation_predicate_calls: site_calls(PREDICATE_SITE_DENSE_ADVANCE),
            dense_retry_advance_validation_predicate_calls: site_calls(
                PREDICATE_SITE_DENSE_RETRY_ADVANCE,
            ),
            dense_retry_extend_validation_predicate_calls: site_calls(
                PREDICATE_SITE_DENSE_RETRY_EXTEND,
            ),
            spawn_predicate_calls: site_calls(PREDICATE_SITE_SPAWN),
            nfa_previous_token_predicate_calls: site_calls(PREDICATE_SITE_NFA_PREVIOUS_TOKEN),
            assertion_consume_predicate_calls: site_calls(PREDICATE_SITE_ASSERTION_CONSUME),
            assertion_previous_token_predicate_calls: site_calls(
                PREDICATE_SITE_ASSERTION_PREVIOUS_TOKEN,
            ),
            capture_token_predicate_calls: site_calls(PREDICATE_SITE_CAPTURE_TOKEN),
            capture_previous_token_predicate_calls: site_calls(
                PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN,
            ),
            evaluated_token_predicate_calls: site_calls(PREDICATE_SITE_EVALUATED_TOKEN),
            evaluated_previous_token_predicate_calls: site_calls(
                PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN,
            ),
            unclassified_predicate_calls: site_calls(PREDICATE_SITE_UNCLASSIFIED),
        }
    }
}

impl Drop for PredicateSession<'_> {
    fn drop(&mut self) {
        if let Some(scratch) = Rc::get_mut(&mut self.scratch) {
            let scratch = std::mem::take(scratch);
            PREDICATE_SCRATCH.with(|cell| *cell.borrow_mut() = scratch);
        }
    }
}

impl PredicateCatalog {
    pub(crate) fn len(&self) -> usize {
        self.predicates.len()
    }

    pub(crate) fn digest(&self) -> PredicateDigest {
        PredicateDigest(self.digest)
    }

    /// The value kinds this catalog's operators require of a bound schema.
    ///
    /// `~` and `!~` are **absent**, and that is the whole of their kind
    /// polymorphism: a containment or regex test reads a string, and a
    /// string-set attribute is a set of strings, so the operator has an
    /// answer over either physical kind and requires neither. Every other
    /// operator names one kind and a schema declaring the other is a bind-time
    /// refusal.
    pub(crate) fn required_kinds(&self) -> Vec<(AttributeKey, AttributeKind)> {
        self.predicates
            .iter()
            .flat_map(|predicate| match predicate {
                CompiledPredicate::Empty => Vec::new(),
                CompiledPredicate::Single(attribute) => vec![attribute],
                CompiledPredicate::Multiple(attributes) => attributes.iter().collect(),
            })
            .filter_map(|attribute| {
                let kind = match attribute.value {
                    CompiledValue::SetHas(_)
                    | CompiledValue::SetHasAny(_)
                    | CompiledValue::SetNotHas(_)
                    | CompiledValue::SetNotHasAny(_) => AttributeKind::StringSet,
                    CompiledValue::Single(_)
                    | CompiledValue::OneOf(_)
                    | CompiledValue::NotIn(_) => AttributeKind::Scalar,
                    CompiledValue::Contains(_)
                    | CompiledValue::NotContains(_)
                    | CompiledValue::Regex(_)
                    | CompiledValue::NotRegex(_) => return None,
                };
                Some((attribute.attr.key().clone(), kind))
            })
            .collect()
    }

    pub(crate) fn bind_attributes(&mut self, projection: &BoundProjection) {
        for predicate in &mut self.predicates {
            let attributes: &mut [CompiledAttribute] = match predicate {
                CompiledPredicate::Empty => continue,
                CompiledPredicate::Single(attribute) => std::slice::from_mut(attribute),
                CompiledPredicate::Multiple(attributes) => attributes,
            };
            for attribute in attributes {
                // A key the schema does not carry stays unbound: the bound
                // execution path answers an unbound locator with `None`, which
                // is exactly a token that lacks the attribute.
                if let Some(ordinal) = projection.ordinal_for_key(attribute.attr.key()) {
                    attribute.attr.bind(ordinal);
                }
            }
        }
    }

    /// Classify one compiled predicate's finite-positive atoms.
    ///
    /// Both the sort and the `OneOf` operand sort are load-bearing: they make
    /// the canonical `scheme_a` selection, and so the dispatcher's index, a
    /// function of the compiled predicate rather than of its source order.
    pub(crate) fn spawn_candidate_descriptor(&self, id: PredicateId) -> SpawnPredicateDescriptor {
        let attributes: &[CompiledAttribute] = match &self.predicates[id.index()] {
            CompiledPredicate::Empty => &[],
            CompiledPredicate::Single(attribute) => std::slice::from_ref(attribute),
            CompiledPredicate::Multiple(attributes) => attributes,
        };
        let mut finite_atoms = Vec::new();
        let mut shape_parts = Vec::with_capacity(attributes.len());
        for attribute in attributes {
            let operator = compiled_value_operator(&attribute.value);
            shape_parts.push(format!(
                "{}:{}:{}",
                attribute.attr.key().as_str(),
                operator.as_str(),
                if attribute.complement {
                    "complement"
                } else {
                    "positive"
                }
            ));
            if attribute.complement {
                continue;
            }
            let values = match &attribute.value {
                CompiledValue::Single(value) => Some(vec![value.clone()]),
                CompiledValue::OneOf(values) => Some(values.to_vec()),
                CompiledValue::NotIn(_)
                | CompiledValue::SetHas(_)
                | CompiledValue::SetHasAny(_)
                | CompiledValue::SetNotHas(_)
                | CompiledValue::SetNotHasAny(_)
                | CompiledValue::Contains(_)
                | CompiledValue::NotContains(_)
                | CompiledValue::Regex(_)
                | CompiledValue::NotRegex(_) => None,
            };
            if let Some(mut values) = values {
                values.sort();
                values.dedup();
                finite_atoms.push(FinitePositiveAtom {
                    attribute: attribute.attr.clone(),
                    operator,
                    values: values.into_boxed_slice(),
                });
            }
        }
        finite_atoms.sort_by(|left, right| {
            (&left.attribute, &left.values, &left.operator).cmp(&(
                &right.attribute,
                &right.values,
                &right.operator,
            ))
        });
        shape_parts.sort();
        SpawnPredicateDescriptor {
            scheme_a: finite_atoms.first().cloned(),
            finite_atoms: finite_atoms.into_boxed_slice(),
            shape_signature: if shape_parts.is_empty() {
                "empty".to_owned()
            } else {
                shape_parts.join("+")
            },
        }
    }

    pub(crate) fn intern(&mut self, attrs: &[SymbolicAttrValue]) -> PredicateId {
        if let Some(index) = self
            .predicates
            .iter()
            .position(|predicate| predicate.definition_eq(attrs, &self.regexes))
        {
            return PredicateId(u32::try_from(index).expect("predicate index fits in u32"));
        }

        let id = PredicateId(
            u32::try_from(self.predicates.len()).expect("predicate catalog length fits in u32"),
        );
        absorb_predicate(&mut self.digest, attrs);
        let compiled = attrs
            .iter()
            .map(|attr| CompiledAttribute {
                attr: AttributeLocator::symbolic(attr.attr.clone()),
                complement: attr.complement,
                value: match &attr.value {
                    AttrValueType::Single(value) => CompiledValue::Single(value.clone()),
                    // Copied out of the shared list rather than sharing it:
                    // `intern` dedupes, so identical predicates collapse to one
                    // entry however many sites reference the set, and the
                    // matcher never paid the M·N the binder did. Holding an
                    // `Arc` here would add atomic refcount traffic to the hot
                    // evaluation path without sharing additional values.
                    AttrValueType::OneOf(values) => {
                        CompiledValue::OneOf(values.to_vec().into_boxed_slice())
                    }
                    AttrValueType::NotIn(values) => {
                        CompiledValue::NotIn(values.to_vec().into_boxed_slice())
                    }
                    AttrValueType::SetHas(value) => CompiledValue::SetHas(value.clone()),
                    AttrValueType::SetHasAny(values) => {
                        CompiledValue::SetHasAny(values.to_vec().into_boxed_slice())
                    }
                    AttrValueType::SetNotHas(value) => CompiledValue::SetNotHas(value.clone()),
                    AttrValueType::SetNotHasAny(values) => {
                        CompiledValue::SetNotHasAny(values.to_vec().into_boxed_slice())
                    }
                    AttrValueType::Contains(value) => CompiledValue::Contains(value.clone()),
                    AttrValueType::NotContains(value) => CompiledValue::NotContains(value.clone()),
                    AttrValueType::Regex(regex) => CompiledValue::Regex(self.intern_regex(regex)),
                    AttrValueType::NotRegex(regex) => {
                        CompiledValue::NotRegex(self.intern_regex(regex))
                    }
                },
            })
            .collect::<Vec<_>>();
        self.predicates.push(match compiled.len() {
            0 => CompiledPredicate::Empty,
            1 => CompiledPredicate::Single(
                compiled
                    .into_iter()
                    .next()
                    .expect("single compiled predicate has one attribute"),
            ),
            _ => CompiledPredicate::Multiple(compiled.into_boxed_slice()),
        });
        id
    }

    pub(crate) fn intern_position_assertion(
        &mut self,
        assertion: &PositionAssertion,
    ) -> CompiledPositionAssertion {
        match assertion {
            PositionAssertion::SentenceStart => CompiledPositionAssertion::SentenceStart,
            PositionAssertion::SentenceEnd => CompiledPositionAssertion::SentenceEnd,
            PositionAssertion::PreviousToken(predicate) => {
                CompiledPositionAssertion::PreviousToken(self.intern(predicate))
            }
            PositionAssertion::Lookaround(occurrence) => {
                CompiledPositionAssertion::Lookaround(*occurrence)
            }
        }
    }

    fn intern_regex(&mut self, regex: &regex::Regex) -> RegexId {
        if let Some(index) = self
            .regexes
            .iter()
            .position(|candidate| candidate.as_str() == regex.as_str())
        {
            return RegexId(u32::try_from(index).expect("regex index fits in u32"));
        }

        let id =
            RegexId(u32::try_from(self.regexes.len()).expect("regex catalog length fits in u32"));
        self.regexes.push(regex.clone());
        id
    }

    #[inline]
    pub(crate) fn matches<T: TokenInfoTrait>(&self, id: PredicateId, token: &T) -> bool {
        PredicateEvaluator { catalog: self }.matches(id, token)
    }

    fn regex_is_match(&self, id: RegexId, text: &str) -> bool {
        LOCAL_REGEXES.with(|local| {
            let mut local = local.borrow_mut();
            if !local
                .owner
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, &self.owner))
            {
                local.owner = Some(Arc::clone(&self.owner));
                local.regexes.clear();
            }
            if local.regexes.len() < self.regexes.len() {
                local.regexes.resize_with(self.regexes.len(), || None);
            }
            let slot = &mut local.regexes[id.index()];
            if slot.is_none() {
                *slot = Some(
                    regex::Regex::new(self.regexes[id.index()].as_str())
                        .expect("catalog regex was compiled with Regex::new"),
                );
            }
            slot.as_ref()
                .expect("local regex slot initialized")
                .is_match(text)
        })
    }
}

fn compiled_value_operator(value: &CompiledValue) -> SpawnAtomOperator {
    match value {
        CompiledValue::Single(_) => SpawnAtomOperator::Single,
        CompiledValue::OneOf(_) => SpawnAtomOperator::OneOf,
        CompiledValue::NotIn(_) => SpawnAtomOperator::NotIn,
        CompiledValue::SetHas(_) => SpawnAtomOperator::SetHas,
        CompiledValue::SetHasAny(_) => SpawnAtomOperator::SetHasAny,
        CompiledValue::SetNotHas(_) => SpawnAtomOperator::SetNotHas,
        CompiledValue::SetNotHasAny(_) => SpawnAtomOperator::SetNotHasAny,
        CompiledValue::Contains(_) => SpawnAtomOperator::Contains,
        CompiledValue::NotContains(_) => SpawnAtomOperator::NotContains,
        CompiledValue::Regex(_) => SpawnAtomOperator::Regex,
        CompiledValue::NotRegex(_) => SpawnAtomOperator::NotRegex,
    }
}

/// Whether `token` satisfies one finite-positive atom.
///
/// A token lacking the attribute satisfies neither polarity of any operator,
/// so it matches no `Single` or `OneOf` atom.
///
/// The production dispatcher never calls this: its index resolves membership
/// by lookup instead. It remains the census's measured predicate and the
/// reference the soundness test checks the index against, so it is compiled
/// only for tests and for the gated census.
#[cfg(any(test, feature = "bench-instrumentation"))]
pub(crate) fn finite_atom_matches<T: TokenInfoTrait>(atom: &FinitePositiveAtom, token: &T) -> bool {
    matches!(
        token.value_locator(&atom.attribute),
        Some(crate::token_match::AttributeValueRef::Scalar(actual))
            if atom.values.binary_search_by(|candidate| candidate.as_str().cmp(actual)).is_ok()
    )
}

#[cfg(feature = "bench-instrumentation")]
fn checked_increment(total: &mut u64, label: &str) {
    checked_add_metric(total, 1, label);
}

#[cfg(feature = "bench-instrumentation")]
fn checked_add_metric(total: &mut u64, value: u64, label: &str) {
    *total = total.checked_add(value).expect(label);
}

impl PredicateEvaluator<'_> {
    #[inline]
    fn matches<T: TokenInfoTrait>(&self, id: PredicateId, token: &T) -> bool {
        let predicate = &self.catalog.predicates[id.index()];
        match predicate {
            CompiledPredicate::Empty => true,
            CompiledPredicate::Single(attribute) => self.attribute_matches(attribute, token),
            CompiledPredicate::Multiple(attributes) => attributes
                .iter()
                .all(|attribute| self.attribute_matches(attribute, token)),
        }
    }

    #[inline]
    fn attribute_matches<T: TokenInfoTrait>(
        &self,
        expected: &CompiledAttribute,
        token: &T,
    ) -> bool {
        let Some(actual) = token.value_locator(&expected.attr) else {
            // The complement of "present and satisfying" is satisfied by
            // absence; an uncomplemented atom is not.
            return expected.complement;
        };
        let held = match &expected.value {
            CompiledValue::Single(value) => {
                matches!(actual, crate::token_match::AttributeValueRef::Scalar(actual) if actual == value)
            }
            CompiledValue::OneOf(values) => {
                matches!(actual, crate::token_match::AttributeValueRef::Scalar(actual) if values.iter().any(|value| value == actual))
            }
            CompiledValue::NotIn(values) => {
                matches!(actual, crate::token_match::AttributeValueRef::Scalar(actual) if values.iter().all(|value| value != actual))
            }
            // `~` and `!~` read whichever kind the schema declared. Over a
            // string set they are existential, exactly as `has` and `not has`
            // are: `~` holds when some entry satisfies the needle, `!~` when
            // none does. An absent attribute reached the early return above,
            // so neither polarity holds for it.
            CompiledValue::Contains(needle) => match actual {
                crate::token_match::AttributeValueRef::Scalar(actual) => {
                    actual.contains(needle.as_str())
                }
                crate::token_match::AttributeValueRef::StringSet(actual) => {
                    actual.iter().any(|entry| entry.contains(needle.as_str()))
                }
            },
            CompiledValue::NotContains(needle) => match actual {
                crate::token_match::AttributeValueRef::Scalar(actual) => {
                    !actual.contains(needle.as_str())
                }
                crate::token_match::AttributeValueRef::StringSet(actual) => {
                    actual.iter().all(|entry| !entry.contains(needle.as_str()))
                }
            },
            CompiledValue::Regex(regex_id) => match actual {
                crate::token_match::AttributeValueRef::Scalar(actual) => {
                    self.catalog.regex_is_match(*regex_id, actual)
                }
                crate::token_match::AttributeValueRef::StringSet(actual) => actual
                    .iter()
                    .any(|entry| self.catalog.regex_is_match(*regex_id, entry)),
            },
            CompiledValue::NotRegex(regex_id) => match actual {
                crate::token_match::AttributeValueRef::Scalar(actual) => {
                    !self.catalog.regex_is_match(*regex_id, actual)
                }
                crate::token_match::AttributeValueRef::StringSet(actual) => actual
                    .iter()
                    .all(|entry| !self.catalog.regex_is_match(*regex_id, entry)),
            },
            CompiledValue::SetHas(value) => {
                matches!(actual, crate::token_match::AttributeValueRef::StringSet(actual) if actual.iter().any(|member| member == value))
            }
            CompiledValue::SetHasAny(values) => {
                matches!(actual, crate::token_match::AttributeValueRef::StringSet(actual) if sorted_sets_intersect(actual, values))
            }
            CompiledValue::SetNotHas(value) => {
                matches!(actual, crate::token_match::AttributeValueRef::StringSet(actual) if actual.iter().all(|member| member != value))
            }
            CompiledValue::SetNotHasAny(values) => {
                matches!(actual, crate::token_match::AttributeValueRef::StringSet(actual) if !sorted_sets_intersect(actual, values))
            }
        };
        held != expected.complement
    }
}

fn sorted_sets_intersect(left: &[CompactString], right: &[CompactString]) -> bool {
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() && right_index < right.len() {
        match left[left_index]
            .as_bytes()
            .cmp(right[right_index].as_bytes())
        {
            std::cmp::Ordering::Less => left_index += 1,
            std::cmp::Ordering::Greater => right_index += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

impl CompiledPredicate {
    fn definition_eq(&self, attrs: &[SymbolicAttrValue], regexes: &[regex::Regex]) -> bool {
        match self {
            Self::Empty => attrs.is_empty(),
            Self::Single(compiled) => {
                attrs.len() == 1 && compiled.definition_eq(&attrs[0], regexes)
            }
            Self::Multiple(compiled) => {
                compiled.len() == attrs.len()
                    && compiled
                        .iter()
                        .zip(attrs)
                        .all(|(compiled, raw)| compiled.definition_eq(raw, regexes))
            }
        }
    }
}

impl CompiledAttribute {
    fn definition_eq(&self, raw: &SymbolicAttrValue, regexes: &[regex::Regex]) -> bool {
        self.attr.key() == &raw.attr
            && self.complement == raw.complement
            && match (&self.value, &raw.value) {
                (CompiledValue::Single(left), AttrValueType::Single(right)) => left == right,
                (CompiledValue::OneOf(left), AttrValueType::OneOf(right)) => {
                    left.as_ref() == right.as_ref()
                }
                (CompiledValue::NotIn(left), AttrValueType::NotIn(right)) => {
                    left.as_ref() == right.as_ref()
                }
                (CompiledValue::SetHas(left), AttrValueType::SetHas(right)) => left == right,
                (CompiledValue::SetHasAny(left), AttrValueType::SetHasAny(right)) => {
                    left.as_ref() == right.as_ref()
                }
                (CompiledValue::SetNotHas(left), AttrValueType::SetNotHas(right)) => left == right,
                (CompiledValue::SetNotHasAny(left), AttrValueType::SetNotHasAny(right)) => {
                    left.as_ref() == right.as_ref()
                }
                (CompiledValue::Contains(left), AttrValueType::Contains(right)) => left == right,
                (CompiledValue::NotContains(left), AttrValueType::NotContains(right)) => {
                    left == right
                }
                (CompiledValue::Regex(id), AttrValueType::Regex(right)) => {
                    regexes[id.index()].as_str() == right.as_str()
                }
                (CompiledValue::NotRegex(id), AttrValueType::NotRegex(right)) => {
                    regexes[id.index()].as_str() == right.as_str()
                }
                _ => false,
            }
    }
}

pub(crate) fn clear_local_regexes() {
    LOCAL_REGEXES.with(|local| {
        let mut local = local.borrow_mut();
        local.owner = None;
        local.regexes.clear();
        local.regexes.shrink_to_fit();
    });
}

pub(crate) fn clear_predicate_scratch() {
    PREDICATE_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        scratch.cells.clear();
        scratch.cells.shrink_to_fit();
        scratch.call_generation = 0;
    });
}

#[cfg(test)]
pub(crate) fn predicate_scratch_capacity() -> usize {
    PREDICATE_SCRATCH.with(|cell| cell.borrow().cells.capacity())
}

impl PredicateId {
    #[inline]
    pub(crate) fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 predicate id fits in usize")
    }
}

impl RegexId {
    #[inline]
    fn index(self) -> usize {
        usize::try_from(self.0).expect("u32 regex id fits in usize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "bench-instrumentation")]
    use crate::fsm::matcher::spawn_candidate_metrics::SpawnAtomOperator;
    use crate::token_match::{TokenAttributeIndex, TokenInfo, TokenInfoTrait};
    use compact_str::CompactString;

    fn attr(attr: TokenAttributeIndex, value: AttrValueType) -> SymbolicAttrValue {
        SymbolicAttrValue {
            attr: attr.into(),
            value,
            complement: false,
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    fn complemented_attr(
        attr: TokenAttributeIndex,
        value: AttrValueType,
        complement: bool,
    ) -> SymbolicAttrValue {
        SymbolicAttrValue {
            attr: attr.into(),
            value,
            complement,
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn finite_positive_classifier_is_exhaustive_and_canonical() {
        let cases = vec![
            (
                AttrValueType::Single("b".into()),
                SpawnAtomOperator::Single,
                true,
                "single",
            ),
            (
                AttrValueType::OneOf(Arc::from(vec!["b".into(), "a".into(), "b".into()])),
                SpawnAtomOperator::OneOf,
                true,
                "one_of",
            ),
            (
                AttrValueType::NotIn(Arc::from(vec!["a".into(), "b".into()])),
                SpawnAtomOperator::NotIn,
                false,
                "not_in",
            ),
            (
                AttrValueType::Contains("a".into()),
                SpawnAtomOperator::Contains,
                false,
                "contains",
            ),
            (
                AttrValueType::NotContains("a".into()),
                SpawnAtomOperator::NotContains,
                false,
                "not_contains",
            ),
            (
                AttrValueType::Regex(regex::Regex::new("a+").expect("fixture regex")),
                SpawnAtomOperator::Regex,
                false,
                "regex",
            ),
            (
                AttrValueType::NotRegex(regex::Regex::new("a+").expect("fixture regex")),
                SpawnAtomOperator::NotRegex,
                false,
                "not_regex",
            ),
        ];

        let mut catalog = PredicateCatalog::default();
        let empty = catalog.intern(&[]);
        let empty_descriptor = catalog.spawn_candidate_descriptor(empty);
        assert!(empty_descriptor.finite_atoms.is_empty());
        assert!(empty_descriptor.scheme_a.is_none());
        assert_eq!(empty_descriptor.shape_signature, "empty");

        for (value, operator, safe, operator_key) in cases {
            for complement in [false, true] {
                let id = catalog.intern(&[complemented_attr(
                    TokenAttributeIndex::Orth,
                    value.clone(),
                    complement,
                )]);
                let descriptor = catalog.spawn_candidate_descriptor(id);
                let expected_safe = usize::from(safe && !complement);
                assert_eq!(descriptor.finite_atoms.len(), expected_safe);
                assert_eq!(descriptor.scheme_a.is_some(), expected_safe == 1);
                // `text`, not `orth`: a shape signature spells the attribute's
                // canonical key, and `TokenAttributeIndex::Orth`'s key became
                // `text` in the 2026-08-24 rename. The enum variant keeps the
                // UniDic column's own name; the portable key it publishes does
                // not.
                assert_eq!(
                    descriptor.shape_signature,
                    format!(
                        "text:{operator_key}:{}",
                        if complement { "complement" } else { "positive" }
                    )
                );
                if expected_safe == 1 {
                    let atom = &descriptor.finite_atoms[0];
                    assert_eq!(
                        atom.attribute.key(),
                        &AttributeKey::from(TokenAttributeIndex::Orth)
                    );
                    assert_eq!(atom.operator, operator);
                    let expected_values: &[&str] = if operator == SpawnAtomOperator::OneOf {
                        &["a", "b"]
                    } else {
                        &["b"]
                    };
                    assert_eq!(
                        atom.values
                            .iter()
                            .map(CompactString::as_str)
                            .collect::<Vec<_>>(),
                        expected_values
                    );
                }
            }
        }
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn scheme_a_selection_ignores_attribute_and_operand_source_order() {
        let first = [
            attr(TokenAttributeIndex::Orth, AttrValueType::Single("z".into())),
            attr(
                TokenAttributeIndex::Lemma,
                AttrValueType::OneOf(Arc::from(vec!["b".into(), "a".into(), "b".into()])),
            ),
        ];
        let second = [
            attr(
                TokenAttributeIndex::Lemma,
                AttrValueType::OneOf(Arc::from(vec!["a".into(), "b".into()])),
            ),
            attr(TokenAttributeIndex::Orth, AttrValueType::Single("z".into())),
        ];
        let mut first_catalog = PredicateCatalog::default();
        let first_id = first_catalog.intern(&first);
        let mut second_catalog = PredicateCatalog::default();
        let second_id = second_catalog.intern(&second);

        let first_descriptor = first_catalog.spawn_candidate_descriptor(first_id);
        let second_descriptor = second_catalog.spawn_candidate_descriptor(second_id);
        assert_eq!(first_descriptor, second_descriptor);
        let selected = first_descriptor
            .scheme_a
            .as_ref()
            .expect("fixture has finite-positive atoms");
        assert_eq!(
            selected.attribute.key(),
            &AttributeKey::from(TokenAttributeIndex::Lemma)
        );
        assert_eq!(selected.operator, SpawnAtomOperator::OneOf);
        assert_eq!(
            selected
                .values
                .iter()
                .map(CompactString::as_str)
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            first_descriptor.shape_signature,
            "lemma:one_of:positive+text:single:positive"
        );
    }

    fn literal_reference(token: &TokenInfo, predicate: &[SymbolicAttrValue]) -> bool {
        predicate.iter().all(|expected| {
            token
                .scalar_key(&expected.attr)
                .is_some_and(|actual| match &expected.value {
                    AttrValueType::Single(value) => actual == value,
                    AttrValueType::OneOf(values) => {
                        values.iter().any(|value| value.as_str() == actual)
                    }
                    AttrValueType::NotIn(values) => {
                        values.iter().all(|value| value.as_str() != actual)
                    }
                    AttrValueType::Contains(needle) => actual.contains(needle.as_str()),
                    AttrValueType::NotContains(needle) => !actual.contains(needle.as_str()),
                    AttrValueType::Regex(regex) => regex.is_match(actual),
                    AttrValueType::NotRegex(regex) => !regex.is_match(actual),
                    AttrValueType::SetHas(_)
                    | AttrValueType::SetHasAny(_)
                    | AttrValueType::SetNotHas(_)
                    | AttrValueType::SetNotHasAny(_) => false,
                })
        })
    }

    #[test]
    fn evaluator_matches_literal_semantics_for_values_sets_regex_and_missing_fields() {
        let mut complete = TokenInfo::default();
        complete.set(TokenAttributeIndex::Orth, "猫猫".into());
        complete.set(TokenAttributeIndex::Lemma, "猫".into());
        complete.set(TokenAttributeIndex::Pos, "NOUN".into());

        let missing = TokenInfo::default();
        let predicates = vec![
            vec![attr(
                TokenAttributeIndex::Lemma,
                AttrValueType::Single("猫".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Pos,
                AttrValueType::OneOf(Arc::from(vec!["VERB".into(), "NOUN".into()])),
            )],
            vec![attr(
                TokenAttributeIndex::Lemma,
                AttrValueType::NotIn(Arc::from(vec!["犬".into(), "鳥".into()])),
            )],
            vec![attr(
                TokenAttributeIndex::Lemma,
                AttrValueType::NotIn(Arc::from(vec!["猫".into()])),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::Regex(regex::Regex::new("^猫+$").unwrap()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::Regex(regex::Regex::new("^a+$").unwrap()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::Contains("猫".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::Contains("犬".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::NotContains("猫".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::NotContains("犬".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::NotRegex(regex::Regex::new("^猫+$").unwrap()),
            )],
            vec![attr(
                TokenAttributeIndex::Orth,
                AttrValueType::NotRegex(regex::Regex::new("^a+$").unwrap()),
            )],
            // Containment and negated search against an attribute the token
            // does not carry: neither polarity may be satisfied.
            vec![attr(
                TokenAttributeIndex::Kana,
                AttrValueType::Contains("ネ".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Kana,
                AttrValueType::NotContains("ネ".into()),
            )],
            vec![attr(
                TokenAttributeIndex::Kana,
                AttrValueType::NotRegex(regex::Regex::new("ネ").unwrap()),
            )],
            vec![attr(
                TokenAttributeIndex::Kana,
                AttrValueType::Single("ネコ".into()),
            )],
            vec![
                attr(
                    TokenAttributeIndex::Lemma,
                    AttrValueType::Single("猫".into()),
                ),
                attr(
                    TokenAttributeIndex::Pos,
                    AttrValueType::Single("NOUN".into()),
                ),
            ],
        ];

        let mut catalog = PredicateCatalog::default();
        for predicate in predicates {
            let id = catalog.intern(&predicate);
            for token in [&complete, &missing] {
                assert_eq!(
                    catalog.matches(id, token),
                    literal_reference(token, &predicate),
                    "predicate={predicate:?}, token={token:?}",
                );
            }
        }
    }

    #[test]
    fn interning_deduplicates_exact_predicates_and_assigns_distinct_ids() {
        let first = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single(CompactString::from("a")),
        )];
        let second = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single(CompactString::from("b")),
        )];
        let mut catalog = PredicateCatalog::default();

        let first_id = catalog.intern(&first);
        assert_eq!(catalog.intern(&first), first_id);
        assert_ne!(catalog.intern(&second), first_id);
    }

    #[test]
    fn interning_never_conflates_a_match_mode_with_its_negation_or_with_equality() {
        // `definition_eq` pairs a compiled value with a raw one variant by
        // variant. A missing pair falls through to `_ => false`, which is safe;
        // a *wrong* pair would silently give `[k = v]` and `[k ~ v]` one id.
        let mut catalog = PredicateCatalog::default();
        let ids = [
            AttrValueType::Single("ない".into()),
            AttrValueType::Contains("ない".into()),
            AttrValueType::NotContains("ない".into()),
            AttrValueType::NotIn(Arc::from(vec!["ない".into()])),
            AttrValueType::Regex(regex::Regex::new("ない").unwrap()),
            AttrValueType::NotRegex(regex::Regex::new("ない").unwrap()),
        ]
        .map(|value| catalog.intern(&[attr(TokenAttributeIndex::Orth, value)]));

        for (index, id) in ids.iter().enumerate() {
            for other in &ids[index + 1..] {
                assert_ne!(id, other, "distinct match modes must intern separately");
            }
        }

        // `definition_eq` also compares `complement`. A version that dropped
        // that conjunct would treat this as the same predicate as `ids[0]`
        // (`Single("ない")`, uncomplemented) and hand back its id instead of a
        // fresh one.
        let complemented_single_id = catalog.intern(&[SymbolicAttrValue {
            attr: TokenAttributeIndex::Orth.into(),
            value: AttrValueType::Single("ない".into()),
            complement: true,
        }]);
        for id in &ids {
            assert_ne!(
                &complemented_single_id, id,
                "a complemented atom must intern separately from every uncomplemented match \
                 mode, including its own uncomplemented counterpart"
            );
        }
    }

    #[test]
    fn regex_slots_are_matcher_scoped_and_clear_releases_thread_local_storage() {
        clear_local_regexes();
        let regex_predicate = |pattern: &str| {
            [attr(
                TokenAttributeIndex::Orth,
                AttrValueType::Regex(regex::Regex::new(pattern).unwrap()),
            )]
        };
        let mut a_catalog = PredicateCatalog::default();
        let a_id = a_catalog.intern(&regex_predicate("^a+$"));
        let mut b_catalog = PredicateCatalog::default();
        let b_id = b_catalog.intern(&regex_predicate("^b+$"));
        let mut a = TokenInfo::default();
        a.set(TokenAttributeIndex::Orth, "aaa".into());
        let mut b = TokenInfo::default();
        b.set(TokenAttributeIndex::Orth, "bbb".into());

        assert!(a_catalog.matches(a_id, &a));
        assert!(!a_catalog.matches(a_id, &b));
        assert!(b_catalog.matches(b_id, &b));
        assert!(!b_catalog.matches(b_id, &a));
        assert!(a_catalog.matches(a_id, &a));

        LOCAL_REGEXES.with(|local| {
            let local = local.borrow();
            assert!(local.owner.is_some());
            assert_eq!(local.regexes.len(), 1);
            assert!(local.regexes[0].is_some());
        });
        clear_local_regexes();
        LOCAL_REGEXES.with(|local| {
            let local = local.borrow();
            assert!(local.owner.is_none());
            assert_eq!(local.regexes.capacity(), 0);
        });
    }

    #[test]
    fn one_cell_memo_remains_correct_when_positions_move_backward_and_forward() {
        let expected = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single("match".into()),
        )];
        let mut catalog = PredicateCatalog::default();
        let predicate = catalog.intern(&expected);
        let tokens = ["other", "other", "match"].map(|orth| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, orth.into());
            token
        });
        let session = PredicateSession::new(&catalog);

        assert!(session.matches_at(predicate, &tokens, 2));
        assert!(!session.matches_at(predicate, &tokens, 1));
        assert!(session.matches_at(predicate, &tokens, 2));
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn predicate_metrics_distinguish_hits_recomputations_and_other_calls() {
        let expected = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single("match".into()),
        )];
        let mut catalog = PredicateCatalog::default();
        let predicate = catalog.intern(&expected);
        let tokens = ["match", "other"].map(|orth| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, orth.into());
            token
        });
        let session = PredicateSession::new(&catalog);

        assert!(session.matches_at(predicate, &tokens, 0));
        assert!(session.matches_at(predicate, &tokens, 0));
        assert!(!session.matches_at(predicate, &tokens, 1));
        assert!(session.matches_at(predicate, &tokens, 0));

        let metrics = session.metrics();
        assert_eq!(metrics.predicate_calls, 4);
        assert_eq!(metrics.predicate_memo_hits, 1);
        assert_eq!(metrics.predicate_evaluations, 3);
        assert_eq!(metrics.predicate_distinct_pairs, 2);
        assert_eq!(metrics.predicate_recomputations, 1);
        assert_eq!(metrics.dense_predicate_calls, 0);
        assert_eq!(metrics.dense_predicate_memo_hits, 0);
        assert_eq!(metrics.dense_predicate_evaluations, 0);
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn predicate_metrics_attribute_every_call_site() {
        let expected = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single("match".into()),
        )];
        let mut catalog = PredicateCatalog::default();
        let predicate = catalog.intern(&expected);
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "match".into());
        let tokens = [token];
        let session = PredicateSession::new(&catalog);

        for _ in 0..1 {
            assert!(session.matches_at_dense_main(predicate, &tokens, 0));
        }
        for _ in 0..2 {
            assert!(session.matches_at_dense_match_closure(predicate, &tokens, 0));
        }
        for _ in 0..3 {
            assert!(session.matches_at_dense_advance(predicate, &tokens, 0));
        }
        for _ in 0..4 {
            assert!(session.matches_at_dense_retry_advance(predicate, &tokens, 0));
        }
        for _ in 0..5 {
            assert!(session.matches_at_dense_retry_extend(predicate, &tokens, 0));
        }
        for _ in 0..6 {
            assert!(session.matches_at_spawn(predicate, &tokens, 0));
        }
        for _ in 0..7 {
            assert!(session.matches_at_nfa_previous_token(predicate, &tokens, 0));
        }
        for _ in 0..8 {
            assert!(session.matches_at_assertion_consume(predicate, &tokens, 0));
        }
        for _ in 0..9 {
            assert!(session.matches_at_assertion_previous_token(predicate, &tokens, 0));
        }
        for _ in 0..10 {
            assert!(
                session.matches_at_site::<PREDICATE_SITE_CAPTURE_TOKEN, _>(predicate, &tokens, 0,)
            );
        }
        for _ in 0..11 {
            assert!(
                session.matches_at_site::<PREDICATE_SITE_CAPTURE_PREVIOUS_TOKEN, _>(
                    predicate, &tokens, 0,
                )
            );
        }
        for _ in 0..12 {
            assert!(
                session
                    .matches_at_site::<PREDICATE_SITE_EVALUATED_TOKEN, _>(predicate, &tokens, 0,)
            );
        }
        for _ in 0..13 {
            assert!(
                session.matches_at_site::<PREDICATE_SITE_EVALUATED_PREVIOUS_TOKEN, _>(
                    predicate, &tokens, 0,
                )
            );
        }
        for _ in 0..14 {
            assert!(session.matches_at(predicate, &tokens, 0));
        }

        let metrics = session.metrics();
        assert_eq!(metrics.predicate_calls, 105);
        assert_eq!(metrics.predicate_memo_hits, 104);
        assert_eq!(metrics.predicate_evaluations, 1);
        assert_eq!(metrics.predicate_distinct_pairs, 1);
        assert_eq!(metrics.predicate_recomputations, 0);
        assert_eq!(metrics.dense_predicate_calls, 15);
        assert_eq!(metrics.dense_predicate_memo_hits, 14);
        assert_eq!(metrics.dense_predicate_evaluations, 1);
        assert_eq!(metrics.dense_main_transition_predicate_calls, 1);
        assert_eq!(metrics.dense_match_closure_predicate_calls, 2);
        assert_eq!(metrics.dense_advance_validation_predicate_calls, 3);
        assert_eq!(metrics.dense_retry_advance_validation_predicate_calls, 4);
        assert_eq!(metrics.dense_retry_extend_validation_predicate_calls, 5);
        assert_eq!(metrics.spawn_predicate_calls, 6);
        assert_eq!(metrics.nfa_previous_token_predicate_calls, 7);
        assert_eq!(metrics.assertion_consume_predicate_calls, 8);
        assert_eq!(metrics.assertion_previous_token_predicate_calls, 9);
        assert_eq!(metrics.capture_token_predicate_calls, 10);
        assert_eq!(metrics.capture_previous_token_predicate_calls, 11);
        assert_eq!(metrics.evaluated_token_predicate_calls, 12);
        assert_eq!(metrics.evaluated_previous_token_predicate_calls, 13);
        assert_eq!(metrics.unclassified_predicate_calls, 14);
    }

    #[cfg(feature = "bench-instrumentation")]
    #[test]
    fn unmemoized_predicate_metrics_keep_distinct_pair_invariant() {
        let expected = [attr(
            TokenAttributeIndex::Orth,
            AttrValueType::Single("match".into()),
        )];
        let mut catalog = PredicateCatalog::default();
        let predicate = catalog.intern(&expected);
        let tokens = ["match"].map(|orth| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Orth, orth.into());
            token
        });
        let session = PredicateSession::new_unmemoized(&catalog);

        assert!(session.matches_at(predicate, &tokens, 0));
        assert!(session.matches_at(predicate, &tokens, 0));

        let metrics = session.metrics();
        assert_eq!(metrics.predicate_calls, 2);
        assert_eq!(metrics.predicate_memo_hits, 0);
        assert_eq!(metrics.predicate_evaluations, 2);
        assert_eq!(metrics.predicate_distinct_pairs, 1);
        assert_eq!(metrics.predicate_recomputations, 1);
    }

    #[test]
    fn predicate_generation_wrap_clears_zero_stamps_and_restarts_at_one() {
        let mut scratch = PredicateScratch {
            cells: vec![Cell::new(MemoCell(MATCHED_BIT | (u64::MAX >> 1)))],
            call_generation: u32::MAX,
            #[cfg(feature = "bench-instrumentation")]
            predicate_counters: Default::default(),
            #[cfg(feature = "bench-instrumentation")]
            seen_generations: RefCell::default(),
            #[cfg(feature = "bench-instrumentation")]
            distinct_pairs: Cell::default(),
            #[cfg(feature = "bench-instrumentation")]
            candidates_produced: Cell::default(),
            #[cfg(feature = "bench-instrumentation")]
            candidates_retained: Cell::default(),
            #[cfg(feature = "bench-instrumentation")]
            spawn_candidates: RefCell::default(),
        };
        #[cfg(feature = "bench-instrumentation")]
        {
            scratch
                .seen_generations
                .get_mut()
                .push(vec![MAX_CALL_GENERATION, 1]);
            scratch.distinct_pairs.set(2);
            scratch.predicate_counters[0].calls.set(7);
            scratch.candidates_produced.set(5);
            scratch.candidates_retained.set(4);
        }

        scratch.begin_call(1);

        assert_eq!(scratch.call_generation, 1);
        assert_eq!(scratch.cells[0].get().0, 0);
        #[cfg(feature = "bench-instrumentation")]
        {
            assert_eq!(scratch.seen_generations.get_mut()[0], [0, 0]);
            assert_eq!(scratch.distinct_pairs.get(), 0);
            assert_eq!(scratch.predicate_counters[0].calls.get(), 0);
            assert_eq!(scratch.candidates_produced.get(), 0);
            assert_eq!(scratch.candidates_retained.get(), 0);
        }
    }
}
