//! The bounded per-record explanation trace (`policy-sublanguage.md` §7).
//!
//! Every test here compiles a real catalog and runs its program, for the same
//! reason `policy_engine_program_test` does: a subject or operand is a name only
//! a compilation can resolve, so a hand-built policy cannot carry a program.

use lingrep_core::fsm::policy_trace::{PolicyTrace, TracedOutcome};
use lingrep_core::fsm::{PolicyStats, ProgramStats, apply_policy_records};

use super::policy_helpers::{matcher_for, no_anchors, tokens_with_orth};

/// Two patterns and one statement that selects only the first.
const ONE_SELECTIVE_STATEMENT: &str = "fragment TAIL := [text=鳥].\n\
                                       pattern a := [text=猫] [text=犬] [text=鳥].\n\
                                       pattern b := [text=犬] [text=鳥].\n\
                                       policy\n  \
                                       detach @a with: @TAIL, relation: at_end, evidence: raw.\n\
                                       end\n";

/// Runs a catalog over one sentence, collecting a trace of the given capacity.
fn trace_of(catalog: &str, words: &[&str], capacity: usize) -> PolicyTrace {
    let matcher = matcher_for(catalog);
    let tokens = tokens_with_orth(words);
    let mut trace = PolicyTrace::with_capacity(capacity);
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
    trace
}

/// Tracing does not change what is published.
///
/// The property that makes the flag safe to hand a researcher, and the reason
/// it is the first test in this file. Asserted on complete `PublishedRecord`
/// values rather than on counts or spans, because a trace that perturbed a
/// derived field would be invisible to a span comparison.
#[test]
fn tracing_publishes_identical_records() {
    let matcher = matcher_for(ONE_SELECTIVE_STATEMENT);
    let tokens = tokens_with_orth(&["猫", "犬", "鳥"]);
    let matches = matcher.find_matches(&tokens);

    let published = |trace: Option<&mut PolicyTrace>| {
        apply_policy_records(
            matcher.catalog_policy(),
            &matches,
            &tokens,
            &no_anchors(),
            &mut PolicyStats::default(),
            &mut ProgramStats::for_program(matcher.catalog_policy().program()),
            trace,
        )
        .expect("no anchor is declared")
    };

    let untraced = published(None);
    let mut trace = PolicyTrace::with_capacity(64);
    let traced = published(Some(&mut trace));

    assert_eq!(untraced, traced);
    assert!(
        !trace.records().is_empty(),
        "the trace must have observed something, or this proves nothing"
    );
}

/// A trace names every statement that looked at a record, including the ones
/// that passed it over.
///
/// "No statement selected it" is the answer to "why is this record unchanged",
/// and it is not deducible from the statements that did act. A trace listing
/// only effects would answer the easy half of the question.
#[test]
fn a_trace_records_statements_that_did_not_select() {
    let trace = trace_of(ONE_SELECTIVE_STATEMENT, &["猫", "犬", "鳥"], 64);

    let b = trace
        .records()
        .iter()
        .find(|record| record.definition == "b")
        .expect("`b` matches 犬鳥 and is published");
    assert_eq!(
        b.decisions,
        vec![(0, TracedOutcome::NotSelected)],
        "the `detach` names @a, so it passed `b` over"
    );
}

/// A selected record reports the reshape, with what the reshape cost.
#[test]
fn a_narrowed_record_reports_its_new_span_and_dropped_captures() {
    let trace = trace_of(ONE_SELECTIVE_STATEMENT, &["猫", "犬", "鳥"], 64);

    let a = trace
        .records()
        .iter()
        .find(|record| record.definition == "a")
        .expect("`a` matches the whole sentence");
    assert_eq!(
        a.decisions,
        vec![(
            0,
            TracedOutcome::Narrowed {
                start: 0,
                end: 2,
                captures_dropped: 0,
            }
        )],
        "the trailing 鳥 is detached"
    );
    assert_eq!(
        (a.raw_start, a.raw_end),
        (0, 3),
        "raw geometry is untouched"
    );
}

/// A dropped record's trace ends at the statement that dropped it.
///
/// It left the candidate set, so no later statement saw it. The truncation is
/// the answer to "why is this absent", not a gap.
#[test]
fn a_dropped_record_traces_up_to_its_drop() {
    let trace = trace_of(
        "fragment TAIL := [text=鳥].\n\
         pattern a := [text=猫] [text=犬].\n\
         policy\n  \
         require @a with: @TAIL, relation: at_start, evidence: raw.\n  \
         resolve all rank: [longest, start, end, pattern].\n\
         end\n",
        &["猫", "犬", "鳥"],
        64,
    );

    let a = trace
        .records()
        .iter()
        .find(|record| record.definition == "a")
        .expect("`a` matches 猫犬");
    assert_eq!(
        a.decisions,
        vec![(0, TracedOutcome::Dropped)],
        "no 鳥 starts where `a` starts, so the `require` drops it and the \
         `resolve` never sees it"
    );
}

/// The trace stops at its capacity and says it stopped.
///
/// Bounded is the requirement, and a silent truncation reads as "that was
/// everything" — the failure this workspace's no-silent-caps rule names.
#[test]
fn a_trace_stops_at_capacity_and_reports_the_truncation() {
    let trace = trace_of(ONE_SELECTIVE_STATEMENT, &["猫", "犬", "鳥"], 1);

    assert_eq!(trace.records().len(), 1);
    assert!(
        trace.truncated(),
        "the cap was reached and must be reported"
    );
}

/// A trace under its capacity does not claim to have been truncated.
#[test]
fn an_unfilled_trace_is_not_truncated() {
    let trace = trace_of(ONE_SELECTIVE_STATEMENT, &["猫", "犬", "鳥"], 64);

    assert!(!trace.records().is_empty());
    assert!(!trace.truncated());
}

/// A capacity-zero trace collects nothing and is not an error.
///
/// What a caller threading an optional trace through a path that may not want
/// one can pass without a branch.
#[test]
fn a_zero_capacity_trace_collects_nothing() {
    let trace = trace_of(ONE_SELECTIVE_STATEMENT, &["猫", "犬", "鳥"], 0);

    assert!(trace.records().is_empty());
    assert!(trace.truncated());
}
