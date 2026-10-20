//! Compile-time token-value dispatch for the spawn predicate table.
//!
//! See [`SpawnDispatch`] for what the index answers and why its merge needs no
//! ordering work.

use super::pool::FastBuild;
use super::predicate::SpawnPredicateDescriptor;
use crate::token_match::{AttributeLocator, BoundProjection, TokenInfoTrait};
use compact_str::CompactString;
use std::collections::HashMap;

/// Minimum group count worth indexing.
///
/// A one-group table can at best trade one predicate call for one hash lookup
/// plus a mask operation, so it cannot win on argument alone. This is a
/// structural bound, not a tuned threshold: it must not acquire a measured
/// constant without its own evidence.
const MIN_DISPATCH_GROUPS: usize = 2;

/// Catalog-authored value to postings offset.
///
/// [`FastBuild`] is the matcher's established hasher, but `pool`'s rationale
/// for it — keys are state ids rather than strings — does not apply here,
/// because these keys are strings. A stronger one does: every key comes from
/// the compiled catalog, so corpus input can neither grow this map nor choose
/// its keys. It holds at most one entry per literal the catalog wrote, and a
/// lookup that misses simply contributes nothing.
type ValueMap = HashMap<CompactString, u32, FastBuild>;

/// Compile-time `token value -> candidate group` index over a spawn table.
///
/// The executor otherwise evaluates every group's predicate at every token —
/// measured at 40.953 predicate calls per token on the researcher catalog,
/// almost all of which fail. This index answers "which groups could possibly
/// match here?" from the token's own attribute values first.
///
/// Each group contributes its canonical finite-positive atom to exactly one
/// attribute's map, so postings drawn from different attributes are disjoint.
/// That is what makes the runtime merge a plain OR: no deduplication, no merge
/// order, and no ordering restoration.
///
/// Set bits are iterated ascending, which visits groups in their existing table
/// order and so keeps the dispatched path a strict filter over the undispatched
/// sequence. That is an argument convenience rather than a requirement: no
/// observable distinguishes the orders, because the frontier is a
/// [`StateSet`](super::pool::StateSet) and `Trace` entries are compared as a
/// multiset. A mutation to descending iteration accordingly passes every test,
/// which is the expected result and not a gap in them.
///
/// Groups with no finite-positive atom cannot be discriminated and join
/// [`always`](Self::always), which is OR'd in unconditionally.
#[derive(Clone)]
pub(crate) struct SpawnDispatch {
    /// Indexed canonical attributes in key order, each with its own value map.
    /// Sorted so the index is a function of the compiled catalog.
    attributes: Vec<(AttributeLocator, ValueMap)>,
    /// Flat postings: `words` `u64`s per entry, addressed by a map's offset.
    postings: Vec<u64>,
    /// Groups that must be considered at every token.
    always: Vec<u64>,
    words: usize,
}

/// Observable shape of a built index, for characterization tests.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SpawnDispatchShape {
    pub indexed_attributes: usize,
    pub indexed_values: usize,
    pub words: usize,
    pub always_population: usize,
}

impl SpawnDispatch {
    /// Build the index, or `None` when the table is too small to benefit.
    pub(crate) fn build(groups: &[SpawnPredicateDescriptor]) -> Option<Self> {
        if groups.len() < MIN_DISPATCH_GROUPS {
            return None;
        }
        let words = groups.len().div_ceil(u64::BITS as usize);
        let mut attributes: Vec<(AttributeLocator, ValueMap)> = Vec::new();
        let mut postings: Vec<u64> = Vec::new();
        let mut always = vec![0u64; words];

        for (group, descriptor) in groups.iter().enumerate() {
            let Some(atom) = descriptor.scheme_a.as_ref() else {
                always[group / 64] |= 1 << (group % 64);
                continue;
            };
            let slot = attributes
                .iter()
                .position(|(attribute, _)| attribute == &atom.attribute)
                .unwrap_or_else(|| {
                    attributes.push((atom.attribute.clone(), ValueMap::default()));
                    attributes.len() - 1
                });
            for value in &atom.values {
                let offset = if let Some(known) = attributes[slot].1.get(value) {
                    *known as usize
                } else {
                    let offset = postings.len();
                    postings.resize(offset + words, 0);
                    attributes[slot].1.insert(
                        value.clone(),
                        u32::try_from(offset).expect("spawn postings fit in u32"),
                    );
                    offset
                };
                postings[offset + group / 64] |= 1 << (group % 64);
            }
        }
        attributes.sort_by(|left, right| left.0.cmp(&right.0));
        Some(Self {
            attributes,
            postings,
            always,
            words,
        })
    }

    /// Width of a candidate bitset, in `u64` words.
    pub(crate) fn words(&self) -> usize {
        self.words
    }

    /// Write `token`'s candidate group bitset into `out`, which must be
    /// exactly [`words`](Self::words) long. Overwrites, so the caller need not
    /// clear it between positions.
    pub(crate) fn candidates<T: TokenInfoTrait>(&self, token: &T, out: &mut [u64]) {
        debug_assert_eq!(out.len(), self.words);
        out.copy_from_slice(&self.always);
        for (attribute, values) in &self.attributes {
            let Some(crate::token_match::AttributeValueRef::Scalar(actual)) =
                token.value_locator(attribute)
            else {
                continue;
            };
            let Some(&offset) = values.get(actual) else {
                continue;
            };
            let offset = offset as usize;
            for (candidate, posting) in out
                .iter_mut()
                .zip(&self.postings[offset..offset + self.words])
            {
                *candidate |= *posting;
            }
        }
    }

    pub(crate) fn bind_attributes(&mut self, projection: &BoundProjection) {
        for (attribute, _) in &mut self.attributes {
            // An unavailable key stays unbound and yields no value, so its
            // finite-positive candidates never fire — the indexed answer to a
            // predicate that can match no token of this schema.
            if let Some(ordinal) = projection.ordinal_for_key(attribute.key()) {
                attribute.bind(ordinal);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn shape_for_test(&self) -> SpawnDispatchShape {
        SpawnDispatchShape {
            indexed_attributes: self.attributes.len(),
            indexed_values: self.attributes.iter().map(|(_, map)| map.len()).sum(),
            words: self.words,
            always_population: self
                .always
                .iter()
                .map(|word| word.count_ones() as usize)
                .sum(),
        }
    }
}

/// Compiled attribute operator used by the spawn candidate classifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpawnAtomOperator {
    /// Exact equality with one literal.
    Single,
    /// Membership in a finite literal set.
    OneOf,
    /// Exclusion from a finite literal set.
    NotIn,
    SetHas,
    SetHasAny,
    SetNotHas,
    SetNotHasAny,
    /// Literal substring containment.
    Contains,
    /// Negated literal substring containment.
    NotContains,
    /// Regular-expression search.
    Regex,
    /// Negated regular-expression search.
    NotRegex,
}

impl SpawnAtomOperator {
    /// Stable lowercase key used in retained TSV evidence.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::OneOf => "one_of",
            Self::NotIn => "not_in",
            Self::SetHas => "set_has",
            Self::SetHasAny => "set_has_any",
            Self::SetNotHas => "set_not_has",
            Self::SetNotHasAny => "set_not_has_any",
            Self::Contains => "contains",
            Self::NotContains => "not_contains",
            Self::Regex => "regex",
            Self::NotRegex => "not_regex",
        }
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use std::collections::BTreeMap;

    fn dispatch_for(source: &str) -> Option<SpawnDispatch> {
        let compiled = compile_catalog(
            PatternSource::named("dispatch.lx", source),
            CompileLimits::default(),
        )
        .unwrap();
        compiled
            .matcher
            .dense_for_test()
            .spawn_table
            .dispatch
            .clone()
    }

    /// The tracked catalogs' index shapes, as
    /// `(indexed attributes, distinct indexed values, words, always population)`.
    ///
    /// `double-wo` and `test-simple` have one spawn group each, so they build
    /// no index at all and keep the historical per-group scan.
    ///
    /// The attribute and always-population columns are not free parameters:
    /// they must equal the spawn candidate-set census's `A attributes` and
    /// `Zero safe` columns, which they do — `sf-experimental` 4 and 0, `sfe` 6
    /// and 2, `sketch-grammar` 1 and 0. A disagreement means the production
    /// index and the measured scheme have diverged, and is a defect rather
    /// than an expectation to update.
    #[test]
    fn tracked_catalog_dispatch_shape() {
        let mut expected: BTreeMap<&str, Option<(usize, usize, usize, usize)>> = BTreeMap::from([
            ("patterns/alternation.lx", Some((2, 10, 1, 0))),
            ("patterns/collexeme.lx", Some((1, 1, 1, 0))),
            ("patterns/concord.lx", Some((1, 6, 1, 0))),
            ("patterns/counting.lx", None),
            ("patterns/distributions.lx", None),
            ("patterns/double-wo-dep.lx", None),
            ("patterns/double-wo.lx", None),
            ("patterns/ngrams.lx", None),
            ("patterns/npv-collocation.lx", None),
            ("patterns/passive-agent-dep.lx", None),
            ("patterns/passive-oblique-realization.lx", None),
            ("patterns/portable-content-words.lx", Some((1, 3, 1, 0))),
            // Every reduplication pattern is SpanEquality-recognized, so the
            // dense machine has no spawn groups and builds no index.
            ("patterns/reduplication.lx", None),
            ("patterns/sentence-final.lx", Some((3, 32, 1, 1))),
            ("patterns/test-simple.lx", None),
            // 54 -> 55 with the `form` -> `text` rename. `scheme_a` is
            // `finite_atoms.first()` after sorting by attribute, so renaming
            // the portable surface key moves it from the front of that order
            // to the back and a predicate with several finite atoms picks a
            // different discriminator. Prefilter shape only: the claim-count
            // and baseline pins did not move.
            // The first tracked catalog to MIX relation-bearing and
            // relation-free definitions. Its four `where` patterns are
            // tree-evaluated while the other nine reach the dense machine, so
            // unlike ud-dependency it does build an index.
            ("patterns/comment-register.lx", Some((3, 18, 1, 1))),
            ("patterns/sf-experimental.lx", Some((4, 55, 1, 0))),
            ("patterns/sfe.lx", Some((7, 26, 1, 1))),
            ("patterns/sketch-grammar.lx", Some((2, 6, 1, 0))),
            // Every ud-dependency pattern is Relations-recognized, so the
            // dense machine has no spawn groups and builds no index.
            ("patterns/ud-dependency.lx", None),
            ("patterns/ud-enhanced-argument.lx", None),
            ("patterns/ud-enhanced-consistency.lx", None),
            ("patterns/ud-enhanced-propagation.lx", None),
            ("patterns/ud-nonprojectivity.lx", None),
            ("patterns/ud-object-realization.lx", None),
            ("patterns/ud-subtree-profile.lx", None),
            ("patterns/ud-valency.lx", None),
            ("patterns/wo-predicate-dep.lx", None),
        ]);
        for (name, source) in crate::testing::catalogs::tracked_catalog_sources()
            .into_iter()
            .filter(|(name, _)| name.starts_with("patterns/"))
        {
            let actual = dispatch_for(&source).as_ref().map(|dispatch| {
                let shape = dispatch.shape_for_test();
                (
                    shape.indexed_attributes,
                    shape.indexed_values,
                    shape.words,
                    shape.always_population,
                )
            });
            assert_eq!(
                Some(actual),
                expected.remove(name.as_str()),
                "{name} dispatch shape"
            );
        }
        assert!(
            expected.is_empty(),
            "missing tracked catalogs: {expected:?}"
        );
    }

    /// A group whose predicate has no finite-positive atom cannot be
    /// discriminated, so it must be considered at every token.
    #[test]
    fn a_group_without_a_finite_atom_joins_the_always_mask() {
        let dispatch = dispatch_for(
            "pattern indexed := [text=x] [text=y].\n\
             pattern residual := [text~/a+/] [text=z].\n",
        )
        .expect("two groups build an index");
        let shape = dispatch.shape_for_test();
        assert_eq!(shape.always_population, 1);
        assert_eq!(shape.indexed_attributes, 1);
    }

    /// A token lacking every indexed attribute satisfies no finite-positive
    /// atom, so only the undiscriminated groups survive dispatch.
    #[test]
    fn a_missing_attribute_dispatches_to_the_always_mask_alone() {
        use crate::token_match::TokenInfo;
        let dispatch = dispatch_for(
            "pattern indexed := [text=x] [text=y].\n\
             pattern residual := [text~/a+/] [text=z].\n",
        )
        .unwrap();
        let mut out = vec![0u64; dispatch.words()];
        dispatch.candidates(&TokenInfo::default(), &mut out);
        assert_eq!(out.iter().map(|word| word.count_ones()).sum::<u32>(), 1);
    }

    /// A one-group table is not worth a lookup.
    #[test]
    fn a_single_group_table_builds_no_index() {
        assert!(dispatch_for("pattern only := [text=x] [text=y].\n").is_none());
    }
}

#[cfg(test)]
mod differential_tests {
    use crate::fsm::matcher::nfa_matcher::Matcher;
    use crate::fsm::matcher::predicate::PredicateSession;
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{TokenAttributeIndex, TokenInfo};
    use compact_str::CompactString;

    /// Anchors, previous-token assertions, nullable prefixes, quantifiers,
    /// alternation, set membership and regex — and both conditional and
    /// unconditional groups, which the readiness masks depend on.
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

    fn token(orth: &str, pos1: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, orth.into());
        token.set(TokenAttributeIndex::Pos1, pos1.into());
        token
    }

    fn streams(seed: u64, rounds: usize, max_len: usize) -> Vec<Vec<TokenInfo>> {
        let orths = ["a", "b", "c", "z", "x", "ab", "aab", "そう", "ある", "y"];
        let tags = ["NOUN", "VERB", "ADV", "ADJ", "PART"];
        let mut state = seed;
        let mut random = move || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        (0..rounds)
            .map(|_| {
                let len = random() % max_len;
                (0..len)
                    .map(|_| token(orths[random() % orths.len()], tags[random() % tags.len()]))
                    .collect()
            })
            .collect()
    }

    /// Dispatch is a filter over the same group sequence, so it must be
    /// observationally identical to the undispatched scan — complete `Match`
    /// values, contexts and captures included.
    #[test]
    fn dispatch_matches_the_undispatched_path() {
        let compiled = compile_catalog(
            PatternSource::named("dispatch-differential.lx", DIFFERENTIAL_PATTERN),
            CompileLimits::default(),
        )
        .unwrap();
        let predicates = compiled.matcher.predicates_for_test().clone();
        let dense = compiled.matcher.dense_for_test().clone();
        assert!(
            dense.spawn_table.dispatch.is_some(),
            "fixture must build an index"
        );
        let mut undispatched = dense.clone();
        undispatched.spawn_table.dispatch = None;

        let mut total = 0usize;
        for tokens in streams(0x9e37_79b9, 400, 17) {
            let expected_session = PredicateSession::new_unmemoized(&predicates);
            let expected = Matcher::find_matches_dense(&undispatched, &expected_session, &tokens);
            drop(expected_session);
            let actual_session = PredicateSession::new_unmemoized(&predicates);
            let actual = Matcher::find_matches_dense(&dense, &actual_session, &tokens);
            assert_eq!(actual, expected, "tokens: {tokens:?}");
            total += actual.len();
        }
        assert!(total > 100, "vacuous differential: {total}");
    }

    /// The fixture differentials use one small catalog. This runs every
    /// tracked catalog — `sf-experimental` included — over tokens built from
    /// the catalogs' own indexed values, so the streams actually reach the
    /// groups the index discriminates rather than missing them all.
    #[test]
    fn dispatch_matches_the_undispatched_path_on_tracked_catalogs() {
        let mut compared = 0usize;
        let mut total = 0usize;
        for (name, source) in crate::testing::catalogs::tracked_catalog_sources() {
            let compiled = compile_catalog(
                PatternSource::named(&name, &source),
                CompileLimits::default(),
            )
            .unwrap();
            let predicates = compiled.matcher.predicates_for_test().clone();
            let dense = compiled.matcher.dense_for_test().clone();
            if dense.spawn_table.dispatch.is_none() {
                continue;
            }
            let mut undispatched = dense.clone();
            undispatched.spawn_table.dispatch = None;

            // Every value any group's canonical atom accepts, so a generated
            // token can satisfy a real discriminator rather than only miss.
            let vocabulary: Vec<(TokenAttributeIndex, CompactString)> = dense
                .spawn_table
                .groups_for_test()
                .iter()
                .filter_map(|(_, descriptor)| descriptor.scheme_a.clone())
                .flat_map(|atom| {
                    let attribute = TokenAttributeIndex::from_attribute_key(atom.attribute.key())
                        .expect("test predicates use physical fixture keys");
                    atom.values
                        .iter()
                        .map(move |value| (attribute, value.clone()))
                        .collect::<Vec<_>>()
                })
                .collect();
            if vocabulary.is_empty() {
                continue;
            }

            let mut state = 0xc0ff_ee00_u64;
            let mut random = move || {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as usize
            };

            for _ in 0..120 {
                let len = random() % 14;
                let tokens: Vec<TokenInfo> = (0..len)
                    .map(|_| {
                        let mut token = TokenInfo::default();
                        for _ in 0..=(random() % 3) {
                            let (attribute, value) = &vocabulary[random() % vocabulary.len()];
                            token.set(*attribute, value.clone());
                        }
                        token
                    })
                    .collect();
                let expected_session = PredicateSession::new_unmemoized(&predicates);
                let expected =
                    Matcher::find_matches_dense(&undispatched, &expected_session, &tokens);
                drop(expected_session);
                let actual_session = PredicateSession::new_unmemoized(&predicates);
                let actual = Matcher::find_matches_dense(&dense, &actual_session, &tokens);
                assert_eq!(actual, expected, "{name} tokens: {tokens:?}");
                compared += 1;
                total += actual.len();
            }
        }
        assert!(compared > 200, "too few tracked comparisons: {compared}");
        assert!(total > 100, "vacuous tracked differential: {total}");
    }

    /// The trace observable: the emitted entry multiset and `Trace.matches`
    /// must survive dispatch, which is what pins ascending-bit iteration to
    /// the historical group order.
    #[test]
    fn dispatch_preserves_the_trace_entry_multiset() {
        let compiled = compile_catalog(
            PatternSource::named("dispatch-trace.lx", DIFFERENTIAL_PATTERN),
            CompileLimits::default(),
        )
        .unwrap();
        let predicates = compiled.matcher.predicates_for_test().clone();
        let dense = compiled.matcher.dense_for_test().clone();
        let mut undispatched = dense.clone();
        undispatched.spawn_table.dispatch = None;

        let mut total = 0usize;
        for tokens in streams(0x51ed_2701, 100, 12) {
            let expected_session = PredicateSession::new_unmemoized(&predicates);
            let expected =
                Matcher::find_matches_with_trace_dense(&undispatched, &expected_session, &tokens);
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
            total += actual_entries.len();
        }
        assert!(total > 100, "vacuous trace differential: {total}");
    }
}

#[cfg(test)]
mod soundness_tests {
    use crate::fsm::matcher::predicate::{PredicateSession, finite_atom_matches};
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{TokenAttributeIndex, TokenInfo};

    /// Tokens covering, for every canonical value any tracked catalog names:
    /// that value, a value no catalog names, and a token with nothing set.
    fn probe_tokens(values: &[(TokenAttributeIndex, String)]) -> Vec<TokenInfo> {
        let mut tokens = vec![TokenInfo::default()];
        for (attribute, value) in values {
            let mut present = TokenInfo::default();
            present.set(*attribute, value.as_str().into());
            tokens.push(present);
            let mut absent = TokenInfo::default();
            absent.set(*attribute, format!("{value}-absent").as_str().into());
            tokens.push(absent);
        }
        tokens
    }

    /// The lemma the whole dispatcher rests on. Scheme A's canonical atom is a
    /// conjunct of its group's predicate — every `CompiledPredicate` is a
    /// conjunction, because a multi-branch token test compiles to an
    /// alternation of one-token branches — so an atom miss is a predicate
    /// miss, and the dispatcher may skip the group without evaluating it.
    #[test]
    fn canonical_atom_miss_implies_predicate_miss() {
        let mut checked = 0usize;
        for (name, source) in crate::testing::catalogs::tracked_catalog_sources() {
            let compiled = compile_catalog(
                PatternSource::named(&name, &source),
                CompileLimits::default(),
            )
            .unwrap();
            let predicates = compiled.matcher.predicates_for_test().clone();
            let dense = compiled.matcher.dense_for_test().clone();
            let groups = dense.spawn_table.groups_for_test();

            let values: Vec<(TokenAttributeIndex, String)> = groups
                .iter()
                .filter_map(|(_, descriptor)| descriptor.scheme_a.as_ref())
                .flat_map(|atom| {
                    let attribute = TokenAttributeIndex::from_attribute_key(atom.attribute.key())
                        .expect("test predicates use physical fixture keys");
                    atom.values
                        .iter()
                        .map(move |value| (attribute, value.to_string()))
                })
                .collect();

            for token in probe_tokens(&values) {
                let tokens = [token];
                let session = PredicateSession::new_unmemoized(&predicates);
                for (predicate, descriptor) in &groups {
                    let Some(atom) = descriptor.scheme_a.as_ref() else {
                        continue;
                    };
                    if finite_atom_matches(atom, &tokens[0]) {
                        continue;
                    }
                    assert!(
                        !session.matches_at_spawn(*predicate, &tokens, 0),
                        "{name}: canonical atom missed but the predicate matched: \
                         {descriptor:?} on {:?}",
                        tokens[0]
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "vacuous soundness check: {checked}");
    }
}
