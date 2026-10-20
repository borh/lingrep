// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::string_slice)]
//! `resolve … partition: <key>` — the partition-by-dimension resolve.
//!
//! The statement's selected set is subdivided by the string value of the named
//! metadata key on each record's definition, and the rank contest runs
//! independently per class. The map is resolved at policy-compile time, so the
//! engine stays metadata-free; a selected definition lacking the key is not
//! selected by the statement at all, which is the language's warn-not-refuse
//! stance rather than an error.

use lingrep_core::fsm::DiagnosticCode;

use crate::policy_helpers::{
    compile_errors, compile_warnings, matcher_for, published_records, tokens_with_orth,
};

// --- compile-time acceptance --------------------------------------------------

/// The accepting form: `partition:` is an argument `resolve` takes.
#[test]
fn resolve_accepts_a_partition_argument() {
    matcher_for(
        "layer: \"S1\"\n\
         a := [text=あ].\n\
         layer: \"S2\"\n\
         b := [text=あ].\n\
         policy\n\
           resolve all partition: layer, rank: [longest, start, end, pattern].\n\
         end\n",
    );
}

/// A `partition:` on any verb other than `resolve` is refused with its own
/// diagnostic, pointing at the `partition` key rather than at the statement.
#[test]
fn partition_on_a_non_resolve_verb_is_refused_at_the_key() {
    const SOURCE: &str = "a := [text=あ].\n\
                          b := [text=い].\n\
                          policy\n\
                            exclude @a with: @b, relation: after, evidence: raw, partition: layer.\n\
                          end\n";
    let errors = compile_errors(SOURCE);
    let partition = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::PartitionOnNonResolve)
        .unwrap_or_else(|| panic!("expected PartitionOnNonResolve, got {errors:?}"));
    assert_eq!(
        &SOURCE[partition.location().byte_range()],
        "partition",
        "the diagnostic points at the partition key"
    );
}

/// A selected definition carrying the key with a non-string value is refused,
/// and the diagnostic points at that definition's own key — the language's
/// metadata-diagnostic rule.
#[test]
fn a_non_string_partition_value_is_refused_at_the_definitions_key() {
    const SOURCE: &str = "layer: 3\n\
                          a := [text=あ].\n\
                          layer: \"S2\"\n\
                          b := [text=い].\n\
                          policy\n\
                            resolve all partition: layer, rank: [longest, start, end, pattern].\n\
                          end\n";
    let errors = compile_errors(SOURCE);
    let partition = errors
        .iter()
        .find(|held| held.code() == DiagnosticCode::PartitionKeyNotString)
        .unwrap_or_else(|| panic!("expected PartitionKeyNotString, got {errors:?}"));
    let range = partition.location().byte_range();
    assert_eq!(
        &SOURCE[range.clone()],
        "layer",
        "the diagnostic names the key whose value is wrong"
    );
    assert_eq!(
        range.start,
        SOURCE.find("layer: 3").expect("the annotation is present"),
        "the key pointed at is the one on the offending definition, not the \
         statement's"
    );
}

/// A key no selected definition carries — a typo'd `partition: layr` — makes
/// every class empty, so the statement is honored and permanently inert. That
/// is a warning naming the key, in the same register as the empty-subject
/// warning: the engine executes the statement exactly as written, over
/// nothing, on every sentence, forever.
#[test]
fn a_partition_key_nothing_carries_warns_rather_than_silently_doing_nothing() {
    let warnings = compile_warnings(
        "layer: \"S1\"\n\
         a := [text=あ].\n\
         layer: \"S2\"\n\
         b := [text=あ].\n\
         policy\n\
           resolve all partition: layr, rank: [longest, start, end, pattern].\n\
         end\n",
    );
    let inert = warnings
        .iter()
        .find(|held| held.code() == DiagnosticCode::IneffectivePolicy)
        .unwrap_or_else(|| panic!("expected an IneffectivePolicy warning, got {warnings:?}"));
    assert!(
        inert.message.contains("layr"),
        "the warning names the key: {}",
        inert.message
    );
}

/// A partition key present on the catalog is not warned about: the classes
/// are non-empty, and the statement adjudicates. Control for the test above.
#[test]
fn a_carried_partition_key_draws_no_warning() {
    let warnings = compile_warnings(
        "layer: \"S1\"\n\
         a := [text=あ].\n\
         layer: \"S2\"\n\
         b := [text=あ].\n\
         policy\n\
           resolve all partition: layer, rank: [longest, start, end, pattern].\n\
         end\n",
    );
    assert!(
        !warnings
            .iter()
            .any(|held| held.code() == DiagnosticCode::IneffectivePolicy),
        "a carried key adjudicates and is not inert: {warnings:?}"
    );
}

// --- runtime semantics --------------------------------------------------------

/// Two records at the same span in different classes never contest: both
/// survive a partitioned resolve. The control is the same catalog without
/// `partition:`, where one drops.
#[test]
fn different_classes_never_contest() {
    const PARTITIONED: &str = "layer: \"S1\"\n\
                               a := [text=あ].\n\
                               layer: \"S2\"\n\
                               b := [text=あ].\n\
                               policy\n\
                                 resolve all partition: layer, rank: [longest, start, end, pattern].\n\
                               end\n";
    const UNPARTITIONED: &str = "layer: \"S1\"\n\
                                 a := [text=あ].\n\
                                 layer: \"S2\"\n\
                                 b := [text=あ].\n\
                                 policy\n\
                                   resolve all rank: [longest, start, end, pattern].\n\
                                 end\n";
    let both = published_records(PARTITIONED, &["あ"]);
    assert_eq!(
        both.len(),
        2,
        "same span, different classes: neither contests the other — {both:?}"
    );
    let one = published_records(UNPARTITIONED, &["あ"]);
    assert_eq!(
        one.len(),
        1,
        "the unpartitioned control resolves the overlap to one record — {one:?}"
    );
}

/// Within one class the contest is exactly the unpartitioned contest over that
/// class: rank decides between overlapping records of the same class.
#[test]
fn rank_decides_within_a_class_as_an_unpartitioned_resolve_would() {
    // `a1` matches あ い (two tokens); `a2` matches い (one token, overlapping).
    const PARTITIONED: &str = "layer: \"S1\"\n\
                               a1 := [text=あ] [text=い].\n\
                               layer: \"S1\"\n\
                               a2 := [text=い].\n\
                               policy\n\
                                 resolve all partition: layer, rank: [longest, start, end, pattern].\n\
                               end\n";
    const UNPARTITIONED: &str = "layer: \"S1\"\n\
                                 a1 := [text=あ] [text=い].\n\
                                 layer: \"S1\"\n\
                                 a2 := [text=い].\n\
                                 policy\n\
                                   resolve all rank: [longest, start, end, pattern].\n\
                                 end\n";
    let partitioned = published_records(PARTITIONED, &["あ", "い"]);
    let unpartitioned = published_records(UNPARTITIONED, &["あ", "い"]);
    assert_eq!(
        partitioned, unpartitioned,
        "one class partitions into itself, so the contest is unchanged"
    );
    assert_eq!(partitioned.len(), 1, "longest wins: {partitioned:?}");
    assert_eq!(partitioned[0].pattern, "a1");
}

/// A `%group` subject partitions within the subject: classes subdivide the
/// group's records, and a record outside the group is untouched even at an
/// overlapping span.
#[test]
fn a_group_subject_partitioned_resolve_contests_per_class_within_the_subject() {
    // In `%g`: `a` (S1, あい), `a2` (S1, い, overlapping `a`), `b` (S2, あ).
    // Outside: `c` (S1, あい) — same span as `a`, but not selected.
    // Expected: within `g` class S1 the longest wins (`a` over `a2`); class S2
    // never contests S1, so `b` survives beside `a`; `c` is not in the
    // subject and publishes untouched.
    const SOURCE: &str = "layer: \"S1\"\n\
                          groups: [\"g\"]\n\
                          a := [text=あ] [text=い].\n\
                          layer: \"S1\"\n\
                          groups: [\"g\"]\n\
                          a2 := [text=い].\n\
                          layer: \"S2\"\n\
                          groups: [\"g\"]\n\
                          b := [text=あ].\n\
                          layer: \"S1\"\n\
                          c := [text=あ] [text=い].\n\
                          policy\n\
                            resolve %g partition: layer, rank: [longest, start, end, pattern].\n\
                          end\n";
    let mut published: Vec<&str> = Vec::new();
    let records = published_records(SOURCE, &["あ", "い"]);
    for record in &records {
        published.push(record.pattern.as_str());
    }
    published.sort_unstable();
    assert_eq!(
        published,
        vec!["a", "b", "c"],
        "`a2` loses its class's contest; `b` and `c` are outside it — {records:?}"
    );
}

/// A non-string partition value on a definition **outside** the subject is not
/// an error: the map is built over the definitions the subject selects, and a
/// value never read is never judged.
#[test]
fn a_non_string_value_outside_the_subject_compiles_clean() {
    matcher_for(
        "layer: \"S1\"\n\
         groups: [\"g\"]\n\
         a := [text=あ].\n\
         layer: 3\n\
         b := [text=い].\n\
         policy\n\
           resolve %g partition: layer, rank: [longest, start, end, pattern].\n\
         end\n",
    );
}

/// The trace agrees: a keyless record reads `not selected`, not `kept`.
///
/// The engine and the trace answer selection separately — a decision of `Keep`
/// is what both "kept" and "not selected" look like — so without this pin the
/// trace could claim a contest the record never entered.
#[test]
fn a_keyless_record_traces_as_not_selected_by_a_partitioned_resolve() {
    use lingrep_core::fsm::policy_trace::{PolicyTrace, TracedOutcome};
    use lingrep_core::fsm::{PolicyStats, ProgramStats, apply_policy_records};

    use crate::policy_helpers::no_anchors;

    const SOURCE: &str = "layer: \"S1\"\n\
                          a := [text=あ].\n\
                          keyless := [text=あ].\n\
                          policy\n\
                            resolve all partition: layer, rank: [longest, start, end, pattern].\n\
                          end\n";
    let matcher = matcher_for(SOURCE);
    let tokens = tokens_with_orth(&["あ"]);
    let mut trace = PolicyTrace::with_capacity(8);
    apply_policy_records(
        matcher.catalog_policy(),
        &matcher.find_matches(&tokens),
        &tokens,
        &no_anchors(),
        &mut PolicyStats::default(),
        &mut ProgramStats::for_program(matcher.catalog_policy().program()),
        Some(&mut trace),
    )
    .expect("no anchor is declared");

    let keyless = trace
        .records()
        .iter()
        .find(|record| record.definition == "keyless")
        .expect("`keyless` is published");
    assert_eq!(
        keyless.decisions,
        vec![(0, TracedOutcome::NotSelected)],
        "no `layer`, no class, no contest"
    );
    let carried = trace
        .records()
        .iter()
        .find(|record| record.definition == "a")
        .expect("`a` is published");
    assert_eq!(
        carried.decisions,
        vec![(0, TracedOutcome::Kept)],
        "`a` is in class S1 and won its one-record contest"
    );
}

/// A selected definition lacking the key is not selected by the statement: its
/// records neither contest nor are contested.
#[test]
fn a_definition_lacking_the_key_is_not_selected() {
    // `a` carries `layer`; `keyless` does not. Same span: an unpartitioned
    // resolve would drop one of them, but `keyless` is outside every class and
    // `a` has its class to itself.
    const SOURCE: &str = "layer: \"S1\"\n\
                          a := [text=あ].\n\
                          keyless := [text=あ].\n\
                          policy\n\
                            resolve all partition: layer, rank: [longest, start, end, pattern].\n\
                          end\n";
    let records = published_records(SOURCE, &["あ"]);
    assert_eq!(
        records.len(),
        2,
        "the keyless record flows past the statement untouched — {records:?}"
    );
}
