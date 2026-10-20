//! Shared serving for assets embedded at compile time.
//!
//! Embedded bytes change only when the binary is rebuilt, so a content-derived
//! ETag plus `no-cache` gives clients cheap 304 revalidation without any risk
//! of serving stale content. The ETag is weak (`W/…`) because a compression
//! layer may transform the body between this handler and the client.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
    routing::get,
};

use crate::state::RequestStore;

pub(crate) fn static_asset(
    request_headers: &HeaderMap,
    bytes: &'static [u8],
    content_type: &'static str,
) -> Response<Body> {
    let etag = etag_for(bytes);
    let mut response = if if_none_match_matches(request_headers, &etag) {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response
    } else {
        let mut response = Response::new(Body::from(bytes));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
        response
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("hex ETags are valid header values"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// One embedded asset: request path, compile-time bytes, and content type.
pub(crate) type AssetEntry = (&'static str, &'static [u8], &'static str);

/// Build a router that serves each entry in `table` through [`static_asset`],
/// preserving that handler's weak-ETag / `no-cache` / 304 behavior. Kept as its
/// own function so each asset nest supplies its own table and keeps its own
/// `routes()` entry point (and therefore its own `CompressionLayer` in
/// `app_router`).
pub(crate) fn asset_routes(table: &'static [AssetEntry]) -> Router<RequestStore> {
    let mut router = Router::new();
    for &(path, bytes, content_type) in table {
        router =
            router.route(
                path,
                get(move |headers: HeaderMap| async move {
                    static_asset(&headers, bytes, content_type)
                }),
            );
    }
    router
}

fn etag_for(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("W/\"{:016x}\"", hasher.finish())
}

fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == "*" || strip_weak(candidate) == strip_weak(etag))
        })
}

fn strip_weak(tag: &str) -> &str {
    tag.strip_prefix("W/").unwrap_or(tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn request_with_if_none_match(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn serves_body_and_etag_on_first_request() {
        let response = static_asset(&HeaderMap::new(), b"hello", "text/plain");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        assert!(
            response.headers()[header::ETAG]
                .to_str()
                .unwrap()
                .starts_with("W/\"")
        );
    }

    #[test]
    fn matching_if_none_match_returns_304_without_body() {
        let first = static_asset(&HeaderMap::new(), b"hello", "text/plain");
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let response = static_asset(&request_with_if_none_match(&etag), b"hello", "text/plain");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag.as_str());
        assert!(!response.headers().contains_key(header::CONTENT_TYPE));
    }

    #[test]
    fn strong_tag_and_wildcard_also_match() {
        let first = static_asset(&HeaderMap::new(), b"hello", "text/plain");
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let strong = etag.strip_prefix("W/").unwrap().to_owned();
        for candidate in [strong, "*".to_string(), format!("\"other\", {etag}")] {
            let response = static_asset(
                &request_with_if_none_match(&candidate),
                b"hello",
                "text/plain",
            );
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{candidate}");
        }
    }

    #[test]
    fn changed_bytes_change_the_etag() {
        let old = static_asset(&HeaderMap::new(), b"hello", "text/plain");
        let old_etag = old.headers()[header::ETAG].to_str().unwrap().to_owned();
        let response = static_asset(
            &request_with_if_none_match(&old_etag),
            b"goodbye",
            "text/plain",
        );
        assert_eq!(response.status(), StatusCode::OK);
    }
}
