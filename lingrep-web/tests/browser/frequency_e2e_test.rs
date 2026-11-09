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

fn scalar(value: &str) -> MetadataValue {
    MetadataValue::Scalar(MetadataScalar::String(value.into()))
}

fn number(value: u32) -> MetadataValue {
    MetadataValue::Scalar(MetadataScalar::Number(value.to_string()))
}

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

fn table_key(artifact: &std::path::Path, catalog: &str) -> CountTableKey {
    let opened = lingrep_corpus::CorpusArtifact::open(artifact).unwrap();
    CountTableKey {
        analysis_identity: lingrep_corpus::format_identity(opened.analysis_identity().as_bytes()),
        catalog_fingerprint: lingrep_core::fsm::catalog_fingerprint(catalog),
        matcher_revision: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
    }
}

fn write_table(artifact: &std::path::Path, destination: &std::path::Path, documents: usize) {
    let key = table_key(artifact, CATALOG);
    let documents = (0..documents as u64)
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

async fn open(driver: &WebDriver, url: &str, settled: &str) -> WebDriverResult<()> {
    driver.goto("about:blank").await?;
    driver.goto(url).await?;
    wait_for(driver, settled, url).await?;
    Ok(())
}

const ROWS: &str = "return document.querySelectorAll('table.freq tbody tr').length > 0";
const STATE: &str = "return document.querySelector('[data-fq-state]').textContent.trim() || null";

async fn assert_no_page_errors(driver: &WebDriver) -> WebDriverResult<()> {
    let errors = driver
        .execute("return window.__pageErrors || []", Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(errors, json!([]), "page errors");
    Ok(())
}

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

const NAMES: &str = r"
return Object.fromEntries([...document.querySelectorAll('[data-fq-sentence] .blank')]
  .filter((blank) => blank.querySelector('.pick'))
  .map((blank) => { const nm = blank.querySelector('.nm'); return [blank.querySelector('.pick').dataset.b, nm.dataset.label ?? nm.textContent]; }));";

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

async fn driver() -> WebDriverResult<Option<WebDriver>> {
    let Some(driver_url) = crate::chromedriver_url().await else {
        return Ok(None);
    };
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
    Ok(Some(driver))
}

struct Served {
    base: String,
    statuses: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
    _artifact: support::ArtifactFixture,
    _dictionary: lingrep_core::testing::MiniDictionary,
}

async fn serve(extra: &[&std::ffi::OsStr]) -> Served {
    serve_metadata(extra, metadata()).await
}

async fn serve_metadata(extra: &[&std::ffi::OsStr], metadata: Vec<Metadata>) -> Served {
    let count = metadata.len();
    let sentences = |surface: &str| vec![vec![TokenInfo::default().with_orth(surface)]];
    let artifact = support::build_artifact_with_metadata(
        (0..count)
            .map(|index| ("doc.md", index as u64, false, sentences("x")))
            .collect(),
        Vec::new(),
        metadata,
    );
    serve_artifact(
        artifact,
        CATALOG,
        |artifact, table| write_table(artifact, table, count),
        extra,
    )
    .await
}

/// Serves `artifact` with `catalog` as `catalog.lx` and the count table
/// `write` produces for it.
async fn serve_artifact(
    artifact: support::ArtifactFixture,
    catalog: &str,
    write: impl FnOnce(&std::path::Path, &std::path::Path),
    extra: &[&std::ffi::OsStr],
) -> Served {
    let root = artifact.directory.path().join("patterns");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("catalog.lx"), catalog).unwrap();
    let table = artifact.directory.path().join("fixture.lingtable");
    write(&artifact.path, &table);

    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let mut argv: Vec<&std::ffi::OsStr> = vec![
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
    argv.extend_from_slice(extra);
    let args = lingrep_web::Args::try_parse_from(argv).unwrap();
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
    Served {
        base,
        statuses,
        server,
        _artifact: artifact,
        _dictionary: dictionary,
    }
}

#[tokio::test]
async fn frequency_view_round_trips_its_address_and_keeps_labels_distinct() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve(&[]).await;
    let base = served.base.clone();

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
    assert_eq!(
        driver.execute(NAMES, Vec::new()).await?.json(),
        &json!({
            "author_birth_place_city": "author birth place city",
            "author_birth_place_prefecture": "author birth place prefecture",
            "genre": "genre",
        })
    );

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

    driver.set_window_rect(0, 0, 420, 900).await?;
    let labels = wait_for(
        &driver,
        &format!("const labels = (() => {{ {LABELS} }})(); return labels.some((l) => l.shown !== l.full || l.cut) ? labels : null"),
        "a shortened label",
    )
    .await?;
    assert_label_invariants(&labels);
    driver.set_window_rect(0, 0, 1280, 900).await?;

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
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}

#[tokio::test]
async fn frequency_value_search_finds_options_outside_the_first_page() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let metadata = (0..80)
        .map(|n| vec![("genre".into(), scalar(&format!("v{:02}", n / 2)))].into_boxed_slice())
        .collect();
    let served = serve_metadata(&[], metadata).await;
    let result: WebDriverResult<()> = async {
    open(&driver, &served.base, ROWS).await?;
    driver.find(By::Css("button.add")).await?.click().await?;
    driver
        .find(By::Css("[data-add=genre]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelectorAll('.fq-pop .opts input').length === 30",
        "first value page",
    )
    .await?;
    assert!(
        driver
            .find_all(By::Css(".fq-pop [data-v=v39]"))
            .await?
            .is_empty()
    );
    let search = driver.find(By::Css("[data-find]")).await?;
    search.send_keys("v39").await?;
    wait_for(&driver, "return document.querySelectorAll('.fq-pop .opts input').length === 1 && document.querySelector('.fq-pop [data-v=v39]')", "server-side value search").await?;
    assert_eq!(search.prop("value").await?.as_deref(), Some("v39"));
    search.clear().await?;
    search.send_keys("v00").await?;
    wait_for(&driver, "return document.querySelector('.fq-pop [data-v=v00]')?.closest('label').querySelector('.v').textContent === '6'", "noun facet counts").await?;
    driver.execute("const select = document.querySelector('[data-b=pattern]'); select.value = 'verb'; select.dispatchEvent(new Event('change', {bubbles:true}));", Vec::new()).await?;
    wait_for(&driver, "return document.querySelector('.fq-pop [data-v=v00]')?.closest('label').querySelector('.v').textContent === '1'", "refreshed verb facet counts").await?;
    wait_for(&driver, "return document.querySelector('table.freq tbody td:nth-child(2)')?.textContent === '見る'", "verb query completion").await?;
    assert_eq!(driver.execute("return document.activeElement.matches('[data-find]') && document.activeElement.value === 'v00'", Vec::new()).await?.json(), &json!(true));
    driver.execute("document.querySelector('[data-find]').value = 'v39'; document.querySelector('[data-find]').dispatchEvent(new Event('input', {bubbles:true}));", Vec::new()).await?;
    wait_for(
        &driver,
        "return document.querySelector('.fq-pop [data-v=v39]')",
        "value outside the first page",
    )
    .await?;

    driver
        .find(By::Css(".fq-pop [data-v=v39]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return decodeURIComponent(location.hash).includes('v39')",
        "selected value address",
    )
    .await?;
    wait_for(&driver, "return document.querySelector('.fq-empty')", "selected query completion").await?;
    driver.execute("document.body.click()", Vec::new()).await?;
    driver
        .find(By::Css("[data-b=genre]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('.fq-pop [data-v=v39]:checked')",
        "selection beyond the first page",
    )
    .await?;
    assert_no_page_errors(&driver).await?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
        Ok(())
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

const LABEL_FILE: &str = r#"{
  "vocabulary": "test.fixture",
  "edition": "1",
  "source": "the fixture",
  "licence": "MIT",
  "keys": {
    "@document.genre": { "labels": { "en": "form", "ja": "形式" } },
    "@document.year": { "labels": { "en": "year of publication", "ja": "刊行年" } },
    "@document.author_birth_place_city": { "labels": { "en": "city <b>&\"" } }
  }
}"#;

const SHOWN: &str = r"
const names = (() => { NAMES })();
document.querySelector('button.add').click();
const menu = [...document.querySelectorAll('.fq-pop [data-add]')].map((b) => [b.dataset.add, b.textContent]);
document.querySelector('button.add').click();
return {
  names,
  sentence: document.querySelector('[data-fq-sentence]').textContent,
  menu: Object.fromEntries(menu),
  command: document.querySelector('[data-fq-clause] code').textContent,
  href: location.href,
};";

#[tokio::test]
async fn frequency_view_names_keys_by_their_labels_in_english_and_japanese() -> WebDriverResult<()>
{
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let labels = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(labels.path(), LABEL_FILE).unwrap();
    let served = serve(&["--metadata-labels".as_ref(), labels.path().as_os_str()]).await;
    let script = SHOWN.replace("NAMES", NAMES);
    let url = format!(
        "{}#{}",
        served.base,
        "w=%7B%22genre%22%3A%7B%22m%22%3A%22in%22%2C%22v%22%3A%5B%22novel%22%5D%7D%2C%22year%22%3A%7B%22m%22%3A%22range%22%2C%22lo%22%3A1960%7D%7D"
    );
    let settled = "return document.querySelectorAll('.role .n:not(:empty)').length === 2";
    open(&driver, &url, settled).await?;
    let english = driver.execute(&script, Vec::new()).await?.json().clone();

    driver
        .find(By::Css("[data-locale-toggle]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('button.add')?.textContent === '＋ 条件'",
        "the Japanese sentence",
    )
    .await?;
    let japanese = driver.execute(&script, Vec::new()).await?.json().clone();

    assert_eq!(
        english["names"],
        json!({"genre": "form", "year": "year of publication"})
    );
    assert_eq!(
        english["menu"],
        json!({
            "author_birth_place_city": "city <b>&\"",
            "author_birth_place_prefecture": "author birth place prefecture",
        })
    );
    assert_eq!(
        japanese["names"],
        json!({"genre": "形式", "year": "刊行年"})
    );
    assert_eq!(
        japanese["menu"],
        json!({
            "author_birth_place_city": "author_birth_place_city",
            "author_birth_place_prefecture": "author_birth_place_prefecture",
        })
    );
    let english_sentence = english["sentence"].as_str().unwrap();
    let japanese_sentence = japanese["sentence"].as_str().unwrap();
    assert!(
        english_sentence.contains("with form ")
            && english_sentence.contains("with year of publication "),
        "{english_sentence}"
    );
    assert!(
        japanese_sentence.contains("形式が") && japanese_sentence.contains("刊行年が"),
        "{japanese_sentence}"
    );
    assert!(
        japanese_sentence.contains("1960以上の文書における"),
        "{japanese_sentence}"
    );
    for sentence in [english_sentence, japanese_sentence] {
        assert!(!sentence.contains("genre"), "{sentence}");
    }

    let command = english["command"].as_str().unwrap();
    assert!(
        command.contains(r#"--where '@document.genre = "novel" & @document.year >= 1960'"#),
        "{command}"
    );
    assert_eq!(japanese["command"], english["command"]);
    assert_eq!(english["href"], json!(url));
    assert_eq!(japanese["href"], json!(url));

    driver.find(By::Css("button.add")).await?.click().await?;
    driver
        .find(By::Css("[data-add='author_birth_place_city']"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('.fq-pop:not([hidden]) .opts input')",
        "the city values",
    )
    .await?;
    driver
        .find(By::Css(".fq-pop .opts input"))
        .await?
        .click()
        .await?;
    let command = wait_for(
        &driver,
        "const code = document.querySelector('[data-fq-clause] code')?.textContent ?? ''; return code.includes('author_birth_place_city') && code",
        "the picked city in the command",
    )
    .await?;
    assert!(
        command
            .as_str()
            .unwrap()
            .contains(r#"--where '@document.author_birth_place_city = "Chiyoda" & "#),
        "{command}"
    );
    driver
        .find(By::Css("body"))
        .await?
        .send_keys(Key::Escape)
        .await?;
    assert_eq!(
        driver.execute(NAMES, Vec::new()).await?.json()["author_birth_place_city"],
        "author_birth_place_city"
    );

    driver
        .find(By::Css("[data-locale-toggle]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('button.add')?.textContent === '+ condition'",
        "the English sentence",
    )
    .await?;
    assert_eq!(
        driver.execute(NAMES, Vec::new()).await?.json()["author_birth_place_city"],
        "city <b>&\""
    );
    assert_eq!(
        driver
            .execute(
                "return document.querySelectorAll('[data-fq-sentence] b, .fq-pop b').length",
                Vec::new()
            )
            .await?
            .json(),
        &json!(0)
    );

    assert_no_page_errors(&driver).await?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}

#[tokio::test]
async fn exclusions_explain_missing_fields_in_both_languages() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let mut rows = metadata();
    rows[4] = rows[4]
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                if key == "genre" {
                    scalar("essay")
                } else {
                    value.clone()
                },
            )
        })
        .collect();
    rows[5] = rows[5]
        .iter()
        .filter(|(key, _)| key != "genre")
        .cloned()
        .collect();
    let served = serve_metadata(&[], rows).await;
    let url = format!(
        "{}#w=%7B%22genre%22%3A%7B%22m%22%3A%22out%22%2C%22v%22%3A%5B%22novel%22%5D%7D%7D",
        served.base
    );
    let tooltip =
        "return document.querySelector('[data-share=genre]')?.parentElement.dataset.full || null";
    open(&driver, &url, tooltip).await?;
    let english = driver.execute(tooltip, Vec::new()).await?.json().clone();
    assert!(
        english
            .as_str()
            .unwrap()
            .contains("1 document lacks genre and is excluded")
    );
    driver
        .find(By::Css("[data-locale-toggle]"))
        .await?
        .click()
        .await?;
    let japanese = wait_for(&driver, "return document.querySelector('[data-share=genre]')?.parentElement.dataset.full?.includes('genreがない1件')", "Japanese missing-field explanation").await?;
    assert_eq!(japanese, json!(true));
    assert_no_page_errors(&driver).await?;
    driver.quit().await
}

const ENTRY_CATALOG: &str = "pattern noun := [text in {猫, 本}].\n";

/// Each document's title, genre and year. Titles share whole units and
/// brackets, and each is two documents' so the view offers it as a
/// condition; one document has no genre.
const WORKS: [(&str, Option<&str>, u32); 8] = [
    (
        "日本文学全集　第1巻（小説・物語・随筆）",
        Some("novel"),
        1950,
    ),
    ("日本文学全集　第1巻（詩歌・俳句）", Some("novel"), 1953),
    (
        "日本文学全集　第2巻（小説・物語・随筆）",
        Some("essay"),
        1956,
    ),
    (
        "世界文学全集　第1巻（小説・物語・随筆）",
        Some("poem"),
        1959,
    ),
    (
        "日本文学全集　第1巻（小説・物語・随筆）",
        Some("essay"),
        1962,
    ),
    ("日本文学全集　第1巻（詩歌・俳句）", None, 1965),
    (
        "日本文学全集　第2巻（小説・物語・随筆）",
        Some("poem"),
        1968,
    ),
    (
        "世界文学全集　第1巻（小説・物語・随筆）",
        Some("novel"),
        1971,
    ),
];

/// The sentences of document `document`: each has 猫, and every other one 本.
fn work_sentences(document: usize) -> Vec<Vec<&'static str>> {
    (0..=(document % 3))
        .map(|sentence| {
            if (document + sentence).is_multiple_of(2) {
                vec!["猫", "が", "本"]
            } else {
                vec!["猫", "が", "鳥"]
            }
        })
        .collect()
}

/// The count table `ling summarize --write-table` writes for
/// `ENTRY_CATALOG` over the works: each entry's occurrences per document
/// and the corpus sentences it occurs in.
fn write_entry_table(artifact: &std::path::Path, destination: &std::path::Path) {
    let documents = (0..WORKS.len())
        .map(|document| DocumentTotals {
            document_id: document as u64,
            sentences: work_sentences(document).len() as u64,
            tokens: work_sentences(document).iter().map(Vec::len).sum::<usize>() as u64,
        })
        .collect::<Vec<_>>();
    let values = ["本", "猫"];
    let mut writer = CountTableWriter::create(
        destination,
        table_key(artifact, ENTRY_CATALOG),
        vec!["noun".into()],
        vec![GroupSchema {
            kind: GroupKind::Surface,
            names: Vec::new(),
        }],
        &values,
        &documents,
    )
    .unwrap();
    for (value, surface) in values.iter().enumerate() {
        writer.begin_entry(0, 0, &[value as u32]).unwrap();
        let mut ordinal = 0u64;
        for document in 0..WORKS.len() {
            for sentence in work_sentences(document) {
                if sentence.contains(surface) {
                    writer.add_sentence(document as u32, ordinal, 1).unwrap();
                }
                ordinal += 1;
            }
        }
    }
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
}

async fn serve_entries() -> Served {
    let metadata = WORKS
        .iter()
        .map(|(title, genre, year)| {
            let mut fields = vec![
                ("title".to_owned(), scalar(title)),
                ("year".to_owned(), number(*year)),
            ];
            if let Some(genre) = genre {
                fields.push(("genre".to_owned(), scalar(genre)));
            }
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            fields.into_boxed_slice()
        })
        .collect();
    let artifact = support::build_artifact_with_metadata(
        (0..WORKS.len())
            .map(|document| {
                let sentences = work_sentences(document)
                    .into_iter()
                    .map(|words| {
                        words
                            .into_iter()
                            .map(|word| TokenInfo::default().with_orth(word))
                            .collect()
                    })
                    .collect();
                ("doc.md", document as u64, false, sentences)
            })
            .collect(),
        Vec::new(),
        metadata,
    );
    serve_artifact(artifact, ENTRY_CATALOG, write_entry_table, &[]).await
}

const ENTRY_READY: &str = "const e = document.querySelector('[data-fq-entry]'); \
    return !!e.querySelector('[data-e=profile]') && !e.hasAttribute('aria-busy')";

/// Each bar label of the entry's profile, as `assert_bar_labels` reads them.
const BAR_LABELS: &str = "return [...document.querySelectorAll('.fq-chart')].map((chart) => \
    [...chart.querySelectorAll('.fq-bars .name')].map((name) => ({full: name.dataset.full, shown: name.textContent, \
      fits: name.firstChild.scrollWidth <= name.firstChild.clientWidth})))";

/// No two values of a chart share a visible label; a shortened label
/// replaces whole units with one “…”, fits, and keeps its brackets
/// balanced. Returns how many labels are shortened.
fn assert_bar_labels(charts: &Value) -> usize {
    const SEPARATORS: [char; 4] = ['、', '・', ' ', '　'];
    let mut shortened = 0;
    for chart in charts.as_array().unwrap() {
        let labels = chart.as_array().unwrap();
        let shown = labels
            .iter()
            .map(|label| label["shown"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(shown.len(), labels.len(), "a shared label: {labels:?}");
        for label in labels {
            let (full, shown) = (
                label["full"].as_str().unwrap(),
                label["shown"].as_str().unwrap(),
            );
            if shown == full {
                continue;
            }
            shortened += 1;
            assert_eq!(label["fits"], true, "{shown}");
            assert_eq!(shown.matches('…').count(), 1, "{shown}");
            let (left, right) = shown.split_once('…').unwrap();
            assert!(full.starts_with(left), "{shown} of {full}");
            let unit_start = |at: usize| {
                at == 0
                    || full[..at].ends_with(|c: char| SEPARATORS.contains(&c) || "（）".contains(c))
                    || full[at..].starts_with(|c: char| SEPARATORS.contains(&c) || c == '（')
            };
            assert!(unit_start(left.len()), "{shown} cuts a unit of {full}");
            if !right.chars().all(|c| c == '）') {
                assert!(full.ends_with(right), "{shown} of {full}");
                assert!(
                    unit_start(full.len() - right.len()),
                    "{shown} cuts a unit of {full}"
                );
            }
            for (open, close) in [('（', '）'), ('(', ')'), ('「', '」')] {
                assert_eq!(
                    shown.matches(open).count(),
                    shown.matches(close).count(),
                    "{shown}: unbalanced {open}{close}"
                );
            }
        }
    }
    shortened
}

#[tokio::test]
async fn an_entry_profile_previews_without_moving_the_page_and_names_every_value()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let base = format!("{}#pattern=noun", served.base);
    open(&driver, &base, ROWS).await?;
    driver
        .find(By::Css("table.freq tbody tr:first-child button.open-entry"))
        .await?
        .click()
        .await?;
    wait_for(&driver, ENTRY_READY, "the entry").await?;
    let shown = driver
        .execute(
            "return {href: location.href, entry: new URLSearchParams(location.hash.slice(1)).get('entry'), \
              name: document.querySelector('.fq-entry-name').textContent, \
              figures: document.querySelector('[data-e=figures]').textContent, \
              charts: [...document.querySelectorAll('.fq-chart figcaption')].map((c) => c.textContent), \
              lines: [...document.querySelectorAll('.fq-kwic li')].map((l) => l.querySelector('.kn').textContent + ' ' + l.querySelector('.src').textContent), \
              none: [...document.querySelectorAll('.fq-chart[data-key=genre] [data-none]')].map((b) => b.getAttribute('aria-label'))}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    let sentences = (0..WORKS.len())
        .map(|document| work_sentences(document).len())
        .collect::<Vec<_>>();
    let cats = sentences.iter().sum::<usize>();
    assert_eq!(shown["entry"], json!(r#"{"kind":"surface","value":"猫"}"#));
    assert_eq!(shown["name"], "猫");
    assert_eq!(
        shown["figures"],
        json!(format!("{cats} occurrences in {} documents", WORKS.len()))
    );
    assert_eq!(shown["charts"], json!(["genre", "title", "year"]));
    assert_eq!(
        shown["lines"],
        json!(
            sentences
                .iter()
                .enumerate()
                .flat_map(|(document, n)| std::iter::repeat_n(
                    format!("猫 document {document}"),
                    *n
                ))
                .collect::<Vec<_>>()
        )
    );
    assert_eq!(
        shown["none"],
        json!([format!(
            "no value: {} occurrences in 1 document",
            sentences[5]
        )])
    );
    assert!(assert_bar_labels(&driver.execute(BAR_LABELS, Vec::new()).await?.json().clone()) > 0);

    // The address reproduces the entry.
    let href = shown["href"].as_str().unwrap().to_owned();
    open(&driver, &href, ENTRY_READY).await?;
    assert_eq!(
        driver
            .execute(
                "return [location.href, document.querySelector('.fq-entry-name').textContent]",
                Vec::new()
            )
            .await?
            .json(),
        &json!([href, "猫"])
    );

    // A sweep over every bar at 30 ms each, resting at each end, moves
    // nothing on the page and requests previews only where it rests. The
    // observer starts two frames after the entry is drawn, because Chrome
    // reports a layout shift in the frame after the layout that caused it.
    driver
        .execute_async(
            "const done = arguments[arguments.length - 1]; requestAnimationFrame(() => requestAnimationFrame(() => done(true)));",
            Vec::new(),
        )
        .await?;
    driver
        .execute(
            "window.__shift = 0; window.__moved = []; window.__previews = 0; window.__requests = 0; \
             const fetched = window.fetch; window.fetch = (...a) => { if (String(a[0]).includes('frequency/')) __requests++; return fetched(...a); }; \
             new PerformanceObserver((list) => { for (const e of list.getEntries()) if (!e.hadRecentInput) { __shift += e.value; __moved.push([Math.round(e.startTime), ...(e.sources || []).map((s) => [s.node?.className, Math.round(s.previousRect.y), Math.round(s.currentRect.y)])]); } }).observe({type: 'layout-shift'}); \
             new MutationObserver(() => { if (document.querySelector('.fq-entry.previewing')) __previews++; }).observe(document.querySelector('[data-fq-entry]'), {attributes: true, subtree: true, childList: true});",
            Vec::new(),
        )
        .await?;
    let points = driver
        .execute(
            "return [...document.querySelectorAll('.fq-chart [data-bar], .fq-chart [data-col]')].map((b) => { const r = b.getBoundingClientRect(); return [Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2)]; })",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    let points = points.as_array().unwrap();
    assert!(points.len() > 10, "{points:?}");
    let passes = [points.clone(), points.iter().rev().cloned().collect()];
    for pass in &passes {
        let mut sweep = driver.action_chain_with_delay(None, Some(Duration::from_millis(30)));
        for point in pass {
            sweep = sweep.move_to(point[0].as_i64().unwrap(), point[1].as_i64().unwrap());
        }
        sweep.perform().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let swept = driver
        .execute(
            "return {shift: __shift, moved: __moved, previews: __previews, requests: __requests}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(swept["shift"], json!(0), "{swept}");
    assert!(swept["previews"].as_u64().unwrap() > 0, "{swept}");
    // A preview requests a profile and lines: two per rest, against two per
    // bar without the rest delay.
    let requests = swept["requests"].as_u64().unwrap();
    assert!(
        requests <= 2 * (passes.len() as u64 + 2),
        "{requests} requests over {} bars: {swept}",
        2 * points.len()
    );
    assert!(assert_bar_labels(&driver.execute(BAR_LABELS, Vec::new()).await?.json().clone()) > 0);

    // Setting and clearing a condition with the entry open moves nothing
    // after the entry redraws, though the table's response comes 600 ms late.
    driver
        .execute(
            "window.__late = []; window.__clicked = 0; \
             window.__fetch = window.fetch; window.fetch = async (...a) => { const r = await __fetch(...a); if (/api\\/frequency\\?/.test(String(a[0]))) await new Promise((done) => setTimeout(done, 600)); return r; }; \
             new PerformanceObserver((list) => { for (const e of list.getEntries()) if (e.startTime - __clicked > 300) __late.push(e.value); }).observe({type: 'layout-shift'});",
            Vec::new(),
        )
        .await?;
    for _ in 0..2 {
        driver
            .execute(
                "window.__clicked = performance.now(); document.querySelector('.fq-chart[data-key=genre] [data-bar]').click()",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry under the condition").await?;
        tokio::time::sleep(Duration::from_millis(900)).await;
    }
    assert_eq!(
        driver
            .execute("window.fetch = __fetch; return __late", Vec::new())
            .await?
            .json(),
        &json!([])
    );

    // Keyboard: Tab reaches each title bar, whose full name and counts the
    // chart reads out; Arrow keys walk the bars.
    driver
        .execute(
            "document.querySelector('[data-e=back]').focus()",
            Vec::new(),
        )
        .await?;
    let body = driver.find(By::Css("body")).await?;
    let mut reached = Value::Null;
    for _ in 0..40 {
        body.send_keys(Key::Tab).await?;
        reached = driver
            .execute(
                "const a = document.activeElement; return a.matches('.fq-chart[data-key=title] [data-bar]') ? a.closest('.fq-chart').querySelector('[data-readout]').textContent : null",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        if !reached.is_null() {
            break;
        }
    }
    let titles = WORKS[..4].iter().map(|work| work.0).collect::<Vec<_>>();
    let readout = reached.as_str().unwrap().to_owned();
    assert!(
        titles
            .iter()
            .any(|title| readout.starts_with(&format!("{title}: "))),
        "{readout}"
    );
    let mut heard = std::collections::BTreeSet::from([readout]);
    for _ in 1..4 {
        driver.action_chain().send_keys(Key::Down).perform().await?;
        heard.insert(
            driver
                .execute("return document.querySelector('.fq-chart[data-key=title] [data-readout]').textContent", Vec::new())
                .await?
                .json()
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    for title in &titles {
        assert!(
            heard
                .iter()
                .any(|line| line.starts_with(&format!("{title}: "))),
            "{title} in {heard:?}"
        );
    }

    // Touch: a tap on the chart's table toggle shows every full name, and a
    // tap on a bar reads it out and keeps the value.
    driver
        .execute(
            "document.activeElement.blur(); window.scrollTo(0, 0);",
            Vec::new(),
        )
        .await?;
    let tap = async |selector: &str| -> WebDriverResult<()> {
        let at = driver
            .execute(
                &format!("const e = document.querySelector({selector:?}); e.scrollIntoView({{block: 'center'}}); const r = e.getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2];"),
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        for kind in ["touchStart", "touchEnd"] {
            let points = if kind == "touchStart" {
                json!([{"x": at[0], "y": at[1]}])
            } else {
                json!([])
            };
            driver
                .cdp()
                .send_raw(
                    "Input.dispatchTouchEvent",
                    json!({"type": kind, "touchPoints": points}),
                )
                .await?;
        }
        Ok(())
    };
    driver
        .cdp()
        .send_raw(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": true, "maxTouchPoints": 1}),
        )
        .await?;
    tap(".fq-chart[data-key=title] summary").await?;
    let table = wait_for(
        &driver,
        "const d = document.querySelector('.fq-chart[data-key=title] details'); return d.open && [...d.querySelectorAll('tbody td:first-child')].map((c) => c.textContent)",
        "the title table",
    )
    .await?;
    let mut listed = table
        .as_array()
        .unwrap()
        .iter()
        .map(|cell| cell.as_str().unwrap())
        .collect::<Vec<_>>();
    listed.sort_unstable();
    let mut all = titles.clone();
    all.sort_unstable();
    assert_eq!(listed, all);
    tap(".fq-chart[data-key=genre] [data-bar][data-v=essay]").await?;
    wait_for(
        &driver,
        "return decodeURIComponent(location.hash).includes('\"genre\":{\"m\":\"in\",\"v\":[\"essay\"]}')",
        "the tapped value",
    )
    .await?;
    wait_for(&driver, ENTRY_READY, "the entry under essay").await?;
    let after = driver
        .execute(
            "return {readout: document.querySelector('.fq-chart[data-key=genre] [data-readout]').textContent, \
              pressed: document.querySelector('.fq-chart[data-key=genre] [data-v=essay]').getAttribute('aria-pressed'), \
              table: document.querySelector('.fq-chart[data-key=title] details').open}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    let essays = [2, 4]
        .iter()
        .map(|document| sentences[*document])
        .sum::<usize>();
    assert_eq!(
        after,
        json!({"readout": format!("essay: {essays} occurrences in 2 documents"), "pressed": "true", "table": true})
    );
    driver
        .cdp()
        .send_raw(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": false}),
        )
        .await?;

    // Escape returns to the table with the entry's row focused.
    driver
        .find(By::Css("body"))
        .await?
        .send_keys(Key::Escape)
        .await?;
    let back = wait_for(
        &driver,
        "return !document.querySelector('[data-fq-table]').hidden && document.querySelectorAll('table.freq tbody tr').length && document.activeElement.matches('button.open-entry') && [document.activeElement.textContent, location.hash.includes('entry=')]",
        "the table",
    )
    .await?;
    assert_eq!(back, json!(["猫", false]));

    assert_no_page_errors(&driver).await?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}
