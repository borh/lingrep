use axum::{
    Router,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

use crate::state::RequestStore;
use crate::templates::{CorpusContextPartial, CorpusOption};

/// What the polled partial asks about.
#[derive(Deserialize)]
struct SelectedCorpus {
    /// The selected corpus key, echoed back by the partial that emitted it.
    /// Absent means the default, so a request predating the parameter — a
    /// cached page from an earlier build — still resolves.
    #[serde(default)]
    k: Option<String>,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route("/corpus-context", get(corpus_context_partial))
}

async fn corpus_context_partial(
    State(store): State<RequestStore>,
    Query(selected): Query<SelectedCorpus>,
) -> Response {
    let corpus_context =
        crate::corpus_context::corpus_context_view(&store, selected.k.as_deref()).await;
    // The poll replaces the whole section, so it re-renders the selector too;
    // marking it against the view's own key keeps a poll from disagreeing with
    // the panel it swaps in.
    let corpora = CorpusOption::list(&store.corpora, &corpus_context.corpus_key);
    let body = super::render_or_500(CorpusContextPartial {
        corpus_context,
        corpora,
    });
    ([(header::CACHE_CONTROL, "no-store")], body).into_response()
}
