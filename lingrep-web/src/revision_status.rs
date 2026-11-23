use crate::coordinator::CoordinatorView;
use crate::revision::{AttemptPhase, CancelReason, ErrorCategory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionStatusKind {
    Neutral,
    Success,
    Superseded,
    Abandoned,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusNotice {
    pub key: &'static str,
    pub text: String,
    pub args: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionStatus {
    pub message: String,
    /// Translation key for `message`; `message_args` supplies JSON interpolation values.
    pub message_key: Option<&'static str>,
    pub message_args: Option<String>,
    /// A compiler or binder diagnostic, shown after `message` in English in
    /// every locale.
    pub diagnostic: Option<String>,
    pub notices: Vec<StatusNotice>,
    pub busy: bool,
    pub stale: bool,
    /// Revision of stale results available for export; absent when no pattern matched.
    pub stale_export_revision: Option<u64>,
    pub terminal: bool,
    pub kind: RevisionStatusKind,
}

pub(crate) fn omission_notice(
    omissions: Option<&lingrep_core::execution::OversizedSentenceOmissions>,
) -> Option<String> {
    omission_parts(omissions).map(|parts| parts.0)
}
fn unavailable_notice(unavailable: &crate::snapshot::UnavailableAttributes) -> StatusNotice {
    let attributes = unavailable
        .keys
        .iter()
        .map(|key| format!("`{key}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = &unavailable.schema;
    StatusNotice {
        key: "status.attribute_unavailable",
        text: format!(
            "This corpus's token schema ({schema}) does not carry {attributes}. Comparisons using \
             these attributes, including !=, !~ and not in, match no tokens; their !(…) \
             complements match every token."
        ),
        args: serde_json::json!({ "attributes": attributes, "schema": schema }).to_string(),
    }
}
fn vectors_notice(binding: &lingrep_corpus::word_vectors::VectorsBinding) -> Option<StatusNotice> {
    use lingrep_corpus::word_vectors::VectorsBinding;
    match binding {
        VectorsBinding::Declared => None,
        VectorsBinding::Undeclared { declaration } => Some(StatusNotice {
            key: "status.vectors_undeclared",
            text: format!(
                "No word-vector file declared in the catalog. To declare the server’s file, \
                 add: {declaration}"
            ),
            args: serde_json::json!({ "declaration": declaration }).to_string(),
        }),
        VectorsBinding::Mismatch { declared } => {
            let declared = declared
                .iter()
                .map(|(resource, _)| format!("`{resource}`"))
                .collect::<Vec<_>>()
                .join(", ");
            Some(StatusNotice {
                key: "status.vectors_mismatch",
                text: format!(
                    "The server’s word-vector file differs from {declared} declared in the catalog. \
                     Matches may differ at the same threshold."
                ),
                args: serde_json::json!({ "declared": declared }).to_string(),
            })
        }
    }
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
            diagnostic: None,
            notices: Vec::new(),
            busy: false,
            stale: false,
            stale_export_revision: None,
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
    let mut diagnostic = None;
    let ((message, message_key, message_args), busy, terminal, kind) =
        match attempt.map(|attempt| &attempt.phase) {
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
                        view.progress.sentences_admitted, view.progress.tokens_scanned
                    ),
                    Some("status.scanning"),
                    Some(format!(
                        "{{\"sentences\": {}, \"tokens\": {}}}",
                        view.progress.sentences_admitted, view.progress.tokens_scanned
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
            Some(AttemptPhase::Cancelled(CancelReason::Superseded)) => (
                (
                    "Superseded by a newer edit.".into(),
                    Some("status.superseded"),
                    None,
                ),
                false,
                true,
                RevisionStatusKind::Superseded,
            ),
            Some(AttemptPhase::Cancelled(CancelReason::User)) => (
                ("Search stopped.".into(), None, None),
                false,
                true,
                RevisionStatusKind::Neutral,
            ),
            Some(AttemptPhase::Cancelled(CancelReason::Shutdown)) => (
                ("This search was stopped because the server is shutting down.".into(), Some("status.shutdown"), None),
                false,
                true,
                RevisionStatusKind::Neutral,
            ),
            Some(AttemptPhase::Cancelled(CancelReason::Abandoned)) => (
                (
                    "This search was stopped because the page was closed or disconnected. Run the search again.".into(),
                    Some("status.abandoned"),
                    None,
                ),
                false,
                true,
                RevisionStatusKind::Abandoned,
            ),
            Some(AttemptPhase::Failed(failure)) => (
                match failure.category {
                    ErrorCategory::Compile => {
                        diagnostic = Some(failure.message.clone());
                        (
                            "The pattern did not compile:".into(),
                            Some("status.failed_compile"),
                            None,
                        )
                    }
                    ErrorCategory::Binding => {
                        diagnostic = Some(failure.message.clone());
                        (
                            "This catalog cannot run on this corpus:".into(),
                            Some("status.failed_binding"),
                            None,
                        )
                    }
                    ErrorCategory::Scope => {
                        diagnostic = Some(failure.message.clone());
                        (
                            "This search scope cannot be applied to this corpus:".into(),
                            Some("status.failed_scope"),
                            None,
                        )
                    }
                    ErrorCategory::Cache => {
                        (failure.message.clone(), Some("status.failed_cache"), None)
                    }
                    ErrorCategory::AnalysisChanged => (
                        failure.message.clone(),
                        Some("status.failed_analysis_changed"),
                        None,
                    ),
                    ErrorCategory::VectorsChanged => (failure.message.clone(), None, None),
                    ErrorCategory::Execution
                    | ErrorCategory::ResultLimit
                    | ErrorCategory::Integrity => {
                        diagnostic = Some(failure.message.clone());
                        ("The corpus scan could not be completed.".into(), Some("status.failed_scan"), None)
                    }
                },
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
        attempt.map(|attempt| &attempt.phase),
        Some(AttemptPhase::Completed)
    );
    let notices = view
        .state
        .displayed_snapshot()
        .filter(|_| completed)
        .map(|snapshot| {
            let omitted =
                omission_parts(snapshot.oversized_omissions.as_ref()).map(|(text, args)| {
                    StatusNotice {
                        key: "status.omitted",
                        text,
                        args,
                    }
                });
            let unavailable = snapshot
                .unavailable_attributes
                .as_ref()
                .map(unavailable_notice);
            let vectors = snapshot
                .vectors_binding
                .as_ref()
                .and_then(|vectors| vectors_notice(&vectors.declaration));
            omitted
                .into_iter()
                .chain(unavailable)
                .chain(vectors)
                .collect()
        })
        .unwrap_or_default();
    RevisionStatus {
        message,
        message_key,
        message_args,
        diagnostic,
        notices,
        busy,
        stale,
        stale_export_revision: view
            .state
            .displayed_snapshot()
            .filter(|shown| stale && shown.patterns.iter().any(|pattern| pattern.exact_total > 0))
            .map(|shown| shown.revision.0),
        terminal,
        kind,
    }
}
