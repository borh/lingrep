//! What a run's policy program did, rendered for the progress channel.
//!
//! Three commands publish through a catalog's `policy … end` block, and until
//! this module only `matches` said what the block had done. The other two
//! aggregate — `summarize` into per-pattern counts, `sentence-final` into
//! pattern × surface counts — which is exactly the situation in which a
//! researcher cannot see the answer any other way. A count that came out lower
//! than expected has no visible cause in a total.
//!
//! Two reports, and they are answers to different questions:
//!
//! - [`write_per_statement`] is a run fact, printed unrequested under
//!   `--progress always`. It says what each statement did to the whole run.
//! - [`write_trace`] is requested by `--trace-policy N` and printed whatever
//!   `--progress` says, because a flag that answered nothing unless a second
//!   flag agreed would be a flag that looks broken.
//!
//! Both go to the progress channel and never to the record stream. A JSONL or
//! TSV consumer's contract is the record; putting an explanation into it would
//! change a published contract to serve a debugging affordance.
//!
//! # What the trace does and does not cover
//!
//! It explains the **policy program** — nothing after it. `sentence-final`
//! applies its own sentence-final predicate to the records policy published,
//! and `summarize` may drop a record with no key, so a record this trace ends
//! with `kept` can still be absent from the command's output. That is not a
//! contradiction: policy kept it and the command did not want it. Saying so
//! here is cheaper than teaching each renderer to guess at its caller's filter,
//! and more honest than a header that implies the trace is a complete account
//! of a command it knows nothing about.

use std::io::Write;

use lingrep_core::fsm::{CatalogPolicy, OperationCounters, PolicyTrace, TracedOutcome};

use crate::terminal::PresentationProgress;

/// The longest statement label a trace line carries before eliding.
///
/// Elision happens **here**, at display, and never at storage: a stored label is
/// what a diagnostic quotes back to the author, and a shortened one would send
/// them looking for text their file does not contain.
///
/// Wide enough that a realistic statement survives whole — the longest in
/// `patterns/sf-experimental.lx` is 85 characters — because the trailing
/// `evidence:` argument is exactly what a researcher is checking when they ask
/// why a statement selected what it did.
///
/// The tracked `require %s0_conditional … evidence: current.` statement must
/// fit without losing the evidence argument; projection tests pin that bound.
const LABEL_DISPLAY_WIDTH: usize = 88;

/// A label short enough for one trace line.
///
/// Counts characters rather than bytes: a lingrep patterns statement can name a Japanese
/// definition, and slicing one mid-codepoint would panic.
fn elided(text: &str, width: usize) -> String {
    let mut characters = text.chars();
    let head: String = characters.by_ref().take(width).collect();
    if characters.next().is_none() {
        head
    } else {
        format!("{head}…")
    }
}

/// What one statement did to one record, as a trace line reads it.
fn outcome_text(outcome: &TracedOutcome) -> String {
    match *outcome {
        TracedOutcome::NotSelected => "not selected".to_string(),
        TracedOutcome::Kept => "kept".to_string(),
        TracedOutcome::Dropped => "dropped".to_string(),
        TracedOutcome::Narrowed {
            start,
            end,
            captures_dropped,
        } => {
            let span = format!("{start}..{end}");
            if captures_dropped == 0 {
                span
            } else {
                format!(
                    "{span}, {captures_dropped} capture{} dropped",
                    if captures_dropped == 1 { "" } else { "s" }
                )
            }
        }
    }
}

/// One line per declared statement, in authored order, named by the text the
/// researcher wrote.
///
/// Deliberately *not* filtered to statements that fired. An inert statement is
/// the thing a researcher is most often trying to see, and a filter would make
/// "ran and did nothing" indistinguishable from "is not in the catalog" — the
/// distinction `ProgramStats::for_program` sizes from the program to preserve.
///
/// A catalog declaring no block has no statements and so prints nothing, which
/// is what lets a caller invoke this unconditionally on every policy-applying
/// run without putting a line of zeroes in front of a reader who has no policy.
pub(crate) fn write_per_statement(
    progress: &PresentationProgress<impl Write>,
    policy: &CatalogPolicy,
    per_statement: &[OperationCounters],
) {
    if per_statement.is_empty() {
        return;
    }
    let labels = policy.program().labels();
    // `zip` would truncate silently if these ever disagreed, and a surface
    // reporting fewer statements than the catalog declares reads as a complete
    // account of the run. `BoundProgram::new` refuses to build such a program,
    // so this states the consequence rather than defending against it.
    debug_assert_eq!(labels.len(), per_statement.len());
    for (label, counters) in labels.iter().zip(per_statement) {
        progress.write_line(&format!(
            "  {} ({}) → dropped {}, narrowed {}",
            label.text(),
            label.line(),
            counters.dropped,
            counters.narrowed,
        ));
    }
}

/// The per-record explanation `--trace-policy` asked for.
pub(crate) fn write_trace(
    progress: &PresentationProgress<impl Write>,
    policy: &CatalogPolicy,
    trace: &PolicyTrace,
) {
    let labels = policy.program().labels();
    let count = trace.records().len();
    progress.write_line(&format!(
        "policy trace ({count} record{})",
        if count == 1 { "" } else { "s" }
    ));
    // Zero records has two very different causes and the researcher cannot tell
    // them apart from an empty list. A catalog with no statements explains
    // nothing because there is nothing to explain.
    if labels.is_empty() {
        progress
            .write_line("  this catalog declares no policy block, so nothing shaped these records");
    }
    for traced in trace.records() {
        progress.write_line(&format!(
            "  {} {} {}..{}",
            traced.sentence, traced.definition, traced.raw_start, traced.raw_end
        ));
        for (statement, outcome) in &traced.decisions {
            let (text, line) = labels.get(*statement).map_or_else(
                || ("<unknown statement>".to_string(), 0),
                |label| (elided(label.text(), LABEL_DISPLAY_WIDTH), label.line()),
            );
            progress.write_line(&format!("    {text} ({line})  → {}", outcome_text(outcome)));
        }
    }
    if trace.truncated() {
        progress.write_line(&format!("policy trace truncated at {count} records"));
    }
}
