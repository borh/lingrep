use axum::{
    Router,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde_json::Value;

use crate::state::RequestStore;

const INDEX: &str = include_str!("../question-ui/index.html");

pub(crate) fn routes() -> Router<RequestStore> {
    Router::new().route("/{asset}", get(asset))
}

pub(crate) fn page(seed: Value, frequency_view: bool) -> Html<String> {
    let json = seed
        .to_string()
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    Html(
        INDEX
            .replace(
                "<template id=\"site-routes\"></template>",
                &site_routes("question", frequency_view),
            )
            .replace(
                "</body>",
                &format!(
                    "<script id=\"question-seed\" type=\"application/json\">{json}</script></body>"
                ),
            ),
    )
}

/// The header's route links, shared by the question page and the
/// Frequencies and Compare pages, with `current` (`question`, `frequencies`
/// or `compare`) marked as the current page. Frequencies and Compare are
/// offered only when a count table is served.
pub(crate) fn site_routes(current: &str, frequency_view: bool) -> String {
    let link = |href: &str, page: &str, question_key: &str, key: &str, label: &str| {
        let marked = if page == current {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(r#"<a href="{href}"{marked} data-t="{question_key}" data-i18n="{key}">{label}</a>"#)
    };
    let mut routes = String::from(
        r#"<nav class="routes" aria-label="lingrep"><a class="brand" href="./">lingrep</a>"#,
    );
    routes += &link("./", "question", "questionNav", "nav.question", "Question");
    if frequency_view {
        routes += &link(
            "frequencies",
            "frequencies",
            "frequencyNav",
            "nav.frequencies",
            "Frequencies",
        );
        routes += &link("compare", "compare", "compareNav", "nav.compare", "Compare");
    }
    routes += r#"<a href="help" target="_blank" rel="noopener" data-t="helpNav" data-i18n="nav.help">Help</a></nav>"#;
    routes
}

async fn asset(Path(name): Path<String>, headers: HeaderMap) -> Response {
    let (content_type, contents) = match name.as_str() {
        "site.css" => (
            "text/css; charset=utf-8",
            include_str!("../question-ui/site.css"),
        ),
        "style.css" => (
            "text/css; charset=utf-8",
            include_str!("../question-ui/style.css"),
        ),
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/app.js"),
        ),
        "composer.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/composer.js"),
        ),
        "dependency.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/dependency.js"),
        ),
        "document.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/document.js"),
        ),
        "location.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/location.js"),
        ),
        "model.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/model.js"),
        ),
        "sections.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/sections.js"),
        ),
        "service.js" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/service.js"),
        ),
        "state.mjs" => (
            "text/javascript; charset=utf-8",
            include_str!("../question-ui/state.mjs"),
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    crate::handlers::asset::static_asset(&headers, contents.as_bytes(), content_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[test]
    fn seed_is_inert_json_and_round_trips_script_delimiters() {
        let source = "p := [text=\"</script><img src=x onerror=alert(1)> & \"];.\u{2028}\u{2029}";
        let expected = serde_json::json!({"source":source,"run":"0"});
        let html = page(expected.clone(), false).0;
        assert!(!html.contains("<base"));
        assert!(!html.contains("</script><img"));
        let json = html
            .split("id=\"question-seed\" type=\"application/json\">")
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(json).unwrap(), expected);
        assert!(!json.contains(['<', '>', '&', '\u{2028}', '\u{2029}']));
        assert!(html.contains("src=\"assets/questions/app.js\""));
    }

    #[test]
    fn the_header_routes_to_frequencies_and_compare_only_when_a_count_table_is_served() {
        let header = |frequency_view| {
            let html = page(serde_json::json!({}), frequency_view).0;
            let (_, routes) = html.split_once("<nav class=\"routes\"").unwrap();
            routes.split_once("</nav>").unwrap().0.to_owned()
        };
        let served = header(true);
        assert!(served.contains(r#"<a href="frequencies" data-t="frequencyNav""#));
        assert!(served.contains(r#"<a href="compare" data-t="compareNav""#));
        assert!(served.contains(r#"<a href="help""#));
        let plain = header(false);
        assert!(!plain.contains("frequencies"));
        assert!(plain.contains(r#"<a href="help""#));
    }

    #[tokio::test]
    async fn root_assets_workbench_and_security_survive_a_nested_mount() {
        let app = crate::app_router_at(
            RequestStore::default(),
            &crate::BasePath::parse("/research/tools").unwrap(),
        );
        for path in [
            "/research/tools/",
            "/research/tools/workbench",
            "/research/tools/assets/questions/style.css",
            "/research/tools/assets/questions/site.css",
            "/research/tools/assets/questions/app.js",
            "/research/tools/assets/questions/service.js",
            "/research/tools/assets/questions/dependency.js",
            "/research/tools/assets/questions/state.mjs",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("base-uri 'none'")
            );
            let body = String::from_utf8(
                response
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .to_vec(),
            )
            .unwrap();
            if path == "/research/tools/" {
                assert!(body.contains("id=\"question-seed\""));
                assert!(!body.contains("<base"));
            }
            if path.ends_with("workbench") {
                assert!(body.contains("data-query-tab"));
                assert!(!body.contains("id=\"question-seed\""));
            }
        }
        for path in [
            "/assets/questions/app.js",
            "/research/tools/assets/questions/private.rs",
            "/research/tools/assets/questions/index.html",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }
}
