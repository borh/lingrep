// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::string_slice)]

use lingrep_core::fsm::DiagnosticCode;
use serde_json::json;

use crate::policy_helpers::{compile_errors, compile_warnings, matcher_for};

/// The defining behavior: `source: none` on one definition deletes the file
/// default there, while a sibling without the annotation inherits it.
#[test]
fn an_unset_key_is_absent_while_a_sibling_inherits_the_default() {
    let matcher = matcher_for(
        "defaults\n\
        \x20 source: \"x\"\n\
         end\n\
         \n\
         source: none\n\
         p := [text=あ].\n\
         q := [text=い].\n",
    );
    let patterns = matcher.patterns();
    let p = patterns.get("p").expect("`p` compiles");
    assert!(
        !p.metadata.contains_key("source"),
        "`source: none` unsets the default, so `p` lacks the key: {:?}",
        p.metadata
    );
    let q = patterns.get("q").expect("`q` compiles");
    assert_eq!(
        q.metadata.get("source"),
        Some(&json!("x")),
        "`q` carries no annotation, so the default materializes untouched"
    );
}

#[test]
fn unset_without_a_file_default_is_refused_at_the_key() {
    const SOURCE: &str = "source: none\np := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let unset = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::UnsetWithoutDefault)
        .unwrap_or_else(|| panic!("expected UnsetWithoutDefault, got {errors:?}"));
    assert_eq!(
        &SOURCE[unset.location().byte_range()],
        "source",
        "the diagnostic points at the key whose default is missing"
    );
}

#[test]
fn unset_inside_the_defaults_block_is_refused_at_the_key() {
    const SOURCE: &str = "defaults\n  source: none\nend\n\np := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let unset = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::UnsetOutsideAnnotation)
        .unwrap_or_else(|| panic!("expected UnsetOutsideAnnotation, got {errors:?}"));
    assert_eq!(
        &SOURCE[unset.location().byte_range()],
        "source",
        "the diagnostic points at the defaults entry's key"
    );
}

#[test]
fn unset_inside_a_list_is_refused_at_the_inner_value() {
    const SOURCE: &str = "k: [none]\np := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let unset = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::UnsetOutsideAnnotation)
        .unwrap_or_else(|| panic!("expected UnsetOutsideAnnotation, got {errors:?}"));
    assert_eq!(
        &SOURCE[unset.location().byte_range()],
        "none",
        "the diagnostic points at the nested value"
    );
}

#[test]
fn unset_inside_a_map_is_refused_at_the_inner_value() {
    const SOURCE: &str = "k: {a: none}\np := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let unset = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::UnsetOutsideAnnotation)
        .unwrap_or_else(|| panic!("expected UnsetOutsideAnnotation, got {errors:?}"));
    assert_eq!(
        &SOURCE[unset.location().byte_range()],
        "none",
        "the diagnostic points at the nested value"
    );
}

#[test]
fn unset_of_a_key_the_defaults_block_lacks_is_refused_at_the_key() {
    const SOURCE: &str = "defaults\n  status: \"x\"\nend\n\nsource: none\np := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let unset = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::UnsetWithoutDefault)
        .unwrap_or_else(|| panic!("expected UnsetWithoutDefault, got {errors:?}"));
    assert_eq!(
        &SOURCE[unset.location().byte_range()],
        "source",
        "the diagnostic points at the key whose default is missing"
    );
}

/// Unset composes with `required`: a definition that unsets a required key is
/// exactly a definition that does not carry it, so the existing
/// missing-required diagnostic fires and names the definition. Nothing new is
/// needed; this pins the composition.
#[test]
fn unsetting_a_required_key_is_the_existing_missing_required_refusal() {
    const SOURCE: &str = "defaults\n\
                          \x20 source: \"x\"\n\
                          end\n\
                          \n\
                          schema\n\
                          \x20 pattern source: string required\n\
                          end\n\
                          \n\
                          source: none\n\
                          p := [text=あ].\n";
    let errors = compile_errors(SOURCE);
    let missing = errors
        .iter()
        .find(|held| held.message.contains("requires `source`"))
        .unwrap_or_else(|| panic!("expected the missing-required refusal, got {errors:?}"));
    assert!(
        missing.message.contains("`p` does not carry it"),
        "the refusal names the definition that unset the key: {}",
        missing.message
    );
}

// --- composition with partition and derived groups ----------------------------

/// Unset composes with derived groups: `none` deletes the key at the one
/// place metadata is materialized, so a predicate over that key never sees
/// the file default — the unset definition is a member of no form while the
/// sibling inherits the default and joins.
#[test]
fn an_unset_key_excludes_a_definition_from_a_derived_group() {
    let matcher = matcher_for(
        "defaults\n\
        \x20 layer: \"S1\"\n\
         end\n\
         \n\
         group g := layer = \"S1\".\n\
         layer: none\n\
         p := [text=あ].\n\
         q := [text=い].\n",
    );
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("g")
        .expect("`g` is declared");
    assert_eq!(
        members.len(),
        1,
        "only `q` inherits the default and satisfies the predicate: {members:?}"
    );
}

/// Unset composes with `partition:` the same way: the unset definition lacks
/// the key, so a partitioned resolve does not select it. With every selected
/// definition unset, every class is empty and the statement draws the inert
/// warning instead of adjudicating.
#[test]
fn an_unset_key_leaves_a_partitioned_resolve_inert() {
    let warnings = compile_warnings(
        "defaults\n\
        \x20 layer: \"S1\"\n\
         end\n\
         \n\
         layer: none\n\
         p := [text=あ].\n\
         policy\n\
        \x20 resolve all partition: layer, rank: [longest, start, end, pattern].\n\
         end\n",
    );
    let inert = warnings
        .iter()
        .find(|held| held.code() == DiagnosticCode::IneffectivePolicy)
        .unwrap_or_else(|| panic!("expected an IneffectivePolicy warning, got {warnings:?}"));
    assert!(
        inert.message.contains("layer"),
        "the warning names the key nothing carries: {}",
        inert.message
    );
}

/// Control for the test above: with the annotation removed, the default
/// materializes, the class is non-empty, and the partitioned resolve
/// adjudicates without a warning — pinning that a file default does reach
/// `partition:` through the same materialization `none` interrupts.
#[test]
fn an_inherited_default_reaches_a_partitioned_resolve() {
    let warnings = compile_warnings(
        "defaults\n\
        \x20 layer: \"S1\"\n\
         end\n\
         \n\
         p := [text=あ].\n\
         policy\n\
        \x20 resolve all partition: layer, rank: [longest, start, end, pattern].\n\
         end\n",
    );
    assert!(
        !warnings
            .iter()
            .any(|held| held.code() == DiagnosticCode::IneffectivePolicy),
        "the materialized default classes the record and the statement \
         adjudicates: {warnings:?}"
    );
}
