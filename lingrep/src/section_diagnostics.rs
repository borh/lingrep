//! Reports unavailable section labels and conflicts between scope clauses and read-time section policies.

use lingrep_core::corpus::{DEFAULT_EXCLUDED_SECTIONS, SectionPolicy};
use lingrep_core::scope::ScopeClause;
use std::collections::BTreeSet;

use crate::cli::ExecutionCliConfig;
use crate::commands::query_support::write_warning;
use crate::error::CliError;
use crate::protocol::diagnostic::DiagnosticStage;

/// Returns [`CliError::InvalidArgs`] if a section clause targets an input that records no section labels.
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

/// Warns about unrecorded labels and identifies read-time exclusions; returns an error if writing diagnostics fails.
pub(crate) fn warn_unused_labels(
    clause: &ScopeClause,
    observed: &BTreeSet<String>,
    config: &ExecutionCliConfig,
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
    emit(message, suggestions, config, run_id)
}

/// Warns when read-time policy excludes a requested label; returns an error if writing diagnostics fails.
pub(crate) fn warn_read_time_excluded(
    clause: &ScopeClause,
    policy: &SectionPolicy,
    config: &ExecutionCliConfig,
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
        config,
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
    config: &ExecutionCliConfig,
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
        config,
        run_id,
    )
}

fn emit(
    message: String,
    suggestions: Vec<String>,
    config: &ExecutionCliConfig,
    run_id: &str,
) -> Result<(), CliError> {
    write_warning(
        &config.stderr,
        config.diagnostic_format,
        DiagnosticStage::Input,
        "unused_section_labels",
        message,
        Some(run_id),
        suggestions,
    )
    .map_err(CliError::Io)
}
