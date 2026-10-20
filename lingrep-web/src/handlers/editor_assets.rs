use axum::Router;

use crate::handlers::asset::{AssetEntry, asset_routes};
use crate::state::RequestStore;

const RUNTIME_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/web-tree-sitter.js"
));
const RUNTIME_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/web-tree-sitter.wasm"
));
const GRAMMAR_WASM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/tree-sitter-lingrep.wasm"
));
// Vendored CodeMirror bundle for the LSP-like editor; built and pinned via
// `lingrep-web/editor` (see its README) and hash-checked by `editor_asset_test`.
const CODEMIRROR_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/codemirror.js"
));
const HIGHLIGHTS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/highlights.scm"
));
const MANIFEST: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/vendor/manifest.json"
));
const EDITOR_JS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/pattern-editor.js"
));
const EDITOR_CSS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/editor/pattern-editor.css"
));

const ASSETS: &[AssetEntry] = &[
    (
        "/vendor/web-tree-sitter.js",
        RUNTIME_JS,
        "text/javascript; charset=utf-8",
    ),
    (
        "/vendor/web-tree-sitter.wasm",
        RUNTIME_WASM,
        "application/wasm",
    ),
    (
        "/vendor/tree-sitter-lingrep.wasm",
        GRAMMAR_WASM,
        "application/wasm",
    ),
    (
        "/vendor/codemirror.js",
        CODEMIRROR_JS,
        "text/javascript; charset=utf-8",
    ),
    (
        "/vendor/highlights.scm",
        HIGHLIGHTS,
        "text/plain; charset=utf-8",
    ),
    ("/vendor/manifest.json", MANIFEST, "application/json"),
    (
        "/pattern-editor.js",
        EDITOR_JS,
        "text/javascript; charset=utf-8",
    ),
    ("/pattern-editor.css", EDITOR_CSS, "text/css; charset=utf-8"),
];

pub fn routes() -> Router<RequestStore> {
    asset_routes(ASSETS)
}
