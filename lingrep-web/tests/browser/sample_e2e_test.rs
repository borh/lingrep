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
use futures::FutureExt as _;
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
    store: lingrep_web::state::RequestStore,
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
    serve_sectioned(false).await
}

async fn serve_sectioned(sectioned: bool) -> Served {
    let artifact = if sectioned {
        let words = ["HIT", "HIT", "OTHER", "rare", "OTHER", "HIT", "HIT"];
        support::build_artifact_with_sections(
            vec![(
                "doc.md",
                0,
                false,
                words
                    .iter()
                    .map(|word| vec![TokenInfo::default().with_orth(*word)])
                    .collect(),
            )],
            (0..words.len())
                .map(|i| {
                    vec![
                        if i % 2 == 0 {
                            "introduction"
                        } else {
                            "methods"
                        }
                        .into(),
                    ]
                })
                .collect(),
        )
    } else {
        support::build_artifact(vec![(
            "doc.md",
            0,
            false,
            vec![vec![TokenInfo::default().with_orth("x")]],
        )])
    };
    let seed = artifact.directory.path().join("seed.lx");
    std::fs::write(&seed, if sectioned { "fragment unused := [text=NOT].\nhigh := [text=HIT].\nmedium := [text=OTHER].\nlow := [text=rare]." } else { SEED }).unwrap();
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
    let app = lingrep_web::app_router_at(store.clone(), &args.base_path).layer(
        axum::middleware::from_fn(move |request, next| {
            answer_as_gateway(Arc::clone(&down), request, next)
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/sub/sample", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Served {
        base,
        store,
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
async fn tab_moves_focus_out_of_the_pattern_editor_without_editing_it() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let result: WebDriverResult<()> = async {
        driver.goto(&served.base).await?;
        wait_for(&driver, CHECKED, "the editor's first check").await?;
        let content = driver.find(By::Css("#sample-editor .cm-content")).await?;
        content.click().await?;
        content.send_keys(Key::Tab).await?;
        observed(
            &driver,
            "return [document.querySelector('#sample-pattern').value,              !!document.activeElement.closest('.cm-editor')]",
            json!([SEED, false]),
            "the pattern and the focus after Tab",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_sample_sentence_becomes_an_example_and_a_counterexample_that_are_checked()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let set = "const field = document.querySelector(arguments[0]); field.value = arguments[1]; \
               field.dispatchEvent(new Event('input', {bubbles: true}));";
    let claims =
        "return document.querySelector('[data-sample-claims]')?.dataset.sampleClaims ?? null";
    let result: WebDriverResult<()> = async {
        driver.goto(&served.base).await?;
        wait_for(&driver, CHECKED, "the editor's first check").await?;
        driver
            .execute(set, vec![json!("#sample-text"), json!("猫\n鳥")])
            .await?;
        driver
            .execute(set, vec![json!("#sample-pattern"), json!("pattern p := [text=猫].\n")])
            .await?;
        wait_for(
            &driver,
            "return !!document.querySelector('[data-claim-insert=examples]') \
             && !!document.querySelector('[data-claim-insert=counterexamples]')",
            "both claim actions",
        )
        .await?;
        observed(&driver, claims, json!(null), "claims before any is declared").await?;

        driver
            .execute(
                "document.querySelector('[data-claim-insert=examples]').click()",
                vec![],
            )
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-sample-claims]')?.dataset.sampleClaims === 'hold' \
             && !document.querySelector('[data-claim-insert=examples]')",
            "the added example to be checked and no longer offered",
        )
        .await?;
        driver
            .execute(
                "document.querySelector('[data-claim-insert=counterexamples]').click()",
                vec![],
            )
            .await?;
        wait_for(
            &driver,
            "return !document.querySelector('[data-claim-insert]')",
            "the added counterexample to be no longer offered",
        )
        .await?;
        observed(
            &driver,
            "return [document.querySelector('#sample-pattern').value, \
             document.querySelector('.sample-claims-summary').dataset.i18nArgs]",
            json!([
                "examples: [\"猫\"]\ncounterexamples: [\"鳥\"]\npattern p := [text=猫].\n",
                "{\"checked\": 2}"
            ]),
            "the edited pattern and the claims checked",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_match_token_and_the_pattern_element_that_consumed_it_highlight_each_other()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let set = "const field = document.querySelector(arguments[0]); field.value = arguments[1]; \
               field.dispatchEvent(new Event('input', {bubbles: true}));";
    let pattern = "fragment 動物 := [text=猫].\npattern p := @動物 [text=と] [text=犬].\n";
    let offset = |needle: &str| {
        pattern
            .find(needle)
            .map(|byte| pattern[..byte].encode_utf16().count())
    };
    let place_cursor = "return import(new URL('assets/editor/pattern-editor.js', document.baseURI).href) \
        .then((editor) => { editor.selectSource(document.querySelector('#sample-pattern'), arguments[0]); return true; })";
    let consumed = "return [...document.querySelectorAll('.sample-match [data-source-hit]')] \
        .map((token) => token.textContent.trim())";
    let result: WebDriverResult<()> = async {
        driver.goto(&served.base).await?;
        wait_for(&driver, CHECKED, "the editor's first check").await?;
        driver
            .execute(set, vec![json!("#sample-text"), json!("猫と犬")])
            .await?;
        driver
            .execute(set, vec![json!("#sample-pattern"), json!(pattern)])
            .await?;
        wait_for(
            &driver,
            "return document.querySelectorAll('.sample-match').length === 1 \
             && document.querySelectorAll('.sample-match [data-source-ranges]').length === 3",
            "one match of three tokens with source ranges",
        )
        .await?;

        driver
            .execute(
                "document.querySelector('.sample-match [data-source-ranges]').click()",
                vec![],
            )
            .await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('.cm-sample-source')].map((mark) => mark.textContent).join('')",
            json!("[text=猫]@動物"),
            "the fragment element and the reference marked for 猫",
        )
        .await?;

        driver
            .execute(place_cursor, vec![json!(offset("@動物 [text=と]").map(|at| at + 1))])
            .await?;
        observed(&driver, consumed, json!(["猫"]), "the token the reference consumed").await?;
        driver
            .execute(place_cursor, vec![json!(offset("[text=犬]").map(|at| at + 2))])
            .await?;
        observed(&driver, consumed, json!(["犬"]), "the token the element consumed").await
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
               === 'The sample exceeds the test limits. Shorten it or search a corpus with this pattern.'",
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
            json!("The sample exceeds the test limits. Shorten it or search a corpus with this pattern."),
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
    let index = format!("{}workbench", served.base.trim_end_matches("sample"));
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
    let index = format!("{}workbench", served.base.trim_end_matches("sample"));
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
    let index = format!("{}workbench", served.base.trim_end_matches("sample"));
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
            "return document.querySelector('.compound-control-group[title]').title",
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

#[tokio::test]
async fn missing_and_abandoned_search_streams_close_without_retries_or_lost_notices()
-> WebDriverResult<()> {
    use askama::Template as _;
    use lingrep_web::revision::{AttemptPhase, CancelReason, WorkbenchId};
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let registry = &served.store.coordinators;
    let permit = registry.acquire_scan_permit().await;
    let workbench = WorkbenchId::new("abandoned-stream");
    registry
        .accept_revision(
            workbench.clone(),
            served.store.corpora.resolve(None).unwrap(),
            lingrep_web::coordinator::RevisionRequest {
                source: "p := [].".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    tokio::time::pause();
    tokio::time::timeout(std::time::Duration::from_secs(40), async {
        loop {
            if registry
                .snapshot(&workbench)
                .await
                .unwrap()
                .state
                .is_terminal()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::resume();
    drop(permit);
    let cancelled = registry.snapshot(&workbench).await.unwrap();
    assert_eq!(
        cancelled.state.current_attempt().unwrap().phase,
        AttemptPhase::Cancelled(CancelReason::Abandoned)
    );
    let result: WebDriverResult<()> = async {
        driver.goto(format!("{}workbench",served.base.trim_end_matches("sample"))).await?;
        wait_for(&driver, "return !!window.htmx && !!document.querySelector('#search-status')", "the corpus search page").await?;
        driver.execute("window.__streams = []; const create = htmx.createEventSource; htmx.createEventSource = (url) => { const source = create(url); __streams.push(source); return source; };", vec![]).await?;
        for (id, view, message) in [("missing-stream", None, "Request not found"), ("abandoned-stream", Some(&cancelled), "This search was stopped")] {
            let status = lingrep_web::revision_status::project_revision_status(view);
            let html = lingrep_web::templates::StreamContainer { request_id: id, status: &status }.render().unwrap();
            driver.execute("const target = document.querySelector('#search-status'); htmx.trigger(target, 'htmx:beforeCleanupElement', {elt: target}); target.innerHTML = arguments[0]; htmx.process(target);", vec![json!(html)]).await?;
            wait_for(&driver, "return __streams.at(-1)?.readyState === EventSource.CLOSED", "the terminal event stream to close").await?;
            let text = driver.find(By::Id("search-stream")).await?.text().await?;
            assert!(text.contains(message), "{text}");
            assert_eq!(driver.execute("return document.querySelector('#search-stream').dataset.streamFinished", vec![]).await?.json(), &json!("true"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
        assert_eq!(driver.execute("return [__streams.length, document.querySelectorAll('.stream-lost').length]", vec![]).await?.json(), &json!([2, 0]));
        Ok(())
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn export_links_follow_locale_changes_and_new_result_swaps() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve().await;
    let result: WebDriverResult<()> = async {
        driver.goto(format!("{}workbench",served.base.trim_end_matches("sample"))).await?;
        wait_for(&driver, "return !!document.querySelector('[data-corpus-search]') && !!window.htmx", "the search form").await?;
        driver.execute("const pattern = document.querySelector('#corpus-pattern'); pattern.value = 'p := [text=x].'; pattern.dispatchEvent(new Event('input', {bubbles:true})); document.querySelector('[data-corpus-search]').requestSubmit();", vec![]).await?;
        wait_for(&driver, "return document.querySelector('a[data-export-link]')?.href.includes('locale=en')", "English export links").await?;
        driver.find(By::Css("[data-locale-toggle]")).await?.click().await?;
        wait_for(&driver, "return document.querySelector('a[data-export-link]')?.href.includes('locale=ja')", "Japanese export links").await?;
        driver.execute("window.__oldExport = document.querySelector('a[data-export-link]'); document.querySelector('[data-corpus-search]').requestSubmit();", vec![]).await?;
        wait_for(&driver, "return !__oldExport.isConnected && document.querySelector('a[data-export-link]')?.href.includes('locale=ja')", "a Japanese export link after a result swap").await?;
        driver.find(By::Css("[data-locale-toggle]")).await?.click().await?;
        wait_for(&driver, "return document.querySelector('a[data-export-link]')?.href.includes('locale=en')", "restored English export links").await?;
        Ok(())
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn command_summaries_keep_kwic_wide_and_cluster_controls_preserve_focus()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_sectioned(true).await;
    let result = std::panic::AssertUnwindSafe(async {
        driver.set_window_rect(0, 0, 1440, 1000).await?;
        driver.goto(format!("{}workbench",served.base.trim_end_matches("sample"))).await?;
        wait_for(&driver, "return !!document.querySelector('.cm-lint-marker-warning')", "the unused-fragment warning").await?;
        driver.execute("document.querySelector('input[name=derive_compounds]').checked = false; document.querySelector('[data-corpus-search]').requestSubmit()", vec![]).await?;
        wait_for(&driver, "return !!document.querySelector('.kwic-row') && !!document.querySelector('[data-cli-command]')", "the concordance and its command summary").await?;
        let layout = driver.execute("const r = document.querySelector('.pattern-rail').getBoundingClientRect(); const d = document.querySelector('.detail-panel').getBoundingClientRect(); return {rail:r.toJSON(), detail:d.toJSON(), overflow:document.documentElement.scrollWidth-innerWidth};", vec![]).await?.json().clone();
        assert!(layout["detail"]["x"].as_f64().unwrap() >= layout["rail"]["right"].as_f64().unwrap(), "{layout}");
        assert!(layout["detail"]["width"].as_f64().unwrap() > 800.0, "{layout}");
        assert!(layout["overflow"].as_f64().unwrap() <= 1.0, "{layout}");
        driver.find(By::Css(".kwic-row")).await?.click().await?;
        wait_for(&driver, "return !!document.querySelector('#inspector-dock:not([hidden]) .hit-detail')", "the docked inspector").await?;
        let inspector = driver.execute("const d = document.querySelector('.detail-panel').getBoundingClientRect(); const i = document.querySelector('#inspector-dock').getBoundingClientRect(); return [d.width, i.left >= d.right - 1];", vec![]).await?.json().clone();
        assert!(inspector[0].as_f64().unwrap() > 400.0, "{inspector}");
        assert_eq!(inspector[1], true);
        driver.find(By::Css("#inspector-dock [data-inspector-close]")).await?.click().await?;
        crate::resize(&driver, 390, 900).await?;
        let mobile = driver.execute("return [document.querySelector('.detail-panel').getBoundingClientRect().width, document.documentElement.scrollWidth - innerWidth]", vec![]).await?.json().clone();
        assert!(mobile[0].as_f64().unwrap() > 300.0, "{mobile}");
        assert!(mobile[1].as_f64().unwrap() <= 1.0, "{mobile}");
        crate::resize(&driver, 1440, 1000).await?;
        driver.find(By::Css("[data-detail-tab=clusters]")).await?.click().await?;
        let url = driver.current_url().await?;
        driver.execute("window.__clusterPending = 0; document.body.addEventListener('htmx:beforeRequest', e => { if (e.detail.elt.id === 'section-cluster-distance') { __clusterPending++; e.detail.xhr.addEventListener('loadend', () => __clusterPending--, {once:true}); } });", vec![]).await?;
        driver.execute("window.__oldCluster = document.querySelector('[data-section-analysis]')", vec![]).await?;
        driver.execute("const minimum = document.querySelector('#cluster-minimum'); minimum.value = '2'; minimum.focus();", vec![]).await?;
        driver.find(By::Id("cluster-minimum")).await?.send_keys(Key::Enter).await?;
        wait_for(&driver, "return !__oldCluster.isConnected && document.querySelector('#cluster-minimum')?.value === '2' && __clusterPending === 0 && !document.querySelector('.htmx-settling')", "the minimum-two cluster response").await?;
        let leaves = driver.execute("return [...document.querySelectorAll('[data-section-dendrogram] .dendrogram-label title')].map(e=>e.textContent).sort()", vec![]).await?.json().clone();
        assert_eq!(leaves, json!(["high", "medium"]));
        assert_eq!(driver.current_url().await?, url);
        assert_eq!(driver.execute("return document.activeElement.id", vec![]).await?.json(), &json!("cluster-minimum"));
        driver.execute("window.__oldCluster = document.querySelector('[data-section-analysis]')", vec![]).await?;
        driver.find(By::Id("cluster-minimum")).await?.send_keys(Key::Up).await?;
        wait_for(&driver, "return !__oldCluster.isConnected && document.querySelector('#cluster-minimum')?.value === '3' && __clusterPending === 0 && !document.querySelector('.htmx-settling')", "the ArrowUp cluster response").await?;
        assert_eq!(driver.current_url().await?, url);
        assert_eq!(driver.execute("return document.activeElement.id", vec![]).await?.json(), &json!("cluster-minimum"));
        let note = driver.find(By::Css("[data-cluster-below-minimum]")).await?.text().await?;
        assert!(note.starts_with("2 patterns"), "{note}");
        let marker = driver.find(By::Css(".cm-lint-marker-warning")).await?;
        driver.action_chain().move_to_element_center(&marker).perform().await?;
        wait_for(&driver, "return document.querySelector('.cm-tooltip-lint')?.textContent.includes('A fragment is never referenced.')", "the English lint diagnostic").await?;
        driver.find(By::Css("[data-locale-toggle]")).await?.click().await?;
        wait_for(&driver, "return document.documentElement.lang === 'ja' && !!document.querySelector('.cm-lint-marker-warning')", "the Japanese lint marker").await?;
        let marker = driver.find(By::Css(".cm-lint-marker-warning")).await?;
        driver.action_chain().move_to_element_center(&marker).perform().await?;
        wait_for(&driver, "return document.querySelector('.cm-tooltip-lint')?.textContent.includes('参照されていない fragment')", "the Japanese lint diagnostic").await?;
        Ok::<(), thirtyfour::error::WebDriverError>(())
    }).catch_unwind().await;
    driver.quit().await?;
    served.server.abort();
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[tokio::test]
async fn pattern_metadata_categories_preserve_controls_and_exact_section_counts()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_sectioned(true).await;
    let result = std::panic::AssertUnwindSafe(async {
        driver.goto(format!("{}workbench", served.base.trim_end_matches("sample"))).await?;
        wait_for(&driver, "return document.readyState==='complete' && !!window.htmx && !document.querySelector('.htmx-settling') && !!document.querySelector('form[data-corpus-search]')", "the workbench search form").await?;
        driver.execute("const editor=document.querySelector('#corpus-pattern');editor.value='aspect: \"perfect\"\\npattern a := [text=HIT].\\naspect: \"perfect\"\\npattern b := [text=HIT].\\naspect: \"progressive\"\\npattern c := [text=OTHER].\\npattern missing := [text=rare].';editor.dispatchEvent(new Event('input',{bubbles:true}));HTMLFormElement.prototype.requestSubmit.call(document.querySelector('form[data-corpus-search]'));", vec![]).await?;
        wait_for(&driver, "return !document.querySelector('.htmx-settling') && !!document.querySelector('.section-scope-form select[name=group_patterns_by] option[value=aspect]')", "metadata controls after the search").await?;
        driver.find(By::Css("[data-detail-tab=sections]")).await?.click().await?;
        driver.execute("window.__oldMetadata=document.querySelector('#section-analysis');document.querySelector('.section-scope-form select[name=group_patterns_by]').value='aspect';HTMLFormElement.prototype.requestSubmit.call(document.querySelector('form.section-scope-form'));", vec![]).await?;
        wait_for(&driver, "return !document.querySelector('.htmx-settling') && !!window.__oldMetadata && !window.__oldMetadata.isConnected && document.querySelector('.section-scope-form select[name=group_patterns_by]')?.value==='aspect'", "the category projection").await?;
        let rows = driver.execute("return [...document.querySelectorAll('.section-comparison-band > .section-comparison-scroll tbody tr')].map(row=>[row.querySelector('th').textContent.trim(),[...row.querySelectorAll('td')].map(cell=>Number(cell.dataset.count))])", vec![]).await?.json().clone();
        assert_eq!(rows, json!([["perfect",[4,4]],["progressive",[2,0]]]));
        assert_eq!(driver.execute("return document.querySelector('[data-pattern-metadata-omitted]').dataset.patternMetadataOmitted",vec![]).await?.json(), &json!("1"));
        driver.execute("window.__oldMetadata=document.querySelector('#section-analysis');document.querySelector('select[name=contrast_reference]').value='introduction';document.querySelector('select[name=contrast_comparison]').value='methods';document.querySelector('select[name=contrast_comparison]').dispatchEvent(new Event('change',{bubbles:true}));", vec![]).await?;
        wait_for(&driver, "return !document.querySelector('.htmx-settling') && !!window.__oldMetadata && !window.__oldMetadata.isConnected && !!document.querySelector('[data-section-contrast] tbody tr')", "the category contrast").await?;
        assert_eq!(driver.execute("return document.querySelector('.section-scope-form select[name=group_patterns_by]').value",vec![]).await?.json(), &json!("aspect"));
        let leaves = driver.execute("return [...document.querySelectorAll('[data-section-dendrogram] .dendrogram-label title')].map(node=>node.textContent).sort()",vec![]).await?.json().clone();
        assert_eq!(leaves,json!(["perfect","progressive"]));
        driver.execute("window.__oldMetadata=document.querySelector('#section-analysis');document.querySelector('.section-scope-form select[name=pattern_filter]').value='aspect=perfect';HTMLFormElement.prototype.requestSubmit.call(document.querySelector('form.section-scope-form'));", vec![]).await?;
        wait_for(&driver, "return !document.querySelector('.htmx-settling') && !!window.__oldMetadata && !window.__oldMetadata.isConnected && document.querySelector('.section-scope-form select[name=pattern_filter]')?.value==='aspect=perfect'", "the filtered category").await?;
        assert_eq!(driver.execute("return document.querySelector('[data-pattern-metadata-omitted]').dataset.patternMetadataOmitted",vec![]).await?.json(), &json!("2"));
        assert_eq!(driver.execute("return document.querySelectorAll('.section-comparison-band > .section-comparison-scroll tbody tr').length",vec![]).await?.json(), &json!(1));
        driver.execute("document.querySelector('#section-analysis .corpus-analysis-scope').open=true",vec![]).await?;
        for width in [1440,390] {
            crate::resize(&driver, width, 1000).await?;
            let overflow=driver.execute("return document.documentElement.scrollWidth-innerWidth",vec![]).await?.json().as_i64().unwrap();
            assert!(overflow<=1,"metadata controls overflow at {width}: {overflow}");
        }
        Ok::<(),thirtyfour::error::WebDriverError>(())
    }).catch_unwind().await;
    driver.quit().await?;
    served.server.abort();
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
