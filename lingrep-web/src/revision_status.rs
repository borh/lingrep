use crate::coordinator::CoordinatorView;
use crate::revision::AttemptPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionStatusKind {
    Neutral,
    Success,
    Superseded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionStatus {
    pub message: String,
    /// i18n key for `message` (absent: message is a free-text failure whose
    /// contract stays English), plus JSON interpolation args as rendered by
    /// the template (`data-i18n-args`).
    pub message_key: Option<&'static str>,
    pub message_args: Option<String>,
    /// Informational notice attached to a completed-but-degraded run
    /// (oversized sentences omitted). Rendered below the message, never as
    /// an error: the run published results.
    pub omission_notice: Option<String>,
    pub omission_args: Option<String>,
    pub busy: bool,
    pub stale: bool,
    pub terminal: bool,
    pub kind: RevisionStatusKind,
}

fn omission_notice(
    omissions: Option<&lingrep_core::execution::OversizedSentenceOmissions>,
) -> Option<String> {
    omission_parts(omissions).map(|parts| parts.0)
}
fn omission_args(
    omissions: Option<&lingrep_core::execution::OversizedSentenceOmissions>,
) -> Option<String> {
    omission_parts(omissions).map(|parts| parts.1)
}
fn omission_parts(
    omissions: Option<&lingrep_core::execution::OversizedSentenceOmissions>,
) -> Option<(String, String)> {
    let omissions = omissions?;
    if omissions.count == 0 {
        return None;
    }
    let size = omissions.first_actual as f64 / (1 << 20) as f64;
    let limit = omissions.limit as f64 / (1 << 20) as f64;
    Some((
        format!(
            "Skipped {} oversized sentence{} (first: sentence {}, ~{size:.1} MiB tokenized > ~{limit:.1} MiB limit).",
            omissions.count,
            if omissions.count == 1 { "" } else { "s" },
            omissions.first_ordinal,
        ),
        format!(
            "{{\"count\": {}, \"ordinal\": {}, \"size\": {size:.1}, \"limit\": {limit:.1}}}",
            omissions.count, omissions.first_ordinal
        ),
    ))
}

pub fn project_revision_status(view: Option<&CoordinatorView>) -> RevisionStatus {
    let Some(view) = view else {
        return RevisionStatus {
            message: "Ready for a pattern.".into(),
            message_key: Some("status.idle"),
            message_args: None,
            omission_notice: None,
            omission_args: None,
            busy: false,
            stale: false,
            terminal: false,
            kind: RevisionStatusKind::Neutral,
        };
    };
    let attempt = view.state.current_attempt();
    let stale = view
        .state
        .displayed_snapshot()
        .zip(attempt)
        .is_some_and(|(shown, current)| shown.revision != current.revision);
    let ((message, message_key, message_args), busy, terminal, kind) =
        match attempt.map(|attempt| attempt.phase) {
            Some(AttemptPhase::WaitingForCorpus) => (
                (
                    "Preparing the corpus cache…".into(),
                    Some("status.preparing"),
                    None,
                ),
                true,
                false,
                RevisionStatusKind::Neutral,
            ),
            Some(AttemptPhase::Queued) => (
                (
                    "Queued behind the current corpus scan…".into(),
                    Some("status.queued"),
                    None,
                ),
                true,
                false,
                RevisionStatusKind::Neutral,
            ),
            Some(AttemptPhase::Running) => (
                (
                    format!(
                        "Scanning the cached corpus: {} sentences, {} tokens…",
                        view.progress.sentences_matched, view.progress.tokens_matched
                    ),
                    Some("status.scanning"),
                    Some(format!(
                        "{{\"sentences\": {}, \"tokens\": {}}}",
                        view.progress.sentences_matched, view.progress.tokens_matched
                    )),
                ),
                true,
                false,
                RevisionStatusKind::Neutral,
            ),
            Some(AttemptPhase::Completed) => (
                ("Results are current.".into(), Some("status.current"), None),
                false,
                true,
                RevisionStatusKind::Success,
            ),
            Some(AttemptPhase::Cancelled) => (
                (
                    "Superseded by a newer edit.".into(),
                    Some("status.superseded"),
                    None,
                ),
                false,
                true,
                RevisionStatusKind::Superseded,
            ),
            Some(AttemptPhase::Failed) => (
                (
                    attempt
                        .and_then(|attempt| attempt.failure.as_ref())
                        .map_or_else(
                            || "This revision failed.".into(),
                            |failure| failure.message.clone(),
                        ),
                    // Failures fall through to the English message text whose
                    // contract remains English (per-locale delegating coverage
                    // lives in the i18n spec's out-of-scope list).
                    None,
                    None,
                ),
                false,
                true,
                RevisionStatusKind::Error,
            ),
            None => (
                ("Ready for a pattern.".into(), Some("status.idle"), None),
                false,
                false,
                RevisionStatusKind::Neutral,
            ),
        };
    let completed = matches!(
        attempt.map(|attempt| attempt.phase),
        Some(AttemptPhase::Completed)
    );
    let omissions = view
        .state
        .displayed_snapshot()
        .and_then(|snapshot| snapshot.oversized_omissions.as_ref());
    RevisionStatus {
        message,
        message_key,
        message_args,
        omission_notice: completed.then(|| omission_notice(omissions)).flatten(),
        omission_args: completed.then(|| omission_args(omissions)).flatten(),
        busy,
        stale,
        terminal,
        kind,
    }
}
