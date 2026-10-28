use std::io::{self, Write};

use lingrep_core::fsm::{CatalogPolicy, OperationCounters, PolicyTrace, TracedOutcome};

use crate::error::CliError;
use crate::terminal::RunStderr;

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

/// Reports every statement in authored order, including statements with zero counts.
pub(crate) fn write_totals(
    writer: &mut impl Write,
    policy: &CatalogPolicy,
    captures_dropped: u64,
    per_statement: &[OperationCounters],
) -> io::Result<()> {
    let labels = policy.program().labels();
    debug_assert_eq!(labels.len(), per_statement.len());
    let statements = labels.len();
    let dropped = if captures_dropped == 0 {
        String::new()
    } else {
        format!(
            ", {captures_dropped} capture{} dropped",
            if captures_dropped == 1 { "" } else { "s" }
        )
    };
    writeln!(
        writer,
        "policy totals ({statements} statement{}{dropped})",
        if statements == 1 { "" } else { "s" }
    )?;
    if labels.is_empty() {
        writeln!(writer, "  {NO_POLICY_BLOCK}")?;
    }
    for (label, counters) in labels.iter().zip(per_statement) {
        writeln!(
            writer,
            "  {} ({}) → dropped {}, narrowed {}",
            label.text(),
            label.line(),
            counters.dropped,
            counters.narrowed,
        )?;
    }
    Ok(())
}

/// Writes the per-record explanation for a positive `--trace-policy N`.
pub(crate) fn write_trace(
    writer: &mut impl Write,
    policy: &CatalogPolicy,
    trace: &PolicyTrace,
) -> io::Result<()> {
    let labels = policy.program().labels();
    let count = trace.records().len();
    writeln!(
        writer,
        "policy trace ({count} record{})",
        if count == 1 { "" } else { "s" }
    )?;
    // Zero records has two very different causes and the researcher cannot tell
    // them apart from an empty list. A catalog with no statements explains
    // nothing because there is nothing to explain.
    if labels.is_empty() {
        writeln!(writer, "  {NO_POLICY_BLOCK}")?;
    }
    for traced in trace.records() {
        writeln!(
            writer,
            "  {} {} {}..{}",
            traced.sentence, traced.definition, traced.raw_start, traced.raw_end
        )?;
        for (statement, outcome) in &traced.decisions {
            let (text, line) = labels.get(*statement).map_or_else(
                || ("<unknown statement>".to_string(), 0),
                |label| (elided(label.text(), LABEL_DISPLAY_WIDTH), label.line()),
            );
            writeln!(writer, "    {text} ({line})  → {}", outcome_text(outcome))?;
        }
    }
    if trace.truncated() {
        writeln!(writer, "policy trace truncated at {count} records")?;
    }
    Ok(())
}

const NO_POLICY_BLOCK: &str = "This catalog declares no policy statements.";

/// Writes the `--trace-policy N` report to stderr: totals, then, for a positive
/// N, the trace. Returns an error if the write to stderr fails.
pub(crate) fn write_requested(
    stderr: &RunStderr,
    policy: &CatalogPolicy,
    captures_dropped: u64,
    per_statement: &[OperationCounters],
    trace: Option<&PolicyTrace>,
) -> Result<(), CliError> {
    stderr
        .diagnostic(|writer| {
            write_totals(writer, policy, captures_dropped, per_statement)?;
            if let Some(trace) = trace {
                write_trace(writer, policy, trace)?;
            }
            Ok(())
        })
        .map_err(CliError::Io)
}
