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
    pattern: String,
    /// Target corpus key. Absent means the default, which is what a
    /// single-corpus client (and every pre-existing caller) sends.
    #[serde(default)]
    corpus: Option<String>,
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
    // The request's selected corpus, named by the client.
    //
    // The lookup must stay FALLIBLE. `resolve(None)` cannot miss for a validated
    // configuration, but the key arrives from the client, and an undeclared or
    // malformed one resolves to `None`. Panicking here would then be a panicking
    // request handler — a trivial denial of service. `resolve` is deliberately
    // two-valued, so both cases are one lookup miss: startup owns validation.
    let Some(slot) = store.corpora.resolve(form.corpus.as_deref()) else {
        return error(StatusCode::NOT_FOUND, "unknown_corpus");
    };
    // A cold target is not an error. `c` is an assertion about which
    // content produced a result, so a corpus with no identity yet simply has
    // nothing to assert — emitting `k` alone is the correct answer, and is what
    // lets the selector switch to a corpus that has never been built.
    //
    // Deliberately NOT a `start_or_observe` site, unlike the landing page, the
    // search POST and the context poll. Those three either await readiness or
    // promise the reader progress, so a slot they leave Idle hangs. Minting a
    // link neither waits nor promises: it reads the current identity and
    // answers. Starting a build here would make copying a link kick off a
    // multi-gigabyte build the reader never asked for.
    let ready = slot.runtime.ready().await;
    let digest = ready.as_ref().map(|ready| identity_digest(&ready.identity));
    // A query-only response keeps the browser on the current mount: the codec
    // stays path-agnostic and `encode` returns `/?...` for callers that need a
    // root target, while this HTTP surface has enough context to promise a
    // same-document navigation. The conversion belongs to the codec so that no
    // second producer of this response has to re-derive it.
    match permalink::encode_query_only(&form.pattern, slot.name.as_str(), digest.as_deref()) {
        Ok(path) => (StatusCode::OK, Json(PathResponse { path })).into_response(),
        Err(PermalinkError::TooLarge) => error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
        Err(PermalinkError::Malformed) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn error(status: StatusCode, message: &'static str) -> Response {
    (status, Json(ErrorResponse { error: message })).into_response()
}
