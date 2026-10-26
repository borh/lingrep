use askama::Template;
use axum::{
    Form, Router,
    extract::{Path, State},
    response::Html,
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::coordinator::{AdmitError, RevisionRequest};
use crate::revision::WorkbenchId;
use crate::revision_status::project_revision_status;
use crate::state::RequestStore;
use crate::templates::StreamContainer;

#[derive(Deserialize, Serialize)]
pub struct SearchForm {
    #[serde(default)]
    pub search_scope: Option<String>,
    pub pattern: String,
    /// The corpus the page is pointed at, carried by a hidden field the index
    /// renders from the resolved slot. Absent means the default, so a caller
    /// predating the field keeps working unchanged.
    #[serde(default)]
    pub corpus: Option<String>,
    /// How many KWIC lines this run retains per pattern. Absent means the
    /// server's configured ceiling, so a caller predating the control — and
    /// the export and permalink paths, which do not offer one — is unchanged.
    /// A value above the ceiling is clamped at admission, not refused.
    #[serde(default)]
    pub retained: Option<usize>,
    #[serde(default)]
    pub derive_compounds: Option<bool>,
    #[serde(default)]
    pub compound_category: Option<String>,
    #[serde(default)]
    pub document_reach: Option<bool>,
}

/// Matches `handlers::sample::MAX_PATTERN_BYTES` and the compiler's
/// `CompileLimits` source-byte ceiling (1 MiB): reject only what the lingrep patterns
/// compiler itself would reject, so a real grammar file (e.g. the seeded
/// sf-experimental.lx) scans instead of bouncing off an arbitrary cap.
const MAX_PATTERN_BYTES: usize = 1 << 20;

/// The workbench is bound to another corpus, so this request cannot be answered
/// without straddling two. Distinct from [`REGISTRY_FULL_ALERT`] because the two
/// are different facts: this one names something the reader can act on.
const CORPUS_CONFLICT_ALERT: &str = concat!(
    "<div class=\"alert alert-error\" role=\"status\" data-i18n=\"corpus.conflict\">",
    "This search belongs to a different corpus than the one this workbench was ",
    "opened for. Reload the page to search this corpus.</div>"
);

/// Capacity exhaustion. Untranslated inline text, like the other pre-existing
/// alerts in this handler.
const REGISTRY_FULL_ALERT: &str = concat!(
    "<div class=\"alert alert-error\" role=\"status\">",
    "Server busy: too many active sessions. Try again shortly.</div>"
);

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/search/{workbench_id}", post(search))
}

pub async fn search(
    State(store): State<RequestStore>,
    Path(workbench_id): Path<String>,
    Form(form): Form<SearchForm>,
) -> Html<String> {
    if form.pattern.len() > MAX_PATTERN_BYTES {
        return Html(
            "<div class=\"alert alert-error\" role=\"status\">Pattern too large to scan.</div>"
                .to_string(),
        );
    }
    let Ok(scope) = crate::search_scope::SearchScope::parse(form.search_scope.as_deref()) else {
        return Html(
            "<div class=\"alert alert-error\" role=\"status\">Invalid search scope.</div>".into(),
        );
    };
    let workbench = WorkbenchId::new(&workbench_id);
    let Some(slot) = store.corpora.resolve(form.corpus.as_deref()) else {
        return Html(
            "<div class=\"alert alert-error\" role=\"status\" data-i18n=\"corpus.unavailable\">Corpus unavailable.</div>"
                .to_string(),
        );
    };
    slot.runtime.start_or_observe().await;
    let compound_category = if form.compound_category.as_deref() == Some("source") {
        crate::snapshot::CompoundCategory::SourceCategories
    } else {
        crate::snapshot::CompoundCategory::Compound
    };
    if let Err(error) = store
        .coordinators
        .accept_revision(
            workbench.clone(),
            slot,
            RevisionRequest {
                source: form.pattern,
                retained: form.retained,
                derive_compounds: form.derive_compounds.unwrap_or(false),
                compound_category,
                document_reach: form.document_reach.unwrap_or(false),
                search_scope: scope,
            },
        )
        .await
    {
        return Html(
            match error {
                AdmitError::CorpusConflict => CORPUS_CONFLICT_ALERT,
                AdmitError::RegistryFull => REGISTRY_FULL_ALERT,
            }
            .to_string(),
        );
    }
    let view = store.coordinators.snapshot(&workbench).await;
    let status = project_revision_status(view.as_ref());

    StreamContainer {
        request_id: &workbench_id,
        status: &status,
    }
    .render().map_or_else(|_| Html("<div class=\"alert alert-error\" role=\"status\">Unable to render results. Please retry.</div>".to_string()), Html)
}
