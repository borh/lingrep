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
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use lingrep_web::catalog_index::CatalogIndex;
use lingrep_web::{RequestStore, handlers::catalogs};
use tower::ServiceExt;

/// Build a router over a temp root containing one flat and one nested catalog.
fn fixture() -> (tempfile::TempDir, Arc<CatalogIndex>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("flat.lx"), "flat := [].\n").unwrap();
    std::fs::write(dir.path().join("sub/nested.lx"), "nested := [].\n").unwrap();
    let index = Arc::new(CatalogIndex::discover(dir.path()));
    (dir, index)
}

async fn get(index: &Arc<CatalogIndex>, uri: &str) -> (StatusCode, String) {
    let store = RequestStore::default().with_catalogs(Arc::clone(index));
    let app = catalogs::routes().with_state(store);
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn a_flat_catalog_is_served_as_plain_text() {
    let (_dir, index) = fixture();
    let (status, body) = get(&index, "/catalogs/flat.lx").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "flat := [].\n");
}

/// The route is a single catch-all rather than one literal path per key:
/// discovery admits nested keys, and this must resolve through the real
/// router (a percent-decoded capture matched against the index), not merely
/// through an index lookup.
#[tokio::test]
async fn a_nested_catalog_resolves_through_the_router() {
    let (_dir, index) = fixture();
    let (status, body) = get(&index, "/catalogs/sub/nested.lx").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "nested := [].\n");
}

#[tokio::test]
async fn an_unknown_catalog_is_not_found() {
    let (_dir, index) = fixture();
    let (status, _) = get(&index, "/catalogs/absent.lx").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_traversal_attempt_is_not_a_route() {
    let (_dir, index) = fixture();
    for uri in [
        "/catalogs/../../etc/passwd",
        "/catalogs/sub/../flat.lx",
        "/catalogs/%2e%2e%2fflat.lx",
    ] {
        let (status, _) = get(&index, uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not serve content");
    }
}

#[tokio::test]
async fn an_empty_index_registers_no_routes() {
    let index = Arc::new(CatalogIndex::default());
    let (status, _) = get(&index, "/catalogs/anything.lx").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// The catch-all route accepts any path shape; safety comes entirely from the
/// exact lookup against the index, not from what the router will register. This
/// proves the whole namespace discovery admits — including bytes that are
/// axum route-pattern syntax (`{`, `}`, `%`, `#`, `?`, `*`) and would have been
/// unsafe to interpolate into a literal route — resolves correctly through the
/// real router now that no key is ever interpolated into a route pattern.
#[tokio::test]
async fn every_admitted_name_shape_builds_a_router_and_resolves() {
    let dir = tempfile::tempdir().unwrap();
    let names = [
        "plain.lx",
        "パターン.lx",
        "with space.lx",
        "dash-and_underscore.lx",
        "dots.in.name.lx",
        "plus+amp&eq=.lx",
        "paren()brack[].lx",
        "brace{x}.lx",
        "pct%20.lx",
        "hash#.lx",
        "q?.lx",
        "star*.lx",
    ];
    for name in names {
        std::fs::write(dir.path().join(name), format!("/* {name} */\n")).unwrap();
    }
    let index = Arc::new(CatalogIndex::discover(dir.path()));
    assert_eq!(index.len(), names.len(), "all of these must be admitted");

    for name in names {
        let encoded = name
            .split('/')
            .map(urlencoding_encode)
            .collect::<Vec<_>>()
            .join("/");
        let (status, body) = get(&index, &format!("/catalogs/{encoded}")).await;
        assert_eq!(status, StatusCode::OK, "{name} must resolve");
        assert!(body.contains(name), "{name} must serve its own bytes");
    }
}

/// Percent-encode one path segment the way the browser will. Written out rather
/// than pulled from a crate so the test has no new dependency.
fn urlencoding_encode(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(*byte));
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
