// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use lingrep_web::{RequestStore, handlers::results_assets};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use tower_http::compression::CompressionLayer;

fn app() -> axum::Router {
    results_assets::routes().with_state(RequestStore::default())
}

async fn get(app: &axum::Router, uri: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn vendored_htmx_assets_match_the_pinned_manifest() {
    let app = app();
    let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/vendor/manifest.json"
    )))
    .unwrap();
    for (name, hash) in manifest["sha256"].as_object().unwrap() {
        let response = get(&app, &format!("/vendor/{name}")).await;
        assert_eq!(response.status(), StatusCode::OK, "{name}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8",
            "{name}"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            hash.as_str().unwrap(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn static_assets_revalidate_with_etag() {
    let app = app();
    for uri in [
        "/app.css",
        "/app.js",
        "/query-library.js",
        "/editor-sizing.js",
        "/results-explorer.js",
        "/vendor/htmx.min.js",
    ] {
        let first = get(&app, uri).await;
        assert_eq!(first.status(), StatusCode::OK, "{uri}");
        assert_eq!(first.headers()[header::CACHE_CONTROL], "no-cache", "{uri}");
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();

        let revalidated = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED, "{uri}");
        let body = revalidated.into_body().collect().await.unwrap().to_bytes();
        assert!(body.is_empty(), "{uri}");
    }
}

#[tokio::test]
async fn editor_sizing_asset_is_served_as_a_module() {
    let response = get(&app(), "/editor-sizing.js").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    let javascript = response.into_body().collect().await.unwrap().to_bytes();
    let javascript = std::str::from_utf8(&javascript).unwrap();
    assert!(javascript.contains("initializeEditorSizing"));
    assert!(javascript.contains("lingrep-web:editor-height:v1"));
}

#[tokio::test]
async fn query_library_asset_is_served_as_a_module() {
    let response = get(&app(), "/query-library.js").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    let javascript = response.into_body().collect().await.unwrap().to_bytes();
    let javascript = std::str::from_utf8(&javascript).unwrap();
    assert!(javascript.contains("openQueryDatabase"));
    // These tokens deliberately pin the cross-module event protocol: history
    // captures released htmx requests, and explicit runs use the ordinary form.
    assert!(javascript.contains("htmx:beforeRequest"));
    assert!(javascript.contains("form[data-corpus-search]"));
    assert!(javascript.contains("form.requestSubmit()"));
    assert!(!javascript.contains("console.log"));
}

#[tokio::test]
async fn security_headers_cover_every_response() {
    use axum::middleware;
    use lingrep_web::security_headers::security_headers;

    let app = results_assets::routes()
        .with_state(RequestStore::default())
        .layer(middleware::from_fn(security_headers));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let headers = response.headers();
    let csp = headers["content-security-policy"].to_str().unwrap();
    // No inline scripts and no CDN: every script is a vendored asset, and the
    // tree-sitter WASM grammar needs the CSP 3 wasm escape hatch.
    assert!(
        csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
        "{csp}"
    );
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(
        headers["permissions-policy"],
        "geolocation=(), camera=(), microphone=(), payment=()"
    );
    assert_eq!(headers["cross-origin-opener-policy"], "same-origin");
}

#[tokio::test]
async fn asset_layer_compresses_when_the_client_accepts_it() {
    let compressed_app = results_assets::routes()
        .layer(CompressionLayer::new())
        .with_state(RequestStore::default());
    let response = compressed_app
        .oneshot(
            Request::builder()
                .uri("/vendor/htmx.min.js")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
    let compressed = response.into_body().collect().await.unwrap().to_bytes();
    let plain = get(&app(), "/vendor/htmx.min.js").await;
    let plain_bytes = plain.into_body().collect().await.unwrap().to_bytes();
    assert!(
        compressed.len() < plain_bytes.len() / 2,
        "gzip should at least halve htmx: {} vs {}",
        compressed.len(),
        plain_bytes.len()
    );
}

#[tokio::test]
async fn permalink_assets_pin_copy_fallback_and_verified_autorun_hooks() {
    let app = app();
    let javascript = get(&app, "/app.js")
        .await
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let javascript = std::str::from_utf8(&javascript).unwrap();
    assert!(javascript.contains("api/permalink"));
    assert!(javascript.contains("[data-copy-permalink]"));
    assert!(javascript.contains(".permalink-fallback"));
    assert!(javascript.contains("[data-corpus-search][data-autorun]"));
    assert!(javascript.contains("requestSubmit()"));

    let stylesheet = get(&app, "/app.css")
        .await
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let stylesheet = std::str::from_utf8(&stylesheet).unwrap();
    assert!(stylesheet.contains(".permalink-controls"));
    assert!(stylesheet.contains(".permalink-fallback"));
    assert!(stylesheet.contains(".permalink-notice"));
}
