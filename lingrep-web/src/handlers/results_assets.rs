use axum::Router;

use crate::handlers::asset::{AssetEntry, asset_routes};
use crate::state::RequestStore;

const RESULTS_EXPLORER_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/results-explorer.js"
));
const APP_CSS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.css"));
const APP_JS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.js"));
const QUERY_LIBRARY_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/query-library.js"
));
const EDITOR_SIZING_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor-sizing.js"
));
const I18N_JS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/i18n.js"));
const I18N_EN_JSON: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/i18n/en.json"));
const I18N_JA_JSON: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/i18n/ja.json"));
// Vendored so the UI works offline and does not depend on a CDN. Versions and
// hashes are pinned in `assets/vendor/manifest.json`.
const HTMX_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/vendor/htmx.min.js"
));
const HTMX_EXT_SSE_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/vendor/htmx-ext-sse.min.js"
));

const ASSETS: &[AssetEntry] = &[
    (
        "/search-scope.js",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/search-scope.js"
        )),
        "text/javascript; charset=utf-8",
    ),
    (
        "/results-explorer.js",
        RESULTS_EXPLORER_JS,
        "text/javascript; charset=utf-8",
    ),
    ("/app.css", APP_CSS, "text/css; charset=utf-8"),
    ("/app.js", APP_JS, "text/javascript; charset=utf-8"),
    (
        "/query-library.js",
        QUERY_LIBRARY_JS,
        "text/javascript; charset=utf-8",
    ),
    (
        "/editor-sizing.js",
        EDITOR_SIZING_JS,
        "text/javascript; charset=utf-8",
    ),
    (
        "/vendor/htmx.min.js",
        HTMX_JS,
        "text/javascript; charset=utf-8",
    ),
    (
        "/vendor/htmx-ext-sse.min.js",
        HTMX_EXT_SSE_JS,
        "text/javascript; charset=utf-8",
    ),
    ("/i18n.js", I18N_JS, "text/javascript; charset=utf-8"),
    (
        "/i18n/en.json",
        I18N_EN_JSON,
        "application/json; charset=utf-8",
    ),
    (
        "/i18n/ja.json",
        I18N_JA_JSON,
        "application/json; charset=utf-8",
    ),
];

pub fn routes() -> Router<RequestStore> {
    asset_routes(ASSETS)
}
