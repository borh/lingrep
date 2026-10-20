// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
//! The Boolean algebra of token tests.

use crate::support::{refuses_with, token_with};
use lingrep_core::fsm::{
    CompileError, CompileLimits, CompiledCatalog, DiagnosticCode, Match, PatternSource,
    compile_catalog,
};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

use crate::policy_helpers::compile_warnings;

/// A token carrying none of the attributes this suite reads — in particular,
/// no `pos1`. `TokenInfo::default()` sets every attribute to `None`, which is
/// exactly what "missing" means to `TokenInfoTrait::get`.
fn token_without_pos1() -> TokenInfo {
    TokenInfo::default()
}

fn matches_token(matcher: &CompiledCatalog, token: &TokenInfo) -> bool {
    !matcher.find_matches(std::slice::from_ref(token)).is_empty()
}

/// A token carrying only the given `pos1`, so the atom under test has
/// exactly one attribute to read.
fn token_with_pos1(pos1: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Pos1, pos1.into());
    token
}

/// A complemented atom is a *true* complement: it holds for a token that has
/// no such attribute at all. `!=` remains false for a missing attribute.
#[test]
fn a_complemented_atom_holds_for_a_token_missing_the_attribute() {
    let complemented = compile_single("atom := [!(unidic.pos1=動詞)].");
    let plain_negation = compile_single("atom := [unidic.pos1 not in {動詞}].");

    let token = token_without_pos1();

    assert!(
        matches_token(&complemented, &token),
        "!(unidic.pos1=動詞) must hold for a token with no pos1"
    );
    assert!(
        !matches_token(&plain_negation, &token),
        "pos1 not in {{動詞}} must not hold for a token with no pos1 — \
         this is the divergence S3.3 accepted"
    );
}

/// The complement is taken over the *whole* test, not just the missing-value
/// branch: `!(unidic.pos1=動詞)` must still behave as an ordinary negation once the
/// attribute is actually present, so it is `false` when `pos1` equals `動詞`
/// and `true` for any other `pos1`. A defective evaluator that special-cased
/// only the `None` branch of `attribute_matches` — for example by leaving
/// `held` unmodified whenever the attribute was present — would pass the
/// missing-attribute test above but fail here.
#[test]
fn a_complemented_atom_behaves_as_ordinary_negation_once_the_attribute_is_present() {
    let complemented = compile_single("atom := [!(unidic.pos1=動詞)].");

    let matching = token_with_pos1("動詞");
    let non_matching = token_with_pos1("名詞");

    assert!(
        !matches_token(&complemented, &matching),
        "!(unidic.pos1=動詞) must not hold for a token whose pos1 is 動詞"
    );
    assert!(
        matches_token(&complemented, &non_matching),
        "!(unidic.pos1=動詞) must hold for a token whose pos1 is 名詞"
    );
}

/// One sentence covering every attribute this suite's shapes name, in every
/// combination they distinguish — including two tokens that carry *no* `pos1`,
/// which is what separates `!(unidic.pos1=…)` from `pos1!=…`.
///
/// One corpus rather than one per test, so that an equivalence asserted below
/// is asserted over the same universe every time. A shape that agrees on a
/// narrow corpus and diverges on a wide one is the failure these tests exist
/// to catch.
fn corpus() -> Vec<TokenInfo> {
    use TokenAttributeIndex::{Orth, Pos1, Pos3};
    vec![
        // No attributes at all: `[]` matches it, `pos1=x` does not, and
        // `!(unidic.pos1=x)` does.
        TokenInfo::default(),
        token_with(&[(Orth, "a")]),
        token_with(&[(Orth, "b")]),
        token_with(&[(Orth, "q")]),
        token_with(&[(Pos1, "x")]),
        token_with(&[(Orth, "a"), (Pos1, "x")]),
        token_with(&[(Orth, "b"), (Pos1, "x")]),
        token_with(&[(Orth, "a"), (Pos1, "y")]),
        token_with(&[(Orth, "犬"), (Pos1, "名詞")]),
        token_with(&[(Orth, "猫"), (Pos1, "名詞")]),
        token_with(&[(Orth, "犬"), (Pos1, "動詞")]),
        token_with(&[(Orth, "走る"), (Pos1, "動詞")]),
        token_with(&[(Orth, "静か"), (Pos1, "形状詞")]),
        token_with(&[(Orth, "元気"), (Pos1, "名詞"), (Pos3, "形状詞可能")]),
        token_with(&[(Orth, "両方"), (Pos1, "形状詞"), (Pos3, "形状詞可能")]),
    ]
}

fn compile_single(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("token-test.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("must compile: {source}\n{error}"))
    .matcher
}

fn compile_single_err(source: &str) -> CompileError {
    match compile_catalog(
        PatternSource::named("token-test.lx", source),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("expected a compile error, but this compiled: {source}"),
        Err(error) => error,
    }
}

/// Complete `Match` values, not counts and not spans: `Match` carries the
/// pattern name and its canonically ordered captures, and two shapes that
/// agree on span while disagreeing on either are not the same pattern.
fn matches_of(matcher: &CompiledCatalog, tokens: &[TokenInfo]) -> Vec<Match> {
    matcher.find_matches(tokens)
}

/// Precedence inside `[…]` is `!` → `&` → `|`, so this parses as
/// `(a & b) | c`, not `a & (b | c)`.
#[test]
fn conjunction_binds_tighter_than_disjunction() {
    let corpus = corpus();
    let tight = compile_single("t := [unidic.pos1=名詞 & text=犬 | unidic.pos1=動詞].");
    let explicit = compile_single("t := [(unidic.pos1=名詞 & text=犬) | unidic.pos1=動詞].");
    let wrong = compile_single("t := [unidic.pos1=名詞 & (text=犬 | unidic.pos1=動詞)].");

    assert_eq!(matches_of(&tight, &corpus), matches_of(&explicit, &corpus));
    assert_ne!(
        matches_of(&tight, &corpus),
        matches_of(&wrong, &corpus),
        "the other grouping accepts a different language, so the assertion \
         above is not vacuous"
    );
}

/// Complement and negated operators diverge when the attribute is absent.
#[test]
fn a_named_test_complemented_differs_from_the_negated_operator() {
    let complement = compile_single(
        "test verbal := unidic.pos1=動詞.\n\
         t := [!@verbal].",
    );
    let operator = compile_single("t := [unidic.pos1!=動詞].");
    // Index 0 carries no `pos1` at all; every other token does.
    let tokens = corpus();

    assert_ne!(
        matches_of(&complement, &tokens),
        matches_of(&operator, &tokens),
        "the true complement must admit the token with no pos1, and `!=` must not"
    );
    let complement_starts = matches_of(&complement, &tokens)
        .iter()
        .map(|found| found.start)
        .collect::<Vec<_>>();
    let operator_starts = matches_of(&operator, &tokens)
        .iter()
        .map(|found| found.start)
        .collect::<Vec<_>>();
    assert!(
        complement_starts.contains(&0) && !operator_starts.contains(&0),
        "the divergence is exactly the attribute-less token: {complement_starts:?} \
         versus {operator_starts:?}"
    );
}

/// `|` inside `[…]` is sugar for what the catalogs already write by hand.
/// `sfe.lx:39` is the standing example.
#[test]
fn a_token_disjunction_equals_the_hand_written_alternation() {
    let corpus = corpus();
    let sugar = compile_single("t := [unidic.pos1=形状詞 | unidic.pos3=形状詞可能].");
    let manual = compile_single("t := [unidic.pos1=形状詞] | [unidic.pos3=形状詞可能].");
    assert_eq!(matches_of(&sugar, &corpus), matches_of(&manual, &corpus));
    assert!(
        !matches_of(&sugar, &corpus).is_empty(),
        "the corpus must actually exercise both branches"
    );
}

/// `&` is idempotent, commutative and associative inside `[…]` too, and De
/// Morgan and double negation hold over the complement.
#[test]
fn the_algebra_holds() {
    let corpus = corpus();
    for (left, right) in [
        ("t := [text=a | text=a].", "t := [text=a]."),
        ("t := [text=a | text=b].", "t := [text=b | text=a]."),
        ("t := [text=a & text=a].", "t := [text=a]."),
        (
            "t := [(text=a | text=b) & unidic.pos1=x].",
            "t := [unidic.pos1=x & (text=b | text=a)].",
        ),
        (
            "t := [!(text=a & unidic.pos1=x)].",
            "t := [!(text=a) | !(unidic.pos1=x)].",
        ),
        (
            "t := [!(text=a | unidic.pos1=x)].",
            "t := [!(text=a) & !(unidic.pos1=x)].",
        ),
        ("t := [!(!(text=a))].", "t := [text=a]."),
    ] {
        assert_eq!(
            matches_of(&compile_single(left), &corpus),
            matches_of(&compile_single(right), &corpus),
            "{left} and {right} must accept the same language"
        );
    }
}

/// A named test means exactly what writing its body inline would have meant.
#[test]
fn a_test_reference_is_substitution() {
    let corpus = corpus();
    let named = compile_single(
        "test nominal := unidic.pos1=名詞 | unidic.pos3=形状詞可能.\n\
         t := [@nominal & text=元気].",
    );
    let inline = compile_single("t := [(unidic.pos1=名詞 | unidic.pos3=形状詞可能) & text=元気].");
    assert_eq!(matches_of(&named, &corpus), matches_of(&inline, &corpus));
}

/// One name, one referent — whichever kind it is.
#[test]
fn a_test_may_not_share_a_name_with_a_set_or_a_definition() {
    for source in [
        "set X := {a}.\ntest X := text=a.\nt := [@X].",
        "fragment X := [text=a].\ntest X := text=a.\nt := [text=b] @X.",
        "test X := text=a.\ntest X := text=b.\nt := [@X].",
    ] {
        let error = compile_single_err(source);
        assert!(
            // `DuplicateDefinition` is the code `bind`'s `duplicate_name`
            // already reports for a set colliding with a definition; the plan's
            // `DuplicateName` is not a variant this compiler has.
            refuses_with(&error, DiagnosticCode::DuplicateDefinition),
            "{source} must be refused as a duplicate name, got {error}"
        );
    }
}

/// A test reference cycle is refused rather than expanded forever.
#[test]
fn a_test_reference_cycle_is_refused() {
    for source in [
        "test a := @b.\ntest b := @a.\nt := [@a].",
        "test a := @a.\nt := [@a].",
        "test a := text=x & @b.\ntest b := text=y | @c.\ntest c := !(@a).\nt := [@a].",
    ] {
        let error = compile_single_err(source);
        assert!(
            // `ReferenceCycle` is the code the definition graph already reports;
            // the plan's `CircularReference` is not a variant this compiler has.
            refuses_with(&error, DiagnosticCode::ReferenceCycle),
            "{source} must be refused as a cycle, got {error}"
        );
    }
}

/// A reference to a name that is not a test says which kind it found.
#[test]
fn a_reference_of_the_wrong_kind_inside_brackets_names_the_kind_it_found() {
    for (source, expected) in [
        ("fragment F := [text=a].\nt := [@F].", "is a fragment"),
        ("set S := {a}.\nt := [@S].", "is a set"),
        ("t := [@nope].", "undefined test @nope"),
    ] {
        let error = compile_single_err(source);
        assert!(
            refuses_with(&error, DiagnosticCode::UndefinedReference),
            "{source} must be refused as an undefined reference, got {error}"
        );
        assert!(
            error.to_string().contains(expected),
            "{source} must say `{expected}`, got {error}"
        );
    }
}

/// `|` does not terminate the grammar's bare `value` token, though `&` does.
///
/// So `[text=a|text=b]` — the exact analogue of the unspaced `[text=a&unidic.pos1=N]`
/// that means what it looks like — is **one atom** whose value is the literal
/// `a|text=b`. It compiles, matches nothing, and says nothing, which is
/// indistinguishable from the phenomenon being absent from the corpus. This is
/// new: before Boolean token tests, that spelling had one possible reading.
///
/// Refused rather than warned about, because a warning leaves the misreading
/// in the compiled catalog: the run still produces a matcher, the matcher
/// still matches nothing, and a warning on stderr is the only thing standing
/// between the author and a silently empty result set.
#[test]
fn an_unspaced_pipe_is_refused() {
    let error = compile_single_err("t := [text=a|text=b].");
    assert!(
        refuses_with(&error, DiagnosticCode::PipeInBareValue),
        "the unspaced spelling must be refused, got {error}"
    );
    // Both recoveries, because which one is right depends on what was meant
    // and the compiler cannot know: the escape if the pipe belongs to the
    // literal, the spacing if a disjunction does.
    let message = error.to_string();
    assert!(
        message.contains("a|text=b"),
        "the error must show the value that would have been built, got {message}"
    );
    assert!(
        message.contains("\\u007C"),
        "the error must name the escape recovery, got {message}"
    );
    assert!(
        message.contains("space the operands"),
        "the error must name the spacing recovery, got {message}"
    );
}

/// And the spelling the author meant still means it.
///
/// The point of refusing the unspaced form is that the spaced one is a
/// disjunction, so a test that only asserted the refusal would leave the half
/// that matters unpinned.
#[test]
fn the_spaced_disjunction_is_the_one_the_author_meant() {
    let corpus = corpus();
    let spaced = matches_of(&compile_single("t := [text=a | text=b]."), &corpus);
    assert_eq!(
        spaced,
        matches_of(&compile_single("t := ([text=a] | [text=b])."), &corpus),
        "the spaced spelling is the disjunction the author meant"
    );
    assert!(!spaced.is_empty(), "the fixture must discriminate the two");
}

/// The spellings that must stay accepted: a correctly spaced disjunction has
/// no bare value containing `|` at all, and a regex operand's `|` is
/// alternation doing exactly its job.
///
/// The second is why the check lives past the regex fork in
/// `lower_leaf_value`: `[text~/a|b/]` is the ordinary way to write this, and
/// refusing it would be a real loss rather than a disambiguation.
#[test]
fn a_spaced_disjunction_and_a_regex_alternation_still_compile() {
    for source in [
        "t := [text=a | text=b].",
        "t := [text~/a|b/].",
        "t := [text!~/a|b/].",
        "t := [text=a & unidic.pos1=名詞].",
    ] {
        let warnings = compile_warnings(source);
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.code() == DiagnosticCode::PipeInBareValue),
            "{source} must not warn, got {warnings:?}"
        );
    }
}

/// Every operand position an unquoted literal can occupy, because the check
/// sits at the one function all of them pass through and a reader should be
/// able to see that it does.
#[test]
fn every_unquoted_literal_position_is_covered() {
    for source in [
        "t := [text=a|b].",
        "t := [text!=a|b].",
        "t := [text~a|b].",
        "t := [text in {a|b, c}].",
        "t := [text not in {a|b, c}].",
        "set S := {a|b}.\nt := [text in @S].",
    ] {
        let error = compile_single_err(source);
        assert!(
            refuses_with(&error, DiagnosticCode::PipeInBareValue),
            "{source} must be refused, got {error}"
        );
    }
}

/// No tracked catalog spells a `|` inside an unquoted value, so making this an
/// error narrows nothing that ships here.
///
/// That was the evidence for keeping it a warning when the check was new, and
/// it is the evidence for the promotion now: the observation is the same, and
/// what changed is the judgement that a diagnostic which lets a
/// match-nothing catalog through is not a mitigation. The escape `|`
/// keeps every literal that genuinely contains a pipe writable, so the
/// refusal costs a spelling and no capability.
#[test]
fn no_tracked_catalog_contains_a_bare_value_with_a_pipe() {
    for (name, text) in lingrep_core::testing::catalogs::tracked_catalog_sources() {
        compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
            .unwrap_or_else(|error| panic!("{name} must compile: {error}"));
    }
}

/// The escape is the recovery the error names, so it has to work.
///
/// `|` decodes after tokenization, which is what lets a literal pipe
/// survive a rule that refuses a raw one — the same mechanism `[text=a.b]`
/// relies on for `.`.
#[test]
fn the_escaped_pipe_recovery_compiles_and_decodes() {
    let corpus = [token_with(&[(TokenAttributeIndex::Orth, "a|b")])];
    let matches = matches_of(&compile_single("t := [text=a\\u007Cb]."), &corpus);
    assert_eq!(
        matches.len(),
        1,
        "the escaped pipe must decode to the literal `a|b`, got {matches:?}"
    );
}

/// Metadata quotes every value and a token test quotes none, so the one
/// convention carried across that boundary produces a literal with the quotes
/// in it.
///
/// `[text="の"]` compiles clean and matches nothing — the
/// [`DiagnosticCode::PipeInBareValue`] failure mode reached from the other
/// direction. Measured on a 600-sentence GiNZA CoNLL-U corpus before the
/// refusal existed: `[text="の"]` published 0 records where `[text=の]`
/// published 841, and neither spelling warned.
#[test]
fn a_quoted_token_test_value_is_refused() {
    let error = compile_single_err("t := [text=\"の\"].");
    assert!(
        refuses_with(&error, DiagnosticCode::QuotedAttributeValue),
        "a quoted token-test value must be refused, got {error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("\"の\""),
        "the error must show the literal that would have been built, got {message}"
    );
    assert!(
        message.contains("Write `の`"),
        "the error must name the unquoted spelling, got {message}"
    );
    assert!(
        message.contains("\\u0022"),
        "the error must name the escape for a value that really is quoted, got {message}"
    );
}

/// Every operand position, for the reason the pipe check covers all of them:
/// one function sees every unquoted literal, and a reader should be able to
/// confirm that from the test rather than from the implementation.
///
/// `has` and `has any` are here and absent from the pipe test because
/// `conllu.misc has "BunsetuBILabel=B"` is the spelling that cost a debug
/// cycle: it published 0 where the unquoted form published 5141, a figure
/// independently equal to `ginza.bunsetu_spans()`.
#[test]
fn every_unquoted_literal_position_refuses_a_quoted_value() {
    for source in [
        "t := [text=\"a\"].",
        "t := [text!=\"a\"].",
        "t := [text~\"a\"].",
        "t := [text in {\"a\", b}].",
        "t := [text not in {\"a\", b}].",
        "set S := {\"a\"}.\nt := [text in @S].",
        "t := [conllu.misc has \"a=b\"].",
        "t := [conllu.misc has any {\"a=b\"}].",
    ] {
        let error = compile_single_err(source);
        assert!(
            refuses_with(&error, DiagnosticCode::QuotedAttributeValue),
            "{source} must be refused, got {error}"
        );
    }
}

/// The refusal is about a value wrapped in quotes, not about a quote
/// character, and the recovery the error names has to work.
#[test]
fn the_quote_escape_recovery_and_an_interior_quote_still_compile() {
    let corpus = [token_with(&[(TokenAttributeIndex::Orth, "\"x\"")])];
    let matches = matches_of(&compile_single("t := [text=\\u0022x\\u0022]."), &corpus);
    assert_eq!(
        matches.len(),
        1,
        "the escaped quotes must decode to the literal `\"x\"`, got {matches:?}"
    );

    for source in [
        // One quote, not a wrapping pair.
        "t := [text=a\"b].",
        // A regex operand never reaches the literal decoder.
        "t := [text~/\"a\"/].",
    ] {
        compile_catalog(
            PatternSource::named("quote.lx", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{source} must compile: {error}"));
    }
}

/// And the other direction, which the same asymmetry made conspicuous: a
/// `test` named where an *expression* belongs. A test is a token predicate, so
/// `@t` outside `[…]` is the wrong kind in the wrong place exactly as a set is
/// — and reporting "undefined reference" would send the author hunting for a
/// typo with the declaration two lines above.
#[test]
fn a_test_named_in_expression_position_names_the_kind_it_found() {
    let error = compile_single_err("test t := text=a.\npattern p := @t.");
    assert!(
        refuses_with(&error, DiagnosticCode::UndefinedReference),
        "a test in expression position is an undefined reference, got {error}"
    );
    assert!(
        error.to_string().contains("is a test"),
        "the message must name the kind it found, got {error}"
    );
    assert!(
        !error.to_string().contains("undefined reference @t"),
        "the kind-blind wording must not survive, got {error}"
    );
}

/// `[]` is still the any-token predicate, and a redundant grouping is still
/// pure syntax — the two shapes a flat catalog depends on.
#[test]
fn the_any_token_predicate_and_a_redundant_group_are_unchanged() {
    let corpus = corpus();
    let any = compile_single("t := [].");
    assert_eq!(
        matches_of(&any, &corpus).len(),
        corpus.len(),
        "`[]` constrains nothing, so it matches every token including the \
         attribute-less one"
    );
    assert_eq!(
        matches_of(&compile_single("t := [(text=a)]."), &corpus),
        matches_of(&compile_single("t := [text=a]."), &corpus),
    );
}

/// Distribution is bounded, and the diagnostic says by how much and what to
/// do.
///
/// Seven two-way disjunctions conjoined is 128 branches, over the 64 budget.
/// `TokenTestTooComplex` is its own code rather than a shared `LimitExceeded`
/// — see `a_test_reference_chain_that_doubles_at_each_level_is_refused` for
/// the sibling budget this is distinguished from — so the assertion is on the
/// code alone; no substring check is needed to tell the two apart.
#[test]
fn an_over_wide_token_test_is_refused_with_its_branch_count() {
    let body = (0..7)
        .map(|i| format!("(text=a{i} | text=b{i})"))
        .collect::<Vec<_>>()
        .join(" & ");
    let error = compile_single_err(&format!("t := [{body}]."));
    assert!(refuses_with(&error, DiagnosticCode::TokenTestTooComplex));
}

/// A `test` that names another test twice doubles the *resolved tree* at every
/// level, so an eleven-deep chain expands past any fixed budget from four lines
/// of source. That expansion is bounded at substitution, not at distribution.
///
/// The branch budget cannot catch it: the chain is a pure conjunction, so it
/// distributes to exactly **one** branch however deep it goes, and the branch
/// budget (`DiagnosticCode::TokenTestTooComplex`) is never approached. Only
/// the node budget (`DiagnosticCode::TokenTestExpansionTooLarge`) stands
/// between this and an exponential compile — its own code, distinct from the
/// branch budget's, so the assertion below is on the code alone.
///
/// Both directions are checked. A budget that simply refused every chain would
/// satisfy the first assertion alone while making named tests unusable.
/// `test t0 := text=a.` followed by `test tN := @t(N-1) & @t(N-1).`, and a
/// pattern naming the deepest one `references` times.
///
/// Level `n` resolves to `2^(n+1) - 1` nodes, and every reference to it pays
/// that expansion again — which is the whole point of
/// `a_catalog_amplifying_a_deep_chain_across_many_tokens_is_refused_promptly`
/// below.
fn doubling_chain_with(levels: usize, references: usize) -> String {
    let mut source = String::from("test t0 := text=a.\n");
    for level in 1..levels {
        source.push_str(&format!(
            "test t{level} := @t{prev} & @t{prev}.\n",
            prev = level - 1
        ));
    }
    let token = format!("[@t{}]", levels - 1);
    source.push_str("t := ");
    source.push_str(&vec![token; references].join(" "));
    source.push_str(".\n");
    source
}

fn doubling_chain(levels: usize) -> String {
    doubling_chain_with(levels, 1)
}

#[test]
fn a_test_reference_chain_that_doubles_at_each_level_is_refused() {
    let error = compile_single_err(&doubling_chain(12));
    assert!(
        refuses_with(&error, DiagnosticCode::TokenTestExpansionTooLarge),
        "a twelve-level doubling chain must be refused, got {error}"
    );

    // Eight levels is 510 resolved nodes against a budget of 4096, so it must
    // still compile — and mean what the single attribute it expands to means.
    let shallow = compile_single(&doubling_chain(8));
    let corpus = corpus();
    assert_eq!(
        matches_of(&shallow, &corpus),
        matches_of(&compile_single("t := [text=a]."), &corpus),
        "a chain within budget compiles, and `a & a & … & a` is `a`"
    );
}

/// The per-test node budget bounds one `[…]`. It cannot bound a *catalog*: a
/// chain that fits inside it, named by thirty thousand tokens, pays its whole
/// expansion thirty thousand times, and a budget rebuilt per token test sees
/// only each payment.
///
/// This is the compile-work amplification guarded for `set` members: a
/// budget charged after the work rather than during it — reintroduced by
/// inlining `test` bodies. So this asserts the wall clock as well as the code:
/// a refusal that arrives after the exponential work has already been done is
/// not the property under test, and only elapsed time can tell the two apart.
///
/// Ten levels is 1,023 resolved nodes, comfortably inside the 4,096 per-test
/// budget, so the per-test check alone would admit every one of the thirty
/// thousand copies.
#[test]
fn a_catalog_amplifying_a_deep_chain_across_many_tokens_is_refused_promptly() {
    let source = doubling_chain_with(10, 30_000);
    let started = std::time::Instant::now();
    let error = compile_single_err(&source);
    let elapsed = started.elapsed();

    assert!(
        refuses_with(&error, DiagnosticCode::TokenTestExpansionTooLarge),
        "an amplified chain must be refused, got {error}"
    );
    // Thirty thousand copies of a 1,023-node chain is 30.7M resolved nodes;
    // the document budget stops at 262,144, two orders of magnitude fewer.
    // The bound is deliberately loose against a slow or loaded machine and is
    // still far below the minutes an unbounded expansion costs here.
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "refusal must not wait for the full expansion; took {elapsed:?}"
    );
    // One report, not one per remaining token: an exhausted document budget
    // refuses every `[…]` that follows it.
    assert_eq!(
        error
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DiagnosticCode::TokenTestExpansionTooLarge)
            .count(),
        1,
        "an exhausted catalog budget is reported once"
    );
}

/// A distributed token is still one token wide, so quantifying it means what
/// quantifying the hand-written alternation means.
///
/// This is where replacing `BoundExpr::Token` with `BoundExpr::Alternation`
/// could go wrong invisibly: a quantifier over a wider expression counts
/// repetitions of something else.
#[test]
fn a_disjunctive_token_composes_with_a_quantifier() {
    let corpus = corpus();
    assert_eq!(
        matches_of(&compile_single("t := [text=a | text=b]{2}."), &corpus),
        matches_of(&compile_single("t := ([text=a] | [text=b]){2}."), &corpus),
    );
    assert_eq!(
        matches_of(&compile_single("t := [text=a | text=b]+."), &corpus),
        matches_of(&compile_single("t := ([text=a] | [text=b])+."), &corpus),
    );
}

/// And inside a lookaround body, which is answered by a boundary sweep rather
/// than by the recognizer.
#[test]
fn a_disjunctive_token_composes_with_a_lookaround() {
    let corpus = corpus();
    assert_eq!(
        matches_of(
            &compile_single("t := [text=b] followed_by([text=q | unidic.pos1=x])."),
            &corpus,
        ),
        matches_of(
            &compile_single("t := [text=b] followed_by([text=q] | [unidic.pos1=x])."),
            &corpus,
        ),
    );
    assert_eq!(
        matches_of(
            &compile_single(
                "test absent_pos1 := !(unidic.pos1=x).\nt := [text=a] not_preceded_by([@absent_pos1]).",
            ),
            &corpus,
        ),
        matches_of(
            &compile_single("t := [text=a] not_preceded_by([!(unidic.pos1=x)])."),
            &corpus,
        ),
    );
}

/// A `test` nothing reachable references is reported, and the report is
/// transitive: a test held only by an unreachable fragment is unused in the
/// same run that reports the fragment. The
/// set reporting already reuses.
#[test]
fn an_unreferenced_test_is_reported_transitively() {
    let warnings = compile_warnings(
        "test lonely := text=a.\n\
         fragment orphan := [@lonely].\n\
         pattern live := [text=b].",
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.code() == DiagnosticCode::UnusedTest)
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.code() == DiagnosticCode::UnusedFragment)
    );
}

/// The converse of the cascade above: a test referenced by an
/// always-reachable `pattern` is used, not unused, so it must not appear in
/// `UnusedTest` warnings. `compile_warnings` panics if the catalog fails to
/// compile, so a warning-free result here is a genuine "not reported", not an
/// empty list produced by a compile failure.
#[test]
fn a_test_referenced_by_an_always_reachable_pattern_is_not_reported_unused() {
    let warnings = compile_warnings("test used := text=a.\npattern live := [@used].");
    assert!(
        !warnings
            .iter()
            .any(|w| w.code() == DiagnosticCode::UnusedTest),
        "a test reachable from an always-reachable pattern must not be unused: {warnings:?}"
    );
}

/// `test` joins the keywords a *bare* definition cannot be named. The
/// collision is lexical, so an explicit declaration escapes it.
///
/// Both refusals turn out to be `Syntax`, not a semantic duplicate-name or
/// reserved-name code: the grammar itself has no rule that parses a bare
/// `test := …` definition, since `test` there opens a `test` declaration
/// instead, so the parser reports a parse failure rather than the compiler
/// reporting a name collision.
#[test]
fn test_is_reserved_for_a_bare_definition_only() {
    let error = compile_single_err("test := [text=a].");
    assert!(
        refuses_with(&error, DiagnosticCode::Syntax),
        "a bare `test := …` must fail lexically as a syntax error, got {error}"
    );
    compile_single("pattern test := [text=a].");
}

/// A metadata key cannot be `test:`, for the same lexical reason: `test` opens
/// a `test` declaration, not a metadata annotation, so this is also `Syntax`
/// rather than a semantic schema violation.
#[test]
fn test_is_not_available_as_a_metadata_key() {
    let error = compile_single_err("test: \"x\"\npattern p := [text=a].");
    assert!(
        refuses_with(&error, DiagnosticCode::Syntax),
        "a `test:` metadata key must fail lexically as a syntax error, got {error}"
    );
}
