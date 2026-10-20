//! Serves the startup-discovered lingrep patterns catalogs.
//!
//! A single catch-all route (`/catalogs/{*rest}`) is registered, and every
//! request is resolved by an exact lookup against the same `CatalogIndex`
//! discovery built at startup — rather than one literal route per key.
//!
//! axum's `Path<String>` extractor percent-decodes the captured segment
//! before handlers see it (verified directly against this axum version; see
//! `lingrep-web/tests/suite/catalog_index_test.rs`), so a nested key
//! (`sub/x.lx`), a space, or a non-ASCII byte all arrive as the real
//! discovered key regardless of how a client encoded them on the wire. The
//! traversal property is unchanged and still structural: a decoded string
//! that is not a key in the index is a 404, and `..` can never be a key
//! because keys come only from a directory walk at startup — no
//! browser-supplied string ever reaches the filesystem.

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::state::RequestStore;

/// `text/plain` so a browser `fetch` reads lingrep patterns source verbatim; the charset is
/// explicit because catalogs routinely contain Japanese.
const CATALOG_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// Register the single catch-all catalog route.
///
/// Unlike a route-per-key registration, this needs no index at
/// router-construction time: the lookup happens per request, against
/// `RequestStore::catalogs`.
pub fn routes() -> Router<RequestStore> {
    Router::new().route("/catalogs/{*rest}", get(serve_catalog))
}

async fn serve_catalog(State(store): State<RequestStore>, Path(rest): Path<String>) -> Response {
    match store.catalogs.get(&rest) {
        Some(source) => plain_text(source),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn plain_text(source: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(source.to_owned()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(CATALOG_CONTENT_TYPE),
    );
    response
}
