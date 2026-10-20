// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::string_slice)]
//! `group NAME := key <op> value(s).` — derived `%group` membership.
//!
//! A `group` declaration derives membership from a metadata predicate instead
//! of a per-definition `groups:` list. `%name` membership is the union of
//! literal `groups:` entries and derived predicates — except that a literal
//! entry naming a derived group is refused, because derivation and restatement
//! must not coexist: the day the predicate and the list disagree, one of them
//! is silently wrong.
//!
//! Predicate semantics are existential and two-valued over presence: `=`/`in`
//! mean "carries the key, string-valued, value matches"; `!=`/`not in` mean
//! "carries the key, string-valued, value differs". A definition lacking the
//! key, or carrying a non-string value, is a member of NO predicate form —
//! the same presence rule `resolve … partition:` applies to its classes.
//!
//! Group names are their own `%` namespace, shared with nothing `@` resolves:
//! a `group` named like an existing fragment is legal.

use lingrep_core::fsm::{DiagnosticCode, FsmBuilder, PatternSource};

use crate::policy_helpers::{compile_errors, compile_warnings, matcher_for, published_records};

// --- membership semantics -----------------------------------------------------

/// The defining behavior: a derived group is a policy subject. The `in` form
/// selects the definitions whose `layer` is named; the S0 pattern is outside
/// the subject and survives the exclude untouched.
#[test]
fn a_derived_group_subjects_its_members_to_a_policy_statement() {
    const SOURCE: &str = "group g := layer in {\"S1\", \"S2\"}.\n\
                          layer: \"S1\"\n\
                          a := [text=あ].\n\
                          layer: \"S2\"\n\
                          b := [text=い].\n\
                          layer: \"S0\"\n\
                          c := [text=う].\n\
                          x := [text=ん].\n\
                          policy\n\
                            exclude %g with: @x, relation: after, evidence: raw.\n\
                          end\n";
    // `a` at [0,1) with an `x` after it, `b` at [2,3) with an `x` after it,
    // `c` at [4,5) with nothing after it.
    let records = published_records(SOURCE, &["あ", "ん", "い", "ん", "う"]);
    let mut published: Vec<&str> = records
        .iter()
        .map(|record| record.pattern.as_str())
        .collect();
    published.sort_unstable();
    assert_eq!(
        published,
        vec!["c", "x", "x"],
        "`a` and `b` are in `%g` and excluded; `c` is outside the subject — {records:?}"
    );
}

/// Membership is the union of literal lists and derived predicates: one
/// definition may join a curated group by list and a derived group by
/// predicate, and each group sees it.
#[test]
fn literal_and_derived_membership_union_on_one_definition() {
    let matcher = matcher_for(
        "group g := layer = \"S1\".\n\
         layer: \"S1\"\n\
         groups: [\"h\"]\n\
         a := [text=あ].\n",
    );
    let index = matcher.catalog_policy().group_index();
    let derived = index.members_of("g").expect("`g` is declared");
    assert_eq!(derived.len(), 1, "`a` satisfies the predicate: {derived:?}");
    let literal = index.members_of("h").expect("`h` has a member");
    assert_eq!(
        literal.len(),
        1,
        "`a` keeps its literal membership: {literal:?}"
    );
}

/// The two-valued presence rule: a definition lacking the key entirely is a
/// member of NO predicate form, including the negated `!=` — and a non-string
/// value satisfies neither polarity either. Only a string that differs joins.
#[test]
fn a_definition_lacking_the_key_is_not_a_member_of_a_negated_predicate() {
    let matcher = matcher_for(
        "group g := k != \"v\".\n\
         p := [text=あ].\n\
         k: \"w\"\n\
         q := [text=い].\n\
         k: \"v\"\n\
         r := [text=う].\n\
         k: 3\n\
         s := [text=え].\n",
    );
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("g")
        .expect("`g` is declared");
    assert_eq!(
        members.len(),
        1,
        "only `q` carries the key, string-valued, with a differing value: {members:?}"
    );
    assert_eq!(
        members[0].index(),
        1,
        "`p` lacks the key, `r` matches the value, `s` carries a number"
    );
}

/// The `not in` form, whose operator is one node spanning two words: present
/// with a string outside the set joins; absent or named does not.
#[test]
fn not_in_selects_present_values_outside_the_set() {
    let matcher = matcher_for(
        "group g := layer not in {\"S0\"}.\n\
         layer: \"S1\"\n\
         a := [text=あ].\n\
         layer: \"S0\"\n\
         b := [text=い].\n\
         c := [text=う].\n",
    );
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("g")
        .expect("`g` is declared");
    assert_eq!(members.len(), 1, "only `a` qualifies: {members:?}");
    assert_eq!(members[0].index(), 0);
}

/// A predicate derives membership over emitting definitions only: a fragment
/// carrying the key is not selected. A literal list is how a fragment joins a
/// group — a `%group` operand still has to be able to name one.
#[test]
fn a_fragment_is_not_derived_into_a_group() {
    let matcher = matcher_for(
        "group g := layer = \"S1\".\n\
         layer: \"S1\"\n\
         fragment F := [text=ぬ].\n\
         layer: \"S1\"\n\
         a := [text=あ] [text=ぬ].\n",
    );
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("g")
        .expect("`g` is declared");
    assert_eq!(
        members.len(),
        1,
        "only the pattern is derived in: {members:?}"
    );
    assert_eq!(
        members[0].index(),
        1,
        "`F` is the catalog's first definition"
    );
}

/// A declared group whose predicate currently selects nothing still exists:
/// `%g` resolves, and the statement draws the empty-subject warning rather
/// than an unknown-group refusal claiming the catalog does not declare it.
#[test]
fn an_empty_derived_group_binds_and_warns_rather_than_failing_resolution() {
    let warnings = compile_warnings(
        "group g := layer = \"S9\".\n\
         layer: \"S1\"\n\
         a := [text=あ].\n\
         x := [text=ん].\n\
         policy\n\
           exclude %g with: @x, relation: after, evidence: raw.\n\
         end\n",
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code() == DiagnosticCode::IneffectivePolicy),
        "an empty subject is honored and warned, not refused: {warnings:?}"
    );
}

/// A group is scoped to the accumulated catalog, like a set or a test: a
/// predicate declared in one document selects patterns from another, because
/// membership is derived over the accumulation, not the declaring document.
#[test]
fn a_group_declared_in_one_document_selects_patterns_from_another() {
    let mut builder = FsmBuilder::new();
    builder
        .add_document(PatternSource::named(
            "lexicons.lx",
            "group g := layer = \"S1\".\n",
        ))
        .expect("declaring document");
    builder
        .add_document(PatternSource::named(
            "catalog.lx",
            "layer: \"S1\"\npattern p := [text=あ].\n",
        ))
        .expect("pattern document");
    let output = builder.build().expect("the accumulated catalog compiles");
    let members = output
        .matcher
        .catalog_policy()
        .group_index()
        .members_of("g")
        .expect("`g` is declared");
    assert_eq!(
        members.len(),
        1,
        "the other document's pattern satisfies the predicate: {members:?}"
    );
}

/// Group names are their own `%` namespace, shared with nothing `@` resolves:
/// a group named like an existing fragment is legal, where a set or a test
/// with that name would be a one-referent collision.
#[test]
fn a_group_may_share_a_fragments_name() {
    let matcher = matcher_for(
        "fragment F := [text=ぬ].\n\
         a := @F.\n\
         group F := layer = \"S1\".\n",
    );
    assert_eq!(
        matcher
            .catalog_policy()
            .group_index()
            .members_of("F")
            .map(<[_]>::len),
        Some(0),
        "`%F` exists as a declared, currently empty group beside fragment `F`"
    );
}

// --- refusals -----------------------------------------------------------------

/// The one-name-one-predicate rule holds across documents, and the collision
/// diagnostic resolves each declaration to its own file — which is what the
/// span remap into the accumulated registry decides. Two documents each legal
/// on their own can still collide.
#[test]
fn a_cross_document_duplicate_group_names_both_documents() {
    let mut builder = FsmBuilder::new();
    // The first document is longer than the second declaration's byte offset,
    // so an unremapped second span would still resolve inside `first.lx` and
    // confidently name the wrong file rather than erroring.
    builder
        .add_document(PatternSource::named(
            "first.lx",
            "group g := k = \"v\".\npattern a := [text=あ].\npattern b := [text=い].\n",
        ))
        .expect("first document");
    builder
        .add_document(PatternSource::named(
            "second.lx",
            "group g := k = \"w\".\npattern c := [text=う].\n",
        ))
        .expect("second document");
    let Err(error) = builder.build() else {
        panic!("a duplicate group declaration is a compile error");
    };
    let duplicate = error
        .diagnostics
        .iter()
        .find(|held| held.code() == DiagnosticCode::DuplicateGroupDefinition)
        .unwrap_or_else(|| panic!("expected DuplicateGroupDefinition, got {error:?}"));
    assert_eq!(
        duplicate.location().source_name(),
        "second.lx",
        "the diagnostic points at the second declaration's own document"
    );
    let related = duplicate
        .related_locations()
        .first()
        .expect("the first declaration is named");
    assert_eq!(
        related.location().source_name(),
        "first.lx",
        "and the note resolves to the document that declared it first"
    );
}

/// Two `group` declarations with one name are refused at the second.
#[test]
fn a_duplicate_group_declaration_is_refused_at_the_second() {
    const SOURCE: &str = "group g := k = \"v\".\n\
                          group g := k = \"w\".\n\
                          p := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let duplicate = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::DuplicateGroupDefinition)
        .unwrap_or_else(|| panic!("expected DuplicateGroupDefinition, got {errors:?}"));
    assert_eq!(
        &SOURCE[duplicate.location().byte_range()],
        "group g := k = \"w\".",
        "the diagnostic points at the second declaration"
    );
    assert!(
        !duplicate.related_locations().is_empty(),
        "the first declaration is named so the author can find it"
    );
}

/// A literal `groups:` entry naming a derived group is refused at the entry:
/// derivation and restatement must not coexist.
#[test]
fn a_literal_entry_naming_a_derived_group_is_refused_at_the_entry() {
    const SOURCE: &str = "group g := layer = \"S1\".\n\
                          layer: \"S1\"\n\
                          groups: [\"g\"]\n\
                          p := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let literal = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::LiteralEntryForDerivedGroup)
        .unwrap_or_else(|| panic!("expected LiteralEntryForDerivedGroup, got {errors:?}"));
    assert_eq!(
        &SOURCE[literal.location().byte_range()],
        "groups",
        "the diagnostic points at the entry that restates the derivation"
    );
}

/// The refusal a fragment author hits: a fragment joins a group only by
/// literal list, and a derived group refuses literal entries — so together,
/// patterns-only derivation and this refusal mean a derived group can never
/// contain a fragment at all. The diagnostic is the same one, at the entry.
#[test]
fn a_fragments_literal_entry_naming_a_derived_group_is_refused_at_the_entry() {
    const SOURCE: &str = "group g := layer = \"S1\".\n\
                          groups: [\"g\"]\n\
                          fragment F := [text=ぬ].\n\
                          p := [text=あ] @F.\n";
    let errors = compile_errors(SOURCE);
    let literal = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::LiteralEntryForDerivedGroup)
        .unwrap_or_else(|| panic!("expected LiteralEntryForDerivedGroup, got {errors:?}"));
    assert_eq!(
        &SOURCE[literal.location().byte_range()],
        "groups",
        "a fragment's restating entry is refused at the same key a pattern's is"
    );
}

// --- the reserved bare name ---------------------------------------------------

/// `group` joins the keywords a bare definition cannot be named: the collision
/// is lexical, so `group := x.` parses as the declaration keyword and fails.
#[test]
fn group_is_not_a_bare_definition_name() {
    let errors = compile_errors("group := [text=あ].\np := [text=い].\n");
    assert!(
        errors
            .iter()
            .any(|held| held.code() == DiagnosticCode::Syntax),
        "`group := …` is a syntax error: {errors:?}"
    );
}

/// An explicit declaration avoids the collision, exactly as `pattern set`
/// and `pattern schema` do: the keyword sits where the parser expects one.
#[test]
fn an_explicit_pattern_named_group_compiles() {
    let matcher = matcher_for("pattern group := [].\n");
    assert!(
        matcher.patterns().contains_key("group"),
        "`pattern group := …` is a legal declaration"
    );
}
