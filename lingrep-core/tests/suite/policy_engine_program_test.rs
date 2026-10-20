// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic)]
//! Execution of the ordered policy program.
//!
//! The operand restriction and self-exclusion have each been lost once in
//! design drafts, so they are checked directly rather than left to emerge from
//! a verb's behaviour.
//!
//! These fixtures compile real catalogs and run them over real tokens, unlike
//! `policy_engine_test`'s hand-built policies. The program is bound at compile
//! time, so a hand-built `CatalogPolicy` cannot carry one — `install_program`
//! is the compiler's, and every subject and operand here is a name only a
//! compilation can resolve.

use lingrep_core::fsm::policy_program::DefId;
use lingrep_core::fsm::{
    AnchorError, AnchorResolutionError, Match, PolicyStats, ProgramStats, apply_policy_records,
    strip_hidden,
};

use crate::policy_helpers::{
    matcher_for, no_anchors, sym_anchor_matcher, tokens_with_orth, tokens_with_orth_and_pos,
};

/// The published stream for a catalog run over `words`.
fn published(source: &str, words: &[&str]) -> Vec<Match> {
    matcher_for(source)
        .find_published_matches(&tokens_with_orth(words))
        .expect("the fixture declares no anchor, so nothing can fail to resolve")
}

// --- the reserved `sentence_end` anchor ---------------------------------------
//
// See `docs/design/2026-08-06-raw-sentence-end-admission.md`. This anchor
// measures the raw sentence end; every other anchor is derived.

/// The reserved anchor is the token count.
#[test]
fn the_sentence_end_anchor_is_the_raw_token_count() {
    let source = "p := [text=猫].\n\
                  policy\n  admit @p terminal: {exact: true, anchor: sentence_end}.\nend\n";
    assert_eq!(
        published(source, &["猫"]).len(),
        1,
        "a record ending at the sentence end is admitted"
    );
    assert_eq!(
        published(source, &["猫", "犬"]).len(),
        0,
        "a record ending before it is not"
    );
}

/// `within` measures backwards from the same position.
#[test]
fn the_sentence_end_anchor_measures_a_within_distance() {
    let source = "p := [text=猫].\n\
                  policy\n  admit @p terminal: {within: 1, anchor: sentence_end}.\nend\n";
    assert_eq!(published(source, &["猫", "犬"]).len(), 1, "one token short");
    assert_eq!(
        published(source, &["猫", "犬", "鳥"]).len(),
        0,
        "two is past the declared distance"
    );
}

/// It needs no `anchor:` declaration, so catalogs without derived anchors can
/// still name the raw sentence end explicitly.
#[test]
fn the_sentence_end_anchor_needs_no_declaration() {
    let matcher = matcher_for(
        "p := [text=猫].\n\
         policy\n  admit @p terminal: {exact: true, anchor: sentence_end}.\nend\n",
    );
    assert!(
        matcher.catalog_policy().anchors.is_empty(),
        "the fixture must declare no anchor for this to mean anything"
    );
}

/// Resolving it never calls `AnchorPositions::get`, so positions from a
/// structurally different catalog cannot make it fail.
///
/// Asserting "no declarations, and publication succeeded" would prove nothing:
/// a catalog with no declared anchor has nothing for `get` to reject either
/// way. Deliberately mismatched positions are what discriminate — the reserved
/// anchor still succeeds, and a *declared* anchor under the same positions
/// fails with `CatalogMismatch`, which is the control.
#[test]
fn the_reserved_anchor_bypasses_anchor_positions_entirely() {
    let tokens = tokens_with_orth_and_pos(&[("猫", "NOUN"), ("。", "PUNCT")]);
    let foreign = sym_anchor_matcher();

    let reserved = matcher_for(
        "p := [text=猫] [text=。].\n\
         policy\n  admit @p terminal: {exact: true, anchor: sentence_end}.\nend\n",
    );
    let matches = reserved.find_matches(&tokens);
    let mut stats = PolicyStats::default();
    let mut program = ProgramStats::for_program(reserved.catalog_policy().program());
    let admitted = apply_policy_records(
        reserved.catalog_policy(),
        &matches,
        &tokens,
        &foreign.anchor_positions(&tokens),
        &mut stats,
        &mut program,
        None,
    )
    .expect("the reserved anchor consults no positions, so no digest can reject it");
    assert_eq!(admitted.len(), 1);

    // The control: same foreign positions, a *declared* anchor, and the read
    // that the reserved name skipped is now made and refused.
    let declared = sym_anchor_matcher();
    let punct = crate::policy_helpers::punct_anchor_matcher();
    assert_eq!(
        punct
            .anchor_positions(&tokens)
            .get("propositional_end", declared.anchor_digest()),
        Err(AnchorError::CatalogMismatch),
        "a declared anchor under mismatched positions must fail"
    );
}

/// Declaring it is an error, not a shadow.
///
/// Under shadowing, which meaning a statement got would depend on declaration
/// order — correct in the compiler and unlearnable at the desk.
#[test]
fn declaring_the_reserved_anchor_is_refused() {
    let diagnostics = crate::policy_helpers::compile_errors(
        "anchor: {name: \"sentence_end\", exclude: \"X\"}\n\
         max_nesting: {depth: 0, witness: [\"これは重要である|。\"], \
         divergence: [\"これは重要である|（田中）。\"]}\n\
         fragment X := [upos=PUNCT]+.\n\
         p := [text=猫].\n",
    );
    assert!(
        diagnostics.iter().any(|d| {
            d.message.contains("compiler-owned") && d.message.contains("sentence_end")
        }),
        "{diagnostics:?}"
    );
}

/// A declared anchor stays unreferenced when only the reserved name is used.
///
/// The reserved name resolves without a declaration, so it must not count as a
/// *use* of one — otherwise declaring an unused anchor beside an `admit
/// … sentence_end` would silently stop reporting.
#[test]
fn the_reserved_anchor_does_not_keep_a_declared_anchor_referenced() {
    let warnings = crate::policy_helpers::compile_warnings(
        "anchor: {name: \"e\", exclude: \"X\"}\n\
         max_nesting: {depth: 0, witness: [\"これは重要である|。\"], \
         divergence: [\"これは重要である|（田中）。\"]}\n\
         fragment X := [upos=PUNCT]+.\n\
         p := [text=猫].\n\
         policy\n  admit @p terminal: {exact: true, anchor: sentence_end}.\nend\n",
    );
    assert!(
        warnings
            .iter()
            .any(|d| d.code() == lingrep_core::fsm::DiagnosticCode::UnreferencedAnchor),
        "{warnings:?}"
    );
}

#[test]
fn a_non_operand_match_never_triggers_an_effect() {
    let published = published(
        "a := [text=猫].\nb := [text=犬].\nfragment N := [text=鳥].\n\
         policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n",
        &["猫", "犬"],
    );
    assert!(
        published.iter().any(|record| record.pattern == "a"),
        "`b` is not the operand, so it must not exclude `a`: {published:?}"
    );
}

#[test]
fn a_definition_naming_itself_does_not_annihilate_its_own_records() {
    let published = published(
        "a := [text=猫].\n\
         policy\n  exclude @a with: @a, relation: in_sentence, evidence: raw.\nend\n",
        &["猫"],
    );
    assert!(
        published.iter().any(|record| record.pattern == "a"),
        "self-exclusion must remove the subject from its own operand set: {published:?}"
    );
}

#[test]
fn records_outside_the_subject_are_untouched() {
    let published = published(
        "a := [text=猫].\nb := [text=犬].\n\
         policy\n  resolve @a rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬"],
    );
    assert_eq!(
        published
            .iter()
            .filter(|record| record.pattern == "b")
            .count(),
        1,
        "a resolve over `@a` never touches `b`: {published:?}"
    );
}

/// The published names, in publication order.
fn names(published: &[Match]) -> Vec<&str> {
    published
        .iter()
        .map(|record| record.pattern.as_str())
        .collect()
}

#[test]
fn resolve_keeps_the_longest_and_drops_what_overlaps_it() {
    let published = published(
        "long := [text=猫] [text=犬].\nshort := [text=猫].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬"],
    );
    assert_eq!(names(&published), vec!["long"]);
}

#[test]
fn resolve_keeps_the_shortest_when_that_is_what_the_rank_asks_for() {
    // The same fixture under the opposite first criterion. `longest` winning
    // above proves only that the contest ran, not that it read the rank.
    let published = published(
        "long := [text=猫] [text=犬].\nshort := [text=猫].\n\
         policy\n  resolve all rank: [shortest, start, end, pattern].\nend\n",
        &["猫", "犬"],
    );
    assert_eq!(names(&published), vec!["short"]);
}

#[test]
fn resolve_rank_is_a_decision_order_not_a_publication_order() {
    let published = published(
        "first := [text=猫].\nsecond := [text=犬] [text=鳥].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert_eq!(
        names(&published),
        vec!["first", "second"],
        "they do not overlap, so both survive — in canonical order, and \
         `longest` would have put `second` first"
    );
}

#[test]
fn priority_order_decides_a_contest_the_earlier_criteria_tie() {
    // Same span, so `longest`, `start` and `end` all tie; `priority_order` is
    // the only criterion that can separate them, and lower wins.
    let published = published(
        "priority_order: 5\nloser := [text=猫].\n\
         priority_order: 1\nwinner := [text=猫].\n\
         policy\n  resolve all rank: [priority_order, start, end, pattern].\nend\n",
        &["猫"],
    );
    assert_eq!(names(&published), vec!["winner"]);
}

/// A–B–C, where A overlaps B and B overlaps C but A does not overlap C.
///
/// The chain has to be *walked* in that order for the case to bite. Ranked
/// `[shortest, start, …]` the walk is `a`, `b`, `c`: `b` loses to `a` and `c`
/// is then judged against a set `b` is absent from. Judge `c` before `b`
/// loses — the fixture this test first used — and adjudicating against every
/// contestant rather than against the winners gives the same answer, so the
/// case proves nothing.
#[test]
fn a_record_beaten_by_the_winner_stops_shielding_the_one_behind_it() {
    let published = published(
        "a := [text=猫].\nb := [text=猫] [text=犬].\nc := [text=犬] [text=鳥].\n\
         policy\n  resolve all rank: [shortest, start, end, pattern].\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert_eq!(
        names(&published),
        vec!["a", "c"],
        "`b` lost to `a`, so it shields nothing; `c` overlaps only `b`"
    );
}

#[test]
fn a_winner_covering_the_whole_chain_beats_every_link_of_it() {
    let published = published(
        "a := [text=猫].\nmiddle := [text=猫] [text=犬] [text=鳥].\nc := [text=鳥].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert_eq!(names(&published), vec!["middle"]);
}

/// A `%group` subject reaches its members and stops there.
///
/// Stated from the outside in: the overlapping pair is *not* in the group, so
/// what is pinned is that the subject stops at the group boundary rather than
/// what it does to members that contest.
///
/// `a_resolve_over_a_group_now_contests_its_own_members` covers contests within
/// the group; this test pins that non-members remain outside the subject.
#[test]
fn a_resolve_over_a_group_leaves_every_non_member_alone() {
    let published = published(
        "groups: [\"g\"]\nmember := [text=鳥].\n\
         wide := [text=猫] [text=犬].\n\
         narrow := [text=猫].\n\
         policy\n  resolve %g rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert_eq!(
        names(&published),
        vec!["narrow", "wide", "member"],
        "`wide` and `narrow` overlap, and a subject that reached them would \
         have dropped one"
    );
}

/// A `resolve %g` contests overlapping members according to its own rank.
#[test]
fn a_resolve_over_a_group_now_contests_its_own_members() {
    let published = published(
        "groups: [\"g\"]\nwide := [text=猫] [text=犬].\n\
         groups: [\"g\"]\nnarrow := [text=猫].\n\
         policy\n  resolve %g rank: [longest, start, end, pattern].\nend\n",
        &["猫", "犬"],
    );
    assert_eq!(
        names(&published),
        vec!["wide"],
        "the longest member wins and the overlapping one is dropped"
    );
}

/// Whether `a` survives one association over the fragment `N`.
///
/// The four relation tests below differ only in the geometry their definitions
/// produce, so the statement is built rather than written out six times — a
/// table of hand-written catalogs invites one of them to disagree with its own
/// label.
fn survives(effect: &str, relation: &str, definitions: &str, words: &[&str]) -> bool {
    let source = format!(
        "{definitions}policy\n  \
         {effect} @a with: @N, relation: {relation}, evidence: raw.\nend\n"
    );
    published(&source, words)
        .iter()
        .any(|record| record.pattern == "a")
}

/// Every relation, asserted against the geometry it names.
///
/// One fixture per arrangement, each checked against all six relations, so a
/// relation that quietly answered like its neighbour is caught by the five
/// cases it is *not* supposed to hold for rather than the one it is.
fn check_relations(definitions: &str, words: &[&str], holding: &[&str]) {
    for relation in RELATIONS {
        let holds = holding.contains(&relation);
        assert_eq!(
            survives("exclude", relation, definitions, words),
            !holds,
            "exclude under `{relation}` over {words:?}"
        );
        assert_eq!(
            survives("require", relation, definitions, words),
            holds,
            "require under `{relation}` over {words:?}: it is the complement \
             of exclude, over the same operand set"
        );
    }
}

/// Every relation the language admits, so a fixture cannot silently skip one.
const RELATIONS: [&str; 6] = [
    "at_start",
    "at_end",
    "after",
    "before",
    "within",
    "in_sentence",
];

#[test]
fn an_operand_starting_where_the_subject_ends_is_after_it() {
    check_relations(
        // `a` covers (0,1) and `N` covers (1,2).
        "a := [text=猫].\nfragment N := [text=犬].\n",
        &["猫", "犬"],
        &["after", "in_sentence"],
    );
}

#[test]
fn an_operand_ending_where_the_subject_starts_is_before_it() {
    check_relations(
        // `a` covers (1,2) and `N` covers (0,1).
        "a := [text=猫].\nfragment N := [text=犬].\n",
        &["犬", "猫"],
        &["before", "in_sentence"],
    );
}

#[test]
fn an_operand_sharing_the_subjects_first_token_is_at_start_and_within() {
    check_relations(
        // `a` covers (0,2) and `N` covers (0,1).
        "a := [text=猫] [text=犬].\nfragment N := [text=猫].\n",
        &["猫", "犬"],
        &["at_start", "within", "in_sentence"],
    );
}

#[test]
fn an_operand_sharing_the_subjects_last_token_is_at_end_and_within() {
    check_relations(
        // `a` covers (0,2) and `N` covers (1,2).
        "a := [text=猫] [text=犬].\nfragment N := [text=犬].\n",
        &["猫", "犬"],
        &["at_end", "within", "in_sentence"],
    );
}

/// A gap between the two, which only `in_sentence` tolerates.
///
/// Adjacency alone cannot tell `after` from "starts at or past the end": every
/// other fixture here puts the operand flush against the subject, where the two
/// readings agree. `before` has the same blind spot in mirror image.
#[test]
fn an_operand_separated_from_the_subject_stands_in_no_positional_relation() {
    check_relations(
        // `a` covers (0,1) and `N` covers (2,3), with a token between them.
        "a := [text=猫].\nfragment N := [text=鳥].\n",
        &["猫", "犬", "鳥"],
        &["in_sentence"],
    );
    check_relations(
        // The mirror: `a` covers (2,3) and `N` covers (0,1).
        "a := [text=鳥].\nfragment N := [text=猫].\n",
        &["猫", "犬", "鳥"],
        &["in_sentence"],
    );
}

/// An operand with no record at all: `exclude` holds for nothing, `require`
/// for nobody.
#[test]
fn an_operand_that_matched_nothing_excludes_nothing_and_requires_everything_away() {
    let definitions = "a := [text=猫].\nfragment N := [text=鳥].\n";
    for relation in RELATIONS {
        assert!(
            survives("exclude", relation, definitions, &["猫", "犬"]),
            "no `N` record, so no `exclude` can hold: `{relation}`"
        );
        assert!(
            !survives("require", relation, definitions, &["猫", "犬"]),
            "no `N` record, so no `require` can be satisfied: `{relation}`"
        );
    }
}

/// §8: `current` sees an earlier operation's removals; `raw` never does.
#[test]
fn current_evidence_sees_an_earlier_operations_removals_and_raw_does_not() {
    let with_current = published(
        "a := [text=猫].\nb := [text=犬].\nfragment N := [text=鳥].\n\
         policy\n  \
         exclude @b with: @N, relation: in_sentence, evidence: raw.\n  \
         require @a with: @b, relation: after, evidence: current.\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert!(
        !with_current.iter().any(|record| record.pattern == "a"),
        "`b` was removed before the require, so under `current` it is gone: \
         {with_current:?}"
    );

    let with_raw = published(
        "a := [text=猫].\nb := [text=犬].\nfragment N := [text=鳥].\n\
         policy\n  \
         exclude @b with: @N, relation: in_sentence, evidence: raw.\n  \
         require @a with: @b, relation: after, evidence: raw.\nend\n",
        &["猫", "犬", "鳥"],
    );
    assert!(
        with_raw.iter().any(|record| record.pattern == "a"),
        "`raw` is the immutable canonical set, so the removal is invisible to \
         it: {with_raw:?}"
    );
}

/// §8: a filtering operation never invents a record.
#[test]
fn a_filtering_operation_never_invents_a_record() {
    let matcher = matcher_for(
        "a := [text=猫].\nb := [text=犬].\nfragment N := [text=鳥].\n\
         policy\n  exclude @a with: @N, relation: in_sentence, evidence: raw.\nend\n",
    );
    let tokens = tokens_with_orth(&["猫", "犬", "鳥"]);
    let raw = matcher.find_matches(&tokens).len();
    let published = matcher.find_published_matches(&tokens).expect("runs").len();
    assert!(published <= raw, "{published} published from {raw} raw");
}

/// The `a` record's published span.
fn span_of_a(source: &str, words: &[&str]) -> (usize, usize) {
    let published = published(source, words);
    let record = published
        .iter()
        .find(|record| record.pattern == "a")
        .unwrap_or_else(|| panic!("`a` must survive: {published:?}"));
    (record.start, record.end)
}

#[test]
fn detach_narrows_the_published_span_to_exclude_the_operand() {
    assert_eq!(
        span_of_a(
            "a := [text=猫] [text=犬].\nfragment TAIL := [text=犬].\n\
             policy\n  detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
            &["猫", "犬"],
        ),
        (0, 1)
    );
}

#[test]
fn detach_at_start_narrows_the_other_edge() {
    assert_eq!(
        span_of_a(
            "a := [text=猫] [text=犬].\nfragment HEAD := [text=猫].\n\
             policy\n  detach @a with: @HEAD, relation: at_start, evidence: raw.\nend\n",
            &["猫", "犬"],
        ),
        (1, 2)
    );
}

/// **The** strict-bound test.
///
/// `TAIL` covers the entire record, so it is not in `K`, the detach does not
/// fire, and the record publishes whole. With `>=` the span narrows to empty,
/// and the language defines no empty projection.
#[test]
fn a_whole_span_operand_does_not_detach() {
    assert_eq!(
        span_of_a(
            "a := [text=猫].\nfragment TAIL := [text=猫].\n\
             policy\n  detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
            &["猫"],
        ),
        (0, 1),
        "a region that is the entire record is not a trailing region"
    );
}

#[test]
fn a_whole_span_operand_does_not_detach_at_start_either() {
    assert_eq!(
        span_of_a(
            "a := [text=猫].\nfragment HEAD := [text=猫].\n\
             policy\n  detach @a with: @HEAD, relation: at_start, evidence: raw.\nend\n",
            &["猫"],
        ),
        (0, 1),
        "the strict bound is on both edges, not only the trailing one"
    );
}

#[test]
fn detach_preserves_record_count_and_raw_identity() {
    let matcher = matcher_for(
        "a := [text=猫] [text=犬].\nfragment TAIL := [text=犬].\n\
         policy\n  detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    let tokens = tokens_with_orth(&["猫", "犬"]);
    let count = |records: &[Match]| records.iter().filter(|r| r.pattern == "a").count();
    assert_eq!(
        count(&matcher.find_matches(&tokens)),
        count(&matcher.find_published_matches(&tokens).expect("runs")),
        "detach shrinks a record; it never splits one in two"
    );
}

/// §8: a public span only ever narrows.
#[test]
fn a_public_span_only_ever_narrows() {
    assert_eq!(
        span_of_a(
            "a := [text=猫] [text=犬] [text=鳥].\n\
             fragment SHORT := [text=鳥].\nfragment LONG := [text=犬] [text=鳥].\n\
             policy\n  \
             detach @a with: @LONG, relation: at_end, evidence: raw.\n  \
             detach @a with: @SHORT, relation: at_end, evidence: raw.\nend\n",
            &["猫", "犬", "鳥"],
        ),
        (0, 1),
        "the second, shorter detach must not re-expand to (0, 2)"
    );
}

/// One statement, several qualifying regions: the **widest** is detached.
///
/// Every other detach fixture gives a statement exactly one region, where
/// taking the first, the narrowest and the widest all agree. `TAIL` here
/// matches both (1,3) and (2,3), so they do not.
#[test]
fn a_detach_removes_the_widest_region_its_operand_offers() {
    assert_eq!(
        span_of_a(
            "a := [text=猫] [text=犬] [text=鳥].\n\
             fragment TAIL := ([text=犬] [text=鳥]) | [text=鳥].\n\
             policy\n  detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
            &["猫", "犬", "鳥"],
        ),
        (0, 1),
        "the region reaching furthest back is the one that belongs to another \
         record"
    );
}

/// The `at_start` mirror, which needs a **group** operand to state at all.
///
/// The matcher emits one match per start position, so a single fragment can
/// never offer two leading regions: they would share the subject's start and
/// differ only in length. Two fragments in one group can, and that is the only
/// arrangement under which `at_start`'s choice of region is observable.
#[test]
fn a_detach_at_start_removes_the_widest_region_too() {
    assert_eq!(
        span_of_a(
            "a := [text=猫] [text=犬] [text=鳥].\n\
             groups: [\"heads\"]\nfragment SHORT := [text=猫].\n\
             groups: [\"heads\"]\nfragment LONG := [text=猫] [text=犬].\n\
             policy\n  detach @a with: %heads, relation: at_start, evidence: raw.\nend\n",
            &["猫", "犬", "鳥"],
        ),
        (2, 3),
        "`LONG` reaches furthest, so it is the leading region that belongs to \
         another record"
    );
}

/// A capture touching the detached region is dropped, never clamped.
#[test]
fn a_capture_reaching_into_the_detached_region_is_dropped_whole() {
    let matcher = matcher_for(
        "a := (?<head> [text=猫]) (?<tail> [text=犬] [text=鳥]).\n\
         fragment TAIL := [text=鳥].\n\
         policy\n  detach @a with: @TAIL, relation: at_end, evidence: raw.\nend\n",
    );
    let tokens = tokens_with_orth(&["猫", "犬", "鳥"]);
    let mut stats = PolicyStats::default();
    let records = apply_policy_records(
        matcher.catalog_policy(),
        &matcher.find_matches(&tokens),
        &tokens,
        &no_anchors(),
        &mut stats,
        &mut ProgramStats::for_program(matcher.catalog_policy().program()),
        None,
    )
    .expect("the fixture declares no anchor");
    let record = records
        .iter()
        .find(|record| record.name == "a")
        .expect("`a` survives");

    assert_eq!((record.start, record.end), (0, 2));
    assert_eq!(
        record
            .components
            .iter()
            .map(|capture| (capture.name.as_str(), capture.start, capture.end))
            .collect::<Vec<_>>(),
        vec![("head", 0, 1)],
        "`tail` covers (1, 3) and the detached region is (2, 3), so it goes \
         whole rather than being clamped to (1, 2)"
    );
}

/// A record naming a definition this catalog never declared.
///
/// `apply_policy_records` is public and nothing in its signature pairs a policy
/// with the matcher that produced the records, so a caller can hand it a
/// foreign one. Every other pass answers an unknown name by finding no entry
/// and keeping the record; the program must not be the one pass that refuses to
/// run instead.
#[test]
fn a_record_this_catalog_does_not_declare_survives_the_program() {
    let matcher = matcher_for(
        "a := [text=猫].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
    );
    // Overlapping, so a program that did take them as candidates would drop
    // one and the assertion below could not pass by accident.
    let foreign = vec![
        Match {
            pattern: "elsewhere".into(),
            start: 0,
            end: 2,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        },
        Match {
            pattern: "elsewhere".into(),
            start: 1,
            end: 2,
            captures: Box::default(),
            graph_bindings: Box::default(),
            lexical: None,
        },
    ];
    let mut stats = PolicyStats::default();
    let records = apply_policy_records(
        matcher.catalog_policy(),
        &foreign,
        &tokens_with_orth(&["猫", "犬"]),
        &no_anchors(),
        &mut stats,
        &mut ProgramStats::for_program(matcher.catalog_policy().program()),
        None,
    )
    .expect("the fixture declares no anchor");

    assert_eq!(
        records
            .iter()
            .map(|record| (record.name.as_str(), record.start, record.end))
            .collect::<Vec<_>>(),
        vec![("elsewhere", 0, 2), ("elsewhere", 1, 2)],
        "both are carried through untouched, in canonical order"
    );
}

/// The catalog's complete definition list, and which of it can be a subject.
#[test]
fn the_subject_domain_holds_the_emitting_definitions_alone() {
    let matcher = matcher_for(
        "a := [text=猫].\nfragment N := [text=犬].\n\
         policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n",
    );
    let domain = matcher.catalog_policy().definitions();
    let id = |name: &str| domain.id(name).map(DefId::index);

    assert_eq!(
        id("a"),
        Some(0),
        "authored order, and synthesis only appends"
    );
    assert_eq!(id("N"), Some(1));
    assert_eq!(
        id("__policy__N"),
        Some(2),
        "the emitter is a definition too"
    );

    assert_eq!(
        domain
            .eligible()
            .iter()
            .copied()
            .map(DefId::index)
            .collect::<Vec<_>>(),
        vec![0],
        "a fragment never emits, and an evidence emitter is stripped before \
         publication, so neither can carry a subject record"
    );
}

#[test]
fn a_renamed_definition_is_still_selected_by_its_authored_name() {
    // The seam runs before relabeling. If it ran after, `@a` would select
    // nothing, because the record's name would already be "published".
    let published = published(
        "output_name: \"published\"\na := [text=猫].\nfragment N := [text=犬].\n\
         policy\n  exclude @a with: @N, relation: after, evidence: raw.\nend\n",
        &["猫", "犬"],
    );
    assert!(
        !published.iter().any(|record| record.pattern == "published"),
        "the exclude must have applied despite the rename: {published:?}"
    );
}

#[test]
fn an_evidence_emitter_is_matched_to_the_fragment_it_observes() {
    // `definition_of` must map `__policy__N` back to `N`'s `DefId`, or a
    // fragment operand never matches its own evidence and every association
    // over a fragment silently holds for nothing.
    let published = published(
        "a := [text=猫].\nfragment N := [text=犬].\n\
         policy\n  require @a with: @N, relation: after, evidence: raw.\nend\n",
        &["猫", "犬"],
    );
    assert!(
        published.iter().any(|record| record.pattern == "a"),
        "the require held, so the evidence emitter resolved to `N`: {published:?}"
    );
}

/// A catalog whose anchor sits before trailing punctuation.
///
/// `{PROGRAM}` is replaced by the statements under test, so two orderings of
/// the same two statements differ in nothing else. The anchor's `exclude` and
/// its `max_nesting` claim are [`policy_helpers::PUNCT_ANCHOR`]'s verbatim: the
/// claim is categorical, and one that holds there holds here.
///
/// `a` reaches past the anchor by exactly the punctuation `TRAILING_APPARATUS`
/// recognizes, which is what gives `detach` a region to remove and `admit`
/// something to refuse.
const ADMIT_CATALOG: &str = r#"
anchor: {name: "propositional_end", exclude: "TRAILING_APPARATUS"}
max_nesting: {depth: 0, witness: ["これは重要である|。"], divergence: ["これは重要である|（田中）。"]}
fragment TRAILING_APPARATUS := [upos=PUNCT]+.

a := [text=猫] [text=犬] [upos=PUNCT].

policy
{PROGRAM}
end
"#;

/// [`ADMIT_CATALOG`]'s three tokens: two content words and the punctuation.
fn admit_tokens() -> Vec<lingrep_core::token_match::TokenInfo> {
    tokens_with_orth_and_pos(&[("猫", "NOUN"), ("犬", "NOUN"), ("。", "PUNCT")])
}

/// [`ADMIT_CATALOG`] with `program` installed, published over [`admit_tokens`].
fn admitted(program: &str) -> Vec<Match> {
    matcher_for(&ADMIT_CATALOG.replace("{PROGRAM}", program))
        .find_published_matches(&admit_tokens())
        .expect("the catalog's own positions answer its own anchor")
}

const DETACH_STATEMENT: &str =
    "  detach @a with: @TRAILING_APPARATUS, relation: at_end, evidence: raw.";
const ADMIT_STATEMENT: &str = "  admit @a terminal: {exact: true, anchor: propositional_end}.";

/// `detach` then `admit` must differ from `admit` then `detach`.
///
/// `admit` is the one operator reading the **published** end. Reading
/// the raw end instead would make the two orderings agree, and the ordering
/// guarantee that authored order is semantic would not be real.
#[test]
fn admit_observes_the_published_end_so_position_matters() {
    let spans = |program: &str| {
        admitted(program)
            .iter()
            .map(|record| (record.pattern.to_string(), record.start, record.end))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        spans(&format!("{DETACH_STATEMENT}\n{ADMIT_STATEMENT}")),
        vec![("a".to_owned(), 0, 2)],
        "the detach put the published end at the anchor, so the admit keeps it"
    );
    assert_eq!(
        spans(&format!("{ADMIT_STATEMENT}\n{DETACH_STATEMENT}")),
        Vec::new(),
        "judged first, `a` still ends past the anchor and is refused; the \
         detach that would have saved it never gets a record to reshape"
    );
}

/// Naming an anchor the positions cannot answer fails the run.
///
/// The positions here are a structurally different catalog's, which is the one
/// way a compiled `admit` can go unanswered: its own anchor name is resolved at
/// compile time. Nothing may be decided against an unresolved anchor, and no
/// statistic may move before the resolution that would have refused the run.
#[test]
fn an_admit_forces_anchor_resolution_before_any_statistic_moves() {
    let matcher = matcher_for(&ADMIT_CATALOG.replace("{PROGRAM}", ADMIT_STATEMENT));
    let tokens = admit_tokens();
    let mut stats = PolicyStats::default();
    let mut program = ProgramStats::for_program(matcher.catalog_policy().program());

    let outcome = apply_policy_records(
        matcher.catalog_policy(),
        &matcher.find_matches(&tokens),
        &tokens,
        &sym_anchor_matcher().anchor_positions(&tokens),
        &mut stats,
        &mut program,
        None,
    );

    assert_eq!(
        outcome.map(|records| records.len()),
        Err(AnchorResolutionError {
            anchor: "propositional_end".into(),
            cause: AnchorError::CatalogMismatch,
        }),
        "an `admit` names the anchor, so the resolution loop must run for it"
    );
    assert_eq!(stats, PolicyStats::default(), "no record was judged");
    assert_eq!(
        program,
        ProgramStats::for_program(matcher.catalog_policy().program()),
        "and no operation ran"
    );
}

/// Counters are per operation, not per (sentence, operation).
///
/// One entry per sentence would grow without bound on a corpus run. What a
/// researcher asking "which statement did the work" wants is the aggregate, and
/// the aggregate is what the slot holds.
#[test]
fn per_operation_statistics_are_bounded_by_operation_count() {
    let matcher = matcher_for(
        "long := [text=猫] [text=犬].\nshort := [text=猫].\n\
         policy\n  resolve all rank: [longest, start, end, pattern].\nend\n",
    );
    let mut stats = PolicyStats::default();
    let mut program = ProgramStats::for_program(matcher.catalog_policy().program());

    for _ in 0..3 {
        let tokens = tokens_with_orth(&["猫", "犬"]);
        apply_policy_records(
            matcher.catalog_policy(),
            &matcher.find_matches(&tokens),
            &tokens,
            &no_anchors(),
            &mut stats,
            &mut program,
            None,
        )
        .expect("the fixture declares no anchor");
    }

    assert_eq!(
        program.per_operation().len(),
        1,
        "one authored operation, three sentences"
    );
    let counters = program.per_operation()[0];
    assert_eq!(
        (counters.before, counters.after, counters.dropped),
        (6, 3, 3),
        "two candidates in, one out, summed over three sentences"
    );
}

/// §8: the published set does not depend on the order the raw stream arrives in.
///
/// The reversed slice deliberately violates `apply_policy_records`'s documented
/// canonical input order. That is the point: the seam's *callers* always supply
/// canonical order, so nothing here would notice a contest that walked the
/// candidate list instead of the rank.
///
/// The three spans are `(0, 3)`, `(1, 2)` and `(2, 5)`, which is the smallest
/// arrangement that is **not** symmetric under reversal. Walked forwards, the
/// wide `p` is kept and shields both others; walked backwards, `r` is kept, `q`
/// clears it, and `p` then loses to `r`. A chain of three mutually adjacent
/// spans gives the same answer either way and would prove nothing.
#[test]
fn the_published_set_does_not_depend_on_the_raw_streams_order() {
    let matcher = matcher_for(
        "p := [text=猫] [text=犬] [text=鳥].\nq := [text=犬].\n\
         r := [text=鳥] [text=魚] [text=虫].\n\
         policy\n  resolve all rank: [shortest, start, end, pattern].\nend\n",
    );
    let tokens = tokens_with_orth(&["猫", "犬", "鳥", "魚", "虫"]);
    let canonical = matcher.find_matches(&tokens);
    let reversed: Vec<Match> = canonical.iter().rev().cloned().collect();

    let published = |raw: &[Match]| {
        let mut stats = PolicyStats::default();
        let mut spans: Vec<(String, usize, usize)> = apply_policy_records(
            matcher.catalog_policy(),
            raw,
            &tokens,
            &no_anchors(),
            &mut stats,
            &mut ProgramStats::for_program(matcher.catalog_policy().program()),
            None,
        )
        .expect("the fixture declares no anchor")
        .iter()
        .map(|record| (record.name.to_string(), record.start, record.end))
        .collect();
        // Multisets: publication order *is* input order, so the two runs
        // are expected to disagree about it and agree about everything else.
        spans.sort_unstable();
        spans
    };

    assert_eq!(
        published(&canonical),
        published(&reversed),
        "the shortest is judged first whichever end of the slice it entered from"
    );
}

/// §8: renaming every occurrence of a group changes nothing it publishes.
///
/// A **behaviour** property, not a structural one. Sorted-name numbering makes
/// a `GroupId` deterministic, not rename-stable: with two groups declared,
/// renaming `ev` to `zz` moves it past `mm` and swaps both ids. What this pins
/// is that nothing derives meaning from that order.
#[test]
fn renaming_a_group_throughout_publishes_the_same_records() {
    let source = |lead: &str| {
        format!(
            "groups: [\"{lead}\"]\nfragment LEAD := [text=猫].\n\
             groups: [\"mm\"]\nfragment TAIL := [text=鳥].\n\
             a := [text=猫] [text=犬] [text=鳥].\n\
             policy\n  \
             detach @a with: %{lead}, relation: at_start, evidence: raw.\n  \
             detach @a with: %mm, relation: at_end, evidence: raw.\nend\n"
        )
    };
    let spans = |lead: &str| {
        published(&source(lead), &["猫", "犬", "鳥"])
            .iter()
            .map(|record| (record.pattern.to_string(), record.start, record.end))
            .collect::<Vec<_>>()
    };

    assert_eq!(spans("ev"), vec![("a".to_owned(), 1, 2)]);
    assert_eq!(
        spans("zz"),
        spans("ev"),
        "`zz` sorts after `mm`, `ev` before"
    );
}

/// A `policy` block changes what is published and never what is matched.
///
/// Stated over the hidden-stripped stream, because that is what "the raw match
/// stream" means once a fragment operand exists: synthesizing an evidence
/// emitter adds records to the canonical stream by construction, and stripping
/// them is what `--raw-matches` already does. The second assertion is the other
/// half — a synthesized emitter is not a pattern of this catalog.
#[test]
fn a_policy_block_changes_nothing_about_matching() {
    let definitions = "a := [text=猫].\nfragment N := [text=犬].\n";
    let bare = matcher_for(definitions);
    let programmed = matcher_for(&format!(
        "{definitions}policy\n  \
         exclude @a with: @N, relation: after, evidence: raw.\nend\n"
    ));
    let tokens = tokens_with_orth(&["猫", "犬"]);

    assert_eq!(
        strip_hidden(
            programmed.catalog_policy(),
            &programmed.find_matches(&tokens)
        ),
        bare.find_matches(&tokens),
        "the program suppresses `a` at publication, and nothing before it"
    );
    assert!(
        !programmed
            .patterns()
            .keys()
            .any(|name| name.starts_with("__policy__")),
        "an evidence emitter is not a pattern this catalog declares: {:?}",
        programmed.patterns().keys().collect::<Vec<_>>()
    );
}
