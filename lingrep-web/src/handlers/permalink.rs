use axum::{
    Form, Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};

use crate::{
    corpus_context::identity_digest,
    permalink::{self, MAX_PATTERN_BYTES, PermalinkError},
    state::RequestStore,
};

#[derive(Deserialize)]
struct PermalinkForm {
    #[serde(default)]
    search_scope: Option<String>,
    pattern: String,
    /// An absent key selects the server's default corpus.
    #[serde(default)]
    corpus: Option<String>,
    /// An absent value retains the recipient's retention default.
    #[serde(default)]
    retained: Option<usize>,
    /// Absent fields omit link keys and retain recipient defaults.
    #[serde(default)]
    derive_compounds: Option<bool>,
    #[serde(default)]
    compound_category: Option<String>,
    #[serde(default)]
    document_reach: Option<bool>,
    #[serde(default)]
    overlaps: Option<String>,
}

#[derive(Serialize)]
struct PathResponse {
    path: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/permalink", post(create_permalink))
}

async fn create_permalink(
    State(store): State<RequestStore>,
    Form(form): Form<PermalinkForm>,
) -> Response {
    if form.pattern.len() > MAX_PATTERN_BYTES {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    let scope = match crate::search_scope::SearchScope::parse(form.search_scope.as_deref()) {
        Ok(scope) => scope,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_scope"),
    };
    // The request's selected corpus, named by the client.
    //
    // The lookup must stay FALLIBLE. `resolve(None)` cannot miss for a validated
    // configuration, but the key arrives from the client, and an undeclared or
    // malformed one resolves to `None`. Panicking here would then be a panicking
    // request handler — a trivial denial of service. `resolve` is
    // two-valued, so both cases are one lookup miss: startup owns validation.
    let Some(slot) = store.corpora.resolve(form.corpus.as_deref()) else {
        return error(StatusCode::NOT_FOUND, "unknown_corpus");
    };
    // A cold target is not an error. `c` is an assertion about which
    // content produced a result, so a corpus with no identity yet simply has
    // nothing to assert — emitting `k` alone is the correct answer, and is what
    // lets the selector switch to a corpus that has never been built.
    //
    // Does not call `start_or_observe`, unlike the landing page, the
    // search POST and the context poll. Those three either await readiness or
    // report progress, so leaving a slot Idle hangs. Generating a
    // link neither waits nor reports progress: it reads the current identity and
    // returns. Triggering a build here would cause link generation to start an
    // unsolicited multi-gigabyte build.
    let ready = slot.runtime.ready().await;
    let digest = ready.as_ref().map(|ready| identity_digest(&ready.identity));
    // A query-only response keeps the browser on the current mount: the codec
    // stays path-agnostic and `encode` returns `/?...` for callers that need a
    // root target, while this HTTP surface has enough context to promise a
    // same-document navigation. The conversion belongs to the codec so that no
    // second producer of this response has to re-derive it.
    // Clamped as admission clamps a run, so a link never names a retention
    // this server would not itself have run.
    let retained = form
        .retained
        .map(|asked| asked.clamp(1, store.config.detail_limits.max_kwic));
    match permalink::encode_query_only(
        &form.pattern,
        slot.name.as_str(),
        digest.as_deref(),
        retained,
    ) {
        Ok(mut path) => {
            if form.derive_compounds == Some(false) {
                path.push_str("&d=0");
            }
            if form.compound_category.as_deref() == Some("source") {
                path.push_str("&g=source");
            }
            if form.document_reach == Some(false) {
                path.push_str("&n=0");
            }
            permalink::push_run_options(
                &mut path,
                crate::templates::overlaps_of_form(form.overlaps.as_deref()),
                form.search_scope.is_some().then_some(&scope),
            );
            let table = crate::patterns::word_vectors();
            let vectors = if table.is_some() {
                match store
                    .coordinators
                    .pattern_task(move || crate::patterns::resolved_vectors(&form.pattern, table))
                    .await
                {
                    Ok(vectors) => vectors,
                    Err(error) => return super::pattern_task_error(error),
                }
            } else {
                None
            };
            match permalink::pin_vectors(path, vectors) {
                Ok(path) => (StatusCode::OK, Json(PathResponse { path })).into_response(),
                Err(_) => error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            }
        }
        Err(PermalinkError::TooLarge) => error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
        Err(error @ PermalinkError::Malformed) => super::server_error(error),
    }
}

fn error(status: StatusCode, message: &'static str) -> Response {
    (status, Json(ErrorResponse { error: message })).into_response()
}
