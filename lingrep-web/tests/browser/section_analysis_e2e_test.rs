#![allow(clippy::result_large_err)]
#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use std::{
    sync::Arc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    response::Html,
    routing::{get, post},
};
use thirtyfour::prelude::*;
use tokio::{net::TcpListener, task::JoinHandle};

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<script src="/assets/htmx.min.js"></script></head><body>
<div id="corpus-context">{SCOPE}</div>
<button hx-get="/poll" hx-target="#corpus-context" hx-swap="outerHTML">Poll corpus</button>
<main>{ANALYSIS}</main>
</body></html>"##;

#[derive(Clone)]
struct RequestCount(Arc<AtomicUsize>);

struct FixtureServer {
    address: String,
    task: JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn scope_fragment(sections: &[String], request_count: usize) -> String {
    let checkbox = |section: &str| {
        let checked = sections.iter().any(|selected| selected == section);
        format!(
            "<label><input type=\"checkbox\" name=\"section\" value=\"{section}\"{}>{section}</label>",
            if checked { " checked" } else { "" }
        )
    };
    format!(
        "<div id=\"corpus-analysis-scope\" class=\"corpus-context-sections\" data-request-count=\"{request_count}\"><details open><summary>Analysis sections</summary><form hx-post=\"/section-analysis\" hx-target=\"#section-analysis\" hx-swap=\"outerHTML\"><input type=\"hidden\" name=\"submitted\" value=\"1\">{}{}<button type=\"submit\">Apply</button></form></details></div>",
        checkbox("methods"),
        checkbox("results"),
    )
}

fn analysis_fragment(sections: &[String], distance: &str, request_count: usize) -> String {
    let hidden_sections = sections
        .iter()
        .map(|section| format!("<input type=\"hidden\" name=\"section\" value=\"{section}\">"))
        .collect::<String>();
    format!(
        "<div id=\"section-analysis\" data-sections=\"{}\" data-distance=\"{distance}\" data-request-count=\"{request_count}\"><section data-detail-view=\"clusters\"><form hx-post=\"/section-analysis\" hx-trigger=\"change\" hx-target=\"#section-analysis\" hx-swap=\"outerHTML\"><input type=\"hidden\" name=\"submitted\" value=\"1\">{hidden_sections}<select name=\"distance\"><option value=\"jensen-shannon\"{}>Jensen–Shannon</option><option value=\"cosine\"{}>Cosine</option></select></form></section></div>",
        sections.join(","),
        if distance == "jensen-shannon" {
            " selected"
        } else {
            ""
        },
        if distance == "cosine" {
            " selected"
        } else {
            ""
        },
    )
}

async fn index() -> Html<String> {
    let selected = ["methods".to_owned()];
    let initial = analysis_fragment(&selected, "jensen-shannon", 0);
    let scope = scope_fragment(&selected, 0);
    Html(
        PAGE.replace("{ANALYSIS}", &initial)
            .replace("{SCOPE}", &scope),
    )
}

async fn poll_context() -> Html<&'static str> {
    Html(
        "<div id=\"corpus-context\"><div id=\"corpus-analysis-scope\" class=\"corpus-context-sections\" hx-preserve></div></div>",
    )
}

async fn section_analysis(
    State(RequestCount(count)): State<RequestCount>,
    body: Bytes,
) -> Html<String> {
    let request_count = count.fetch_add(1, Ordering::SeqCst) + 1;
    let fields = String::from_utf8(body.to_vec()).expect("form body is UTF-8");
    let fields = fields
        .split('&')
        .filter_map(|field| field.split_once('='))
        .collect::<Vec<_>>();
    assert!(fields.contains(&("submitted", "1")), "{fields:?}");
    let sections = fields
        .iter()
        .filter_map(|(name, value)| (*name == "section").then_some((*value).to_owned()))
        .collect::<Vec<_>>();
    let distance = fields
        .iter()
        .find_map(|(name, value)| (*name == "distance").then_some(*value))
        .unwrap_or("jensen-shannon");
    let analysis = analysis_fragment(&sections, distance, request_count);
    let scope = scope_fragment(&sections, request_count).replace(
        "id=\"corpus-analysis-scope\"",
        "id=\"corpus-analysis-scope\" hx-swap-oob=\"outerHTML\"",
    );
    Html(format!("{analysis}{scope}"))
}

async fn fixture_server() -> FixtureServer {
    let count = RequestCount(Arc::new(AtomicUsize::new(0)));
    let app = Router::new()
        .route("/", get(index))
        .route("/poll", get(poll_context))
        .route("/section-analysis", post(section_analysis))
        .route(
            "/assets/htmx.min.js",
            get(|| async {
                (
                    [("content-type", "text/javascript; charset=utf-8")],
                    include_str!("../../assets/vendor/htmx.min.js"),
                )
            }),
        )
        .with_state(count);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FixtureServer { address, task }
}

async fn analysis_state(driver: &WebDriver) -> WebDriverResult<(String, String, String)> {
    driver
        .execute(
            "const root=document.querySelector('#section-analysis');return [root.dataset.sections,root.dataset.distance,root.dataset.requestCount];",
            vec![],
        )
        .await?
        .convert()
}

async fn chromedriver_available(url: &str) -> bool {
    let address = url.strip_prefix("http://").unwrap_or(url);
    tokio::net::TcpStream::connect(address).await.is_ok()
}

#[tokio::test]
async fn corpus_scope_and_clustering_distance_refresh_their_shared_analysis() -> WebDriverResult<()>
{
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !chromedriver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }

    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.goto(&fixture.address).await?;

    driver
        .find(By::Css("#corpus-analysis-scope input[value='results']"))
        .await?
        .click()
        .await?;
    driver
        .find(By::Css("#corpus-analysis-scope button[type=submit]"))
        .await?
        .click()
        .await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        analysis_state(&driver).await?,
        (
            "methods,results".into(),
            "jensen-shannon".into(),
            "1".into()
        )
    );
    assert_eq!(
        driver
            .find(By::Css("#corpus-analysis-scope"))
            .await?
            .attr("data-request-count")
            .await?,
        Some("1".into())
    );
    driver
        .find(By::Css("button[hx-get='/poll']"))
        .await?
        .click()
        .await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        driver
            .execute(
                "return [...document.querySelectorAll('#corpus-analysis-scope input:checked')].map(input=>input.value).join(',');",
                vec![],
            )
            .await?
            .convert::<String>()?,
        "methods,results"
    );

    driver
        .find(By::Css("#corpus-analysis-scope input[value='methods']"))
        .await?
        .click()
        .await?;
    driver
        .find(By::Css("#corpus-analysis-scope button[type=submit]"))
        .await?
        .click()
        .await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        analysis_state(&driver).await?,
        ("results".into(), "jensen-shannon".into(), "2".into())
    );

    driver
        .execute(
            "const select=document.querySelector('select[name=distance]');select.value='cosine';select.dispatchEvent(new Event('change',{bubbles:true}));",
            vec![],
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        analysis_state(&driver).await?,
        ("results".into(), "cosine".into(), "3".into())
    );
    driver.quit().await?;
    Ok(())
}
