use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use lingrep_web::RequestStore;

#[derive(Deserialize, Serialize)]
struct SearchForm {
    pattern: String,
    dict_path: String,
    corpus_path: String,
}

// Consolidation made this file a module of `suite`, so the inner module now
// nests as `handlers_test::handlers_test`. Renaming it or unwrapping it would
// change every test path in this file; the allow keeps the test names exactly
// as they were before consolidation.
#[cfg(test)]
#[allow(clippy::module_inception)]
mod handlers_test {
    use super::*;
    use lingrep_web::revision::{RevisionId, WorkbenchId};

    fn attribute_value<'a>(html: &'a str, attribute: &str) -> &'a str {
        let marker = format!("{attribute}=\"");
        html.split_once(&marker)
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value)
            .expect("attribute must be present")
    }

    fn make_app() -> Router<RequestStore> {
        lingrep_web::handlers::index::routes()
    }

    fn make_sample_page_app() -> Router<RequestStore> {
        lingrep_web::handlers::sample::page_routes()
    }

    #[tokio::test]
    async fn index_page_preserves_load_bearing_dom() {
        let app = make_app().with_state(RequestStore::default());
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        for hook in [
            r#"id="corpus-pattern""#,
            r#"id="search-status""#,
            r#"id="results-stale""#,
            r#"id="results""#,
            r#"data-editor-size"#,
            r#"data-editor-toggle"#,
            r#"aria-controls="corpus-editor""#,
            r#"class="query-tabs" role="tablist""#,
            r#"id="query-tab-recent""#,
            r#"id="query-tab-saved""#,
            r#"class="btn btn-primary""#,
            r#"class="btn btn-secondary""#,
        ] {
            assert!(html.contains(hook), "missing index hook: {hook}");
        }
        assert!(html.contains("hx-post=\"api/search/"));
        assert!(!html.contains(r#"id="search-stream""#));
        assert!(html.contains(r#"data-sample-drawer-toggle"#));
        assert!(html.contains(r#"data-inline-sample-drawer"#));
        assert!(html.contains(r#"<script type="module" src="assets/editor/pattern-editor.js">"#));
        assert_eq!(html.matches(r#"aria-current="page""#).count(), 1);
        let corpus_marker = html
            .split_once(">Corpus Search</span></a>")
            .expect("Corpus Search nav link present")
            .0;
        assert!(
            corpus_marker
                .rsplit("<a href=\"./\"")
                .next()
                .is_some_and(|segment| segment.contains(r#"aria-current="page""#))
        );
    }

    /// The sample debugger is its own page: the index carries no form that
    /// re-posts `#corpus-pattern` on a keystroke timer.
    #[tokio::test]
    async fn the_index_page_no_longer_carries_the_sample_debugger() {
        let app = make_app().with_state(RequestStore::default());
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(!html.contains(r#"hx-post="api/sample""#));
        assert!(!html.contains(r##"hx-include="#corpus-pattern""##));
        assert!(!html.contains(r#"id="sample-results""#));
        assert!(!html.contains(r#"name="sample""#));
    }

    /// The page is standalone: its own pattern box, its own draft key, and no
    /// workbench, corpus or revision anywhere in it.
    #[tokio::test]
    async fn the_sample_page_carries_its_own_pattern_editor() {
        let app = make_sample_page_app()
            .with_state(RequestStore::default().with_default_pattern("seed := []."));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/sample")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        for hook in [
            r#"id="sample-pattern""#,
            r#"name="pattern""#,
            r#"data-pattern-editor"#,
            r#"data-sample-workspace"#,
            r#"data-query-library"#,
            r#"hx-post="api/sample""#,
            r#"pattern:valid from:#sample-pattern"#,
            r#"id="sample-results""#,
            r#"name="sample""#,
            r#"<script type="module" src="assets/editor/pattern-editor.js">"#,
            r#"<script type="module" src="assets/query-library.js">"#,
        ] {
            assert!(html.contains(hook), "missing sample-page hook: {hook}");
        }
        // Seeded from `--patterns`, exactly as the index is.
        assert!(html.contains("seed := []."));
        // The two pages do not share one live textarea.
        assert!(!html.contains(r#"id="corpus-pattern""#));
        assert!(!html.contains(r##"hx-include="#corpus-pattern""##));
        // Nothing corpus-bound reaches this page.
        assert!(!html.contains("api/search/"));
    }

    #[tokio::test]
    async fn the_sample_page_is_the_current_nav_entry() {
        let app = make_sample_page_app().with_state(RequestStore::default());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/sample")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert_eq!(html.matches(r#"aria-current="page""#).count(), 1);
        let marker = html
            .split_once(">Sample Debugger</span></a>")
            .expect("Sample Debugger nav link present")
            .0;
        assert!(
            marker
                .rsplit("<a href=\"sample\"")
                .next()
                .is_some_and(|segment| segment.contains(r#"aria-current="page""#))
        );
    }

    #[tokio::test]
    async fn application_stylesheet_is_served_locally() {
        let app =
            lingrep_web::handlers::results_assets::routes().with_state(RequestStore::default());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/app.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/css; charset=utf-8"
        );
        let css = response.into_body().collect().await.unwrap().to_bytes();
        assert!(css.starts_with(b":root"));
    }

    #[tokio::test]
    async fn test_search_accepts_form_data() {
        let text = SearchForm {
            pattern: "[upos=VERB].".to_string(),
            dict_path: "/path/to/dict.dic".to_string(),
            corpus_path: "/path/to/corpus.jsonl".to_string(),
        };
        let body = serde_urlencoded::to_string(&text).unwrap();

        let app = lingrep_web::handlers::search::routes().with_state(RequestStore::default());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search/test-tab")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("sse-connect"));
    }

    #[tokio::test]
    async fn corpus_search_reuses_one_tab_identity() {
        let store = RequestStore::default();
        let app = Router::new()
            .merge(lingrep_web::handlers::index::routes())
            .nest("/api", lingrep_web::handlers::search::routes())
            .with_state(store.clone());
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        let search_path = attribute_value(&html, "hx-post").to_owned();
        assert!(search_path.starts_with("api/search/"));
        let search_path = format!("/{search_path}");

        for pattern in ["one := [upos=NOUN].", "two := [upos=VERB]."] {
            let body = serde_urlencoded::to_string([("pattern", pattern)]).unwrap();
            app.clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&search_path)
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let workbench = WorkbenchId::new(search_path.rsplit('/').next().unwrap());
        assert_eq!(
            store
                .coordinators
                .snapshot(&workbench)
                .await
                .unwrap()
                .state
                .current_attempt()
                .unwrap()
                .revision,
            RevisionId(2),
        );
    }

    #[tokio::test]
    async fn test_stream_returns_sse() {
        let store = lingrep_web::RequestStore::default();
        let app = lingrep_web::handlers::stream::routes().with_state(store);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/stream/test-request-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response.headers().get("Content-Type").unwrap();
        assert!(content_type.to_str().unwrap().contains("text/event-stream"));
    }

    /// An oversize pattern is rejected with a swappable error alert *before* the
    /// coordinator or dictionary is ever consulted — so this test needs no dictionary.
    #[tokio::test]
    async fn oversize_search_pattern_returns_a_swappable_error_alert() {
        let app = lingrep_web::handlers::search::routes().with_state(RequestStore::default());
        // One byte over MAX_PATTERN_BYTES (1 MiB), still under the 4 MiB
        // transport ceiling so the handler size guard (not transport) rejects it.
        let pattern = "a".repeat((1 << 20) + 1);
        let text = serde_urlencoded::to_string([("pattern", pattern.as_str())]).unwrap();
        let res = app
            .oneshot(
                Request::post("/search/wb-test")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(text))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Stays 200 so htmx swaps the alert into the page.
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            html.contains("alert-error"),
            "expected an error alert, got: {html}"
        );
        assert!(
            html.contains("too large"),
            "expected a size message, got: {html}"
        );
    }

    /// A grammar-file-sized pattern (64 KiB) is accepted and streamed rather
    /// than bounced as "too large", so the seeded sf-experimental.lx can be
    /// scanned.
    #[tokio::test]
    async fn a_grammar_sized_pattern_is_accepted_not_rejected_as_too_large() {
        let app = lingrep_web::handlers::search::routes().with_state(RequestStore::default());
        let pattern = format!(
            "v := [upos=VERB].\n{}",
            "# padding comment line\n".repeat(3_000)
        );
        assert!(pattern.len() > 64 * 1024 && pattern.len() < (1 << 20));
        let text = serde_urlencoded::to_string([("pattern", pattern.as_str())]).unwrap();
        let res = app
            .oneshot(
                Request::post("/search/wb-big")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(text))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            !html.contains("too large"),
            "a 64 KiB pattern must not be rejected as too large: {html}"
        );
    }

    #[tokio::test]
    async fn assembled_api_router_rejects_a_body_over_the_global_ceiling() {
        let app = lingrep_web::app_router(RequestStore::default());
        let pattern = "a".repeat((4 * 1024 * 1024) + 1);
        let body = serde_urlencoded::to_string([("pattern", pattern.as_str())]).unwrap();
        let response = app
            .oneshot(
                Request::post("/api/search/wb-limit-test")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn full_pages_preserve_navigation_and_help_contract() {
        let app = Router::new()
            .merge(lingrep_web::handlers::index::routes())
            .merge(lingrep_web::handlers::help::routes())
            .merge(lingrep_web::handlers::sample::page_routes())
            .with_state(RequestStore::default());
        for path in ["/", "/help", "/sample"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let html = String::from_utf8(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            assert!(html.contains("href=\"help\""), "missing Help on {path}");
            assert!(
                html.contains("href=\"sample\""),
                "missing Sample Debugger on {path}"
            );
            assert!(
                html.contains("href=\"sample\" target=\"_blank\" rel=\"noopener\""),
                "Sample must preserve the current page on {path}"
            );
            assert!(
                html.contains(
                    "href=\"help\" target=\"_blank\" rel=\"noopener\" data-site-help-open"
                ),
                "Help needs a state-preserving fallback on {path}"
            );
            assert!(
                html.contains("data-site-help aria-labelledby=\"site-help-title\""),
                "Help dialog missing on {path}"
            );
            assert_eq!(html.matches("aria-current=\"page\"").count(), 1, "{path}");
            assert!(!html.contains("github.com/anomalyco/lingrep#pattern-syntax"));
            if path == "/" {
                assert!(html.contains(">Corpus Search</span></a>"));
                continue;
            }

            if path == "/sample" {
                assert!(html.contains(">Sample Debugger</span></a>"));
                continue;
            }

            assert_eq!(path, "/help");
            for title in [
                "Token constraints",
                "Finite values",
                "References",
                "Alternation and quantifiers",
                "Capture comparison",
                "Boundaries",
                "Regular lookaround",
                "Match modes",
            ] {
                assert!(html.contains(title), "missing help example: {title}");
            }
            let help_marker = html
                .split_once(">Help</span></a>")
                .expect("Help nav link present")
                .0;
            assert!(
                help_marker
                    .rsplit("<a href=\"/help\"")
                    .next()
                    .is_some_and(|segment| segment.contains(r#"aria-current="page""#))
            );
        }
    }

    /// A `var(--name)` with no fallback whose name is never defined makes the
    /// whole declaration invalid at computed-value time, so the property falls
    /// back to its inherited or initial value rather than to anything the
    /// stylesheet chose. That is silent under a light theme and wrong under a
    /// dark one: `.dendrogram-label { fill: var(--text-color) }` — one letter
    /// off `--text` — computed to SVG's initial black fill, and the pattern
    /// names disappeared into the dark background. Nothing in the browser
    /// reports it, so the stylesheet is checked against itself here.
    #[test]
    fn every_custom_property_the_stylesheet_reads_is_one_it_or_a_template_sets() {
        let css = include_str!("../../assets/app.css");

        // A definition is `--name:` at the head of a declaration. Templates
        // count too, because an inline `style` attribute is a legitimate place
        // to set one — `--constellation-depth` is set per node that way.
        let mut defined = definitions(css);
        let templates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        for path in template_sources(&templates) {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            defined.extend(definitions(&source));
        }

        let mut undefined = Vec::new();
        for reference in css.split("var(--").skip(1) {
            let mut name = String::from("--");
            let mut terminator = None;
            for character in reference.chars() {
                if matches!(character, ',' | ')' | ' ') {
                    terminator = Some(character);
                    break;
                }
                name.push(character);
            }
            // A fallback makes an undefined name deliberate rather than a typo.
            if terminator == Some(')') && !defined.contains(&name) {
                undefined.push(name);
            }
        }

        assert!(
            undefined.is_empty(),
            "app.css reads custom properties nothing defines, and no fallback \
             covers them: {undefined:?}"
        );
    }

    /// Every `--name` at the head of a declaration in one source.
    fn definitions(source: &str) -> std::collections::BTreeSet<String> {
        source
            .split(['{', '}', ';', '"', '\''])
            .filter_map(|declaration| {
                let declaration = declaration.trim();
                let name = declaration.strip_prefix("--")?;
                let name = name.split_once(':')?.0.trim_end();
                name.chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
                    .then(|| format!("--{name}"))
            })
            .collect()
    }

    /// Template sources under one directory, one level of nesting deep, which
    /// is the whole of `templates`.
    fn template_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut sources = Vec::new();
        let mut directories = vec![root.to_path_buf()];
        while let Some(directory) = directories.pop() {
            let entries = std::fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    directories.push(path);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "html")
                {
                    sources.push(path);
                }
            }
        }
        sources
    }
}
