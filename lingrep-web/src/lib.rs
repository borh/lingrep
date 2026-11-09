pub mod catalog_index;
pub mod cli;
pub mod compound_chains;
pub mod coordinator;
pub mod corpus_context;
pub mod corpus_runtime;
pub mod corpus_set;
mod derived_patterns;
pub mod document_reach;
mod export;
pub mod frequency;
pub mod handlers;
pub mod inspection;
pub mod page_projection;
pub mod pattern_catalog;
pub mod pattern_detail;
pub mod patterns;
pub mod permalink;
pub mod provenance_view;
mod reading;
pub mod revision;
pub mod revision_status;
pub mod sample;
pub mod section_analysis;
pub mod section_comparison;
pub mod section_contrast;
pub mod section_dendrogram;
pub mod section_tally;
pub mod security_headers;
pub mod serve;
pub mod server_context;
pub mod snapshot;
pub mod snapshot_sink;
pub mod state;
pub mod templates;

pub use cli::{Args, BasePath};
pub use state::{Config, RequestStore};

use axum::{
    Router,
    extract::{DefaultBodyLimit, RawQuery},
    middleware,
    response::Redirect,
    routing::get,
};
use tower_http::compression::CompressionLayer;

// Transport ceiling for /api request bodies. Sized above the 1 MiB pattern cap
// plus URL-encoding overhead (up to ~3x for multibyte lingrep patterns) so a pattern the
// compiler would accept is never rejected at the transport layer first. lingrep-web
// defaults to a loopback bind (see `lingrep-web --host`); a non-loopback bind is
// opt-in and assumes a reverse proxy terminates TLS and enforces access
// control upstream, so the transport ceiling is not the only line of defense.
const MAX_API_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Builds the production HTTP surface. Keeping the complete layer composition
/// here lets integration tests exercise security boundaries exactly as served.
pub fn app_router(store: RequestStore) -> Router {
    routes(store).layer(middleware::from_fn(security_headers::security_headers))
}

fn routes(store: RequestStore) -> Router {
    Router::new()
        .merge(handlers::index::routes())
        .merge(handlers::help::routes())
        .merge(handlers::sample::page_routes())
        .merge(handlers::frequency::page_routes())
        .nest(
            "/assets/editor",
            handlers::editor_assets::routes().layer(CompressionLayer::new()),
        )
        .nest(
            "/assets",
            handlers::results_assets::routes().layer(CompressionLayer::new()),
        )
        .nest(
            "/api",
            Router::<RequestStore>::new()
                .merge(handlers::search::routes())
                .merge(handlers::corpus_context::routes())
                .nest("/pattern", handlers::pattern_editor::routes())
                .merge(handlers::stream::routes())
                .merge(handlers::section_analysis::routes())
                .merge(handlers::inspect::routes())
                .merge(handlers::pattern_detail::routes())
                .merge(handlers::sample::routes())
                .merge(handlers::export::routes())
                .merge(handlers::permalink::routes())
                .merge(handlers::catalogs::routes())
                .merge(handlers::frequency::routes())
                .layer(DefaultBodyLimit::max(MAX_API_BODY_BYTES)),
        )
        .with_state(store)
}

/// Builds the production HTTP surface under `base_path`.
///
/// The router itself owns the mount point rather than relying on a proxy to
/// strip it. This keeps direct deployments and proxy deployments explicit and
/// makes paths outside the configured mount return 404. A nested mount serves
/// its start page at `<base_path>/` and redirects `<base_path>` there.
pub fn app_router_at(store: RequestStore, base_path: &BasePath) -> Router {
    if base_path.is_root() {
        return app_router(store);
    }
    let mount = base_path.as_str();
    // A relative Location still resolves to the mount when an outer proxy
    // adds a prefix of its own.
    let last_segment = mount.rsplit('/').next().unwrap_or_default().to_owned();
    let redirect = move |RawQuery(query): RawQuery| async move {
        let target = match query {
            Some(query) => format!("./{last_segment}/?{query}"),
            None => format!("./{last_segment}/"),
        };
        Redirect::permanent(&target)
    };
    Router::new()
        .route(mount, get(redirect))
        .nest(&format!("{mount}/"), routes(store))
        .layer(middleware::from_fn(security_headers::security_headers))
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    use super::{BasePath, RequestStore, app_router_at};

    #[tokio::test]
    async fn mounted_router_serves_assets_only_under_its_base_path() {
        let base_path = BasePath::parse("/temp-lingrep-web-proxy").expect("valid mount path");
        let app = app_router_at(RequestStore::default(), &base_path);

        let mounted = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/temp-lingrep-web-proxy/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mounted.status(), StatusCode::OK);

        let root = app
            .oneshot(
                Request::builder()
                    .uri("/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(root.status(), StatusCode::NOT_FOUND);
    }

    /// The start page's links are relative (`assets/app.js`), so it is served
    /// at the trailing-slash form of the mount, where they resolve inside the
    /// mount; the bare mount redirects there with its query intact.
    #[tokio::test]
    async fn mounted_router_serves_the_start_page_at_the_trailing_slash_form() {
        let base_path = BasePath::parse("/research/lingrep-web").expect("valid mount path");
        let app = app_router_at(RequestStore::default(), &base_path);
        let get = |uri: &str| {
            app.clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        };

        let page = get("/research/lingrep-web/").await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);

        let bare = get("/research/lingrep-web?v=1&q=x").await.unwrap();
        assert_eq!(bare.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            bare.headers()[axum::http::header::LOCATION],
            "./lingrep-web/?v=1&q=x"
        );
        let bare = get("/research/lingrep-web").await.unwrap();
        assert_eq!(
            bare.headers()[axum::http::header::LOCATION],
            "./lingrep-web/"
        );
    }

    /// A last segment such as `a:b` would read as a URI with scheme `a` if it
    /// began the Location.
    #[tokio::test]
    async fn the_redirect_location_is_a_path_when_the_last_segment_contains_a_colon() {
        let base_path = BasePath::parse("/x/a:b").expect("valid mount path");
        let app = app_router_at(RequestStore::default(), &base_path);
        let bare = app
            .oneshot(
                Request::builder()
                    .uri("/x/a:b")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bare.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(bare.headers()[axum::http::header::LOCATION], "./a:b/");
    }

    /// The redirect and the 404s outside the mount are answered by the outer
    /// router, and carry the same security headers as the application.
    #[tokio::test]
    async fn mounted_router_responses_outside_the_application_carry_security_headers() {
        let base_path = BasePath::parse("/mounted").expect("valid mount path");
        let app = app_router_at(RequestStore::default(), &base_path);
        for (uri, status) in [
            ("/mounted", StatusCode::PERMANENT_REDIRECT),
            ("/elsewhere", StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{uri}");
            assert_eq!(
                response.headers()["x-content-type-options"],
                "nosniff",
                "{uri}"
            );
        }
    }
}

pub mod search_scope;
