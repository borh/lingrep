//! Disjunctive normal form for a token test.
//!
//! A token test is exactly one token wide, so a disjunction of tests and an
//! alternation of one-token expressions accept the same language. The
//! compiler therefore never grows a Boolean matcher: it distributes `[…]`
//! into branches here and hands the existing `BoundExpr::Alternation` of
//! `BoundExpr::Token` to the existing lowerer.
//!
//! `patterns/sfe.lx:39` wrote that transformation by hand
//! (`@形状詞 | [unidic.pos3=形状詞可能]`) before the syntax existed. This module
//! mechanizes what the catalog author was already doing.
//!
//! # Why a single branch is left alone
//!
//! Every token test in every tracked catalog is a flat conjunction and yields
//! one branch. Sorting or deduplicating that branch would reorder its
//! attributes and move the fingerprint of every catalog in the repository, to
//! record a property none of them uses. Canonicalization therefore applies
//! only when distribution actually produced alternatives — which is also the
//! only case where two spellings of one meaning can arise.
//!
//! The asymmetry is what makes the multi-branch case free to sort *within* a
//! branch as well as across them: no single-branch program can observe it.

#[cfg(test)]
use super::super::AttrValueType;
use super::super::SymbolicAttrValue;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedTest {
    Atom(SymbolicAttrValue),
    And(Vec<ResolvedTest>),
    Or(Vec<ResolvedTest>),
    Not(Box<ResolvedTest>),
}

/// A test whose distribution exceeds the compile budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DnfOverflow {
    pub(crate) branches: usize,
    pub(crate) maximum: usize,
}

pub(crate) fn to_dnf(
    test: &ResolvedTest,
    max_branches: usize,
) -> Result<Vec<Box<[SymbolicAttrValue]>>, DnfOverflow> {
    let mut branches = distribute(test, false, max_branches)?;
    if branches.len() > 1 {
        // Canonical only in the multi-branch case; see the module docs.
        //
        // Each branch's *atoms* are sorted before the branch *list* is, and
        // both steps are load-bearing. Sorting the list by an order-insensitive
        // key while leaving the branches in source order looks equivalent and is
        // not: the keys of `(a & x)` and `(x & a)` compare equal, so a stable
        // sort leaves both in place and `dedup` — which compares the branches
        // themselves — keeps two. `[(a & x) | (x & a)]` and its mirror then
        // produce different `BoundExpr`, which is the cross-catalog drift this
        // canonicalization exists to prevent.
        for branch in &mut branches {
            branch.sort();
        }
        branches.sort();
        branches.dedup();
    }
    Ok(branches.into_iter().map(Vec::into_boxed_slice).collect())
}

/// De Morgan pushed to the leaves, then cross-product for `And`.
///
/// `negated` carries the complement inward rather than materializing a
/// rewritten tree, so `!!t` costs nothing and no intermediate allocation
/// survives the descent.
fn distribute(
    test: &ResolvedTest,
    negated: bool,
    max_branches: usize,
) -> Result<Vec<Vec<SymbolicAttrValue>>, DnfOverflow> {
    match test {
        ResolvedTest::Atom(atom) => {
            let mut atom = atom.clone();
            if negated {
                atom.complement = !atom.complement;
            }
            Ok(vec![vec![atom]])
        }
        ResolvedTest::Not(inner) => distribute(inner, !negated, max_branches),
        // Under negation `And` and `Or` swap, which is De Morgan and is why
        // one arm serves both.
        ResolvedTest::And(operands) if !negated => cross_product(operands, negated, max_branches),
        ResolvedTest::Or(operands) if negated => cross_product(operands, negated, max_branches),
        ResolvedTest::And(operands) | ResolvedTest::Or(operands) => {
            let mut branches = Vec::new();
            for operand in operands {
                branches.extend(distribute(operand, negated, max_branches)?);
                if branches.len() > max_branches {
                    return Err(DnfOverflow {
                        branches: branches.len(),
                        maximum: max_branches,
                    });
                }
            }
            Ok(branches)
        }
    }
}

fn cross_product(
    operands: &[ResolvedTest],
    negated: bool,
    max_branches: usize,
) -> Result<Vec<Vec<SymbolicAttrValue>>, DnfOverflow> {
    let mut accumulated: Vec<Vec<SymbolicAttrValue>> = vec![Vec::new()];
    for operand in operands {
        let operand_branches = distribute(operand, negated, max_branches)?;
        let width = accumulated.len().saturating_mul(operand_branches.len());
        if width > max_branches {
            return Err(DnfOverflow {
                branches: width,
                maximum: max_branches,
            });
        }
        let mut next = Vec::with_capacity(width);
        for prefix in &accumulated {
            for branch in &operand_branches {
                let mut combined = prefix.clone();
                combined.extend(branch.iter().cloned());
                next.push(combined);
            }
        }
        accumulated = next;
    }
    Ok(accumulated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maps the handful of attribute names every shape below names to the
    /// real `TokenAttributeIndex` values, so test bodies can stay readable.
    fn attr_index(name: &str) -> crate::token_match::AttributeKey {
        match name {
            "orth" => crate::token_match::AttributeKey::new("text").unwrap(),
            "pos1" => crate::token_match::AttributeKey::new("unidic.pos1").unwrap(),
            "pos2" => crate::token_match::AttributeKey::new("unidic.pos2").unwrap(),
            other => panic!("unknown test attribute {other}"),
        }
    }

    fn raw_atom(name: &str, value: &str, complement: bool) -> SymbolicAttrValue {
        SymbolicAttrValue {
            attr: attr_index(name),
            value: AttrValueType::Single(value.into()),
            complement,
        }
    }

    fn atom(name: &str, value: &str) -> ResolvedTest {
        ResolvedTest::Atom(raw_atom(name, value, false))
    }

    /// A complemented atom: true exactly where the plain atom is false,
    /// including on a token missing the attribute.
    fn complemented(name: &str, value: &str) -> ResolvedTest {
        ResolvedTest::Atom(raw_atom(name, value, true))
    }

    /// A token in the exhaustive oracle's tiny universe. `None` represents an
    /// attribute the token lacks entirely, which is the whole point: it is
    /// the case `atom_holds`'s `complement` early return exists for.
    #[derive(Clone, Debug)]
    struct TestToken {
        orth: Option<&'static str>,
        pos1: Option<&'static str>,
        pos2: Option<&'static str>,
    }

    impl TestToken {
        fn get(&self, attr: &crate::token_match::AttributeKey) -> Option<&str> {
            match attr.as_str() {
                "text" => self.orth,
                "unidic.pos1" => self.pos1,
                "unidic.pos2" => self.pos2,
                _ => None,
            }
        }
    }

    /// Mirrors `PredicateEvaluator::attribute_matches` exactly, including the
    /// `complement` early return on a missing attribute.
    fn atom_holds(atom: &SymbolicAttrValue, token: &TestToken) -> bool {
        let Some(actual) = token.get(&atom.attr) else {
            return atom.complement;
        };
        let held = match &atom.value {
            AttrValueType::Single(value) => actual == value.as_str(),
            AttrValueType::OneOf(values) => values.iter().any(|v| v.as_str() == actual),
            AttrValueType::NotIn(values) => !values.iter().any(|v| v.as_str() == actual),
            AttrValueType::SetHas(_)
            | AttrValueType::SetHasAny(_)
            | AttrValueType::SetNotHas(_)
            | AttrValueType::SetNotHasAny(_) => false,
            AttrValueType::Contains(needle) => actual.contains(needle.as_str()),
            AttrValueType::NotContains(needle) => !actual.contains(needle.as_str()),
            AttrValueType::Regex(re) => re.is_match(actual),
            AttrValueType::NotRegex(re) => !re.is_match(actual),
        };
        held != atom.complement
    }

    /// Every combination of present/absent × matching/non-matching for the
    /// three attributes `every_shape()` names — 4 × 3 × 3 = 36 tokens, small
    /// enough for the exhaustive loop to be fast.
    fn every_token() -> Vec<TestToken> {
        let orth_states: [Option<&'static str>; 4] = [None, Some("a"), Some("b"), Some("q")];
        let pos1_states: [Option<&'static str>; 3] = [None, Some("N"), Some("other")];
        let pos2_states: [Option<&'static str>; 3] = [None, Some("X"), Some("other")];
        let mut tokens =
            Vec::with_capacity(orth_states.len() * pos1_states.len() * pos2_states.len());
        for &orth in &orth_states {
            for &pos1 in &pos1_states {
                for &pos2 in &pos2_states {
                    tokens.push(TestToken { orth, pos1, pos2 });
                }
            }
        }
        tokens
    }

    /// Every shape `to_dnf` must handle, per the task brief: a bare atom; a
    /// complemented atom; `And` of two; `Or` of two; `Not(And)`; `Not(Or)`;
    /// `Not(Not(atom))`; `And(Or, Or)`; `Or(And, And)`; and one nesting three
    /// deep.
    fn every_shape() -> Vec<ResolvedTest> {
        vec![
            // A bare atom.
            atom("orth", "a"),
            // A complemented atom.
            complemented("orth", "a"),
            // And of two.
            ResolvedTest::And(vec![atom("orth", "a"), atom("pos1", "N")]),
            // Or of two.
            ResolvedTest::Or(vec![atom("orth", "a"), atom("orth", "b")]),
            // Not(And).
            ResolvedTest::Not(Box::new(ResolvedTest::And(vec![
                atom("orth", "a"),
                atom("pos1", "N"),
            ]))),
            // Not(Or).
            ResolvedTest::Not(Box::new(ResolvedTest::Or(vec![
                atom("orth", "a"),
                atom("orth", "b"),
            ]))),
            // Not(Not(atom)).
            ResolvedTest::Not(Box::new(ResolvedTest::Not(Box::new(atom("orth", "a"))))),
            // And(Or, Or).
            ResolvedTest::And(vec![
                ResolvedTest::Or(vec![atom("orth", "a"), atom("orth", "b")]),
                ResolvedTest::Or(vec![atom("pos1", "N"), atom("pos2", "X")]),
            ]),
            // Or(And, And).
            ResolvedTest::Or(vec![
                ResolvedTest::And(vec![atom("orth", "a"), atom("pos1", "N")]),
                ResolvedTest::And(vec![atom("orth", "b"), atom("pos2", "X")]),
            ]),
            // Three deep: Not(And(Or(...), Not(complemented atom))).
            ResolvedTest::Not(Box::new(ResolvedTest::And(vec![
                ResolvedTest::Or(vec![atom("orth", "a"), atom("orth", "b")]),
                ResolvedTest::Not(Box::new(complemented("pos2", "X"))),
            ]))),
        ]
    }

    /// Evaluate a tree directly, so the oracle never shares code with the
    /// normalizer it checks. Deliberately a different formulation: recursion
    /// over the tree versus DNF's distribution.
    fn evaluate_tree(test: &ResolvedTest, token: &TestToken) -> bool {
        match test {
            ResolvedTest::Atom(atom) => atom_holds(atom, token),
            ResolvedTest::And(operands) => operands.iter().all(|t| evaluate_tree(t, token)),
            ResolvedTest::Or(operands) => operands.iter().any(|t| evaluate_tree(t, token)),
            ResolvedTest::Not(inner) => !evaluate_tree(inner, token),
        }
    }

    fn evaluate_dnf(branches: &[Box<[SymbolicAttrValue]>], token: &TestToken) -> bool {
        branches
            .iter()
            .any(|branch| branch.iter().all(|atom| atom_holds(atom, token)))
    }

    /// The whole correctness claim, checked exhaustively rather than by
    /// example: over every token in a small universe, DNF and the tree agree.
    #[test]
    fn dnf_preserves_the_language_of_every_shape() {
        for shape in every_shape() {
            for token in every_token() {
                assert_eq!(
                    evaluate_dnf(&to_dnf(&shape, 4096).expect("shape is small"), &token),
                    evaluate_tree(&shape, &token),
                    "DNF diverged from the tree on shape {shape:?} at token {token:?}"
                );
            }
        }
    }

    /// `[a|b]` and `[b|a]` must produce the identical branch list, or two
    /// catalogs that mean the same thing report as drift and hash apart.
    #[test]
    fn multi_branch_output_is_canonically_ordered() {
        let ab = ResolvedTest::Or(vec![atom("orth", "a"), atom("orth", "b")]);
        let ba = ResolvedTest::Or(vec![atom("orth", "b"), atom("orth", "a")]);
        assert_eq!(to_dnf(&ab, 4096).unwrap(), to_dnf(&ba, 4096).unwrap());

        // Multi-atom branches, which single-atom cases cannot distinguish: when
        // a branch holds one atom its sort key *is* the branch, so ordering the
        // list by the key and ordering it by the branch coincide. With two atoms
        // they diverge, and sorting only the list leaves `(a & x)` and `(x & a)`
        // as two surviving branches whose spelling the output still records.
        let a_and_x = ResolvedTest::And(vec![atom("orth", "a"), atom("pos1", "N")]);
        let x_and_a = ResolvedTest::And(vec![atom("pos1", "N"), atom("orth", "a")]);
        let one_way = ResolvedTest::Or(vec![a_and_x.clone(), x_and_a.clone()]);
        let other_way = ResolvedTest::Or(vec![x_and_a, a_and_x]);
        assert_eq!(
            to_dnf(&one_way, 4096).unwrap(),
            to_dnf(&other_way, 4096).unwrap(),
            "`(a & x) | (x & a)` and its mirror are one meaning and must be one output"
        );
        assert_eq!(
            to_dnf(&one_way, 4096).unwrap().len(),
            1,
            "the two operands are the same conjunction, so canonicalization must \
             collapse them to a single branch rather than keeping both spellings"
        );
    }

    /// A flat conjunction must remain byte-identical to its input order.
    #[test]
    fn a_flat_conjunction_is_one_branch_in_source_order() {
        let flat = ResolvedTest::And(vec![
            atom("pos1", "助詞"),
            atom("orth", "を"),
            atom("pos2", "格助詞"),
        ]);
        let branches = to_dnf(&flat, 4096).unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(
            branches[0]
                .iter()
                .map(|a| a.attr.clone())
                .collect::<Vec<_>>(),
            vec![
                crate::token_match::AttributeKey::new("unidic.pos1").unwrap(),
                crate::token_match::AttributeKey::new("text").unwrap(),
                crate::token_match::AttributeKey::new("unidic.pos2").unwrap(),
            ],
            "a single branch must not be reordered — reordering moves every \
             existing catalog's fingerprint"
        );
    }

    #[test]
    fn blowup_beyond_the_budget_is_reported_not_built() {
        // (a|b) & (c|d) & (e|f) is 8 branches.
        let wide = ResolvedTest::And(vec![
            ResolvedTest::Or(vec![atom("orth", "a"), atom("orth", "b")]),
            ResolvedTest::Or(vec![atom("orth", "c"), atom("orth", "d")]),
            ResolvedTest::Or(vec![atom("orth", "e"), atom("orth", "f")]),
        ]);
        assert_eq!(to_dnf(&wide, 4096).unwrap().len(), 8);
        let overflow = to_dnf(&wide, 4).expect_err("8 branches exceeds a budget of 4");
        assert_eq!(overflow.maximum, 4);
    }

    /// The other overflow guard: `distribute`'s additive flatten arm
    /// (`And(operands) | Or(operands)`), reached two ways — a plain `Or`
    /// directly, and a negated `And` through De Morgan. The multiplicative
    /// `cross_product` path is `blowup_beyond_the_budget_is_reported_not_built`
    /// above; this covers the arm that sums branch counts instead.
    #[test]
    fn flatten_arm_overflow_is_reported_for_a_plain_or_and_its_de_morgan_and() {
        // A plain `Or` of three reaches the additive flatten arm directly:
        // `Or` with `negated == false` does not match the `cross_product`
        // guard clause, so it falls through to the summing loop.
        let plain_or = ResolvedTest::Or(vec![
            atom("orth", "p"),
            atom("orth", "q"),
            atom("orth", "r"),
        ]);
        assert_eq!(to_dnf(&plain_or, 4096).unwrap().len(), 3);
        let overflow = to_dnf(&plain_or, 2).expect_err("3 branches exceeds a budget of 2");
        assert_eq!(overflow.maximum, 2);

        // `Not(And(...))` reaches the *same* arm through De Morgan: the
        // `Not` flips `negated` to `true` before recursing into `And`, and
        // `And` with `negated == true` also falls through to the summing
        // loop rather than `cross_product`. A test that only drove the
        // plain `Or` above would leave this route unverified.
        let negated_and = ResolvedTest::Not(Box::new(ResolvedTest::And(vec![
            atom("orth", "p"),
            atom("orth", "q"),
            atom("orth", "r"),
        ])));
        assert_eq!(to_dnf(&negated_and, 4096).unwrap().len(), 3);
        let overflow = to_dnf(&negated_and, 2).expect_err("3 branches exceeds a budget of 2");
        assert_eq!(overflow.maximum, 2);
    }
}
