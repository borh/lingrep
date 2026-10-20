//! Why a section question could not be answered, said out loud.
//!
//! An empty active-label set is ambiguous between "this sentence sits in no
//! labelled section" and "this input records no sections at all". The second is
//! not data. Where it is knowable before processing the run refuses; where it is
//! only knowable afterwards — Markdown, which is structurally able to carry
//! labels — the run warns and names what the corpus did record.

use std::collections::BTreeSet;
use std::io::{self, Write};

use lingrep_core::corpus::{DEFAULT_EXCLUDED_SECTIONS, SectionPolicy};
use lingrep_core::scope::ScopeClause;

use crate::cli::DiagnosticFormat;
use crate::commands::query_support::{execution_output_error, execution_output_io};
use crate::error::CliError;
use crate::output::sanitize_terminal;
use crate::protocol::diagnostic::{
    DiagnosticData, DiagnosticEnvelope, DiagnosticEvent, DiagnosticSeverity, DiagnosticStage,
};
use crate::protocol::write_jsonl;

/// Refuses a section clause against an input that records no section labels.
///
/// Answering it with a confident zero and a clean four-bucket accounting would
/// be a false denominator, which is the failure the scope report exists to
/// prevent.
///
/// # Errors
///
/// Returns [`CliError::InvalidArgs`] when the clause names a section and the
/// subject records none.
pub(crate) fn refuse_unanswerable(
    clause: &ScopeClause,
    records_labels: bool,
    subject: &str,
) -> Result<(), CliError> {
    if !clause.names_sections() || records_labels {
        return Ok(());
    }
    Err(CliError::InvalidArgs(format!(
        "--where names `@section`, but {subject} records no section labels, so a section \
         predicate over it could only ever be empty"
    )))
}

/// Warns about every label the clause named that the corpus never recorded.
///
/// A refusal would be wrong: one catalog legitimately runs over several corpora
/// and a label may be recorded by only some of them. Silence would also be
/// wrong — an empty result and a mistyped label look identical.
///
/// When an unrecorded label is one the default policy excludes at *read* time,
/// the message says so and names the flag that changes it. That pairing is the
/// whole reason the flags and the predicates shipped together: without it,
/// `--where '@section = references'` reports an honest empty universe for a
/// reason the researcher cannot see and has no way to alter.
///
/// # Errors
///
/// Returns an error if the diagnostic stream cannot be written.
pub(crate) fn warn_unused_labels(
    clause: &ScopeClause,
    observed: &BTreeSet<String>,
    format: DiagnosticFormat,
    run_id: &str,
) -> Result<(), CliError> {
    let unused: Vec<&str> = clause
        .section_labels()
        .into_iter()
        .filter(|label| !observed.contains(*label))
        .collect();
    if unused.is_empty() {
        return Ok(());
    }
    let excluded_by_default = SectionPolicy::parse_labels(DEFAULT_EXCLUDED_SECTIONS);
    let mut suggestions = Vec::new();
    if unused
        .iter()
        .any(|label| excluded_by_default.contains(*label))
    {
        suggestions.push(
            "some of these are excluded when the corpus is read, by the default section \
             policy; pass --md-exclude-sections \"\" to admit them"
                .to_owned(),
        );
    }
    let message = format!(
        "--where names section {} this corpus does not record; it records {}",
        render(&unused),
        if observed.is_empty() {
            "none".to_owned()
        } else {
            render(&observed.iter().map(String::as_str).collect::<Vec<_>>())
        }
    );
    emit(message, suggestions, format, run_id)
}

/// Warns that the active read-time policy excludes a label the clause names.
///
/// This is the interaction that makes section selection confusing without the
/// flags. `references` is excluded by the default policy *before* a line
/// becomes a sentence, so `--where '@section = references'` reports an entirely
/// honest empty universe — for a reason nothing in the result mentions.
///
/// It is distinct from the unused-label report and cannot be folded into it:
/// the reader still *observes* the label on the heading it excluded, so the
/// corpus does record it. Only the policy knows why nothing survived. Being a
/// property of the clause and the policy alone, it is also checkable before a
/// byte is read.
///
/// # Errors
///
/// Returns an error if the diagnostic stream cannot be written.
pub(crate) fn warn_read_time_excluded(
    clause: &ScopeClause,
    policy: &SectionPolicy,
    format: DiagnosticFormat,
    run_id: &str,
) -> Result<(), CliError> {
    let excluded: Vec<&str> = clause
        .section_labels()
        .into_iter()
        .filter(|label| !policy.admits(&BTreeSet::from([(*label).to_owned()])))
        .collect();
    if excluded.is_empty() {
        return Ok(());
    }
    emit(
        format!(
            "--where names section {}, which this run excludes when the corpus is read, so \
             no sentence can carry it",
            render(&excluded)
        ),
        vec![
            "pass --md-exclude-sections \"\" to admit them, or name them on \
             --md-include-sections"
                .to_owned(),
        ],
        format,
        run_id,
    )
}

fn render(labels: &[&str]) -> String {
    labels
        .iter()
        .map(|label| format!("`{label}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Warns that a named label is a self-label, and so selects nothing.
///
/// # Errors
///
/// Returns an error if the diagnostic stream cannot be written.
pub(crate) fn warn_self_label_no_ops(
    labels: &[String],
    format: DiagnosticFormat,
    run_id: &str,
) -> Result<(), CliError> {
    if labels.is_empty() {
        return Ok(());
    }
    let names = render(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    emit(
        format!(
            "{names} describes its own heading rather than a section, and a heading is never \
             emitted as a sentence, so selecting it matches nothing"
        ),
        vec!["redeclare the set with --md-self-labels to make it a section label".to_owned()],
        format,
        run_id,
    )
}

fn emit(
    message: String,
    suggestions: Vec<String>,
    format: DiagnosticFormat,
    run_id: &str,
) -> Result<(), CliError> {
    let stderr = io::stderr();
    let mut writer = stderr.lock();
    match format {
        DiagnosticFormat::Human => writeln!(
            writer,
            "warning[unused_section_labels]: {}",
            sanitize_terminal(&message)
        )
        .map_err(execution_output_io)
        .map_err(CliError::from),
        DiagnosticFormat::Json => write_jsonl(
            &mut writer,
            &DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
                run_id: Some(run_id.to_owned()),
                observed_input_digest: None,
                stage: DiagnosticStage::Input,
                severity: DiagnosticSeverity::Warning,
                code: "unused_section_labels".into(),
                message,
                suggestions,
                usage: None,
                retry: None,
                location: None,
                related_locations: Vec::new(),
            })),
        )
        .map_err(execution_output_error)
        .map_err(CliError::from),
    }
}
