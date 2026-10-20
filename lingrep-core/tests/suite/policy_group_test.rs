// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Group membership extraction (`%name`), which must reach fragments.
//!
//! Membership is `groups:`. It is classified as publication policy — that is
//! how it reaches `PatternPolicy` in one parse — and publication policy on a
//! fragment is discarded with `IneffectivePolicy`, correctly, for keys that
//! describe publication. Grouping is not one of those: an operand group has to
//! be able to contain a fragment, because a fragment is exactly what an
//! association looks for through its evidence emitter.
//!
//! The extraction is one parse. `groups:` is read into `PatternPolicy` and
//! lifted out before the fragment discard, rather than re-read from metadata by
//! a second reader that could disagree with the first.

use lingrep_core::fsm::DiagnosticCode;

use crate::policy_helpers::{compile_warnings, matcher_for};

#[test]
fn a_fragment_may_declare_group_membership() {
    let matcher = matcher_for("a := [text=あ].\ngroups: [\"ev\"]\nfragment N := [text=ぬ].\n");
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("ev")
        .expect("`ev` is declared");
    assert_eq!(
        members.len(),
        1,
        "a fragment must be able to join a group: {members:?}"
    );
}

#[test]
fn grouping_alone_on_a_fragment_does_not_warn() {
    let warnings =
        compile_warnings("a := [text=あ].\ngroups: [\"ev\"]\nfragment N := [text=ぬ].\n");
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.code() == DiagnosticCode::IneffectivePolicy),
        "grouping is not a publication decision: {warnings:?}"
    );
}

#[test]
fn a_publication_key_on_a_fragment_still_warns() {
    let warnings =
        compile_warnings("a := [text=あ].\noutput_name: \"x\"\nfragment N := [text=ぬ].\n");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code() == DiagnosticCode::IneffectivePolicy),
        "publication policy on a fragment is still ineffective: {warnings:?}"
    );
}

#[test]
fn a_publication_key_beside_a_grouping_key_still_warns() {
    // The regression a naive "skip the fragment branch when a group is
    // present" fix would introduce: one key legitimising the other.
    let warnings = compile_warnings(
        "a := [text=あ].\ngroups: [\"ev\"]\noutput_name: \"x\"\nfragment N := [text=ぬ].\n",
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.code() == DiagnosticCode::IneffectivePolicy),
        "grouping must not legitimise a co-located publication key: {warnings:?}"
    );
}

#[test]
fn a_group_member_id_indexes_the_complete_definition_list() {
    // An ungrouped definition ahead of a grouped one must not shift it.
    let matcher = matcher_for("ungrouped := [text=あ].\ngroups: [\"ev\"]\ngrouped := [text=い].\n");
    let members = matcher
        .catalog_policy()
        .group_index()
        .members_of("ev")
        .expect("declared");
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0].index(),
        1,
        "the grouped definition is the second in the catalog"
    );
}

/// **Parity guard, not a cost guard.** Moving the index construction back
/// before synthesis does *not* fail this test, and that was checked rather
/// than assumed.
///
/// The reason is the same one that killed an earlier draft's bind-order test:
/// synthesis only ever **appends**, and appending never renumbers an authored
/// `DefId`. So an index built from the authored list alone still holds correct
/// member ids; it is merely shorter than the definition table.
///
/// Post-synthesis construction is therefore a **domain-alignment** guarantee,
/// not a currently-observable correctness fix — it is what keeps the index and
/// the definition table describing the same set, and it is what would still be
/// right if synthesis ever inserted rather than appended. What this test does
/// pin is that a synthesized emitter joins no group and that `N` keeps its id,
/// both of which a grouping regression would break.
#[test]
fn a_synthesized_emitter_joins_no_group_and_shifts_none() {
    let matcher = matcher_for(
        "groups: [\"ev\"]\nfragment N := [text=ぬ].\n\
         a := [text=あ] [text=ぬ].\n\
         policy\n  detach @a with: @N, relation: at_end, evidence: raw.\nend\n",
    );
    let policy = matcher.catalog_policy();
    assert!(
        policy.hidden.contains_key("__policy__N"),
        "the fixture must actually synthesize: {:?}",
        policy.hidden
    );
    let members = policy.group_index().members_of("ev").expect("declared");
    assert_eq!(members.len(), 1, "the emitter joins no group: {members:?}");
    assert_eq!(members[0].index(), 0, "`N` is the first definition");
}
