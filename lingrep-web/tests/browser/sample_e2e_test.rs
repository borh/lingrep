#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use clap::Parser;
use lingrep_core::token_match::TokenInfo;
use serde_json::json;
use thirtyfour::prelude::*;
use tokio::net::TcpListener;

use crate::frequency_e2e_test::{driver, observed, wait_for};

#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../suite/v6_support.rs"]
mod support;

const SEED: &str = "seed := [upos=NOUN].\n";

const GATEWAY_PAGE: &str =
    "<html><head><title>503</title></head><body><h1>Service Unavailable</h1></body></html>";

struct Served {
    base: String,
    gateway_down: Arc<AtomicBool>,
    server: tokio::task::JoinHandle<()>,
    _artifact: support::ArtifactFixture,
    _dictionary: lingrep_core::testing::MiniDictionary,
}

async fn answer_as_gateway(down: Arc<AtomicBool>, request: Request, next: Next) -> Response {
    if down.load(Ordering::SeqCst) && request.uri().path().ends_with("/api/sample") {
        return (StatusCode::SERVICE_UNAVAILABLE, Html(GATEWAY_PAGE)).into_response();
    }
    next.run(request).await
}

async fn serve() -> Served {
    let artifact = support::build_artifact(vec![(
        "doc.md",
        0,
        false,
        vec![vec![TokenInfo::default().with_orth("x")]],
    )]);
    let seed = artifact.directory.path().join("seed.lx");
    std::fs::write(&seed, SEED).unwrap();
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let argv: Vec<&std::ffi::OsStr> = vec![
        "lingrep-web".as_ref(),
        "--dict-path".as_ref(),
        dictionary.path().as_os_str(),
        "--input".as_ref(),
        artifact.path.as_os_str(),
        "--patterns".as_ref(),
        seed.as_os_str(),
        "--base-path".as_ref(),
        "/sub".as_ref(),
    ];
    let args = lingrep_web::Args::try_parse_from(argv).unwrap();
    let config = lingrep_web::serve::config(&args).unwrap();
    let store = lingrep_web::serve::store(&args, config).await.unwrap();
    let gateway_down = Arc::new(AtomicBool::new(false));
    let down = Arc::clone(&gateway_down);
    let app = lingrep_web::app_router_at(store, &args.base_path).layer(axum::middleware::from_fn(
        move |request, next| answer_as_gateway(Arc::clone(&down), request, next),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/sub/sample", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Served {
        base,
        gateway_down,
        server,
        _artifact: artifact,
        _dictionary: dictionary,
    }
}

const RECORD_RUNS: &str = "window.__runs = []; \
    document.addEventListener('htmx:configRequest', (event) => { \
      if (event.detail.path.includes('api/sample')) __runs.push(event.detail.formData.get('pattern')); });";

const RESULTS: &str = "return !!document.querySelector('#sample-results > .sample-results')";

const CHECKED: &str = "const status = document.querySelector('#sample-pattern-status')?.textContent; \
    return !!status && status !== 'Checking…'";

const STATUS: &str = "return document.querySelector('[data-sample-status]').textContent";

#[tokio::test]
async fn the_sample_page_posts_one_run_on_arrival() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": RECORD_RUNS}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        driver.goto(&served.base).await?;
        wait_for(&driver, RESULTS, "results for the served pattern").await?;
        wait_for(&driver, CHECKED, "the editor's first check").await?;
        observed(
            &driver,
            "return __runs",
            json!([SEED]),
            "runs on arrival with the editor",
        )
        .await?;

        let draft = "draft := [upos=VERB].\n";
        driver
            .execute(
                "const pattern = document.querySelector('#sample-pattern'); pattern.value = arguments[0]; \
                 pattern.dispatchEvent(new Event('input', {bubbles: true}));",
                vec![json!(draft)],
            )
            .await?;
        wait_for(&driver, "return __runs.length === 2", "a run for the edited pattern").await?;
        driver.refresh().await?;
        wait_for(&driver, RESULTS, "results for the restored draft").await?;
        wait_for(&driver, CHECKED, "the editor's first check of the draft").await?;
        observed(
            &driver,
            "return __runs",
            json!([draft]),
            "runs on a reload that restores a draft",
        )
        .await?;

        driver.cdp().send_raw("Network.enable", json!({})).await?;
        driver
            .cdp()
            .send_raw(
                "Network.setBlockedURLs",
                json!({"urls": ["*/editor/vendor/codemirror.js"]}),
            )
            .await?;
        driver.refresh().await?;
        wait_for(
            &driver,
            "return document.querySelector('#sample-pattern').dataset.lxEnhancement === 'unavailable'",
            "the editor to fail to load",
        )
        .await?;
        wait_for(&driver, RESULTS, "results without the editor").await?;
        observed(
            &driver,
            "return __runs",
            json!([draft]),
            "runs on arrival without the editor",
        )
        .await?;

        let edited = "edited := [upos=ADJ].\n";
        driver
            .execute(
                "const pattern = document.querySelector('#sample-pattern'); pattern.value = arguments[0]; \
                 pattern.dispatchEvent(new Event('input', {bubbles: true}));",
                vec![json!(edited)],
            )
            .await?;
        wait_for(
            &driver,
            "return __runs.length === 2",
            "a run for a pattern edited without the editor",
        )
        .await?;
        observed(
            &driver,
            "return __runs",
            json!([draft, edited]),
            "runs after an edit without the editor",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_failed_sample_run_shows_the_servers_reason_but_not_a_gateway_page() -> WebDriverResult<()>
{
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let run = "document.querySelector('.sample-form button[type=submit]').click()";
    let result: WebDriverResult<()> = async {
        driver.goto(&served.base).await?;
        wait_for(&driver, RESULTS, "results for the served pattern").await?;
        driver
            .execute(
                "document.querySelector('#sample-text').value = 'あ'.repeat(30000);",
                Vec::new(),
            )
            .await?;
        driver.execute(run, Vec::new()).await?;
        wait_for(
            &driver,
            "return document.querySelector('#sample-results > .error[role=status]')?.textContent \
               === 'Sample too large to test — scan the corpus instead.'",
            "the server's reason for rejecting the sample",
        )
        .await?;
        observed(
            &driver,
            STATUS,
            json!(""),
            "the status line beside the reason",
        )
        .await?;

        served.gateway_down.store(true, Ordering::SeqCst);
        driver.execute(run, Vec::new()).await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-sample-status]').textContent \
               === 'Could not run this sample; previous results are still shown.'",
            "the failed run in the status line",
        )
        .await?;
        observed(
            &driver,
            "return document.querySelector('#sample-results').textContent.trim()",
            json!("Sample too large to test — scan the corpus instead."),
            "the results after a gateway's error page",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_reload_shows_the_draft_and_a_followed_link_shows_its_pattern() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let index = served.base.trim_end_matches("sample").to_owned();
    let link = |pattern: &str| {
        let query = lingrep_web::permalink::encode_query_only(pattern, "default", None, None);
        format!("{index}{}", query.unwrap())
    };
    let shown = "return document.querySelector('.pattern-editor .cm-content') \
        && document.querySelector('#corpus-pattern').value";
    let draft = "draft := [upos=VERB].\n";
    let result: WebDriverResult<()> = async {
        driver.goto(&index).await?;
        wait_for(&driver, shown, "the editor on Corpus Search").await?;
        driver
            .execute(
                "const pattern = document.querySelector('#corpus-pattern'); pattern.value = arguments[0]; \
                 pattern.dispatchEvent(new Event('input', {bubbles: true}));",
                vec![json!(draft)],
            )
            .await?;
        driver.refresh().await?;
        wait_for(&driver, shown, "the editor after a reload").await?;
        observed(&driver, shown, json!(draft), "the pattern after a reload").await?;

        for (pattern, what) in [
            (SEED, "a link to the pattern the draft was edited from"),
            ("linked := [upos=ADJ].\n", "a link to another pattern"),
        ] {
            driver.goto(link(pattern)).await?;
            wait_for(&driver, shown, what).await?;
            observed(&driver, shown, json!(pattern), what).await?;
        }

        driver.goto(&index).await?;
        wait_for(&driver, shown, "the editor on a later visit").await?;
        observed(
            &driver,
            shown,
            json!(draft),
            "the pattern on a later visit without a link",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_overlapping_matches_option_changes_which_patterns_are_counted() -> WebDriverResult<()>
{
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let index = served.base.trim_end_matches("sample").to_owned();
    let counts = "const rows = [...document.querySelectorAll('#pattern-table tbody tr')] \
        .map((row) => [row.querySelector('.pattern-selector').textContent.trim(), Number(row.dataset.exactTotal)]) \
        .filter(([name]) => name === 'kept' || name === 'lost'); \
        return JSON.stringify(Object.fromEntries(rows))";
    let result: WebDriverResult<()> = async {
        driver.goto(&index).await?;
        wait_for(
            &driver,
            "return !!document.querySelector('.pattern-editor .cm-content')",
            "the editor on Corpus Search",
        )
        .await?;
        for (overlaps, expected) in [
            ("longest", r#"{"kept":1,"lost":0}"#),
            ("catalog", r#"{"kept":1,"lost":1}"#),
        ] {
            driver
                .execute(
                    "const pattern = document.querySelector('#corpus-pattern'); \
                     pattern.value = 'kept := [text=x].\\nlost := [].\\n'; \
                     pattern.dispatchEvent(new Event('input', {bubbles: true})); \
                     const select = document.querySelector('select[name=overlaps]'); \
                     select.value = arguments[0]; \
                     select.dispatchEvent(new Event('change', {bubbles: true})); \
                     document.querySelector('[data-corpus-search]').requestSubmit();",
                    vec![json!(overlaps)],
                )
                .await?;
            wait_for(
                &driver,
                &format!("{counts} === '{expected}'"),
                &format!("the counts under {overlaps}: {expected}"),
            )
            .await?;
        }
        Ok(())
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

const JAPANESE: &str = "try { localStorage.setItem('lingrep-web:locale:v1', 'ja'); } catch {}";

#[tokio::test]
async fn japanese_is_applied_to_stream_swaps_replaced_elements_and_attribute_slots()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let index = served.base.trim_end_matches("sample").to_owned();
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": JAPANESE}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        driver.goto(&index).await?;
        wait_for(
            &driver,
            "return document.documentElement.lang === 'ja'",
            "the Japanese locale",
        )
        .await?;
        observed(
            &driver,
            "return document.querySelector('.compound-control-group').title",
            json!("隣接する一致の連鎖から、複合項目を生成します。"),
            "a title on an element without a text slot",
        )
        .await?;

        wait_for(
            &driver,
            "return document.querySelector('#corpus-pattern-status')?.textContent.startsWith('パターンは有効です。')",
            "the editor's check result in Japanese",
        )
        .await?;

        driver
            .execute(
                "document.querySelector('[data-corpus-search]').requestSubmit()",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return !!document.querySelector('#results .results-explorer')",
            "the results of the search",
        )
        .await?;
        observed(
            &driver,
            "return document.querySelector('#search-status .revision-status p').textContent",
            json!("結果は最新です。"),
            "the status swapped in by the event stream",
        )
        .await?;
        observed(
            &driver,
            "return document.querySelector('#results .results-explorer').getAttribute('aria-label')",
            json!("コーパス検索結果"),
            "the label of the results swapped in out of band",
        )
        .await?;

        driver
            .execute(
                "const pattern = document.querySelector('#corpus-pattern'); pattern.value = 'p := [upos=VERB'; \
                 pattern.dispatchEvent(new Event('input', {bubbles: true})); \
                 document.querySelector('[data-corpus-search]').requestSubmit();",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return !!document.querySelector('#search-status .revision-status span[lang=en]')",
            "the status of a pattern that does not compile",
        )
        .await?;
        observed(
            &driver,
            "const status = document.querySelector('#search-status .revision-status p'); \
             return [status.firstElementChild.textContent, status.lastElementChild.textContent.length > 0]",
            json!(["パターンをコンパイルできませんでした：", true]),
            "the Japanese sentence before the compiler's English diagnostic",
        )
        .await?;
        wait_for(
            &driver,
            "return document.querySelector('#search-stream').dataset.streamFinished === 'true'",
            "the stream to count the failure status as its end",
        )
        .await?;
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        assert_eq!(
            driver
                .execute("return document.querySelector('.stream-lost') === null", vec![])
                .await?
                .json(),
            &json!(true),
            "a failed search does not report a lost stream"
        );

        driver
            .execute(
                "window.__context = document.querySelector('.corpus-context'); \
                 htmx.ajax('GET', 'api/corpus-context?k=default', {target: '.corpus-context', swap: 'outerHTML'});",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return !__context.isConnected && !!document.querySelector('.corpus-context')",
            "the corpus context to be replaced",
        )
        .await?;
        observed(
            &driver,
            "const context = document.querySelector('.corpus-context'); \
             return [context.getAttribute('aria-label'), context.querySelector('[data-i18n=\"corpus.label\"]').textContent]",
            json!(["コーパス情報", "コーパス："]),
            "the corpus context that replaced the served one",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}
