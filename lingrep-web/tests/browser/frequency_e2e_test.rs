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
//! The frequency view in a browser, served under a subpath from a count
//! table over a prepared fixture.

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

/// Records every response status of 400 or more.
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

fn scalar(value: &str) -> MetadataValue {
    MetadataValue::Scalar(MetadataScalar::String(value.into()))
}

fn number(value: u32) -> MetadataValue {
    MetadataValue::Scalar(MetadataScalar::Number(value.to_string()))
}

/// Six documents whose two long keys share every unit but the last, so a
/// narrow sentence shortens their labels.
fn metadata() -> Vec<Metadata> {
    let rows = [
        ("novel", "Tokyo", "Chiyoda", 1950),
        ("novel", "Tokyo", "Chiyoda", 1960),
        ("essay", "Osaka", "Kita", 1970),
        ("essay", "Kyoto", "Kita", 1980),
        ("poem", "Tokyo", "Chiyoda", 1990),
        ("poem", "Osaka", "Kita", 2000),
    ];
    rows.iter()
        .map(|(genre, prefecture, city, year)| {
            vec![
                ("author_birth_place_city".to_owned(), scalar(city)),
                (
                    "author_birth_place_prefecture".to_owned(),
                    scalar(prefecture),
                ),
                ("genre".to_owned(), scalar(genre)),
                ("year".to_owned(), number(*year)),
            ]
            .into_boxed_slice()
        })
        .collect()
}

/// A table over `artifact` that counts, per document, two nouns and one verb.
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
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let values = ["本", "猫", "見る"];
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
        (0, 0, &[(0, 2), (2, 1), (4, 1)][..]),
        (0, 1, &[(0, 1), (1, 3), (3, 2), (5, 1)][..]),
        (1, 2, &[(1, 1), (2, 2), (5, 1)][..]),
    ] {
        writer.begin_entry(pattern, 0, &[value]).unwrap();
        for (document, count) in pairs {
            writer.add_pair(*document, *count).unwrap();
        }
    }
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
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

/// Loads `url` as a fresh document and waits until the view settles.
async fn open(driver: &WebDriver, url: &str, settled: &str) -> WebDriverResult<()> {
    driver.goto("about:blank").await?;
    driver.goto(url).await?;
    wait_for(driver, settled, url).await?;
    Ok(())
}

const ROWS: &str = "return document.querySelectorAll('table.freq tbody tr').length > 0";
const STATE: &str = "return document.querySelector('[data-fq-state]').textContent.trim() || null";

/// Fails on any console error or uncaught exception the page recorded.
async fn assert_no_page_errors(driver: &WebDriver) -> WebDriverResult<()> {
    let errors = driver
        .execute("return window.__pageErrors || []", Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(errors, json!([]), "page errors");
    Ok(())
}

/// The visible role labels: their full names, shown texts and shares.
const LABELS: &str = r"
return [...document.querySelectorAll('[data-fq-sentence] .blank > .role')].map((role) => {
  const nm = role.querySelector('.nm'), n = role.querySelector('.n');
  return {
    full: nm.dataset.label ?? nm.textContent,
    shown: nm.textContent,
    cut: role.classList.contains('cut'),
    share: n.textContent,
    shareClipped: n.scrollWidth > n.clientWidth + 0.5 || n.getBoundingClientRect().right > role.getBoundingClientRect().right + 0.5 && getComputedStyle(role).overflow !== 'visible',
    shareVisible: n.textContent === '' || n.getBoundingClientRect().width > 0,
  };
});";

fn assert_label_invariants(labels: &Value) {
    let labels = labels.as_array().unwrap();
    let units = |text: &str| -> Vec<String> {
        text.split_inclusive(['_', ' ', '・', '、'])
            .map(str::to_owned)
            .collect()
    };
    for (index, label) in labels.iter().enumerate() {
        let full = label["full"].as_str().unwrap();
        let shown = label["shown"].as_str().unwrap();
        for other in &labels[index + 1..] {
            if other["full"] != label["full"] {
                assert_ne!(other["shown"], label["shown"], "two values share a label");
            }
        }
        if shown != full {
            assert_eq!(shown.matches('…').count(), 1, "{shown}");
            let kept = shown.replace('…', "");
            let whole = units(full);
            let prefix = (1..whole.len())
                .any(|n| whole[..n].concat().trim_end_matches(['_', ' ', '・', '、']) == kept);
            let suffix = (1..whole.len()).any(|n| whole[whole.len() - n..].concat() == kept);
            assert!(
                (shown.ends_with('…') && prefix) || (shown.starts_with('…') && suffix),
                "{shown} does not replace whole units of {full}"
            );
        }
        for (open, close) in [('(', ')'), ('[', ']'), ('「', '」'), ('（', '）')] {
            assert_eq!(
                shown.matches(open).count(),
                shown.matches(close).count(),
                "{shown}: unbalanced {open}{close}"
            );
        }
        let share = label["share"].as_str().unwrap();
        assert!(
            share.is_empty()
                || share.strip_suffix('%').is_some_and(|value| value == "<1"
                    || value
                        .parse::<f64>()
                        .is_ok_and(|value| (0.0..=100.0).contains(&value))),
            "{share}"
        );
        assert_eq!(label["shareClipped"], false, "{label}");
        assert_eq!(label["shareVisible"], true, "{label}");
    }
}

#[tokio::test]
async fn frequency_view_round_trips_its_address_and_keeps_labels_distinct() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    let address = driver_url
        .strip_prefix("http://")
        .unwrap_or(&driver_url)
        .to_owned();
    if tokio::net::TcpStream::connect(&address).await.is_err() {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        eprintln!("SKIP: Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }

    let sentences = |surface: &str| vec![vec![TokenInfo::default().with_orth(surface)]];
    let artifact = support::build_artifact_with_metadata(
        (0..6)
            .map(|index| ("doc.md", index, false, sentences("x")))
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
    let args = lingrep_web::Args::try_parse_from([
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
    ] as [&std::ffi::OsStr; 11])
    .unwrap();
    let config = lingrep_web::serve::config(&args).unwrap();
    let store = lingrep_web::serve::store(&args, config).await.unwrap();
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&statuses);
    let app = lingrep_web::app_router_at(store, &args.base_path).layer(axum::middleware::from_fn(
        move |request, next| record_errors(Arc::clone(&recorded), request, next),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/sub/frequencies", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__pageErrors = [];\
                addEventListener('error', (e) => __pageErrors.push(String(e.message)));\
                addEventListener('unhandledrejection', (e) => __pageErrors.push(String(e.reason)));\
                { const error = console.error; console.error = (...a) => { __pageErrors.push(a.map(String).join(' ')); error(...a); }; }"}),
        )
        .await?;
    driver.set_window_rect(0, 0, 1280, 900).await?;

    // A bare address gets the first pair and pattern and is not rewritten.
    open(&driver, &base, ROWS).await?;
    let bare = driver
        .execute(
            "return {href: location.href, pattern: document.querySelector('[data-b=pattern]').value, rows: [...document.querySelectorAll('table.freq tbody tr td:nth-child(2)')].map((cell) => cell.textContent)}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(
        bare,
        json!({"href": base, "pattern": "noun", "rows": ["猫", "本"]})
    );
    assert_no_page_errors(&driver).await?;

    // Stating conditions through the blanks records them in the address.
    for key in [
        "genre",
        "author_birth_place_prefecture",
        "author_birth_place_city",
    ] {
        driver.find(By::Css("button.add")).await?.click().await?;
        driver
            .find(By::Css(format!("[data-add='{key}']")))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('.fq-pop:not([hidden]) .opts input')",
            key,
        )
        .await?;
        let labels = driver
            .execute("return [...document.querySelectorAll('.fq-pop .opts label > span:nth-child(2)')].map((s) => s.textContent)", Vec::new())
            .await?
            .json()
            .clone();
        let labels = labels.as_array().unwrap();
        let distinct = labels
            .iter()
            .map(Value::to_string)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), labels.len(), "{key}: {labels:?}");
        driver
            .find(By::Css(".fq-pop .opts input"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            &format!("return decodeURIComponent(location.hash).includes('\"{key}\"')"),
            key,
        )
        .await?;
        driver
            .find(By::Css("body"))
            .await?
            .send_keys(Key::Escape)
            .await?;
    }
    wait_for(
        &driver,
        "return document.querySelectorAll('.role .n:not(:empty)').length === 3",
        "three shares",
    )
    .await?;
    let stated = driver
        .execute("return location.href", Vec::new())
        .await?
        .json()
        .as_str()
        .unwrap()
        .to_owned();
    let sentence = driver
        .execute(
            "return document.querySelector('[data-fq-sentence]').textContent",
            Vec::new(),
        )
        .await?
        .json()
        .clone();

    // Loading the address reproduces the view and leaves the address alone.
    open(
        &driver,
        &stated,
        "return document.querySelectorAll('.role .n:not(:empty)').length === 3",
    )
    .await?;
    let reloaded = driver
        .execute("return {href: location.href, sentence: document.querySelector('[data-fq-sentence]').textContent}", Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(reloaded, json!({"href": stated, "sentence": sentence}));
    assert_label_invariants(&driver.execute(LABELS, Vec::new()).await?.json().clone());

    // In a narrow window the long labels shorten, stay distinct and keep
    // their shares whole.
    driver.set_window_rect(0, 0, 420, 900).await?;
    let labels = wait_for(
        &driver,
        &format!("const labels = (() => {{ {LABELS} }})(); return labels.some((l) => l.shown !== l.full || l.cut) ? labels : null"),
        "a shortened label",
    )
    .await?;
    assert_label_invariants(&labels);
    driver.set_window_rect(0, 0, 1280, 900).await?;

    // Whatever width the sentence has, no shortened form of one label is a
    // form of a sibling, so the labels it can show stay distinct.
    let shared = driver
        .execute_async(
            "const done = arguments[arguments.length - 1];\
             import(new URL('assets/frequency.js', location.href)).then(({ shortForms }) => {\
               const sets = [['author birth place city', 'author birth place prefecture', 'genre'],\
                             ['source edition year', 'source edition title', 'year'], ['a_b_c', 'a_b_d', 'x_b_c']];\
               done(sets.flatMap((names) => names.flatMap((name, i) => names.slice(i + 1).flatMap((other) =>\
                 shortForms(name, names).filter((form) => shortForms(other, names).includes(form))))));\
             });",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(shared, json!([]));
    assert_no_page_errors(&driver).await?;

    // No document meets the conditions: an empty state, not an error.
    let empty = format!(
        "{base}#{}",
        "w=%7B%22year%22%3A%7B%22m%22%3A%22range%22%2C%22lo%22%3A3000%7D%7D"
    );
    open(
        &driver,
        &empty,
        "return document.querySelector('.fq-empty')",
    )
    .await?;
    let text = driver
        .execute(
            "return document.querySelector('.fq-empty').textContent",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(text, "No occurrence meets these conditions.");
    assert_eq!(
        driver
            .execute("return location.href", Vec::new())
            .await?
            .json(),
        &json!(empty)
    );

    // An @section clause, even from an address, is unsupported here.
    open(
        &driver,
        &format!("{base}#where=%40section%20%3D%20abstract"),
        STATE,
    )
    .await?;
    let state = driver.execute(STATE, Vec::new()).await?.json().clone();
    assert!(
        state
            .as_str()
            .unwrap()
            .starts_with("@section cannot be answered here"),
        "{state}"
    );

    // A catalog with no table explains the missing table and keeps the address.
    let missing = format!("{base}#corpus=default&catalog=other.lx&pattern=noun");
    open(&driver, &missing, STATE).await?;
    let state = driver.execute(STATE, Vec::new()).await?.json().clone();
    assert!(
        state
            .as_str()
            .unwrap()
            .contains("No count table is registered for other.lx"),
        "{state}"
    );
    assert_eq!(
        driver
            .execute("return location.href", Vec::new())
            .await?
            .json(),
        &json!(missing)
    );

    assert_no_page_errors(&driver).await?;
    assert_eq!(*statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    server.abort();
    Ok(())
}
