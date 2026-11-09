#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{extract::Request, middleware::Next, response::Response};
use clap::Parser;
use lingrep_core::corpus::{Metadata, MetadataScalar, MetadataValue};
use lingrep_core::execution::CancellationFlag;
use lingrep_core::token_match::TokenInfo;
use lingrep_corpus::{CountTableKey, CountTableWriter, DocumentTotals, GroupKind, GroupSchema};
use serde_json::{Value, json};
use thirtyfour::prelude::*;
use tokio::net::TcpListener;

#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../suite/v6_support.rs"]
mod support;

const CATALOG: &str = "pattern noun := [upos=NOUN].\npattern verb := [upos=VERB].\n";

async fn record_errors(
    statuses: Arc<Mutex<Vec<String>>>,
    request: Request,
    next: Next,
) -> Response {
    let uri = request.uri().to_string();
    let response = next.run(request).await;
    if response.status().as_u16() >= 400 {
        statuses
            .lock()
            .unwrap()
            .push(format!("{} {uri}", response.status()));
    }
    response
}

fn metadata() -> Vec<Metadata> {
    let text = |value: &str| MetadataScalar::String(value.into());
    [
        ("novel", &["x", "y"][..]),
        ("novel", &["x"][..]),
        ("essay", &["y"][..]),
        ("essay", &["x", "z"][..]),
        ("poem", &["y"][..]),
        ("poem", &["z"][..]),
    ]
    .iter()
    .map(|(genre, tags)| {
        vec![
            ("genre".to_owned(), MetadataValue::Scalar(text(genre))),
            (
                "tags".to_owned(),
                MetadataValue::Scalars(tags.iter().map(|tag| text(tag)).collect()),
            ),
        ]
        .into_boxed_slice()
    })
    .collect()
}

fn write_table(artifact: &std::path::Path, destination: &std::path::Path) {
    let opened = lingrep_corpus::CorpusArtifact::open(artifact).unwrap();
    let key = CountTableKey {
        analysis_identity: lingrep_corpus::format_identity(opened.analysis_identity().as_bytes()),
        catalog_fingerprint: lingrep_core::fsm::catalog_fingerprint(CATALOG),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    };
    let documents = (0..6)
        .map(|document_id| DocumentTotals {
            document_id,
            sentences: 1,
            tokens: 1000,
        })
        .collect::<Vec<_>>();
    let values = ["本", "猫", "見る", "雨"];
    let mut writer = CountTableWriter::create(
        destination,
        key,
        vec!["noun".into(), "verb".into()],
        vec![GroupSchema {
            kind: GroupKind::Surface,
            names: Vec::new(),
        }],
        &values,
        &documents,
    )
    .unwrap();
    for (pattern, value, pairs) in [
        (0, 0, &[(0, 20), (1, 12), (3, 1)][..]),
        (0, 1, &[(0, 1), (2, 9), (3, 2), (4, 14)][..]),
        (0, 3, &[(1, 3), (5, 6)][..]),
        (1, 2, &[(1, 1), (2, 2), (5, 1)][..]),
    ] {
        writer.begin_entry(pattern, 0, &[value]).unwrap();
        for (document, count) in pairs {
            writer
                .add_sentence(*document, u64::from(*document), *count)
                .unwrap();
        }
    }
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
}

struct Served {
    base: String,
    statuses: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
    _artifact: support::ArtifactFixture,
    _dictionary: lingrep_core::testing::MiniDictionary,
}

async fn serve() -> Served {
    let sentences = vec![vec![TokenInfo::default().with_orth("x")]];
    let artifact = support::build_artifact_with_metadata(
        (0..6)
            .map(|index| ("doc.md", index, false, sentences.clone()))
            .collect(),
        Vec::new(),
        metadata(),
    );
    let root = artifact.directory.path().join("patterns");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("catalog.lx"), CATALOG).unwrap();
    let table = artifact.directory.path().join("fixture.lingtable");
    write_table(&artifact.path, &table);
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let argv: Vec<&std::ffi::OsStr> = vec![
        "lingrep-web".as_ref(),
        "--dict-path".as_ref(),
        dictionary.path().as_os_str(),
        "--input".as_ref(),
        artifact.path.as_os_str(),
        "--pattern-root".as_ref(),
        root.as_os_str(),
        "--count-table".as_ref(),
        table.as_os_str(),
        "--base-path".as_ref(),
        "/sub".as_ref(),
    ];
    let args = lingrep_web::Args::try_parse_from(argv).unwrap();
    let config = lingrep_web::serve::config(&args).unwrap();
    let store = lingrep_web::serve::store(&args, config).await.unwrap();
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&statuses);
    let app = lingrep_web::app_router_at(store, &args.base_path).layer(axum::middleware::from_fn(
        move |request, next| record_errors(Arc::clone(&recorded), request, next),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/sub/compare", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Served {
        base,
        statuses,
        server,
        _artifact: artifact,
        _dictionary: dictionary,
    }
}

async fn driver(width: u32) -> WebDriverResult<Option<WebDriver>> {
    let Some(driver_url) = crate::chromedriver_url().await else {
        return Ok(None);
    };
    let mut capabilities = DesiredCapabilities::chrome();
    for arg in [
        "--headless",
        "--no-sandbox",
        "--disable-dev-shm-usage",
        "--disable-gpu",
    ] {
        capabilities.add_arg(arg)?;
    }
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__pageErrors = []; window.__shift = 0;\
                addEventListener('error', (e) => __pageErrors.push(String(e.message)));\
                addEventListener('unhandledrejection', (e) => __pageErrors.push(String(e.reason)));\
                { const error = console.error; console.error = (...a) => { __pageErrors.push(a.map(String).join(' ')); error(...a); }; }\
                new PerformanceObserver((list) => { for (const e of list.getEntries()) if (!e.hadRecentInput) { __shift += e.value; (window.__moved ||= []).push([Math.round(e.startTime), e.value, ...(e.sources || []).map((s) => [s.node?.nodeName, s.node?.className, Math.round(s.previousRect.y), Math.round(s.currentRect.y), Math.round(s.previousRect.height), Math.round(s.currentRect.height)])]); } }).observe({type: 'layout-shift', buffered: true});"}),
        )
        .await?;
    driver.set_window_rect(0, 0, width, 900).await?;
    Ok(Some(driver))
}

async fn wait_for(driver: &WebDriver, script: &str, what: &str) -> WebDriverResult<Value> {
    for _ in 0..500 {
        let value = driver.execute(script, Vec::new()).await?.json().clone();
        if !value.is_null() && value != json!(false) {
            return Ok(value);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(WebDriverError::Timeout(format!("waiting for {what}")))
}

const SETTLED: &str = "return document.querySelector('[data-compare]:not([aria-busy])') && document.querySelectorAll('table.cmp-table tbody tr').length > 0";

const PAGE: &str = r"
return {
  href: location.href,
  sentence: (() => {
    const read = document.querySelector('[data-cmp-sentence]').cloneNode(true);
    read.querySelectorAll('.role, .cmp-swatch, .cmp-scope-name, [data-add]').forEach((node) => node.remove());
    read.querySelectorAll('select').forEach((node) => node.replaceWith(node.selectedOptions[0]?.textContent ?? ''));
    return read.textContent.replace(/\s+/g, ' ').trim();
  })(),
  relation: document.querySelector('.cmp-relation')?.dataset.relation ?? null,
  command: document.querySelector('[data-c=code]')?.textContent ?? null,
  rows: [...document.querySelectorAll('table.cmp-table tbody tr')].map((row) => [...row.cells].map((cell) => cell.textContent)),
  state: document.querySelector('[data-cmp-state]').textContent,
  shift: window.__shift,
  moved: window.__moved ?? [],
  errors: window.__pageErrors,
};";

async fn page(driver: &WebDriver) -> WebDriverResult<Value> {
    driver
        .execute_async(
            "const done = arguments[0]; requestAnimationFrame(() => requestAnimationFrame(() => done()));",
            Vec::new(),
        )
        .await?;
    Ok(driver.execute(PAGE, Vec::new()).await?.json().clone())
}

fn address(base: &str, entries: &[(&str, &str)]) -> String {
    let encoded = entries
        .iter()
        .map(|(key, value)| {
            let value = value
                .bytes()
                .map(|byte| match byte {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (byte as char).to_string()
                    }
                    _ => format!("%{byte:02X}"),
                })
                .collect::<String>();
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}#{encoded}")
}

#[tokio::test]
async fn the_compare_page_loads_an_address_as_written_and_round_trips_its_state()
-> WebDriverResult<()> {
    let Some(driver) = driver(1280).await? else {
        return Ok(());
    };
    let served = serve().await;
    let written = address(
        &served.base,
        &[
            ("corpus", "default"),
            ("catalog", "catalog.lx"),
            ("pattern", "noun"),
            ("w", r#"{"tags":{"m":"in","v":["x"]}}"#),
            ("v", r#"{"tags":{"m":"in","v":["y"]}}"#),
        ],
    );
    driver.goto(&written).await?;
    wait_for(&driver, SETTLED, "rows").await?;
    let loaded = page(&driver).await?;
    assert_eq!(loaded["href"], written, "the address is kept as written");
    assert_eq!(loaded["relation"], "overlapping");
    assert_eq!(loaded["state"], "");
    assert_eq!(loaded["errors"], json!([]));
    assert_eq!(loaded["shift"], 0, "{loaded}");
    let sentence = loaded["sentence"].as_str().unwrap();
    assert!(
        sentence.starts_with("Compare noun in catalog.lx between "),
        "{sentence}"
    );
    assert!(sentence.contains("documents with tags x and"), "{sentence}");
    let command = loaded["command"].as_str().unwrap();
    assert!(
        command.contains(
            r#"--where '@document.tags contains "x"' --versus '@document.tags contains "y"'"#
        ),
        "{command}"
    );
    let rows = loaded["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let entries = rows.iter().map(|row| row[1].clone()).collect::<Vec<_>>();
    assert!(entries.contains(&json!("本")), "{entries:?}");
    let ratios = driver
        .execute(
            "return [...document.querySelectorAll('.cmp-axis')].map((axis) => \
               [axis.getAttribute('aria-label'), axis.querySelector('i')?.dataset.scope ?? null])",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    let mut sides = Vec::new();
    for ratio in ratios.as_array().unwrap() {
        let (label, toward) = (ratio[0].as_str().unwrap(), &ratio[1]);
        let Some(rest) = label.strip_prefix("rate in ") else {
            continue;
        };
        let (high, rest) = rest.split_once(" is ").unwrap();
        let factor: f64 = rest.split_once('×').unwrap().0.parse().unwrap();
        assert!(factor >= 1.0, "{label}");
        let side = if high == "A" { "where" } else { "versus" };
        assert_eq!(toward, side, "{label}");
        sides.push(side);
    }
    sides.sort_unstable();
    sides.dedup();
    assert_eq!(
        sides,
        ["versus", "where"],
        "both directions occur: {ratios}"
    );

    driver
        .find(By::Css("[data-scope=versus][data-add]"))
        .await?
        .click()
        .await?;
    driver
        .find(By::Css(".fq-pop [data-add='genre']"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('.fq-pop:not([hidden]) input[data-v=essay]')",
        "values",
    )
    .await?;
    driver
        .find(By::Css(".fq-pop input[data-v=essay]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return decodeURIComponent(location.hash).includes('\"genre\"') && document.querySelector('[data-compare]:not([aria-busy]) .cmp-relation')",
        "genre condition",
    )
    .await?;
    driver
        .find(By::Css("body"))
        .await?
        .send_keys(Key::Escape)
        .await?;
    wait_for(&driver, SETTLED, "rows").await?;
    let stated = page(&driver).await?;
    assert!(
        stated["command"]
            .as_str()
            .unwrap()
            .contains(r#"--versus '@document.genre = "essay" & @document.tags contains "y"'"#),
        "{stated}"
    );

    let href = stated["href"].as_str().unwrap().to_owned();
    driver.back().await?;
    wait_for(
        &driver,
        &format!("return location.href === {} && document.querySelector('[data-compare]:not([aria-busy])')", json!(written)),
        "back",
    )
    .await?;
    let back = page(&driver).await?;
    assert_eq!(back["rows"], loaded["rows"]);
    assert_eq!(back["command"], loaded["command"]);
    driver.forward().await?;
    wait_for(
        &driver,
        &format!("return location.href === {} && document.querySelector('[data-compare]:not([aria-busy])')", json!(href)),
        "forward",
    )
    .await?;
    assert_eq!(page(&driver).await?["rows"], stated["rows"]);

    driver.goto("about:blank").await?;
    driver.goto(&href).await?;
    wait_for(&driver, SETTLED, "rows").await?;
    let reopened = page(&driver).await?;
    for key in ["href", "sentence", "relation", "command", "rows"] {
        assert_eq!(reopened[key], stated[key], "{key}");
    }
    assert_eq!(reopened["shift"], 0, "{reopened}");

    driver
        .find(By::Css("[data-locale-toggle]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('[data-cmp-sentence]').textContent.includes('比べる')",
        "Japanese sentence",
    )
    .await?;
    let after = page(&driver).await?;
    let ja = after["sentence"].as_str().unwrap();
    assert!(
        ja.contains("tagsがxの文書") && ja.contains("genreがessay、tagsがyの文書"),
        "{ja}"
    );
    assert_eq!(after["errors"], json!([]));
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}

#[tokio::test]
async fn the_compare_page_states_identical_and_empty_scopes() -> WebDriverResult<()> {
    let Some(driver) = driver(420).await? else {
        return Ok(());
    };
    let served = serve().await;
    for (w, v, relation, note) in [
        (
            r#"{"tags":{"m":"in","v":["x"]}}"#,
            r#"{"tags":{"m":"in","v":["x"]}}"#,
            "identical",
            "every G² is 0",
        ),
        (
            r#"{"genre":{"m":"in","v":["novel"]}}"#,
            r#"{"genre":{"m":"in","v":["poem"]}}"#,
            "disjoint",
            "No document is in both",
        ),
    ] {
        let written = address(
            &served.base,
            &[
                ("corpus", "default"),
                ("catalog", "catalog.lx"),
                ("pattern", "noun"),
                ("w", w),
                ("v", v),
            ],
        );
        driver.goto("about:blank").await?;
        driver.goto(&written).await?;
        wait_for(&driver, SETTLED, "rows").await?;
        let shown = page(&driver).await?;
        assert_eq!(shown["relation"], relation);
        let text = driver
            .execute(
                "return document.querySelector('.cmp-relation').textContent",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert!(text.as_str().unwrap().contains(note), "{text}");
        assert_eq!(shown["shift"], 0, "{shown}");
        assert_eq!(shown["errors"], json!([]));
        let overflow = driver
            .execute(
                "return document.documentElement.scrollWidth <= innerWidth",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert_eq!(overflow, json!(true), "the page fits 420 px");
    }
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}

#[tokio::test]
async fn a_slow_ranking_keeps_the_previous_answer_in_place_and_aborts_a_superseded_request()
-> WebDriverResult<()> {
    let Some(driver) = driver(420).await? else {
        return Ok(());
    };
    let served = serve().await;
    let written = address(
        &served.base,
        &[
            ("corpus", "default"),
            ("catalog", "catalog.lx"),
            ("pattern", "noun"),
            ("w", r#"{"tags":{"m":"in","v":["x"]}}"#),
            ("v", r#"{"tags":{"m":"in","v":["y"]}}"#),
        ],
    );
    driver.goto(&written).await?;
    wait_for(&driver, SETTLED, "rows").await?;
    let before = page(&driver).await?;
    driver
        .execute(
            "window.__shift = 0; window.__moved = []; window.__signals = []; window.__held = []; \
             window.__release = () => { for (const send of __held.splice(0)) send(); }; \
             const fetched = window.fetch; \
             window.fetch = (url, init = {}) => { \
               if (!String(url).includes('api/contrast')) return fetched(url, init); \
               __signals.push(init.signal); \
               return new Promise((resolve, reject) => { \
                 init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))); \
                 __held.push(() => fetched(url, init).then(resolve, reject)); \
               }); \
             }; \
             for (const pattern of ['verb', 'noun']) { \
               const select = document.querySelector('select[data-b=pattern]'); \
               select.value = pattern; select.dispatchEvent(new Event('change', {bubbles: true})); \
             }",
            Vec::new(),
        )
        .await?;
    let waiting = driver
        .execute(
            "return {busy: document.querySelector('[data-compare]').hasAttribute('aria-busy'), \
               line: document.querySelector('.cmp-count-line').textContent, \
               rows: document.querySelectorAll('table.cmp-table tbody tr').length, \
               aborted: __signals.map((signal) => signal.aborted)}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(
        waiting,
        json!({"busy": true, "line": "Ranking entries…", "rows": 3, "aborted": [true, false]})
    );
    driver.execute("window.__release();", Vec::new()).await?;
    wait_for(&driver, SETTLED, "rows").await?;
    let after = page(&driver).await?;
    assert_eq!(after["rows"], before["rows"]);
    assert_eq!(after["shift"], 0, "{after}");
    assert_eq!(after["errors"], json!([]));
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}

#[tokio::test]
async fn value_search_ignores_errors_for_an_earlier_query() -> WebDriverResult<()> {
    let Some(driver) = driver(1280).await? else {
        return Ok(());
    };
    let served = serve().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": r#"
                const originalFetch = window.fetch.bind(window);
                window.fetch = async (input, options) => {
                    const url = new URL(input, location.href);
                    if (url.pathname.endsWith('/frequency/values')) {
                        return new Promise((resolve, reject) => {
                            window.__rejectValues = () => reject(new Error('earlier search failed'));
                        });
                    }
                    const response = await originalFetch(input, options);
                    if (!url.pathname.endsWith('/frequency/meta')) return response;
                    const meta = await response.json();
                    meta.dimensions.default.find((d) => d.key === 'genre').values = 31;
                    return Response.json(meta);
                };
            "#}),
        )
        .await?;
    driver
        .goto(address(
            &served.base,
            &[("w", r#"{"genre":{"m":"in","v":[]}}"#)],
        ))
        .await?;
    wait_for(&driver, SETTLED, "comparison rows").await?;
    driver
        .find(By::Css(".pick[data-scope=where][data-key=genre]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return window.__rejectValues && document.querySelector('.fq-pop [data-find]')",
        "pending value request and search field",
    )
    .await?;
    driver
        .execute(
            r#"
        const originalTimeout = window.setTimeout;
        window.setTimeout = (f, delay, ...args) => delay === 150
            ? (window.__heldSearch = f, 0) : originalTimeout(f, delay, ...args);
        const search = document.querySelector('.fq-pop [data-find]');
        search.value = 'nov';
        search.dispatchEvent(new Event('input', {bubbles: true}));
        window.__rejectValues();
    "#,
            Vec::new(),
        )
        .await?;
    let text = driver
        .execute(
            "return document.querySelector('.fq-pop .opts').textContent",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    driver.quit().await?;
    served.server.abort();
    assert_eq!(text, "", "the error belongs to the previous search");
    Ok(())
}
