use askama::Template;
use axum::{
    Form, Router,
    extract::{Path, State},
    response::Html,
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::coordinator::AdmitError;
use crate::revision::WorkbenchId;
use crate::revision_status::project_revision_status;
use crate::state::RequestStore;
use crate::templates::StreamContainer;

#[derive(Deserialize, Serialize)]
pub struct SearchForm {
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
    let workbench = WorkbenchId::new(&workbench_id);
    // The request's selected corpus, which admission then binds the workbench
    // to for good. The key now arrives from the client: the index renders it as
    // a hidden field, so the corpus the page is pointed at — not the server's
    // default — is what this request answers from.
    //
    // The lookup must stay FALLIBLE. `resolve(None)` cannot miss for a validated
    // configuration, but a supplied key is client data, and an undeclared or
    // malformed one resolves to `None`. Panicking here would then be a panicking
    // request handler — a trivial denial of service.
    //
    // This is the seam where a client's key first becomes DURABLE state: the
    // slot below is what binds the workbench to a corpus for good. So an
    // unresolvable selection must return before `try_accept_next_revision` —
    // admitting against a fallback corpus would silently answer from the wrong
    // data, which is the failure this whole task exists to close.
    //
    // The message reuses the existing `corpus.unavailable` key rather than
    // introducing a string, so this adds no i18n entry; the `data-i18n`
    // attribute is what the client swaps at `htmx:afterSwap`.
    let Some(slot) = store.corpora.resolve(form.corpus.as_deref()) else {
        return Html(
            "<div class=\"alert alert-error\" role=\"status\" data-i18n=\"corpus.unavailable\">Corpus unavailable.</div>"
                .to_string(),
        );
    };
    // The step without which a never-selected slot stays Idle with readiness
    // Checking forever: start_or_observe only transitions from Idle, and nothing
    // else in this path calls it. Idempotent once building or ready.
    //
    // It must run BEFORE `try_accept_next_revision`, because that admission is
    // what spawns `run_revision` — whose `wait_for_ready` awaits a readiness
    // change that an unstarted runtime will never send. The revision would then
    // never become terminal, so its entry would never become evictable, and each
    // such request would burn one session for the life of the process. A tab left
    // open on `/?…&k=sci` across a restart posts exactly this.
    slot.runtime.start_or_observe().await;
    // The two refusals are different facts and are reported as different facts.
    // A conflict says the workbench is already bound elsewhere — reachable only
    // now that the request names a corpus — and is something the reader can act
    // on; capacity exhaustion is not.
    let compound_category = if form.compound_category.as_deref() == Some("source") {
        crate::snapshot::CompoundCategory::SourceCategories
    } else {
        crate::snapshot::CompoundCategory::Compound
    };
    if let Err(error) = store
        .coordinators
        .try_accept_next_revision_with_compounds(
            workbench.clone(),
            slot,
            form.pattern,
            form.retained,
            form.derive_compounds.unwrap_or(false),
            compound_category,
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
