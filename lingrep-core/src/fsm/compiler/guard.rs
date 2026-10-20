//! Compiling a transform guard written inside metadata.
//!
//! Guards are lingrep patterns token predicates, not a second predicate dialect that
//! resembles one. The cheapest way to make that true
//! rather than aspirational is to hand the authored text to the same parser a
//! definition body goes through: `[upos in {AUX, ADP} & lemma != れる]` then
//! means here exactly what it means there, including the rule that a token
//! lacking the attribute satisfies neither polarity — which arrives for free
//! instead of being reimplemented and drifting.
//!
//! # Why only a token predicate
//!
//! The guard grammar is deliberately narrower than the expression grammar, on
//! two independent grounds.
//!
//! Every v1 op judges **one token at a time**: `drop_edge` walks inward token
//! by token, `truncate_before` tests one position, `substitute` targets one
//! edge. A sequence or a quantified body is therefore a guard that no op could
//! evaluate — it would compile, and then be rejected by whichever op tried to
//! use it. Enforcing that at each operation would defer an authoring error, so
//! the parser rejects wide guards at their declaration.
//!
//! A reference is refused for a second and stronger reason: reference-driven
//! edge removal is what a `policy` block's `detach` statement already performs.
//! Admitting `@FRAG` here would ship a second general edge-removal mechanism
//! differing from the first only in whether the operand is spelled as a
//! predicate or as a name. The diagnostic therefore points at `detach` rather
//! than merely refusing.
//!
//! Captures and lookaround are refused as meaningless in this position: there
//! is no match in progress to bind a capture to, and no surrounding context for
//! an assertion to consult.
//!
//! A named set — `[upos in @NAME]` — is refused on a third ground: timing. A
//! `set` is scoped to the accumulated catalog and resolves at bind, whereas a
//! guard is compiled with the definition whose metadata carries it, so the
//! table the name would resolve against does not exist yet. The alternative is
//! not "resolve it later" but "resolve it to nothing", and a guard that
//! silently matches no token is worse than one the author is told to rewrite.

use super::model::{ParsedAttr, ParsedAttrValue, ParsedExpr, ParsedTest};
use super::{CompileLimits, PatternSource, lower::lower_document};
use crate::fsm::AttrValue;
use crate::fsm::policy::CompiledGuard;

/// The synthetic definition name a guard is lowered under.
///
/// Short and unremarkable because it can reach a researcher: a syntax error
/// inside a guard is reported by the parser against this document, and the
/// caller quotes that message. It carries no `__policy__` prefix — that prefix
/// marks synthesized *emitters*, which participate in matching, and a guard
/// never does.
const GUARD_DEFINITION: &str = "guard";

/// Compiles one guard source into an evaluable token predicate.
///
/// The error is a bare sentence rather than a `Diagnostic`: the caller knows
/// which policy key and which definition the guard was written on, and it
/// composes that context around this. Reporting from here would either lose
/// that context or duplicate it.
pub(super) fn compile_guard(source: &str) -> Result<CompiledGuard, String> {
    let document = format!("{GUARD_DEFINITION} := {source}.");
    let program = lower_document(
        &PatternSource::named("<guard>", &document),
        &CompileLimits::default(),
    )
    .map_err(|error| format!("is not a valid lingrep patterns token predicate ({error})"))?;

    let [definition] = program.definitions.as_slice() else {
        // One definition in, one definition out. A guard source containing its
        // own `.` terminator would land here rather than silently compiling
        // only its first clause.
        return Err("must be a single token predicate, not several declarations".to_owned());
    };

    match unwrap_groups(
        definition
            .expr()
            .expect("guard source lowers to an ordinary pattern"),
    ) {
        ParsedExpr::Token(test) => Ok(CompiledGuard::new(
            flatten_guard_conjunction(test)?
                .into_iter()
                .map(resolve_guard_attr)
                .collect::<Result<Vec<_>, _>>()?,
            source.trim().to_owned(),
        )),
        ParsedExpr::Reference { name, .. } => Err(format!(
            "names `@{name}`, but a guard is a token predicate — removing an edge by naming a \
             definition is what a `policy` block's `detach … relation: at_end` does"
        )),
        ParsedExpr::NamedGroup { .. } | ParsedExpr::SameAs { .. } => Err(
            "may not capture: a guard is evaluated against one token, with no match in progress \
             to bind a name in"
                .to_owned(),
        ),
        ParsedExpr::Lookaround(_) | ParsedExpr::Assertion(_) => Err(
            "may not assert position: a guard is evaluated against one token, with no surrounding \
             context to consult"
                .to_owned(),
        ),
        // `Group` cannot arrive here, since `unwrap_groups` strips it. It
        // shares this arm rather than panicking: the match stays total by
        // construction, and a future parser change that produced a nested
        // group would give a researcher a sentence instead of a crash.
        ParsedExpr::Sequence(_)
        | ParsedExpr::Alternation(_)
        | ParsedExpr::AnyOrder { .. }
        | ParsedExpr::Intersection { .. }
        | ParsedExpr::Repeat { .. }
        | ParsedExpr::Group(_) => Err(
            "must be a single token predicate — every operation that reads a guard judges one \
             token at a time, so a wider expression has nothing to be evaluated against"
                .to_owned(),
        ),
    }
}

/// Flattens a guard's token test to the conjunction a [`CompiledGuard`] is.
///
/// `|` and `!` are refused rather than distributed. A guard is one conjunction
/// of attributes evaluated against one token by [`CompiledGuard`], so a
/// disjunction has no representation here — the operation that reads the guard
/// would have to become a matcher to honour it. A test reference is refused for
/// the reason a set reference is, and the message below says so.
fn flatten_guard_conjunction(test: &ParsedTest) -> Result<Vec<&ParsedAttr>, String> {
    let mut attributes = Vec::new();
    collect_guard_attrs(test, &mut attributes)?;
    Ok(attributes)
}

fn collect_guard_attrs<'a>(
    test: &'a ParsedTest,
    out: &mut Vec<&'a ParsedAttr>,
) -> Result<(), String> {
    match test {
        ParsedTest::Attr(attribute) => {
            out.push(attribute);
            Ok(())
        }
        ParsedTest::And(operands) => {
            for operand in operands {
                collect_guard_attrs(operand, out)?;
            }
            Ok(())
        }
        ParsedTest::Or(_) | ParsedTest::Not(_) => Err(
            "must be a conjunction of attribute tests: every operation that reads a guard judges \
             one token against one predicate, so `|` and `!` have nothing to be evaluated as"
                .to_owned(),
        ),
        ParsedTest::Reference { name, .. } => Err(format!(
            "references the test `@{name}`, but a guard is compiled with the definition that \
             carries it, before the catalog's `test` declarations are accumulated — write the \
             attributes inline"
        )),
    }
}

/// Sees through redundant parentheses, which are pure syntax.
fn unwrap_groups(expr: &ParsedExpr) -> &ParsedExpr {
    let mut current = expr;
    while let ParsedExpr::Group(inner) = current {
        current = inner;
    }
    current
}

/// Resolves one parsed guard attribute to its evaluable form.
///
/// A named set is refused here rather than resolved. A guard is compiled with
/// the definition whose metadata carries it, while a `set` is scoped to the
/// accumulated catalog and resolves at bind — so at this point the table the
/// name would be looked up in does not exist yet. Resolving to an empty list
/// instead would give the author a guard that silently matches nothing, which
/// is the one outcome worse than a refusal.
fn resolve_guard_attr(parsed: &ParsedAttr) -> Result<crate::fsm::SymbolicAttrValue, String> {
    match &parsed.value {
        ParsedAttrValue::Resolved(value) => Ok(AttrValue {
            attr: parsed.attr.clone(),
            value: value.clone(),
            complement: false,
        }),
        ParsedAttrValue::Set { name, .. } | ParsedAttrValue::SetAny { name, .. } => Err(format!(
            "references the set `@{name}`, but a guard is compiled with the definition that \
             carries it, before the catalog's `set` declarations are accumulated — write the \
             members inline"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_match::{TokenAttributeIndex, TokenInfoTrait};
    use compact_str::CompactString;

    /// A token carrying exactly the attributes it was given, so that "absent"
    /// is expressible — the neither-polarity rule is otherwise untestable.
    struct Token(Vec<(TokenAttributeIndex, CompactString)>);

    impl Token {
        fn new(attributes: &[(TokenAttributeIndex, &str)]) -> Self {
            Self(
                attributes
                    .iter()
                    .map(|(index, value)| (*index, CompactString::new(value)))
                    .collect(),
            )
        }
    }

    impl TokenInfoTrait for Token {
        fn get(&self, index: TokenAttributeIndex) -> Option<&CompactString> {
            self.0
                .iter()
                .find(|(candidate, _)| *candidate == index)
                .map(|(_, value)| value)
        }
    }

    #[test]
    fn a_token_predicate_admits_and_refuses_by_the_matchers_own_semantics() {
        let guard = compile_guard("[upos=AUX]").expect("a token predicate compiles");
        assert!(guard.matches(&Token::new(&[(TokenAttributeIndex::Pos, "AUX")])));
        assert!(!guard.matches(&Token::new(&[(TokenAttributeIndex::Pos, "VERB")])));
        // Neither polarity: a token with no `pos` is not `upos=AUX`, and would
        // not be `upos!=AUX` either.
        assert!(!guard.matches(&Token::new(&[(TokenAttributeIndex::Lemma, "AUX")])));
    }

    #[test]
    fn set_membership_and_negation_compile_as_one_predicate() {
        let guard = compile_guard("[upos in {AUX, ADP} & lemma != れる]")
            .expect("a conjunction of attribute tests compiles");
        assert!(guard.matches(&Token::new(&[
            (TokenAttributeIndex::Pos, "ADP"),
            (TokenAttributeIndex::Lemma, "か"),
        ])));
        assert!(!guard.matches(&Token::new(&[
            (TokenAttributeIndex::Pos, "AUX"),
            (TokenAttributeIndex::Lemma, "れる"),
        ])));
        assert!(!guard.matches(&Token::new(&[
            (TokenAttributeIndex::Pos, "VERB"),
            (TokenAttributeIndex::Lemma, "か"),
        ])));
    }

    #[test]
    fn the_any_token_predicate_admits_a_token_with_no_attributes() {
        let guard = compile_guard("[]").expect("the any-token predicate compiles");
        assert!(guard.matches(&Token::new(&[])));
    }

    #[test]
    fn redundant_parentheses_are_not_a_wider_expression() {
        let guard = compile_guard("([upos=AUX])").expect("a parenthesized predicate compiles");
        assert!(guard.matches(&Token::new(&[(TokenAttributeIndex::Pos, "AUX")])));
    }

    #[test]
    fn a_reference_is_refused_and_points_at_the_operation_that_takes_one() {
        let error = compile_guard("@DA").expect_err("a reference is not a guard");
        assert!(error.contains("detach"), "{error}");
    }

    /// A named set inside a guard is refused, not resolved to nothing.
    ///
    /// `[upos in @NAME]` is one token predicate, so it is reachable authored
    /// syntax — the guard grammar's narrowing does not exclude it. What
    /// excludes it is timing: a `set` resolves against the accumulated
    /// catalog at bind, and a guard is compiled with its definition, before
    /// any catalog exists. The only resolution available here is the empty
    /// list, which would silently match no token at all.
    #[test]
    fn a_named_set_is_refused_because_the_catalog_does_not_exist_yet() {
        let error =
            compile_guard("[upos in @CONTENT]").expect_err("a set reference is not a guard");
        assert!(error.contains("CONTENT"), "names the set: {error}");
        assert!(
            error.contains("before the catalog's `set` declarations are accumulated"),
            "says why, rather than only that: {error}"
        );
    }

    /// A named test inside a guard is refused for the same reason a named set
    /// is, and the mirror of
    /// [`a_named_set_is_refused_because_the_catalog_does_not_exist_yet`].
    ///
    /// `[@boundary]` is one token predicate, so it is reachable authored syntax
    /// — a guard arrives as a `transform:` metadata string, which the grammar
    /// narrows no further than a `[…]` body. What excludes it is timing: a
    /// `test` resolves against the accumulated catalog at bind, and a guard is
    /// compiled with its definition, before any catalog exists.
    #[test]
    fn a_named_test_is_refused_because_the_catalog_does_not_exist_yet() {
        let error = compile_guard("[@boundary]").expect_err("a test reference is not a guard");
        assert!(error.contains("boundary"), "names the test: {error}");
        assert!(
            error.contains("before the catalog's `test` declarations are accumulated"),
            "says why, rather than only that: {error}"
        );
    }

    /// `|` and `!` inside a guard are refused rather than distributed.
    ///
    /// Also reachable authored syntax, and refused for a different reason than
    /// the reference above: a `CompiledGuard` *is* one conjunction of
    /// attributes, so there is nowhere to put a second branch. Distribution
    /// would mean the operation reading the guard became a matcher, which is
    /// the same argument `a_sequence_is_refused_because_no_operation_could_evaluate_it`
    /// makes about width.
    #[test]
    fn a_disjunction_or_a_complement_is_refused_because_a_guard_is_one_conjunction() {
        for source in [
            "[upos=AUX | upos=ADP]",
            "[!(upos=AUX)]",
            "[upos=AUX & !(lemma=だ)]",
        ] {
            let error = compile_guard(source).expect_err("a Boolean test is not a guard");
            assert!(
                error.contains("conjunction of attribute tests"),
                "{source} must be refused as not a conjunction: {error}"
            );
            assert!(
                error.contains("`|` and `!`"),
                "{source} must name the operators it refused: {error}"
            );
        }
    }

    #[test]
    fn a_sequence_is_refused_because_no_operation_could_evaluate_it() {
        // Nothing reads a multi-token guard, so admitting one would only defer
        // the error beyond the authored declaration.
        let error = compile_guard("[upos=AUX] [upos=ADP]").expect_err("a sequence is not a guard");
        assert!(error.contains("single token predicate"), "{error}");
    }

    #[test]
    fn a_quantifier_is_refused_for_the_same_reason_as_a_sequence() {
        let error = compile_guard("[upos=AUX]+").expect_err("a quantified body is not a guard");
        assert!(error.contains("single token predicate"), "{error}");
    }

    #[test]
    fn a_capture_is_refused_because_there_is_no_match_to_bind_it_in() {
        let error = compile_guard("(?<x> [upos=AUX])").expect_err("a named group is not a guard");
        assert!(error.contains("may not capture"), "{error}");
    }

    #[test]
    fn lookaround_is_refused_because_a_guard_sees_one_token() {
        let error =
            compile_guard("followed_by([upos=AUX])").expect_err("lookaround is not a guard");
        assert!(error.contains("may not assert position"), "{error}");
    }

    #[test]
    fn a_syntactically_broken_guard_reports_the_parse_failure() {
        let error = compile_guard("[upos=").expect_err("unbalanced brackets do not compile");
        assert!(
            error.contains("not a valid lingrep patterns token predicate"),
            "{error}"
        );
    }

    #[test]
    fn guards_are_equal_when_they_constrain_tokens_identically() {
        // Equality ignores authored whitespace, so a policy comparison does not
        // turn on formatting.
        assert_eq!(
            compile_guard("[upos=AUX]").expect("compiles"),
            compile_guard("[ upos = AUX ]").expect("compiles"),
        );
        assert_ne!(
            compile_guard("[upos=AUX]").expect("compiles"),
            compile_guard("[upos=ADP]").expect("compiles"),
        );
    }
}
