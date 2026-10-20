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
//! Named value sets: `set NAME := {…}.` and `[attr in @NAME]`.

use lingrep_core::fsm::{
    CompileLimits, CompiledCatalog, DiagnosticCode, FsmBuilder, PatternSource, compile_catalog,
};
use lingrep_core::testing::tokenize_japanese;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

/// A token carrying only `orth` and `kana`, for the `same_as` projection.
fn token(orth: &str, kana: &str) -> TokenInfo {
    let mut token = TokenInfo::default();
    token.set(TokenAttributeIndex::Orth, orth.into());
    token.set(TokenAttributeIndex::Kana, kana.into());
    token
}

fn compile(source: &str) -> CompiledCatalog {
    compile_catalog(
        PatternSource::named("sets.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile: {error}"))
    .matcher
}

/// A named set and the inline set it replaces compile to the same matcher.
///
/// Resolution must produce the
/// literal vector the written-out form produces, so the migration in Tasks 9
/// and 10 cannot change what a catalog matches.
#[test]
fn a_named_set_matches_exactly_where_its_inline_form_does() {
    let named =
        compile("set IU := {言う, いう}.\npattern p := [text=と] [unidic.orthbase in @IU].\n");
    let inline = compile("pattern p := [text=と] [unidic.orthbase in {言う, いう}].\n");

    let tokens = tokenize_japanese("それは事実だという");
    assert_eq!(
        named.find_matches(&tokens),
        inline.find_matches(&tokens),
        "a named set and its inline form are the same predicate"
    );
    assert!(
        !named.find_matches(&tokens).is_empty(),
        "the fixture must actually match, or this test compares two empties"
    );
}

/// One declaration serves both polarities.
///
/// Member validation is polarity-independent, so the negation belongs to the
/// reference. A set referenced under `in` and `not in` yields complementary
/// predicates from one list.
#[test]
fn one_set_serves_both_polarities() {
    let matcher = compile(
        "set CONTENT := {動詞, 助動詞}.\n\
         pattern verbal := [unidic.pos1 in @CONTENT].\n\
         pattern other := [unidic.pos1 not in @CONTENT].\n",
    );
    let tokens = tokenize_japanese("猫が走る");
    let matches = matcher.find_matches(&tokens);
    assert!(
        matches.iter().any(|m| m.pattern == "verbal"),
        "the positive polarity matches: {matches:?}"
    );
    assert!(
        matches.iter().any(|m| m.pattern == "other"),
        "the negative polarity matches: {matches:?}"
    );
}

/// The `\u{3000}` escape survives inside a set declaration.
///
/// `double-wo.lx` spells U+3000 as an escape because the character is
/// invisible in a diff and one was silently dropped from a predicate once.
/// Members reuse `value_set`, so the escape decodes at a declaration exactly
/// as it does inline — pinned, because "reuses the same node" is a claim
/// about the grammar and this is a claim about the lowerer.
#[test]
fn an_escape_decodes_inside_a_set_declaration() {
    let named = compile("set SPACE := {\\u3000, x}.\npattern p := [text in @SPACE].\n");
    let inline = compile("pattern p := [text in {\\u3000, x}].\n");
    let tokens = tokenize_japanese("猫\u{3000}犬");
    assert_eq!(named.find_matches(&tokens), inline.find_matches(&tokens));
}

/// A diagnostic about a set names the document the set is in.
///
/// `add_document` rewrites every span's `SourceId` as it accumulates, and the
/// expression walk that does it skipped tokens because a token carried no
/// span. A deferred set operand carries one. Without the remap this points at
/// whichever document sorted first, which is worse than an error: it is a
/// confident wrong answer.
#[test]
fn a_cross_document_set_diagnostic_names_the_right_document() {
    let mut builder = FsmBuilder::new();
    // Three definitions, so `first.lx` is longer than the reference's byte
    // offset in `second.lx`. Without the padding, dropping the remap makes
    // the span run off the end of the first document and panic inside
    // `SourceRegistry` before this test's own assertion is reached — so the
    // mutation would be caught by an unrelated message rather than by the
    // claim under test.
    builder
        .add_document(PatternSource::named(
            "first.lx",
            "pattern a := [text=あ].\npattern b := [text=い].\npattern c := [text=う].\n",
        ))
        .expect("first document");
    builder
        .add_document(PatternSource::named(
            "second.lx",
            "pattern d := [text in @NOWHERE].\n",
        ))
        .expect("second document");
    let Err(error) = builder.build() else {
        panic!("an undefined set is a compile error");
    };
    // Asserted on the location rather than on `to_string()`, which renders
    // messages only: the file a span resolves to is exactly what the remap
    // decides, so it is what has to be read.
    let sources: Vec<&str> = error
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.location().source_name())
        .collect();
    assert_eq!(
        sources,
        vec!["second.lx"],
        "the diagnostic names the document holding the reference, not the one \
         that happens to be first: {error}"
    );
}

/// The one-referent rule holds across documents, and says where both are.
///
/// Asserted on both locations, not on `to_string()`. The message is identical
/// whichever declaration the diagnostic points at, so a rendered-text check
/// passes while the note names the wrong one — which is exactly what it did:
/// the set loop runs after the symbol loop unconditionally, so every collision
/// claimed the *definition* was first, on a catalog where the `set` is.
#[test]
fn a_name_cannot_be_a_set_here_and_a_definition_there() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named("lexicons.lx", "set X := {あ}.\n"))
        .expect("lexicon document");
    builder
        .add_document(PatternSource::named(
            "catalog.lx",
            "fragment X := [text=あ].\npattern p := @X.\n",
        ))
        .expect("catalog document");
    let Err(error) = builder.build() else {
        panic!("a name declared as both a set and a definition is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("duplicate definition") && rendered.contains('X'),
        "the rule is catalog-scoped, so it fires across documents: {rendered}"
    );
    let duplicate = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
        .expect("the collision is reported");
    assert_eq!(
        duplicate.location().source_name(),
        "catalog.lx",
        "the later declaration is the one the author just added: {rendered}"
    );
    let [note] = duplicate.related_locations() else {
        panic!("the collision names both declarations");
    };
    assert_eq!(
        note.location().source_name(),
        "lexicons.lx",
        "and the earlier one is what `first definition is here` points at: {rendered}"
    );
}

/// The same rule with the kinds swapped, so the ordering cannot be hard-coded.
///
/// The pair discriminates: reporting the set as primary unconditionally passes
/// the test above and fails this one, and reporting the definition as primary
/// unconditionally does the reverse. Either alone would have been satisfied by
/// the ordering defect this pair was written to catch.
#[test]
fn the_earlier_declaration_carries_the_note_whichever_kind_it_is() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "catalog.lx",
            "fragment X := [text=あ].\npattern p := @X.\n",
        ))
        .expect("catalog document");
    builder
        .add_document(PatternSource::named("lexicons.lx", "set X := {あ}.\n"))
        .expect("lexicon document");
    let Err(error) = builder.build() else {
        panic!("a name declared as both a definition and a set is a compile error");
    };
    let duplicate = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
        .expect("the collision is reported");
    assert_eq!(
        duplicate.location().source_name(),
        "lexicons.lx",
        "the set is later this time, so it is primary: {error}"
    );
    let [note] = duplicate.related_locations() else {
        panic!("the collision names both declarations");
    };
    assert_eq!(
        note.location().source_name(),
        "catalog.lx",
        "and the definition carries the note: {error}"
    );
}

/// The set-vs-definition collision within one document, not just across two.
///
/// The cross-document pair above only ever exercises `duplicate_name`'s
/// `(SourceId, start)` ordering through `SourceId`: two documents always
/// differ there, so the `start` tiebreak never runs. Both declarations in one
/// document are the only way to reach it for this branch (`bind.rs:139`,
/// set-vs-definition) — the sibling set-vs-set branch (`bind.rs:151`) already
/// has `a_set_declared_twice_is_refused_and_names_the_first` for that.
#[test]
fn a_set_and_a_fragment_share_a_document_and_the_later_one_is_primary() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set X := {あ}.\nfragment X := [text=あ].\npattern p := @X.\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a name declared as both a set and a definition is a compile error");
    };
    let duplicate = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
        .expect("the collision is reported");
    assert_eq!(
        duplicate.location().line(),
        2,
        "the fragment is later, so it is primary: {error}"
    );
    let [note] = duplicate.related_locations() else {
        panic!("the collision names both declarations");
    };
    assert_eq!(
        note.location().line(),
        1,
        "and the set carries the note: {error}"
    );
}

/// The same pair, declarations swapped, within one document.
///
/// Discriminates the same way the cross-document pair does: reporting
/// whichever kind unconditionally as primary passes one of these two tests
/// and fails the other.
#[test]
fn the_same_pair_with_the_declarations_swapped_shares_a_document() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "fragment X := [text=あ].\npattern p := @X.\nset X := {あ}.\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a name declared as both a definition and a set is a compile error");
    };
    let duplicate = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
        .expect("the collision is reported");
    assert_eq!(
        duplicate.location().line(),
        3,
        "the set is later this time, so it is primary: {error}"
    );
    let [note] = duplicate.related_locations() else {
        panic!("the collision names both declarations");
    };
    assert_eq!(
        note.location().line(),
        1,
        "and the fragment carries the note: {error}"
    );
}

/// Two sets with one name are reported, not silently collapsed.
///
/// `IndexMap::insert` on a repeated key keeps the last declaration and says
/// nothing, so without this the second `{い}` would quietly win over the first
/// `{あ}` and every `@X` would resolve to members its author did not write.
/// The note points at the first, which pins the ordering on this path too.
#[test]
fn a_set_declared_twice_is_refused_and_names_the_first() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set X := {あ}.\nset X := {い}.\npattern p := [text in @X].\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a name declared twice as a set is a compile error");
    };
    let duplicate = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::DuplicateDefinition)
        .expect("the collision is reported");
    assert_eq!(
        duplicate.location().line(),
        2,
        "the second declaration is primary: {error}"
    );
    let [note] = duplicate.related_locations() else {
        panic!("the collision names both declarations");
    };
    assert_eq!(
        note.location().line(),
        1,
        "and the first carries the note: {error}"
    );
}

/// An annotation run before a `set` is refused rather than drifting.
///
/// A `set` publishes nothing and inherits nothing, so a run above one binds to
/// no definition. Ignoring it would let it slide onto the definition that
/// follows, which is silent and wrong; the second half pins that a run *after*
/// the set still reaches the definition it was written for.
#[test]
fn an_annotation_run_before_a_set_is_refused_and_does_not_drift() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "layer: \"S1\"\nset S := {あ}.\npattern p := [text in @S].\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("an annotation run above a `set` is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("annotates no definition"),
        "reuses the dangling-run message: {rendered}"
    );
    assert_eq!(
        error.diagnostics[0].location().line(),
        1,
        "and points at the key that carried the value: {rendered}"
    );

    let matcher = compile("set S := {あ}.\nlayer: \"S1\"\npattern p := [text in @S].\n");
    assert_eq!(
        matcher.patterns().len(),
        1,
        "a run after the set still binds to the definition that follows it"
    );
}

/// A repeated member is refused at a declaration as it is inline.
#[test]
fn a_duplicate_set_member_is_refused() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set S := {あ, あ}.\npattern p := [text in @S].\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a repeated member is a compile error");
    };
    assert!(
        error.to_string().contains("set members must be unique"),
        "names the defect: {error}"
    );
}

/// A regex member is refused without naming an operator the declaration lacks.
///
/// `lower_literal_value`'s `negated` argument only selects between recommending
/// `~` and `!~`. A declaration carries no operator, so it passes `false` — and
/// this pins that the message does not therefore invent one.
#[test]
fn a_regex_set_member_is_refused_without_naming_an_operator() {
    let Err(error) = compile_catalog(
        PatternSource::named("sets.lx", "set S := {/あ/}.\npattern p := [text in @S].\n"),
        CompileLimits::default(),
    ) else {
        panic!("a regex member is a compile error");
    };
    let rendered = error.to_string();
    assert!(rendered.contains("regex"), "names the defect: {rendered}");
    assert!(
        !rendered.contains("!~"),
        "and does not recommend the complement of an operator the declaration \
         never wrote: {rendered}"
    );
}

/// An undefined set names the kind, not just a bare "not found".
///
/// Pins the diagnostic's exact distinction between an undefined set and a bare
/// missing name.
#[test]
fn an_undefined_set_reference_names_the_kind() {
    let Err(error) = compile_catalog(
        PatternSource::named("sets.lx", "pattern p := [text in @MISSING].\n"),
        CompileLimits::default(),
    ) else {
        panic!("an undefined set is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("undefined set") && rendered.contains("MISSING"),
        "names the kind and the name: {rendered}"
    );
}

/// A fragment after `in` says so, rather than looking like a typo.
///
/// The sigil is shared, so this is the predictable mistake: a reader who
/// knows `@ARU` is a fragment will reach for it here.
#[test]
fn a_fragment_after_in_is_told_apart_from_a_typo() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "fragment ARU := [text=ある].\npattern p := [text in @ARU].\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a fragment after `in` is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("is a fragment") && rendered.contains("only a set can follow"),
        "names what was found and what was wanted: {rendered}"
    );
}

/// And a pattern says "pattern", because it is not a fragment.
///
/// The two halves fail to different edits: hard-coding either visibility
/// passes one of these tests and fails the other, which is the point of
/// having both.
#[test]
fn a_pattern_after_in_names_its_own_visibility() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "pattern ARU := [text=ある].\npattern p := [text in @ARU].\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a pattern after `in` is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("is a pattern") && rendered.contains("only a set can follow"),
        "names the visibility that is there, not a guess: {rendered}"
    );
}

/// A set in pattern position is the mirror mistake and gets the mirror
/// message.
#[test]
fn a_set_in_pattern_position_is_told_apart_from_a_typo() {
    let Err(error) = compile_catalog(
        PatternSource::named("sets.lx", "set S := {あ}.\npattern p := @S [text=x].\n"),
        CompileLimits::default(),
    ) else {
        panic!("a set in pattern position is a compile error");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("is a set") && rendered.contains("not an expression"),
        "names what was found and what was wanted: {rendered}"
    );
}

/// A sigil with no name after it is refused by the recovery check.
///
/// This is what makes `lower_attribute`'s "set reference is missing its name"
/// fallback unreachable from authored syntax rather than merely unlikely:
/// `set_reference` is `seq('@', $.identifier)`, so tree-sitter supplies a
/// MISSING `identifier` rather than a childless node, and
/// `check_recovery_node` — which runs on the reference first and scans every
/// child, anonymous ones included, for `is_missing()` — refuses it here. The
/// fallback stays as the total-match arm, in the shape `compile_guard`'s
/// unreachable `Group` arm already uses: a sentence rather than a panic.
#[test]
fn a_set_sigil_with_no_name_is_refused_by_recovery() {
    for source in [
        "pattern p := [text in @].\n",
        "pattern p := [text in @ ].\n",
    ] {
        let Err(error) = compile_catalog(
            PatternSource::named("s.lx", source),
            CompileLimits::default(),
        ) else {
            panic!("`@` with no name is a syntax error: {source:?}");
        };
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
        assert!(
            error.to_string().contains("missing `identifier`"),
            "the parser's recovery names what is absent, and the lowerer's own \
             fallback is never reached: {error}"
        );
    }
}

/// A set referenced only by an unused fragment is reported in the same run.
///
/// Sets join the reference graph as leaves, so the reachability fixpoint covers them:
/// the cascade surfaces on this run rather than the next.
/// This is where a leaf-node mistake in the traversal would show.
#[test]
fn an_unused_set_and_the_unused_fragment_holding_it_report_together() {
    let output = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set S := {あ}.\nfragment orphan := [text in @S].\npattern p := [text=x].\n",
        ),
        CompileLimits::default(),
    )
    .expect("compiles");
    let codes: Vec<_> = output
        .diagnostics
        .iter()
        .map(lingrep_core::fsm::Diagnostic::code)
        .collect();
    assert!(
        codes.contains(&DiagnosticCode::UnusedFragment),
        "the orphan fragment is reported: {codes:?}"
    );
    assert!(
        codes.contains(&DiagnosticCode::UnusedSet),
        "and so is the set only it referenced, in the same run: {codes:?}"
    );
}

/// A set a reachable pattern references is not reported.
#[test]
fn a_referenced_set_is_not_reported_unused() {
    let output = compile_catalog(
        PatternSource::named("sets.lx", "set S := {あ}.\npattern p := [text in @S].\n"),
        CompileLimits::default(),
    )
    .expect("compiles");
    assert!(
        !output
            .diagnostics
            .iter()
            .any(|d| d.code() == DiagnosticCode::UnusedSet),
        "a referenced set is used: {:?}",
        output.diagnostics
    );
}

/// A set declared in one document resolves from another.
///
/// This is what "catalog-scoped" means and the only test that can see it: a
/// catalog is not a file, `add_document` accumulates, and binding happens
/// once over everything. Were sets document-scoped instead, `@VERBS` here
/// would be undefined and the language would carry two scoping units for one
/// sigil.
#[test]
fn a_set_declared_in_one_document_resolves_from_another() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "lexicons.lx",
            "set VERBS := {走る, 鳴く}.\n",
        ))
        .expect("lexicon document");
    builder
        .add_document(PatternSource::named(
            "catalog.lx",
            "pattern verbal := [text in @VERBS].\n",
        ))
        .expect("catalog document");
    let output = builder.build().expect("binds across documents");

    let tokens = tokenize_japanese("猫が走る");
    assert!(
        !output.matcher.find_matches(&tokens).is_empty(),
        "the reference resolved against the other document's set"
    );
}

/// The order of accumulation does not decide resolution.
///
/// Binding happens once, over the complete definition set, so a reference may
/// precede the declaration it names — the same rule `add_document` already
/// documents for fragment references.
#[test]
fn a_set_may_be_declared_after_the_document_referencing_it() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "catalog.lx",
            "pattern verbal := [text in @VERBS].\n",
        ))
        .expect("catalog document");
    builder
        .add_document(PatternSource::named(
            "lexicons.lx",
            "set VERBS := {走る, 鳴く}.\n",
        ))
        .expect("lexicon document");
    let output = builder.build().expect("binds regardless of order");

    let tokens = tokenize_japanese("猫が走る");
    assert!(!output.matcher.find_matches(&tokens).is_empty());
}

/// A set resolves inside lookaround exactly as it does in an ordinary token.
///
/// Lookaround is compiled by a separate path — a zero-width guard over its own
/// sub-expression — so "a reference resolves" is a claim that has to be made
/// about that path too, not inherited from the token-position tests above. It
/// also reaches the projection rule: an attribute used *only* inside
/// lookaround is included in cache projection automatically, and a set is how
/// such an attribute is most easily written.
#[test]
fn a_set_resolves_inside_lookaround() {
    let named = compile(
        "set IU := {言う, いう}.\npattern p := [text=と] followed_by([unidic.orthbase in @IU]).\n",
    );
    let inline =
        compile("pattern p := [text=と] followed_by([unidic.orthbase in {言う, いう}]).\n");

    let tokens = tokenize_japanese("それは事実だという");
    let matches = named.find_matches(&tokens);
    assert_eq!(
        matches,
        inline.find_matches(&tokens),
        "the guard's sub-expression resolves the set like any other"
    );
    assert!(
        !matches.is_empty(),
        "the fixture must actually match, or this test compares two empties"
    );
}

/// A quantifier repeating a set-bearing predicate resolves once per site.
///
/// A quantifier duplicates the expression it repeats, which is the shape that
/// made set expansion cost M·N before the members were shared. Compiling one
/// is therefore worth pinning: the repeat must see the same resolved list, and
/// the bound must still be the quantifier's rather than the set's size.
#[test]
fn a_set_resolves_under_a_quantifier() {
    let named =
        compile("set CONTENT := {動詞, 助動詞}.\npattern p := [unidic.pos1 in @CONTENT]{1,2}.\n");
    let inline = compile("pattern p := [unidic.pos1 in {動詞, 助動詞}]{1,2}.\n");

    let tokens = tokenize_japanese("それは事実だという");
    let matches = named.find_matches(&tokens);
    assert_eq!(
        matches,
        inline.find_matches(&tokens),
        "a repeated set-bearing predicate matches where its inline form does"
    );
    assert!(
        !matches.is_empty(),
        "the fixture must actually match, or this test compares two empties"
    );
}

/// A set inside a named capture survives into `same_as`.
///
/// The capture is the observable half and `same_as` is the consuming half, and
/// a set sits under both here. Synthetic tokens rather than the tokenizer: the
/// claim is about an exact reduplication over a chosen `kana` projection, and
/// pinning that to a dictionary's segmentation would make the test about the
/// dictionary.
#[test]
fn a_set_resolves_inside_a_capture_consumed_by_same_as() {
    let named = compile(
        "set X := {a, b}.\npattern p := (?<unit> [text in @X]) same_as($unit, by=unidic.kana).\n",
    );
    let inline =
        compile("pattern p := (?<unit> [text in {a, b}]) same_as($unit, by=unidic.kana).\n");

    let tokens = [token("a", "ア"), token("a", "ア"), token("c", "ウ")];
    let matches = named.find_matches(&tokens);
    assert_eq!(
        matches,
        inline.find_matches(&tokens),
        "the captured predicate resolves the set like its inline form"
    );
    let [single] = matches.as_slice() else {
        panic!("exactly the one reduplication, not the trailing `c`: {matches:?}");
    };
    assert_eq!((single.start, single.end), (0, 2));
    assert_eq!(
        single.captures[0].name, "unit",
        "and the capture is still observable"
    );
}

/// A policy operand naming a declared set is told what is actually wrong.
///
/// `@` spans one name space, so `@S` is well-formed whichever kind `S` is, and
/// only binding discovers that a policy operand resolves against definitions
/// alone. The generic unresolved-name message claimed the catalog "does not
/// declare `S`" one line below `set S := …`, which is false and sends the
/// author hunting a typo that is not there. Rejecting the operand stays
/// correct — a set emits no records — so this pins the explanation, not the
/// outcome.
#[test]
fn a_policy_operand_naming_a_set_says_so_rather_than_calling_it_undeclared() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set S := {動詞}.\npattern p := [unidic.pos1 in @S].\npolicy\n  exclude @p with: @S, \
             relation: after, evidence: raw.\nend\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("a set is not a definition, so the operand does not resolve");
    };
    let rendered = error.to_string();
    assert!(
        !rendered.contains("does not declare"),
        "the catalog declares `S` on line 1; saying otherwise is false: {rendered}"
    );
    assert!(
        rendered.contains("declares as a `set`"),
        "the author is told which kind `@S` actually is: {rendered}"
    );
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code() == DiagnosticCode::UndefinedReference)
        .expect("still an unresolved reference, so a consumer filtering that code sees it");
    let [note] = diagnostic.related_locations() else {
        panic!("the message points at the declaration it is talking about: {rendered}");
    };
    assert_eq!(note.location().line(), 1, "which is line 1: {rendered}");
}

/// The set arm does not swallow a genuinely undeclared operand.
///
/// Discriminates against a fix that reports every unresolved policy name as a
/// set: `@NOPE` is declared nowhere, and the original message is the right one.
#[test]
fn an_operand_naming_nothing_at_all_keeps_the_undeclared_message() {
    let Err(error) = compile_catalog(
        PatternSource::named(
            "sets.lx",
            "set S := {動詞}.\npattern p := [unidic.pos1 in @S].\npolicy\n  exclude @p with: @NOPE, \
             relation: after, evidence: raw.\nend\n",
        ),
        CompileLimits::default(),
    ) else {
        panic!("`@NOPE` names nothing");
    };
    let rendered = error.to_string();
    assert!(
        rendered.contains("does not declare") && rendered.contains("NOPE"),
        "an operand naming nothing is still undeclared: {rendered}"
    );
}
