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
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, extract::State, http::header, response::Html, routing::get};
use thirtyfour::prelude::*;
use tokio::net::TcpListener;

const PREPARING: &str = r#"<section class="corpus-context" aria-label="Corpus context" hx-get="/api/corpus-context" hx-trigger="load delay:0s, every 0.2s" hx-swap="outerHTML"><p class="corpus-context-note">Corpus is preparing…</p></section>"#;
const READY: &str = r#"<section class="corpus-context" aria-label="Corpus context"><p class="corpus-context-counts">3 sentences · 1 documents</p><p class="corpus-context-ready-marker">ready</p></section>"#;

// Preparing for the first two polls, then ready (whose body carries no hx-trigger).
async fn partial(State(calls): State<Arc<AtomicUsize>>) -> impl axum::response::IntoResponse {
    let n = calls.fetch_add(1, Ordering::SeqCst);
    let body = if n < 2 { PREPARING } else { READY };
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        ],
        body,
    )
}

async fn fixture() -> String {
    let calls = Arc::new(AtomicUsize::new(0));
    let page = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><script src="/assets/vendor/htmx.min.js"></script></head><body>{PREPARING}</body></html>"#
    );
    let app = Router::new()
        .route(
            "/",
            get(move || {
                let page = page.clone();
                async move { Html(page) }
            }),
        )
        .route("/api/corpus-context", get(partial))
        .route(
            "/assets/vendor/htmx.min.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/vendor/htmx.min.js"),
                )
            }),
        )
        .with_state(calls);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

async fn driver_available(url: &str) -> bool {
    let address = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    tokio::net::TcpStream::connect(address).await.is_ok()
}

#[tokio::test]
async fn preparing_strip_swaps_to_ready_and_stops_polling() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".to_string());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        return Ok(());
    }
    let address = fixture().await;
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new")?;
    let driver = WebDriver::new(&driver_url, caps).await?;

    driver.goto(&address).await?;

    // htmx polls the endpoint; after it returns ready, the section is swapped.
    let mut swapped = false;
    for _ in 0..50 {
        if driver
            .find(By::Css(".corpus-context-ready-marker"))
            .await
            .is_ok()
        {
            swapped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Race-free "stops polling" proof: the swapped-in LIVE section carries no
    // hx-get, so htmx cannot poll again — no navigation, no counter race.
    let post_swap_hx_get = if swapped {
        driver
            .find(By::Css(".corpus-context"))
            .await?
            .attr("hx-get")
            .await?
    } else {
        None
    };

    driver.quit().await?;
    assert!(
        swapped,
        "preparing strip must swap to ready in a real browser"
    );
    assert!(
        post_swap_hx_get.is_none(),
        "ready strip must carry no polling trigger"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Corpus selector: switching corpora navigates to a freshly minted permalink.
// ---------------------------------------------------------------------------

/// The two corpora the selector fixture declares, keyed as the server keys them
/// (`--input` occupies the reserved `default`).
const CORPORA: [(&str, &str); 2] = [("default", "everyday-corpus"), ("sci", "science-corpus")];

fn display_name(key: &str) -> &'static str {
    CORPORA
        .iter()
        .find(|(name, _)| *name == key)
        .map_or("corpus", |(_, display)| *display)
}

/// The REAL corpus-context partial, so this test observes the DOM contract the
/// production template emits rather than a hand-copied imitation of it.
fn render_section(view: lingrep_web::corpus_context::CorpusContextView, active: &str) -> String {
    use askama::Template;
    use lingrep_web::templates::{CorpusContextPartial, CorpusOption};

    CorpusContextPartial {
        corpus_context: view,
        corpora: CORPORA
            .iter()
            .map(|(key, display)| CorpusOption {
                key: (*key).to_owned(),
                display_name: (*display).to_owned(),
                active: *key == active,
            })
            .collect(),
    }
    .render()
    .unwrap()
}

/// A ready corpus: terminal, so the section carries no polling trigger.
fn context_section(active: &str) -> String {
    use lingrep_web::corpus_context::CorpusContextView;

    let digest = if active == "sci" {
        "bbbbbbbbbbbb"
    } else {
        "aaaaaaaaaaaa"
    };
    let view = CorpusContextView {
        corpus_key: active.to_owned(),
        ..CorpusContextView::ready(
            display_name(active).to_owned(),
            digest.to_owned(),
            3,
            1,
            vec![],
        )
    };
    render_section(view, active)
}

/// A preparing corpus: the section polls itself and is swapped whole. `processed`
/// is the visible marker that a swap actually happened.
fn preparing_section(active: &str, processed: Option<u64>) -> String {
    use lingrep_web::corpus_context::CorpusContextView;

    let view = CorpusContextView {
        corpus_key: active.to_owned(),
        ..CorpusContextView::preparing(display_name(active).to_owned(), processed)
    };
    render_section(view, active)
}

/// The landing page for `active`, seeded with `pattern`. `ran` marks the form
/// `data-autorun` exactly as the server does for a link that may run, which is
/// what makes "the results re-ran against the new corpus" observable.
fn selector_page(active: &str, pattern: &str, ran: bool) -> String {
    let section = context_section(active);
    let autorun = if ran { " data-autorun" } else { "" };
    format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<script src="/assets/vendor/htmx.min.js"></script></head><body>
{section}
<p class="corpus-select-status muted" role="status" data-corpus-select-status></p>
<form hx-post="/api/search?k={active}" hx-target="#results" hx-swap="innerHTML"
      data-corpus-search{autorun}>
  <textarea id="corpus-pattern" name="pattern">{pattern}</textarea>
  <button type="submit">Search</button>
</form>
<div id="results"></div>
<script type="module" src="/assets/app.js"></script>
</body></html>"##
    )
}

/// `k` and `q` off the landing target, without pulling in a query parser: the
/// fixture emits them itself via `permalink::encode`.
fn wire_param(raw: Option<&str>, key: &str) -> Option<String> {
    raw?.split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_owned())
}

async fn selector_fixture() -> String {
    use axum::extract::{Form, Query, RawQuery};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use lingrep_web::RequestStore;
    use std::collections::HashMap;

    let landing = |RawQuery(raw): RawQuery| async move {
        let active = wire_param(raw.as_deref(), "k").unwrap_or_else(|| "default".to_owned());
        let seeded = wire_param(raw.as_deref(), "q")
            .and_then(|query| lingrep_web::permalink::decode(&query).ok());
        let ran = seeded.is_some();
        let pattern = seeded.unwrap_or_else(|| "verb := [upos=VERB].".to_owned());
        Html(selector_page(&active, &pattern, ran))
    };

    // Mirrors the real endpoint's answers, including the shapes the switch must
    // refuse: an undeclared key is 404 `unknown_corpus`, never a path.
    let permalink = |Form(form): Form<HashMap<String, String>>| async move {
        let pattern = form.get("pattern").cloned().unwrap_or_default();
        let corpus = form.get("corpus").cloned().unwrap_or_default();
        if !CORPORA.iter().any(|(key, _)| *key == corpus) {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": "unknown_corpus"})),
            )
                .into_response();
        }
        // The codec's own query-only form, which is what the real endpoint
        // answers with: hand-writing the shape here is what let this fixture
        // drift behind the subpath-mount cutover.
        match lingrep_web::permalink::encode_query_only(&pattern, &corpus, None) {
            Ok(path) => axum::Json(serde_json::json!({ "path": path })).into_response(),
            Err(_) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                axum::Json(serde_json::json!({"error": "too_large"})),
            )
                .into_response(),
        }
    };

    let search = |Query(params): Query<HashMap<String, String>>| async move {
        let key = params.get("k").cloned().unwrap_or_default();
        Html(format!(
            r#"<p class="results-marker">results from {key}</p>"#
        ))
    };

    let app = Router::<RequestStore>::new()
        .route("/", get(landing))
        .route("/api/permalink", post(permalink))
        .route("/api/search", post(search))
        .nest("/assets", lingrep_web::handlers::results_assets::routes())
        .with_state(RequestStore::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

async fn wait_for<F>(driver: &WebDriver, mut ready: F) -> bool
where
    F: AsyncFnMut(&WebDriver) -> bool,
{
    for _ in 0..100 {
        if ready(driver).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn selecting_a_corpus_navigates_to_it_and_reruns_the_query() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".to_string());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        return Ok(());
    }
    let address = selector_fixture().await;
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new")?;
    let driver = WebDriver::new(&driver_url, caps).await?;

    let outcome = async {
        driver.goto(&address).await?;
        // The landing is pointed at the default corpus.
        let panel = driver
            .find(By::Css(".corpus-context-name"))
            .await?
            .text()
            .await?;
        assert!(panel.contains("everyday-corpus"), "landing panel: {panel}");

        // Edit the query, so the switch is proved to carry the CURRENT editor
        // contents rather than whatever the previous page was seeded with.
        let editor = driver.find(By::Css("#corpus-pattern")).await?;
        editor.clear().await?;
        editor.send_keys("switched := [upos=NOUN].").await?;

        let select = driver.find(By::Css("[data-corpus-select]")).await?;
        thirtyfour::components::SelectElement::new(&select)
            .await?
            .select_by_value("sci")
            .await?;

        let navigated = wait_for(&driver, async |driver| {
            driver
                .current_url()
                .await
                .is_ok_and(|url| url.as_str().contains("k=sci"))
        })
        .await;
        let url = driver.current_url().await?.to_string();
        assert!(navigated, "selecting a corpus must navigate: {url}");

        let panel = driver
            .find(By::Css(".corpus-context-name"))
            .await?
            .text()
            .await?;
        assert!(
            panel.contains("science-corpus"),
            "the panel must describe the newly selected corpus: {panel}"
        );
        let selected = driver
            .find(By::Css("[data-corpus-select] option[value='sci']"))
            .await?
            .prop("selected")
            .await?;
        assert_eq!(
            selected.as_deref(),
            Some("true"),
            "the new corpus stays selected"
        );

        // The edited pattern travelled in the link, and re-ran against `sci`.
        let carried = driver
            .find(By::Css("#corpus-pattern"))
            .await?
            .prop("value")
            .await?;
        assert_eq!(carried.as_deref(), Some("switched := [upos=NOUN]."));
        let rerun = wait_for(&driver, async |driver| {
            driver.find(By::Css(".results-marker")).await.is_ok()
        })
        .await;
        assert!(rerun, "the query must re-run against the selected corpus");
        let results = driver
            .find(By::Css(".results-marker"))
            .await?
            .text()
            .await?;
        assert_eq!(results, "results from sci");
        Ok::<(), WebDriverError>(())
    }
    .await;

    driver.quit().await?;
    outcome
}

/// A refused switch must still be readable after the corpus-context poll fires.
///
/// The live region sits outside the polled section for exactly this reason: a
/// preparing corpus polls every few seconds, and that is precisely the state in
/// which a researcher is most likely to try switching away.
async fn refusing_fixture() -> String {
    use axum::extract::Form;
    use axum::http::{StatusCode, header};
    use axum::routing::post;
    use lingrep_web::RequestStore;
    use std::collections::HashMap;

    let page = format!(
        r##"<!doctype html><html lang="en"><head><meta charset="utf-8">
<script src="/assets/vendor/htmx.min.js"></script></head><body>
{section}
<p class="corpus-select-status muted" role="status" data-corpus-select-status></p>
<textarea id="corpus-pattern" name="pattern">verb := [upos=VERB].</textarea>
<script type="module" src="/assets/app.js"></script>
</body></html>"##,
        section = preparing_section("default", None)
    );

    // Every mint is refused, which is the 404 `unknown_corpus` shape.
    let permalink = |Form(_form): Form<HashMap<String, String>>| async move {
        (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": "unknown_corpus"})),
        )
    };
    // The poll's answer differs visibly from the served section: "7 records read"
    // appears only after a swap really happened.
    let polled = || async {
        (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            ],
            preparing_section("default", Some(7)),
        )
    };

    let app = Router::<RequestStore>::new()
        .route(
            "/",
            get(move || {
                let page = page.clone();
                async move { Html(page) }
            }),
        )
        .route("/api/corpus-context", get(polled))
        .route("/api/permalink", post(permalink))
        .nest("/assets", lingrep_web::handlers::results_assets::routes())
        .with_state(RequestStore::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

#[tokio::test]
async fn a_refused_switch_reports_and_the_message_survives_a_poll_swap() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".to_string());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        return Ok(());
    }
    let address = refusing_fixture().await;
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new")?;
    let driver = WebDriver::new(&driver_url, caps).await?;

    let outcome = async {
        driver.goto(&address).await?;
        let select = driver.find(By::Css("[data-corpus-select]")).await?;
        thirtyfour::components::SelectElement::new(&select)
            .await?
            .select_by_value("sci")
            .await?;

        let reported = wait_for(&driver, async |driver| {
            status_text(driver).await.contains("not available")
        })
        .await;
        assert!(
            reported,
            "a refused switch must say so: {:?}",
            status_text(&driver).await
        );
        // The control is restored, so it never claims to be on a corpus the page
        // failed to switch to.
        let selected = driver
            .find(By::Css("[data-corpus-select] option[value='default']"))
            .await?
            .prop("selected")
            .await?;
        assert_eq!(selected.as_deref(), Some("true"));

        // The polled section is swapped whole; the message must outlive it.
        let swapped = wait_for(&driver, async |driver| {
            match driver.find(By::Css(".corpus-context-note")).await {
                Ok(note) => note
                    .text()
                    .await
                    .is_ok_and(|text| text.contains("records read")),
                Err(_) => false,
            }
        })
        .await;
        let note = driver
            .find(By::Css(".corpus-context-note"))
            .await?
            .text()
            .await?;
        assert!(swapped, "the corpus-context poll must have swapped: {note}");
        assert!(
            status_text(&driver).await.contains("not available"),
            "the reported failure must survive the swap"
        );
        Ok::<(), WebDriverError>(())
    }
    .await;

    driver.quit().await?;
    outcome
}

/// The live region's current text, or the empty string when it is absent.
async fn status_text(driver: &WebDriver) -> String {
    match driver.find(By::Css("[data-corpus-select-status]")).await {
        Ok(element) => element.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    }
}
