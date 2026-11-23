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

pub(crate) async fn record_errors(
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

pub(crate) fn table_key(artifact: &std::path::Path, catalog: &str) -> CountTableKey {
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

pub(crate) async fn wait_for(
    driver: &WebDriver,
    script: &str,
    what: &str,
) -> WebDriverResult<Value> {
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

pub(crate) async fn assert_no_page_errors(driver: &WebDriver) -> WebDriverResult<()> {
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

pub(crate) async fn driver() -> WebDriverResult<Option<WebDriver>> {
    let Some(driver_url) = crate::chromedriver_url().await else {
        return Ok(None);
    };
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = crate::session(&driver_url, capabilities).await?;
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

pub(crate) struct Served {
    pub(crate) base: String,
    statuses: Arc<Mutex<Vec<String>>>,
    pub(crate) server: tokio::task::JoinHandle<()>,
    _artifact: support::ArtifactFixture,
    _dictionary: lingrep_core::testing::MiniDictionary,
}

pub(crate) async fn serve(extra: &[&std::ffi::OsStr]) -> Served {
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
        wait_for(
            &driver,
            &format!("return document.querySelector('.role .n[data-share=\"{key}\"]:not(:empty)')"),
            &format!("the {key} share"),
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

    crate::resize(&driver, 390, 900).await?;
    let labels = wait_for(
        &driver,
        &format!("const labels = (() => {{ {LABELS} }})(); return labels.some((l) => l.shown !== l.full || l.cut) ? labels : null"),
        "a shortened label",
    )
    .await?;
    assert_label_invariants(&labels);
    crate::resize(&driver, 1280, 900).await?;

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
            .starts_with("Section filters are unavailable here"),
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

fn work_sentences(document: usize) -> Vec<Vec<&'static str>> {
    let count = if document == WORKS.len() - 1 {
        LINES
    } else {
        document % 3 + 1
    };
    (0..count)
        .map(|sentence| {
            if (document + sentence).is_multiple_of(2) {
                vec!["猫", "が", "本"]
            } else {
                vec!["猫", "が", "鳥"]
            }
        })
        .collect()
}

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
    serve_entries_with_tags(false).await
}

async fn serve_entries_with_tags(tags: bool) -> Served {
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
            if tags {
                fields.push((
                    "tags".to_owned(),
                    MetadataValue::Scalars(
                        vec![
                            MetadataScalar::String("x".into()),
                            MetadataScalar::String("y".into()),
                        ]
                        .into(),
                    ),
                ));
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

const LINES: usize = 50;

const COUNT_FETCHES: &str = "window.__fetches = {started: 0, settled: 0, profiles: []}; const sent = window.fetch; \
    window.fetch = (...a) => { const url = String(a[0]); if (!url.includes('api/frequency')) return sent(...a); \
      __fetches.started++; if (url.includes('frequency/profile')) __fetches.profiles.push(JSON.parse(new URL(url, location.href).searchParams.get('w') || '{}')); \
      const done = () => __fetches.settled++; \
      return sent(...a).then((r) => { const json = r.json.bind(r); r.json = () => json().finally(done); return r; }, (e) => { done(); throw e; }); };";

const FETCHES_SETTLED: &str = "return __fetches.started === __fetches.settled";

pub(crate) async fn two_frames(driver: &WebDriver) -> WebDriverResult<()> {
    driver
        .execute_async(
            "const done = arguments[arguments.length - 1]; requestAnimationFrame(() => requestAnimationFrame(() => done(true)));",
            Vec::new(),
        )
        .await?;
    Ok(())
}

const ENTRY_READY: &str = "const e = document.querySelector('[data-fq-entry]'); \
    return !!e.querySelector('[data-e=profile]') && !e.hasAttribute('aria-busy')";

const BAR_LABELS: &str = "return [...document.querySelectorAll('.fq-chart')].map((chart) => \
    [...chart.querySelectorAll('.fq-bars .name')].map((name) => ({full: name.dataset.full, shown: name.textContent, \
      fits: name.firstChild.scrollWidth <= name.firstChild.clientWidth})))";

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
async fn a_double_click_on_the_table_more_button_appends_one_page() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__tableRequests = 0; const fetched = window.fetch.bind(window); \
                window.fetch = (input, init) => { const url = String(input); \
                  if (!url.includes('api/frequency?')) return fetched(input, init); \
                  __tableRequests++; return fetched(url.replace('limit=100', 'limit=1'), init); };"}),
        )
        .await?;
    open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
    driver.execute(COUNT_FETCHES, Vec::new()).await?;
    driver
        .execute(
            "window.__tableRequests = 0; const more = document.querySelector('[data-fq-table] > .more'); more.click(); more.click();",
            Vec::new(),
        )
        .await?;
    wait_for(&driver, FETCHES_SETTLED, "the next page").await?;
    two_frames(&driver).await?;
    let table = driver
        .execute(
            "return {requests: __tableRequests, ranks: [...document.querySelectorAll('table.freq tbody tr')].map((row) => row.cells[0].textContent), \
              more: document.querySelector('[data-fq-table] > .more').hidden}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_no_page_errors(&driver).await?;
    driver.quit().await?;
    served.server.abort();
    assert_eq!(
        table,
        json!({"requests": 1, "ranks": ["1", "2"], "more": true})
    );
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
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
              lines: [...document.querySelectorAll('.fq-kwic li')].map((l) => l.querySelector('.kn').textContent + ' ' + l.querySelector('.src > span').textContent), \
              inspect: document.querySelectorAll('.fq-kwic li .src .fq-inspect').length, \
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
    let lines = sentences
        .iter()
        .enumerate()
        .flat_map(|(document, n)| std::iter::repeat_n(format!("猫 document {document}"), *n))
        .collect::<Vec<_>>();
    assert!(lines.len() > LINES);
    assert_eq!(shown["lines"], json!(lines[..LINES]));
    assert_eq!(shown["inspect"], json!(LINES));
    assert_eq!(
        shown["none"],
        json!([format!(
            "no value: {} occurrences in 1 document",
            sentences[5]
        )])
    );
    assert!(assert_bar_labels(&driver.execute(BAR_LABELS, Vec::new()).await?.json().clone()) > 0);

    driver.execute(COUNT_FETCHES, Vec::new()).await?;
    driver
        .execute(
            "const more = document.querySelector('[data-e=more]'); more.click(); more.click();",
            Vec::new(),
        )
        .await?;
    wait_for(&driver, FETCHES_SETTLED, "the next lines").await?;
    assert_eq!(
        driver
            .execute(
                "return {requests: __fetches.started, lines: [...document.querySelectorAll('.fq-kwic li')].map((l) => l.querySelector('.kn').textContent + ' ' + l.querySelector('.src > span').textContent), \
                  more: document.querySelector('[data-e=more]').hidden}",
                Vec::new(),
            )
            .await?
            .json(),
        &json!({"requests": 1, "lines": lines, "more": true})
    );

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

    two_frames(&driver).await?;
    driver.execute(COUNT_FETCHES, Vec::new()).await?;
    driver
        .execute(
            "window.__shift = 0; window.__moved = []; window.__previews = 0; window.__sweep = []; window.__bar = null; \
             const fetched = window.fetch; window.fetch = (...a) => { if (String(a[0]).includes('frequency/')) __sweep.push(['request', performance.now()]); return fetched(...a); }; \
             document.querySelector('[data-fq-entry]').addEventListener('pointerover', (e) => { const b = e.target.closest?.('[data-bar], [data-col]'); \
               const bar = b && `${b.closest('.fq-chart')?.dataset.key} ${b.dataset.v ?? b.dataset.lo}`; \
               if (bar && bar !== __bar) { __bar = bar; __sweep.push(['bar', performance.now()]); } }, true); \
             new PerformanceObserver((list) => { for (const e of list.getEntries()) if (!e.hadRecentInput) { __shift += e.value; __moved.push([Math.round(e.startTime), ...(e.sources || []).map((s) => [s.node?.className, Math.round(s.previousRect.y), Math.round(s.currentRect.y)])]); } }).observe({type: 'layout-shift'}); \
             new MutationObserver(() => { if (document.querySelector('.fq-entry.previewing')) __previews++; }).observe(document.querySelector('[data-fq-entry]'), {attributes: true, subtree: true, childList: true});",
            Vec::new(),
        )
        .await?;
    let points = driver
        .execute(
            "return [...document.querySelectorAll('.fq-chart [data-bar], .fq-chart [data-col]')].map((b) => { const r = b.getBoundingClientRect(); \
               return [Math.round(r.left + r.width / 2), Math.round(r.top + r.height / 2), b.closest('.fq-chart').dataset.key, \
                 b.hasAttribute('data-bar') ? {m: 'in', v: [b.dataset.v]} : {m: 'range', lo: +b.dataset.lo, hi: +b.dataset.hi}]; })",
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
        let rest = pass.last().unwrap();
        wait_for(
            &driver,
            &format!(
                "const last = __fetches.profiles.at(-1), canon = (w) => w && JSON.stringify(Object.keys(w).sort().map((k) => [k, w[k]])); \
                 return __fetches.started === __fetches.settled && !!last && canon(last[{key}]) === canon({want})",
                key = rest[2],
                want = rest[3],
            ),
            "the preview where the sweep rests",
        )
        .await?;
        two_frames(&driver).await?;
    }
    let swept = driver
        .execute(
            "return {shift: __shift, moved: __moved, previews: __previews, sweep: __sweep}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(swept["shift"], json!(0), "{swept}");
    assert!(swept["previews"].as_u64().unwrap() > 0, "{swept}");
    const REST_MS: f64 = 120.0;
    let mut reached = None;
    let mut early = Vec::new();
    for event in swept["sweep"].as_array().unwrap() {
        let at = event[1].as_f64().unwrap();
        if event[0] == "bar" {
            reached = Some(at);
            continue;
        }
        let rested = reached.map(|bar| at - bar);
        if rested.is_none_or(|rested| rested < REST_MS - 1.0) {
            early.push(rested);
        }
    }
    assert!(
        early.is_empty(),
        "requests after resting {early:?} ms on a bar: {swept}"
    );
    assert!(assert_bar_labels(&driver.execute(BAR_LABELS, Vec::new()).await?.json().clone()) > 0);

    driver
        .execute(
            "window.__late = []; window.__clicked = 0; window.__delayed = 0; \
             window.__fetch = window.fetch; window.fetch = async (...a) => { const r = await __fetch(...a); if (/api\\/frequency\\?/.test(String(a[0]))) { await new Promise((done) => setTimeout(done, 600)); __delayed++; } return r; }; \
             new PerformanceObserver((list) => { for (const e of list.getEntries()) if (e.startTime - __clicked > 300) __late.push(e.value); }).observe({type: 'layout-shift'});",
            Vec::new(),
        )
        .await?;
    for clicks in 1..=2 {
        driver
            .execute(
                "window.__clicked = performance.now(); document.querySelector('.fq-chart[data-key=genre] [data-bar]').click()",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry under the condition").await?;
        wait_for(
            &driver,
            &format!("return __delayed >= {clicks} && __fetches.started === __fetches.settled"),
            "the late table",
        )
        .await?;
        two_frames(&driver).await?;
    }
    assert_eq!(
        driver
            .execute("window.fetch = __fetch; return __late", Vec::new())
            .await?
            .json(),
        &json!([])
    );
    driver
        .execute(
            "const pad = document.createElement('div'); pad.id = 'shift-probe'; pad.style.height = '40px'; \
             document.querySelector('[data-fq-entry]').prepend(pad);",
            Vec::new(),
        )
        .await?;
    two_frames(&driver).await?;
    let late = driver
        .execute(
            "document.getElementById('shift-probe').remove(); return __late",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert!(!late.as_array().unwrap().is_empty(), "{late}");

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
    driver.action_chain().move_to(0, 0).perform().await?;
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

    driver
        .find(By::Css("body"))
        .await?
        .send_keys(Key::Escape)
        .await?;
    let back = wait_for(
        &driver,
        "return !document.querySelector('[data-fq-table]').hidden && document.querySelectorAll('table.freq tbody tr').length > 0 && document.activeElement.matches('button.open-entry') && [document.activeElement.textContent, location.hash.includes('entry=')]",
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

#[tokio::test]
async fn escape_in_the_help_dialog_keeps_the_entry_open() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
    driver
        .find(By::Css("table.freq tbody tr:first-child button.open-entry"))
        .await?
        .click()
        .await?;
    wait_for(&driver, ENTRY_READY, "the entry").await?;
    driver
        .find(By::Css("[data-site-help-open]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return document.querySelector('[data-site-help]').open",
        "the help dialog",
    )
    .await?;
    driver
        .action_chain()
        .send_keys(Key::Escape)
        .perform()
        .await?;
    wait_for(
        &driver,
        "return !document.querySelector('[data-site-help]').open",
        "the help dialog to close",
    )
    .await?;
    let entry = driver
        .execute(
            "return [new URLSearchParams(location.hash.slice(1)).has('entry'), !!document.querySelector('[data-fq-entry] [data-e=profile]')]",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    driver.quit().await?;
    served.server.abort();
    assert_eq!(entry, json!([true, true]));
    Ok(())
}

pub(crate) async fn observed(
    driver: &WebDriver,
    script: &str,
    expected: Value,
    what: &str,
) -> WebDriverResult<()> {
    let actual = driver.execute(script, Vec::new()).await?.json().clone();
    if actual == expected {
        return Ok(());
    }
    Err(WebDriverError::ParseError(format!(
        "{what}: {actual}, expected {expected}"
    )))
}

pub(crate) const PAGE_ERRORS: &str = "return window.__pageErrors || []";

#[tokio::test]
async fn the_condition_picker_keeps_keyboard_focus_and_typed_bounds() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve(&[]).await;
    let focused =
        |selector: &str| format!("return !!document.activeElement?.matches('{selector}')");
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver.execute(COUNT_FETCHES, Vec::new()).await?;
        driver
            .execute(
                "window.__held = []; window.__holding = true; const counted = window.fetch; \
                 window.__release = () => { __holding = false; for (const send of __held.splice(0)) send(); }; \
                 window.fetch = (input, init) => __holding && String(input).includes('api/frequency?') \
                   ? new Promise((resolve, reject) => __held.push(() => counted(input, init).then(resolve, reject))) \
                   : counted(input, init);",
                Vec::new(),
            )
            .await?;

        driver.find(By::Css("button.add")).await?.click().await?;
        driver
            .find(By::Css(".fq-pop [data-add=year]"))
            .await?
            .click()
            .await?;
        observed(
            &driver,
            &focused(".fq-pop [data-lo]"),
            json!(true),
            "focus on the first bound of a chosen range condition",
        )
        .await?;
        driver
            .action_chain()
            .send_keys("1960")
            .send_keys(Key::Tab)
            .send_keys("19")
            .perform()
            .await?;
        observed(
            &driver,
            "const held = __held.length; __release(); return held;",
            json!(1),
            "table requests held while the upper bound is typed",
        )
        .await?;
        wait_for(
            &driver,
            "return __fetches.started > 0 && __fetches.started === __fetches.settled",
            "the lower bound's reply",
        )
        .await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            "const hi = document.querySelector('.fq-pop [data-hi]'); \
             return [document.querySelector('.fq-pop [data-lo]').value, hi.value, document.activeElement === hi, \
               new URLSearchParams(location.hash.slice(1)).get('w')]",
            json!(["1960", "19", true, r#"{"year":{"m":"range","lo":1960}}"#]),
            "the bounds, focus and conditions after the lower bound's reply",
        )
        .await?;
        driver
            .action_chain()
            .send_keys(Key::Escape)
            .perform()
            .await?;
        observed(
            &driver,
            "return [document.querySelector('.fq-pop').hidden, document.activeElement?.dataset.b, new URLSearchParams(location.hash.slice(1)).get('w')]",
            json!([true, "year", r#"{"year":{"m":"range","lo":1960}}"#]),
            "the popover, focus and conditions after Escape",
        )
        .await?;

        driver.find(By::Css("button.add")).await?.click().await?;
        driver
            .find(By::Css(".fq-pop [data-add=genre]"))
            .await?
            .click()
            .await?;
        observed(
            &driver,
            &focused(".fq-pop [data-mode=in]"),
            json!(true),
            "focus on the first control of a chosen value condition",
        )
        .await?;
        driver
            .action_chain()
            .key_down(Key::Shift)
            .send_keys(Key::Tab)
            .key_up(Key::Shift)
            .perform()
            .await?;
        observed(
            &driver,
            &focused("[data-fq-sentence] *"),
            json!(true),
            "focus in the sentence after Shift+Tab from the popover",
        )
        .await?;
        wait_for(
            &driver,
            "return document.querySelector('.fq-pop [data-v=essay]')",
            "the genre values",
        )
        .await?;
        driver
            .find(By::Css(".fq-pop [data-v=essay]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return decodeURIComponent(location.hash).includes('essay') && __fetches.started === __fetches.settled",
            "the value's reply",
        )
        .await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            &focused(".fq-pop [data-v=essay]:checked"),
            json!(true),
            "focus on the ticked value after its reply",
        )
        .await?;
        driver
            .find(By::Css(".fq-pop [data-any]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return !decodeURIComponent(location.hash).includes('essay') && __fetches.started === __fetches.settled",
            "the cleared condition's reply",
        )
        .await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            "return [document.querySelector('.fq-pop').hidden, document.activeElement?.dataset.b]",
            json!([true, "genre"]),
            "the popover and focus after clearing the condition",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_table_reply_leaves_focus_outside_the_open_condition_menu() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve(&[]).await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver.execute(COUNT_FETCHES, Vec::new()).await?;
        driver
            .find(By::Css("button.add"))
            .await?
            .send_keys(Key::Enter)
            .await?;
        observed(
            &driver,
            "return !!document.activeElement?.matches('.fq-pop [data-add]')",
            json!(true),
            "focus on the first item of the opened menu",
        )
        .await?;
        driver
            .action_chain()
            .key_down(Key::Shift)
            .send_keys(Key::Tab)
            .send_keys(Key::Tab)
            .key_up(Key::Shift)
            .send_keys("x")
            .perform()
            .await?;
        wait_for(
            &driver,
            "return __fetches.started > 0 && __fetches.started === __fetches.settled",
            "the filtered table's reply",
        )
        .await?;
        two_frames(&driver).await?;
        driver.action_chain().send_keys("y").perform().await?;
        observed(
            &driver,
            "return [document.activeElement?.dataset.b, document.querySelector('[data-b=q]').value, document.querySelector('.fq-pop').hidden]",
            json!(["q", "xy", false]),
            "focus, the filter text and the menu after the table's reply",
        )
        .await?;
        driver
            .action_chain()
            .send_keys(Key::Tab)
            .send_keys(Key::Tab)
            .send_keys(Key::Escape)
            .perform()
            .await?;
        observed(
            &driver,
            "return [document.querySelector('.fq-pop').hidden, document.activeElement?.dataset.b]",
            json!([true, "add"]),
            "the menu and focus after Escape from a menu item",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_sentence_redraw_keeps_the_filter_text_and_caret_being_typed() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve(&[]).await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__held = []; window.__holding = false; window.__filters = []; \
                window.__release = () => { __holding = false; for (const send of __held.splice(0)) send(); }; \
                const fetched = window.fetch.bind(window); \
                window.fetch = (input, init) => { \
                  const url = String(input); \
                  if (!url.includes('api/frequency?')) return fetched(input, init); \
                  const q = new URL(url, location.href).searchParams.get('q'); \
                  if (q) return fetched(input, init).finally(() => __filters.push(q)); \
                  if (!__holding) return fetched(input, init); \
                  return new Promise((resolve, reject) => __held.push(() => fetched(input, init).then(resolve, reject))); \
                };"}),
        )
        .await?;
    let filter = "const q = document.querySelector('[data-b=q]'); \
        return [q.value, q.selectionStart, q.selectionEnd, document.activeElement === q]";
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute(
                "__holding = true; \
                 const sort = document.querySelector('select[data-b=sort]'); \
                 sort.value = 'key'; sort.dispatchEvent(new Event('change', {bubbles: true})); \
                 const q = window.__typedInto = document.querySelector('[data-b=q]'); \
                 q.focus(); q.value = 'ab'; q.setSelectionRange(1, 1); \
                 q.dispatchEvent(new Event('input', {bubbles: true})); \
                 __release();",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-b=q]') !== __typedInto && document.querySelectorAll('table.freq tbody tr').length > 0",
            "the sentence redrawn by the sort's reply",
        )
        .await?;
        observed(
            &driver,
            filter,
            json!(["ab", 1, 1, true]),
            "the filter's text, caret and focus after the redraw",
        )
        .await?;
        wait_for(
            &driver,
            "return __filters.length > 0",
            "the typed filter's request",
        )
        .await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            &format!("return [__filters, new URLSearchParams(location.hash.slice(1)).get('q'), (() => {{ {filter} }})()]"),
            json!([["ab"], "ab", ["ab", 1, 1, true]]),
            "the filter requests, the address and the filter box after the typed filter's reply",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn every_restyled_button_shows_a_focus_ring_for_keyboard_focus() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    // Keyboard focus indication persists across script focus changes
    // (https://drafts.csswg.org/selectors-4/#the-focus-visible-pseudo).
    let rings = |selectors: &[&str]| {
        format!(
            "return {}.map((selector) => {{ const e = document.querySelector(selector); e?.focus(); \
               if (!e) return [selector, false, null, null]; \
               const style = getComputedStyle(e), box = e.getBoundingClientRect(); \
               const reach = Math.max(0, parseFloat(style.outlineOffset) + parseFloat(style.outlineWidth)); \
               let clipped = null; \
               for (let a = e.parentElement; a && !clipped; a = a.parentElement) {{ \
                 const s = getComputedStyle(a), outer = a.getBoundingClientRect(), px = parseFloat; \
                 if (s.overflowX !== 'visible' && (box.left - reach < outer.left + px(s.borderLeftWidth) || box.right + reach > outer.right - px(s.borderRightWidth))) clipped = a.className || a.tagName; \
                 if (s.overflowY !== 'visible' && (box.top - reach < outer.top + px(s.borderTopWidth) || box.bottom + reach > outer.bottom - px(s.borderBottomWidth))) clipped = a.className || a.tagName; \
               }} \
               return [selector, e.matches(':focus-visible'), style.outlineStyle, clipped]; }})",
            json!(selectors)
        )
    };
    let ringed = |selectors: &[&str]| {
        Value::Array(
            selectors
                .iter()
                .map(|selector| json!([selector, true, "solid", null]))
                .collect(),
        )
    };
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .find(By::Css("body"))
            .await?
            .send_keys(Key::Tab)
            .await?;
        let table = ["button.add", "[data-c=edit]", "button.open-entry"];
        driver
            .execute(
                "document.querySelector('[data-fq-machinery]').open = true",
                Vec::new(),
            )
            .await?;
        observed(&driver, &rings(&table), ringed(&table), "table view").await?;
        driver
            .execute("document.querySelector('button.add').click()", Vec::new())
            .await?;
        let menu = [".fq-pop [data-add=genre]"];
        observed(&driver, &rings(&menu), ringed(&menu), "condition menu").await?;
        driver
            .execute(
                "document.querySelector('.fq-pop [data-add=genre]').click()",
                Vec::new(),
            )
            .await?;
        let values = [
            ".fq-pop [data-mode=in]",
            ".fq-pop [data-mode=out]",
            ".fq-pop [data-any]",
        ];
        observed(&driver, &rings(&values), ringed(&values), "value popover").await?;
        driver
            .execute(
                "document.querySelector('button.open-entry').click()",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        let entry = ["[data-bar]", "[data-col]", "[data-e=more]"];
        observed(&driver, &rings(&entry), ringed(&entry), "entry view").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_filter_reload_during_an_entry_load_still_shows_the_entry() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__held = []; window.__holding = false; window.__tables = 0; \
                window.__release = () => { __holding = false; for (const send of __held.splice(0)) send(); }; \
                const fetched = window.fetch.bind(window); \
                window.fetch = (input, init = {}) => { \
                  const url = String(input); \
                  if (url.includes('api/frequency?')) __tables++; \
                  if (!__holding || !url.includes('frequency/profile')) return fetched(input, init); \
                  return new Promise((resolve, reject) => { \
                    init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))); \
                    __held.push(() => fetched(input, init).then(resolve, reject)); \
                  }); \
                };"}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute(
                "__holding = true; __tables = 0; document.querySelector('button.open-entry').click(); \
                 const q = document.querySelector('[data-b=q]'); q.value = '猫'; q.dispatchEvent(new Event('input', {bubbles: true}));",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, "return __tables === 2", "the filter's table request").await?;
        driver.execute("__release()", Vec::new()).await?;
        wait_for(&driver, ENTRY_READY, "the entry after the filter reload").await?;
        observed(
            &driver,
            "return document.querySelector('.fq-entry-name').textContent",
            json!("猫"),
            "the entry's name",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn an_unparsable_clause_in_an_entry_address_shows_the_parse_error() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let url = format!(
        "{}#pattern=noun&entry=%7B%22kind%22%3A%22surface%22%2C%22value%22%3A%22%E7%8C%AB%22%7D&where=%40%40",
        served.base
    );
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": COUNT_FETCHES}),
        )
        .await?;
    open(
        &driver,
        &url,
        "return __fetches.started > 1 && __fetches.started === __fetches.settled",
    )
    .await?;
    two_frames(&driver).await?;
    let state = driver.execute(STATE, Vec::new()).await?.json().clone();
    driver.quit().await?;
    served.server.abort();
    assert!(
        state.as_str().unwrap().starts_with("Invalid condition: "),
        "{state}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_next_page_is_reported_in_the_state_line() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__fail = false; const fetched = window.fetch.bind(window); \
                window.fetch = (input, init) => { const url = String(input); \
                  if (__fail && url.includes('offset=')) return Promise.resolve(Response.json({error: 'no next page'}, {status: 500})); \
                  return fetched(url.includes('api/frequency?') ? url.replace('limit=100', 'limit=1') : input, init); };"}),
        )
        .await?;
    let reported = "const more = document.querySelector(arguments[0]); \
        return document.querySelector('[data-fq-state]').textContent === 'no next page' && !more.disabled && !more.hidden";
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        for (more, ready) in [
            ("[data-fq-table] > .more", ROWS),
            ("[data-e=more]", ENTRY_READY),
        ] {
            wait_for(&driver, ready, more).await?;
            driver
                .execute(
                    "__fail = true; document.querySelector(arguments[0]).click()",
                    vec![json!(more)],
                )
                .await?;
            wait_for(
                &driver,
                &reported.replace("arguments[0]", &json!(more).to_string()),
                &format!("the failure of {more} in the state line, with the button enabled"),
            )
            .await?;
            observed(&driver, PAGE_ERRORS, json!([]), "page errors").await?;
            driver
                .execute(
                    "__fail = false; document.querySelector('button.open-entry')?.click()",
                    Vec::new(),
                )
                .await?;
        }
        Ok(())
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_late_or_failed_metadata_reply_raises_no_page_error_on_the_frequency_page()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve(&[]).await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "try { localStorage.setItem('lingrep-web:locale:v1', 'ja'); } catch {} \
                window.__held = []; window.__release = () => { for (const send of __held.splice(0)) send(); }; \
                const fetched = window.fetch.bind(window); \
                window.fetch = (input, init) => { \
                  if (!String(input).includes('frequency/meta')) return fetched(input, init); \
                  if (sessionStorage.getItem('fail-meta')) return Promise.resolve(Response.json({error: 'no metadata'}, {status: 500})); \
                  return new Promise((resolve, reject) => __held.push(() => fetched(input, init).then(resolve, reject))); \
                };"}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        open(
            &driver,
            &format!("{}#pattern=noun", served.base),
            "return document.documentElement.lang === 'ja' && __held.length === 1",
        )
        .await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            PAGE_ERRORS,
            json!([]),
            "page errors before the metadata reply",
        )
        .await?;
        driver.execute("__release()", Vec::new()).await?;
        wait_for(&driver, ROWS, "rows after the metadata reply").await?;
        driver
            .find(By::Css("[data-locale-toggle]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('button.add')?.textContent === '+ condition'",
            "the locale toggle to switch the page to English",
        )
        .await?;

        driver
            .execute(
                "sessionStorage.setItem('fail-meta', '1'); location.reload()",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-fq-state]')?.textContent === 'no metadata'",
            "the metadata failure in the state line",
        )
        .await?;
        observed(
            &driver,
            PAGE_ERRORS,
            json!([]),
            "page errors after the metadata failure",
        )
        .await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn superseded_frequency_requests_are_aborted() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        observed(
            &driver,
                "window.__signals = []; window.__held = []; const fetched = window.fetch; \
                 window.fetch = (url, init = {}) => { \
                   if (!/api\\/frequency(\\/profile|\\/lines)?\\?/.test(String(url))) return fetched(url, init); \
                   __signals.push([String(url).match(/frequency(?:\\/(\\w+))?\\?/)[1] ?? 'table', init.signal]); \
                   return new Promise((resolve, reject) => { \
                     init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))); \
                     __held.push(() => fetched(url, init).then(resolve, reject)); \
                   }); \
                 }; \
                 document.querySelector('button.open-entry').click(); \
                 document.querySelector('[data-e=back], [data-fq-sentence]').dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true})); \
                 for (const sort of ['works', 'key']) { \
                   const select = document.querySelector('select[data-b=sort]'); \
                   select.value = sort; select.dispatchEvent(new Event('change', {bubbles: true})); \
                 } \
                 return __signals.map(([kind, signal]) => [kind, signal?.aborted ?? null]);",
            json!([
                ["profile", true],
                ["lines", true],
                ["table", true],
                ["table", true],
                ["table", true],
                ["table", false]
            ]),
            "each request and whether it was aborted",
        )
        .await?;
        driver
            .execute("for (const send of __held.splice(0)) send();", Vec::new())
            .await?;
        wait_for(&driver, ROWS, "rows in key order").await?;
        observed(&driver, STATE, Value::Null, "the state line").await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_locale_switch_during_a_next_page_request_keeps_its_button_disabled()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__held = []; window.__holding = false; window.__more = 0; \
                window.__release = () => { __holding = false; for (const send of __held.splice(0)) send(); }; \
                const fetched = window.fetch.bind(window); \
                window.fetch = (input, init = {}) => { \
                  let url = String(input); \
                  if (url.includes('api/frequency?')) url = url.replace('limit=100', 'limit=1'); \
                  if (!url.includes('offset=')) return fetched(url, init); \
                  __more++; \
                  if (!__holding) return fetched(url, init); \
                  return new Promise((resolve, reject) => { \
                    init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))); \
                    __held.push(() => fetched(url, init).then(resolve, reject)); \
                  }); \
                };"}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        for (more, redrawn, items, total) in [
            (
                "[data-fq-table] > .more",
                "return document.querySelector('table.freq th').textContent === '順位'",
                "table.freq tbody tr",
                2,
            ),
            (
                "[data-e=more]",
                "return document.querySelector('.fq-lines h3').textContent.startsWith('Lines')",
                ".fq-kwic li",
                LINES + 13,
            ),
        ] {
            driver
                .execute(
                    "__more = 0; __holding = true; document.querySelector(arguments[0]).click()",
                    vec![json!(more)],
                )
                .await?;
            driver
                .find(By::Css("[data-locale-toggle]"))
                .await?
                .click()
                .await?;
            wait_for(&driver, redrawn, "the page in the other language").await?;
            driver
                .execute(
                    "document.querySelector(arguments[0]).click(); __release()",
                    vec![json!(more)],
                )
                .await?;
            wait_for(
                &driver,
                &format!("return document.querySelectorAll('{items}').length >= {total}"),
                "the next page",
            )
            .await?;
            two_frames(&driver).await?;
            observed(
                &driver,
                &format!("return [__more, document.querySelectorAll('{items}').length]"),
                json!([1, total]),
                &format!("requests and items after two clicks on {more}"),
            )
            .await?;
            driver
                .execute(
                    "document.querySelector('button.open-entry')?.click()",
                    Vec::new(),
                )
                .await?;
            wait_for(&driver, ENTRY_READY, "the entry").await?;
        }
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn a_superseded_next_page_request_leaves_its_button_enabled() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__held = []; window.__holding = false; window.__failFilter = false; window.__filtered = 0; \
                const fetched = window.fetch.bind(window); \
                window.fetch = (input, init = {}) => { \
                  let url = String(input); \
                  if (url.includes('api/frequency?')) { \
                    url = url.replace('limit=100', 'limit=1'); \
                    if (/[?&]q=/.test(url)) { \
                      if (__failFilter) return Promise.resolve(Response.json({error: 'no filtered table'}, {status: 500})); \
                      return fetched(url, init).finally(() => __filtered++); \
                    } \
                  } \
                  if (!__holding || !url.includes('offset=')) return fetched(url, init); \
                  return new Promise((resolve, reject) => { \
                    init.signal?.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError'))); \
                    __held.push(() => fetched(url, init).then(resolve, reject)); \
                  }); \
                };"}),
        )
        .await?;
    let page_then_filter = "__holding = true; document.querySelector(arguments[0]).click(); \
        const q = document.querySelector('[data-b=q]'); q.value = '猫'; q.dispatchEvent(new Event('input', {bubbles: true}));";
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute(
                "document.querySelector('button.open-entry').click()",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        driver
            .execute(page_then_filter, vec![json!("[data-e=more]")])
            .await?;
        wait_for(
            &driver,
            "return __filtered === 1 && !document.querySelector('[data-e=more]').disabled",
            "the lines button enabled after the filter superseded its request",
        )
        .await?;
        observed(
            &driver,
            "return [document.querySelectorAll('.fq-kwic li').length, document.querySelector('[data-fq-state]').textContent]",
            json!([LINES, ""]),
            "the lines and the state line after the superseded request",
        )
        .await?;
        driver
            .execute(
                "__holding = false; document.querySelector('[data-e=more]').click()",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            &format!(
                "return document.querySelectorAll('.fq-kwic li').length === {}",
                LINES + 13
            ),
            "the next page of lines",
        )
        .await?;

        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute(
                &format!("__failFilter = true; {page_then_filter}"),
                vec![json!("[data-fq-table] > .more")],
            )
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-fq-state]').textContent === 'no filtered table' \
               && !document.querySelector('[data-fq-table] > .more').disabled",
            "the table button enabled after the superseding filter request failed",
        )
        .await?;
        observed(
            &driver,
            "return document.querySelectorAll('table.freq tbody tr').length",
            json!(1),
            "rows after the superseded request",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    Ok(())
}

#[tokio::test]
async fn malformed_url_conditions_show_an_error_and_allow_reopening_the_view() -> WebDriverResult<()>
{
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    crate::resize(&driver, 390, 844).await?;
    let served = serve(&[]).await;
    for condition in [
        json!({"genre": {"m": "in", "v": "ab"}}),
        json!({"year": {"m": "range", "lo": {"value": 1900}}}),
    ] {
        let params = serde_urlencoded::to_string([("w", condition.to_string())]).unwrap();
        tokio::time::timeout(
            Duration::from_secs(8),
            open(&driver, &format!("{}#{params}", served.base), STATE),
        )
        .await
        .expect("malformed address produces an error state within 8 seconds")?;
        assert_no_page_errors(&driver).await?;
        let message = driver.execute(STATE, vec![]).await?.json().clone();
        assert!(
            message.as_str().is_some_and(|text| !text.is_empty()),
            "{message}"
        );
        open(&driver, &served.base, ROWS).await?;
        assert_no_page_errors(&driver).await?;
    }
    driver.quit().await?;
    Ok(())
}

#[tokio::test]
async fn frequency_profiles_explain_overlapping_values_and_help_covers_analysis_pages()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries_with_tags(true).await;
    let result: WebDriverResult<()> = async {
        crate::resize(&driver, 390, 900).await?;
        driver.goto(format!("{}#pattern=noun", served.base)).await?;
        wait_for(
            &driver,
            "return !!document.querySelector('button.open-entry')",
            "frequency entries",
        )
        .await?;
        driver
            .find(By::Css("button.open-entry"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return !!document.querySelector('.fq-chart[data-key=tags] .fq-multivalue-note')",
            "the explanation of overlapping values",
        )
        .await?;
        let note = driver
            .find(By::Css(".fq-chart[data-key=tags] .fq-multivalue-note"))
            .await?
            .text()
            .await?;
        assert!(note.contains("sum can exceed"), "{note}");
        assert!(
            driver
                .find_all(By::Css(".fq-chart[data-key=genre] .fq-multivalue-note"))
                .await?
                .is_empty()
        );
        driver
            .find(By::Css("[data-site-help-open]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-site-help]').open",
            "the help dialog",
        )
        .await?;
        let help = driver
            .find(By::Css("[data-site-help] [data-analysis-help]"))
            .await?
            .text()
            .await?;
        for term in ["Frequencies", "Compare", "Download", "CLI"] {
            assert!(help.contains(term), "{term}: {help}");
        }
        driver
            .find(By::Css("[data-site-help-close]"))
            .await?
            .click()
            .await?;
        driver
            .find(By::Css("[data-locale-toggle]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.documentElement.lang === 'ja'",
            "Japanese",
        )
        .await?;
        let note = driver
            .find(By::Css(".fq-chart[data-key=tags] .fq-multivalue-note"))
            .await?
            .text()
            .await?;
        assert!(note.contains("合計"), "{note}");
        assert_eq!(
            driver
                .execute(
                    "return document.documentElement.scrollWidth > innerWidth",
                    vec![]
                )
                .await?
                .json(),
            &json!(false)
        );
        Ok(())
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn unsupported_frequency_conditions_keep_their_explanation_after_a_locale_switch()
-> WebDriverResult<()> {
    use futures::FutureExt as _;
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result = std::panic::AssertUnwindSafe(async {
        driver
            .goto(format!("{}#where=%40section%20%3D%20abstract", served.base))
            .await?;
        wait_for(&driver, STATE, "the unsupported-condition explanation").await?;
        driver
            .find(By::Css("[data-locale-toggle]"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.documentElement.lang === 'ja'",
            "Japanese",
        )
        .await?;
        let text = driver.execute(STATE, Vec::new()).await?.json().clone();
        assert!(
            text.as_str()
                .is_some_and(|text| text.contains("セクション")),
            "{text}"
        );
        Ok::<(), thirtyfour::error::WebDriverError>(())
    })
    .catch_unwind()
    .await;
    driver.quit().await?;
    served.server.abort();
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[tokio::test]
async fn an_opened_or_restored_entry_takes_focus_at_its_name() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        let focused = "return document.activeElement?.classList.contains('fq-entry-name') ?? false";
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute(
                "document.querySelector('button.open-entry').click()",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, ENTRY_READY, "the opened entry").await?;
        observed(
            &driver,
            focused,
            json!(true),
            "focus on the opened entry's name",
        )
        .await?;
        let address: String = driver
            .execute("return location.href", Vec::new())
            .await?
            .convert()?;
        open(&driver, &address, ENTRY_READY).await?;
        observed(
            &driver,
            focused,
            json!(true),
            "focus on the restored entry's name",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result?;
    Ok(())
}

fn write_key_table(artifact: &std::path::Path, destination: &std::path::Path) {
    let entries: [([&str; 3], &[(u32, u64)]); 4] = [
        (["坂", "を", "上る"], &[(0, 6), (1, 4)]),
        (["坂", "を", "下りる"], &[(1, 4)]),
        (["坂", "が", "ある"], &[(0, 3)]),
        (["山", "を", "上る"], &[(1, 2)]),
    ];
    let mut values = entries
        .iter()
        .flat_map(|(key, _)| key.iter().copied())
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    let rank = |value: &str| values.binary_search(&value).unwrap() as u32;
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(key, _)| key.map(rank));
    let documents = (0..6u64)
        .map(|document_id| DocumentTotals {
            document_id,
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let mut writer = CountTableWriter::create(
        destination,
        table_key(artifact, CATALOG),
        vec!["noun".into(), "verb".into()],
        vec![
            GroupSchema {
                kind: GroupKind::Key,
                names: vec!["$n.lemma".into(), "$p.lemma".into(), "$v.lemma".into()],
            },
            GroupSchema {
                kind: GroupKind::Surface,
                names: Vec::new(),
            },
        ],
        &values,
        &documents,
    )
    .unwrap();
    for (key, pairs) in &sorted {
        writer.begin_entry(0, 0, &key.map(rank)).unwrap();
        for (document, count) in *pairs {
            writer
                .add_sentence(*document, u64::from(*document), *count)
                .unwrap();
        }
    }
    writer.begin_entry(1, 1, &[rank("上る")]).unwrap();
    writer.add_sentence(0, 0, 1).unwrap();
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
}

async fn serve_key_table() -> Served {
    serve_artifact(
        support::build_artifact_with_metadata(
            (0..6u64)
                .map(|index| {
                    (
                        "doc.md",
                        index,
                        false,
                        vec![vec![TokenInfo::default().with_orth("x")]],
                    )
                })
                .collect(),
            Vec::new(),
            metadata(),
        ),
        CATALOG,
        write_key_table,
        &[],
    )
    .await
}

const COLUMNS: &str = r"
const shown = (e) => getComputedStyle(e).display !== 'none' && e.getBoundingClientRect().width > 0;
const extent = (e) => { const r = e.getBoundingClientRect(); return [Math.round(r.left), Math.round(r.right)]; };
const heads = [...document.querySelectorAll('table.freq thead th')].filter(shown);
const cells = [...document.querySelectorAll('table.freq tbody tr:first-child td')].filter(shown);
const pm = document.querySelector('table.freq tbody tr:first-child td.pm');
const entry = document.querySelector('table.freq tbody tr:first-child td.entry-cell');
const over = heads.find((h) => { const [l, r] = extent(h), [a, b] = extent(pm); return l === a && r === b; });
return {
  heads: heads.map((h) => [h.textContent, ...extent(h)]),
  cells: cells.map((c) => [c.textContent, ...extent(c)]),
  perMillion: over ? over.textContent : null,
  brokenWords: heads.flatMap((h) => {
    const walker = document.createTreeWalker(h, NodeFilter.SHOW_TEXT), broken = [];
    for (let text = walker.nextNode(); text; text = walker.nextNode()) {
      for (const word of text.textContent.matchAll(/[A-Za-z0-9]+/g)) {
        const tops = new Set([...word[0]].map((_, i) => { const r = document.createRange(); r.setStart(text, word.index + i); r.setEnd(text, word.index + i + 1); return Math.round(r.getBoundingClientRect().top); }));
        if (tops.size > 1) broken.push(word[0]);
      }
    }
    return broken;
  }),
  entryEms: entry.getBoundingClientRect().width / parseFloat(getComputedStyle(entry).fontSize),
  overflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
  fieldNames: [document.querySelector('table.freq thead .entry-names')?.textContent ?? null, document.querySelectorAll('table.freq tbody .entry-cell small:not(.gloss)').length],
  wholeWords: (() => {
    const text = entry.querySelector('.open-entry').firstChild;
    let at = 0;
    return text.textContent.split(' ').every((word) => {
      const tops = new Set([...word].map((_, i) => { const r = document.createRange(); r.setStart(text, at + i); r.setEnd(text, at + i + 1); return Math.round(r.getBoundingClientRect().top); }));
      at += word.length + 1;
      return tops.size === 1;
    });
  })(),
};";

#[tokio::test]
async fn a_phone_width_table_keeps_each_header_over_its_own_values() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    crate::resize(&driver, 390, 844).await?;
    let served = serve_key_table().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &served.base, ROWS).await?;
        let exact = serde_urlencoded::to_string([("k", json!({"$n.lemma": "坂"}).to_string())]).unwrap();
        for (locale, subset_share, per_million, legend) in [
            ("ja", "絞り込み内の割合", "100万語あたり", json!(["完全一致", "n · p · v", "文書に占める割合", "100万語あたり"])),
            ("en", "Share of subset", "Per million tokens", json!(["exactly", "n · p · v", "Share of documents", "Per million tokens"])),
        ] {
        driver
            .execute(&format!("localStorage.setItem('lingrep-web:locale:v1', '{locale}')"), Vec::new())
            .await?;
        for (address, subset) in [
            (format!("{}#pattern=noun&{exact}", served.base), true),
            (format!("{}#pattern=noun", served.base), false),
        ] {
            open(&driver, &address, ROWS).await?;
            let subset_head = format!("return [...document.querySelectorAll('table.freq thead th')].some((h) => h.textContent === '{subset_share}')");
            if subset {
                wait_for(&driver, &subset_head, "the subset column").await?;
            }
            let subset = format!("{locale} {subset}");
            let columns = driver.execute(COLUMNS, Vec::new()).await?.json().clone();
            let heads = columns["heads"].as_array().unwrap();
            let cells = columns["cells"].as_array().unwrap();
            assert_eq!(heads.len(), cells.len(), "{subset}: {columns}");
            for (head, cell) in heads.iter().zip(cells) {
                assert_eq!(
                    (&head[1], &head[2]),
                    (&cell[1], &cell[2]),
                    "{subset}: {head} over {cell}"
                );
            }
            assert_eq!(columns["perMillion"], json!(per_million), "{subset}: {columns}");
            assert_eq!(columns["brokenWords"], json!([]), "{subset}: {columns}");
            assert!(columns["entryEms"].as_f64().unwrap() >= 4.0, "{subset}: {columns}");
            assert!(columns["overflow"].as_f64().unwrap() <= 0.0, "{subset}: {columns}");
            assert_eq!(columns["wholeWords"], json!(true), "{subset}: {columns}");
            assert_eq!(columns["fieldNames"], json!(["n · p · v", 0]), "named once, over the entries: {columns}");
            observed(
                &driver,
                "return [...document.querySelectorAll('details.fq-legend dt')].map((d) => d.textContent)",
                legend.clone(),
                "the table's terms explained beside it",
            )
            .await?;
        }
        }
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn an_inspected_line_steps_through_its_entrys_lines() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let cats = (0..WORKS.len())
        .map(|document| work_sentences(document).len())
        .sum::<usize>();
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute("document.querySelector('button.open-entry').click()", Vec::new())
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        driver
            .find(By::Css(".fq-kwic li[data-i='1'] .fq-inspect"))
            .await?
            .click()
            .await?;
        let title = "const lab = document.querySelector('#token-lab'); \
            return lab && !lab.hidden ? document.querySelector('#token-origin').textContent : null";
        let at = |n: usize| {
            format!(
                "const lab = document.querySelector('#token-lab'), t = document.querySelector('#token-origin')?.textContent ?? ''; \
                 return !!lab && !lab.hidden && t.startsWith('猫 · {n} / {cats} · ') && t"
            )
        };
        wait_for(&driver, &at(2), "the second line").await?;
        let listing = "const rows = [...document.querySelectorAll('.listing-entry .entry-row')]; \
            const open = rows.findIndex((row) => row.getAttribute('aria-expanded') === 'true'); \
            return {rows: rows.length, open, lab: rows[open]?.nextElementSibling?.id ?? null, \
              header: document.querySelector('.listing-entry .evidence-sentence')?.textContent ?? null, \
              note: document.querySelector('.listing-entry .line-note')?.textContent ?? null, \
              read: rows.every((row) => row.querySelector('mark').textContent === '猫'), \
              whole: !!document.querySelector('.listing-entry .entry-exit:not([hidden])')}";
        wait_for(
            &driver,
            "return [...document.querySelectorAll('.entry-row mark')].filter((mark) => mark.textContent).length === 50",
            "the listed lines' text",
        )
        .await?;
        observed(
            &driver,
            listing,
            json!({"rows": LINES, "open": 1, "lab": "token-lab",
                "header": format!("猫 · {cats} matches in {} documents.", WORKS.len()),
                "note": format!("The first {LINES} lines listed in Frequencies."), "read": true, "whole": true}),
            "the entry's lines as the result",
        )
        .await?;
        let chip = "return document.querySelector('[data-clear-limit=entry]')?.textContent ?? null";
        observed(&driver, chip, json!("Examples: 猫 ×"), "the entry chip").await?;
        let step = |direction: &'static str| {
            format!("document.querySelector('[data-example-step=\"{direction}\"]').click()")
        };
        for (direction, n) in [("1", 3), ("1", 4), ("-1", 3), ("-1", 2), ("-1", 1)] {
            driver.execute(&step(direction), Vec::new()).await?;
            wait_for(&driver, &at(n), &format!("line {n}")).await?;
        }
        observed(
            &driver,
            "return document.querySelector('[data-example-step=\"-1\"]').disabled",
            json!(true),
            "no step before the first line",
        )
        .await?;
        driver
            .execute("document.querySelector('.entry-row[data-entry-line=\"4\"]').click()", Vec::new())
            .await?;
        wait_for(&driver, &at(5), "the fifth listed line").await?;
        driver
            .execute("document.querySelector('[data-clear-limit=entry]').click()", Vec::new())
            .await?;
        observed(&driver, chip, Value::Null, "the removed chip").await?;
        wait_for(
            &driver,
            "return document.querySelectorAll('.entry-row').length === 0 && \
              !!document.querySelector('.kwic-row[data-sequence][aria-expanded=true] + #token-lab')",
            "the whole result around the open example",
        )
        .await?;
        let plain = driver.execute(title, Vec::new()).await?.json().clone();
        assert!(!plain.as_str().unwrap().contains(" / "), "{plain}");
        observed(
            &driver,
            "const a = document.querySelector('#frequency-return'); return !a.hidden && a.getAttribute('href').startsWith('frequencies#')",
            json!(true),
            "the return link",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_page_filter_names_the_lines_it_filters_and_links_an_empty_result_to_frequencies()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute("document.querySelector('button.open-entry').click()", Vec::new())
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        driver
            .find(By::Css(".fq-kwic li[data-i='0'] .fq-inspect"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "const lab = document.querySelector('#token-lab'); return !!lab && !lab.hidden",
            "the opened line",
        )
        .await?;
        driver.execute("document.querySelector('[data-clear-limit=entry]').click()", Vec::new()).await?;
        select_question_kwic(&driver).await?;
        wait_for(
            &driver,
            "const input = document.querySelector('.line-controls input'); return !!input && !input.parentElement.hidden && input.placeholder !== '' && !document.querySelector('.listing-entry')",
            "the search's lines",
        )
        .await?;
        let filter = "const input = document.querySelector('.line-controls input'); \
            const r = {placeholder: input.placeholder, label: input.getAttribute('aria-label'), \
              count: document.querySelector('.line-controls span').textContent, \
              empty: document.querySelector('.kwic-list > p.line-note')?.textContent ?? null, \
              link: document.querySelector('.kwic-list > p.line-note a')?.getAttribute('href') ?? null};";
        let before = driver
            .execute(&format!("{filter} return r;"), Vec::new()).await?.json().clone();
        let shown = before["placeholder"].as_str().unwrap().to_owned();
        assert!(shown.starts_with("Filter the ") && shown.ends_with(" lines shown"), "{before}");
        assert_eq!(before["label"], before["placeholder"]);
        driver
            .execute(
                "const input = document.querySelector('.line-controls input'); input.value = '犬'; \
                 input.dispatchEvent(new Event('input', {bubbles: true}))",
                Vec::new(),
            )
            .await?;
        let after = wait_for(
            &driver,
            &format!("{filter} return r.link ? r : false;"),
            "the Frequencies link",
        )
        .await?;
        let n = shown.trim_start_matches("Filter the ").trim_end_matches(" lines shown");
        assert!(
            after["count"].as_str().unwrap().starts_with(&format!("0 of the {n} lines shown · ")),
            "{after}"
        );
        assert!(
            after["empty"].as_str().unwrap().starts_with(&format!("None of the {n} lines shown contain “犬”.")),
            "{after}"
        );
        let link = after["link"].as_str().unwrap();
        let params: std::collections::BTreeMap<String, String> =
            serde_urlencoded::from_str(link.strip_prefix("frequencies#").unwrap()).unwrap();
        assert_eq!(params.get("pattern").map(String::as_str), Some("noun"), "{link}");
        assert_eq!(params.get("q").map(String::as_str), Some("犬"), "{link}");
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

const COUNT_FIRST_CATALOG: &str = "key: [upos]\npos := [].\nnoun := [upos=NOUN].\n";

async fn serve_count_first(extra: &[&std::ffi::OsStr]) -> Served {
    serve_artifact(
        support::build_artifact_with_metadata(
            (0..6u64)
                .map(|index| {
                    (
                        "doc.md",
                        index,
                        false,
                        vec![vec![TokenInfo::default().with_orth("x")]],
                    )
                })
                .collect(),
            Vec::new(),
            metadata(),
        ),
        COUNT_FIRST_CATALOG,
        |artifact, destination| {
            let documents = (0..6u64)
                .map(|document_id| DocumentTotals {
                    document_id,
                    sentences: 1,
                    tokens: 10,
                })
                .collect::<Vec<_>>();
            let mut writer = CountTableWriter::create(
                destination,
                table_key(artifact, COUNT_FIRST_CATALOG),
                vec!["pos".into(), "noun".into()],
                vec![GroupSchema {
                    kind: GroupKind::Surface,
                    names: Vec::new(),
                }],
                &["NOUN", "本"],
                &documents,
            )
            .unwrap();
            for (pattern, value) in [(0, 0), (1, 1)] {
                writer.begin_entry(pattern, 0, &[value]).unwrap();
                writer.add_sentence(0, 0, 1).unwrap();
            }
            writer
                .finish(&CancellationFlag::new())
                .unwrap()
                .publish()
                .unwrap();
        },
        extra,
    )
    .await
}

#[tokio::test]
async fn frequencies_open_on_the_first_definition_that_is_not_count_only() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_count_first(&[]).await;
    let result: WebDriverResult<()> = async {
        let chosen = "return document.querySelector('[data-fq-sentence] select[data-b=pattern]')?.value ?? null";
        open(&driver, &served.base, ROWS).await?;
        observed(&driver, chosen, json!("noun"), "the default definition").await?;
        open(&driver, &format!("{}#pattern=pos", served.base), ROWS).await?;
        observed(&driver, chosen, json!("pos"), "the addressed definition").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_table_starts_above_the_fold_with_the_pattern_machinery_folded() -> WebDriverResult<()>
{
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    driver.set_window_rect(0, 0, 1280, 800).await?;
    let served = serve_key_table().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        let shown = driver
            .execute(
                "const folded = document.querySelector('[data-fq-machinery]'), row = document.querySelector('table.freq tbody tr'); \
                 const field = getComputedStyle(document.querySelector('.fq-key input')), blank = getComputedStyle(document.querySelector('select[data-b=pattern]')); \
                 return {open: folded.open, holds: [!!folded.querySelector('[data-fq-source] pre'), !!folded.querySelector('[data-fq-source] a'), !!folded.querySelector('[data-c=code]')], \
                   underFigures: folded.compareDocumentPosition(document.querySelector('[data-fq-figures]')) === Node.DOCUMENT_POSITION_PRECEDING, \
                   rowBottom: row.getBoundingClientRect().bottom, viewport: innerHeight, \
                   field: [field.borderTopWidth, field.color === blank.color, field.backgroundColor]}",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert_eq!(shown["open"], json!(false), "{shown}");
        assert_eq!(shown["holds"], json!([true, true, true]), "{shown}");
        assert_eq!(shown["underFigures"], json!(true), "{shown}");
        assert!(shown["rowBottom"].as_f64().unwrap() <= shown["viewport"].as_f64().unwrap(), "{shown}");
        assert_eq!(shown["field"], json!(["0px", true, "rgba(0, 0, 0, 0)"]), "{shown}");
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn an_exact_value_without_entries_offers_to_clear_the_exact_values() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_key_table().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        let names = driver
            .execute(
                "return [...document.querySelectorAll('.fq-key input')].map((i) => i.getAttribute('aria-label'))",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        let names = names.as_array().unwrap();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(
            names.iter().all(|name| name.as_str().is_some_and(|name| name.ends_with(", exact value"))),
            "{names:?}"
        );
        driver
            .find(By::Css(".fq-key input"))
            .await?
            .send_keys("坂坂")
            .await?;
        let empty = "const p = document.querySelector('[data-fq-table]'); \
            return p.querySelector('.fq-empty') && p.querySelector('[data-clear-keys]') ? p.textContent : null";
        let text = wait_for(&driver, empty, "the empty exact result").await?;
        assert!(text.as_str().unwrap().contains("filter above"), "{text}");
        driver
            .find(By::Css("[data-clear-keys]"))
            .await?
            .click()
            .await?;
        wait_for(&driver, ROWS, "the entries again").await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('.fq-key input')].map((i) => i.value).join('')",
            json!(""),
            "the cleared fields",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn each_inspect_link_names_its_line_and_work() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=noun", served.base), ROWS).await?;
        driver
            .execute("document.querySelector('button.open-entry').click()", Vec::new())
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        let names = driver
            .execute(
                "return [...document.querySelectorAll('.fq-kwic li')].map((li) => [li.querySelector('.src span').textContent, li.querySelector('.fq-inspect')?.getAttribute('aria-label')])",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        let names = names.as_array().unwrap();
        assert!(names.len() > 1, "{names:?}");
        for (line, pair) in names.iter().enumerate() {
            assert_eq!(
                pair[1],
                json!(format!("Inspect tokens: line {}, {}", line + 1, pair[0].as_str().unwrap())),
                "{names:?}"
            );
        }
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

fn limited_question(base: &str, source: &str, run: bool) -> String {
    let root = base.trim_end_matches("frequencies");
    let query = serde_urlencoded::to_string([
        ("fresh", "1"),
        ("run", if run { "1" } else { "0" }),
        ("source", source),
        ("pattern", "noun"),
    ])
    .unwrap();
    format!("{root}#{query}")
}

async fn select_question_kwic(driver: &WebDriver) -> WebDriverResult<()> {
    wait_for(
        driver,
        "return !!document.querySelector('.result-modes:not([hidden]) [data-layout=kwic]')",
        "the result view controls",
    )
    .await?;
    driver
        .find(By::Css("[data-layout=kwic]"))
        .await?
        .click()
        .await
}

#[tokio::test]
async fn question_actions_fit_at_intermediate_widths() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(served.base.trim_end_matches("frequencies")).await?;
        for width in [481, 600] {
            crate::resize(&driver, width, 550).await?;
            two_frames(&driver).await?;
            let summary = driver.find(By::Css("#question-actions > summary")).await?;
            assert!(summary.is_displayed().await?, "question actions at {width}px");
            summary.click().await?;
            observed(&driver,
                "const nav = document.querySelector('#question-actions nav').getBoundingClientRect(); \
                 return nav.width > 0 && nav.left >= 0 && nav.right <= document.documentElement.clientWidth",
                json!(true), "question actions fit the viewport").await?;
            driver.find(By::Id("language")).await?.click().await?;
            summary.click().await?;
        }
        crate::resize(&driver, 1100, 700).await?;
        two_frames(&driver).await?;
        assert!(driver.find(By::Id("language")).await?.is_displayed().await?);
        assert_no_page_errors(&driver).await
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn work_zoom_keeps_the_open_match_when_the_view_changes() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        let rows = ".work-sample-list > .kwic-row";
        wait_for(
            &driver,
            &format!("return document.querySelectorAll('{rows}').length === 5"),
            "one preview for each of the first five works",
        ).await?;
        let title = driver.find(By::Css(".work-sample-list button[data-work-toggle]")).await?;
        title.send_keys(Key::Enter).await?;
        wait_for(
            &driver,
            &format!("return document.querySelectorAll('{rows}').length > 5 && document.querySelector('[data-work-toggle]').getAttribute('aria-expanded') === 'true'"),
            "the work title expands matches inline",
        ).await?;
        title.send_keys(Key::Enter).await?;
        wait_for(
            &driver,
            &format!("return document.querySelectorAll('{rows}').length === 5 && document.querySelector('[data-work-toggle]').getAttribute('aria-expanded') === 'false'"),
            "the work title collapses its matches",
        ).await?;
        driver.find(By::Css(".more-works")).await?.click().await?;
        wait_for(
            &driver,
            &format!("return document.querySelectorAll('{rows}').length === 8"),
            "the next page of works",
        ).await?;
        observed(
            &driver,
            "return document.querySelector('#line-limit').matches(':disabled') && !document.querySelector('#line-limit').getClientRects().length",
            json!(true),
            "the line count is hidden in By work",
        ).await?;
        driver.execute(
            "window.previewText = document.querySelector('.work-sample-list .context-right').firstChild",
            Vec::new(),
        ).await?;
        let zoom = driver.find(By::Css("[data-work-zoom]")).await?;
        zoom.send_keys(thirtyfour::Key::Right).await?;
        wait_for(
            &driver,
            &format!("return document.querySelectorAll('{rows}').length > 8"),
            "more matches in the same works",
        ).await?;
        observed(
            &driver,
            "return window.previewText === document.querySelector('.work-sample-list .context-right').firstChild",
            json!(true),
            "zooming to more matches preserves the first excerpt's text node",
        ).await?;
        zoom.send_keys(thirtyfour::Key::Right).await?;
        wait_for(&driver, "return !!document.querySelector('.work-sample-list .has-sentence')", "full sentences").await?;
        let sequence = driver.find(By::Css(rows)).await?.attr("data-sequence").await?;
        driver.find(By::Css(rows)).await?.click().await?;
        wait_for(&driver, "return !document.querySelector('#token-lab').hidden", "the open match").await?;
        crate::resize(&driver, 360, 550).await?;
        zoom.send_keys(thirtyfour::Key::Home).await?;
        assert_eq!(zoom.attr("aria-valuetext").await?.as_deref(), Some("One per work"));
        driver.find(By::Css("[data-layout=kwic]")).await?.click().await?;
        wait_for(
            &driver,
            "return !document.querySelector('#token-lab').hidden && document.querySelector('[data-layout=kwic]').getAttribute('aria-pressed') === 'true' && !!document.querySelector('.kwic-list:not(.work-sample-list) > [aria-expanded=true]')",
            "the same match in KWIC",
        ).await?;
        assert_eq!(
            driver.find(By::Css(".kwic-list > [aria-expanded=true]")).await?.attr("data-sequence").await?,
            sequence
        );
        driver.back().await?;
        wait_for(
            &driver,
            "return !!document.querySelector('.work-sample-list > [aria-expanded=true]')",
            "the selected match after returning to By work",
        ).await?;
        driver.execute(
            "[...document.querySelectorAll('.work-sample-list > .kwic-row')].at(-2).scrollIntoView({block:'center'})",
            Vec::new(),
        ).await?;
        two_frames(&driver).await?;
        assert_eq!(driver.execute(
            "const box = document.querySelector('[data-work-zoom]').getBoundingClientRect(); return scrollY > 500 && box.top >= 0 && box.bottom < 100",
            Vec::new(),
        ).await?.json(), &json!(true), "zoom stays reachable while reading");
        assert_no_page_errors(&driver).await
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn by_work_summary_uses_the_full_result_scope_after_scoped_kwic() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return /in 8 documents/.test(document.querySelector('.evidence-sentence')?.textContent)", "the complete result total").await?;
        let caption = driver.find(By::Css(".evidence-sentence")).await?.text().await?;
        select_question_kwic(&driver).await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-list > .kwic-row').length === 20", "the unscoped KWIC").await?;
        observed(
            &driver,
            "return !document.querySelector('#line-limit').disabled && document.querySelector('#line-limit').getClientRects().length > 0",
            json!(true),
            "the line count is available in KWIC",
        ).await?;
        driver.find(By::Css(".kwic-list .document-heading > a")).await?.click().await?;
        wait_for(&driver, "return /in 1 document\\./.test(document.querySelector('.evidence-sentence')?.textContent)", "the scoped KWIC total").await?;
        driver.find(By::Css("[data-layout=works]")).await?.click().await?;
        wait_for(&driver, "return document.querySelectorAll('.work-sample-list > .document-heading').length === 5", "the cross-work previews").await?;
        assert_eq!(driver.find(By::Css(".evidence-sentence")).await?.text().await?, caption);
        assert!(!driver.find(By::Css(".document-scope")).await?.is_displayed().await?);
        assert_no_page_errors(&driver).await
    }.await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_definition_limit_survives_an_edit_that_breaks_the_source() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver
            .goto(limited_question(&served.base, ENTRY_CATALOG, true))
            .await?;
        wait_for(&driver, "return !!document.querySelector('[data-clear-limit=pattern]')", "the restored definition limit").await?;
        let chip = "return [document.querySelector('[data-clear-limit=pattern]')?.textContent ?? null, document.querySelector('#source-label').textContent]";
        observed(&driver, chip, json!(["Definition: noun ×", "noun"]), "the limit").await?;
        assert_eq!(
            driver.execute("return document.querySelector('#example')", Vec::new()).await?.json(),
            &Value::Null,
            "one search button"
        );
        driver
            .execute(
                "const t = document.querySelector('#source'); t.value = t.value.replace('noun :=', 'noun :=='); \
                 t.dispatchEvent(new Event('input', {bubbles: true})); document.querySelector('#run').click()",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "const e = document.querySelector('#edit-pattern-error'); return e && !e.hidden",
            "the pattern error",
        )
        .await?;
        observed(&driver, chip, json!(["Definition: noun ×", "noun"]), "the limit after the error").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_source_editor_opens_on_the_selected_definition() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let filler = (0..40).map(|n| format!("# note {n}\n")).collect::<String>();
    let source = format!(
        "{filler}{ENTRY_CATALOG}\n{}",
        (0..40)
            .map(|n| format!("pattern other{n} := [text=本].\n"))
            .collect::<String>()
    );
    let result: WebDriverResult<()> = async {
        driver
            .goto(limited_question(&served.base, &source, false))
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('#source-label').textContent === 'noun' || null",
            "the limited definition",
        )
        .await?;
        driver
            .execute("document.querySelector('#edit-source').click()", Vec::new())
            .await?;
        let shown = driver
            .execute(
                "const t = document.querySelector('#source'), at = t.value.indexOf('pattern noun'); \
                 const line = parseFloat(getComputedStyle(t).lineHeight), top = t.value.slice(0, at).split('\\n').length - 1; \
                 return {caret: t.selectionStart === at, focused: document.activeElement === t, \
                   visible: top * line >= t.scrollTop && (top + 1) * line <= t.scrollTop + t.clientHeight}",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert_eq!(shown, json!({"caret": true, "focused": true, "visible": true}));
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn printing_the_question_page_leaves_out_its_controls() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver
            .goto(limited_question(&served.base, ENTRY_CATALOG, true))
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('.result-header a[href$=\"/export\"]') && document.querySelector('.line-controls input') ? true : null",
            "the results",
        )
        .await?;
        let printed = driver
            .execute(
                "const hidden = [...document.styleSheets].flatMap((sheet) => [...sheet.cssRules]) \
                   .filter((rule) => rule.media?.mediaText === 'print').flatMap((rule) => [...rule.cssRules]) \
                   .filter((rule) => rule.style?.display === 'none').map((rule) => rule.selectorText); \
                 const dropped = (selector) => { const e = document.querySelector(selector); return !!e && hidden.some((s) => e.closest(s)); }; \
                 return Object.fromEntries(['header.site', '#run', '.line-controls input', '.document-nav', '.result-header a[href$=\"/export\"]', '#attribute-remix', \
                   '.question-sentence', '.line-controls span', '.kwic-list'].map((s) => [s, dropped(s)]))",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert_eq!(
            printed,
            json!({"header.site": true, "#run": true, ".line-controls input": true, ".document-nav": true,
                   ".result-header a[href$=\"/export\"]": true, "#attribute-remix": true,
                   ".question-sentence": false, ".line-controls span": false, ".kwic-list": false})
        );
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_first_tab_leads_to_the_question_sentence() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver
            .goto(limited_question(&served.base, ENTRY_CATALOG, true))
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('.line-controls input') ? true : null",
            "the results",
        )
        .await?;
        let body = driver.find(By::Css("body")).await?;
        body.send_keys(Key::Tab).await?;
        let active = "const a = document.activeElement; return [a.getAttribute('href'), a.textContent, !!a.closest('.question-sentence')]";
        assert_eq!(
            driver.execute(active, Vec::new()).await?.json(),
            &json!(["#question-sentence", "Skip to the question", false])
        );
        driver
            .action_chain()
            .send_keys(Key::Enter)
            .send_keys(Key::Tab)
            .perform()
            .await?;
        assert_eq!(driver.execute(active, Vec::new()).await?.json()[2], json!(true));
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_definition_blank_is_a_menu_of_the_catalogs_definitions() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let catalog = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(catalog.path(), COUNT_FIRST_CATALOG).unwrap();
    let served = serve_count_first(&["--patterns".as_ref(), catalog.path().as_os_str()]).await;
    let result: WebDriverResult<()> = async {
        driver.goto(served.base.trim_end_matches("frequencies")).await?;
        let chip = "return document.querySelector('[data-clear-limit=pattern]')?.textContent ?? null";
        wait_for(&driver, &format!("{chip} === 'Definition: noun ×'"), "the default definition").await?;
        driver.execute("document.querySelector('#edit-source').click()", Vec::new()).await?;
        let menu = "const menu = document.querySelector('#definition-menu'); \
            return {open: !menu.hidden, expanded: document.querySelector('#edit-source').getAttribute('aria-expanded'), \
              searched: [...menu.querySelectorAll('button[data-definition]')].map((b) => [b.dataset.definition, b.getAttribute('aria-pressed')]), \
              counted: [...menu.querySelectorAll('li > a')].map((a) => [a.textContent, new URLSearchParams(a.getAttribute('href').split('#')[1]).get('pattern')]), \
              heading: menu.querySelector('h3')?.textContent ?? null, last: menu.querySelector('.definition-actions > :last-child')?.hasAttribute('data-edit-source') ?? false, \
              focused: document.activeElement?.dataset?.definition ?? null}";
        wait_for(&driver, "return document.querySelectorAll('#definition-menu li > a').length === 1", "the count-only definition's Frequencies link").await?;
        observed(
            &driver,
            menu,
            json!({"open": true, "expanded": "true", "searched": [["noun", "true"]], "counted": [["pos", "pos"]],
                "heading": "Counts only (see Frequencies)", "last": true, "focused": "noun"}),
            "the definition menu",
        )
        .await?;
        driver.find(By::Css("#definition-menu [data-definition='noun']")).await?.send_keys(Key::Escape).await?;
        observed(
            &driver,
            "return [document.querySelector('#definition-menu').hidden, document.activeElement.id]",
            json!([true, "edit-source"]),
            "Escape closes the menu",
        )
        .await?;
        driver.execute("document.querySelector('[data-clear-limit=pattern]').click()", Vec::new()).await?;
        observed(
            &driver,
            &format!("return [document.querySelector('#definition-menu').hidden, (() => {{ {chip} }})()]"),
            json!([false, "Definition: noun ×"]),
            "the chip's × reopens the menu and keeps the definition",
        )
        .await?;
        driver.execute("document.querySelector('[data-edit-source]').click()", Vec::new()).await?;
        observed(
            &driver,
            "return [document.querySelector('#definition-menu').hidden, document.querySelector('#source-editor').hidden]",
            json!([true, false]),
            "editing the source from the menu",
        )
        .await?;
        wait_for(
            &driver,
            "return document.activeElement?.closest('#source-editor .pattern-editor') !== null",
            "the focused pattern editor",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_question_page_searches_only_when_asked() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-row').length > 0", "the default search's lines").await?;
        driver.execute("document.querySelector('.kwic-row').click()", Vec::new()).await?;
        wait_for(&driver, "return !document.querySelector('#token-lab').hidden", "the open example").await?;
        driver.find(By::Css("#token-ribbon button")).await?.send_keys(Key::Escape).await?;
        wait_for(&driver, "return document.querySelector('#token-lab').hidden", "Escape closing the example").await?;
        driver
            .execute(
                "window.__posts = 0; const fetched = window.fetch; \
                 window.fetch = (url, options) => { if (options?.method === 'POST' && String(url).endsWith('api/questions/runs')) window.__posts++; return fetched(url, options); }; \
                 document.querySelector('#source-editor').hidden = false;",
                Vec::new(),
            )
            .await?;
        let source = driver.find(By::Css("#source")).await?;
        source.click().await?;
        source.send_keys(" ").await?;
        driver.find(By::Css("#corpus")).await?.focus().await?;
        observed(
            &driver,
            "return [window.__posts, !document.querySelector('#draft-badge').hidden]",
            json!([0, true]),
            "leaving the edited source",
        )
        .await?;
        driver
            .execute(
                "const source = document.querySelector('#source'); source.value = 'broken := ['; \
                 source.dispatchEvent(new Event('input')); document.querySelector('#query-form').requestSubmit();",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, "return !document.querySelector('#edit-pattern-error').hidden", "the compile failure").await?;
        observed(&driver, "return document.querySelector('#retry').hidden", json!(true), "no rerun of the broken source").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

/// Safari does not focus a button on mouse click, so after a token click focus
/// is on `#results`; the test focuses `#results` to reproduce that in Chromium.
#[tokio::test]
async fn escape_closes_the_example_when_a_token_click_leaves_focus_on_the_results()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-row').length > 1", "the default search's lines").await?;
        driver.find_all(By::Css(".kwic-row")).await?[1].click().await?;
        wait_for(&driver, "return !document.querySelector('#token-lab').hidden", "the open example").await?;
        let opened: String = driver
            .execute("return document.querySelector('.kwic-row[aria-expanded=true]').dataset.sequence", Vec::new())
            .await?
            .convert()?;
        driver.find(By::Css("#token-ribbon button")).await?.click().await?;
        driver.find(By::Css("#context-size")).await?.focus().await?;
        driver.action_chain().send_keys(Key::Escape).perform().await?;
        observed(
            &driver,
            "return document.querySelector('#token-lab').hidden",
            json!(false),
            "Escape in a number field leaves the example open",
        )
        .await?;
        driver.execute("document.querySelector('#results').focus()", Vec::new()).await?;
        driver.action_chain().send_keys(Key::Escape).perform().await?;
        wait_for(&driver, "return document.querySelector('#token-lab').hidden", "Escape closing the example").await?;
        observed(
            &driver,
            "return [document.activeElement.classList.contains('kwic-row'), document.activeElement.dataset.sequence]",
            json!([true, opened]),
            "focus on the line that opened the example",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn an_example_opened_during_the_initial_search_stays_open() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    driver
        .cdp()
        .send_raw(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({"source": "window.__held = []; window.__hold = true; \
                const Native = window.EventSource; \
                window.EventSource = class extends Native { addEventListener(type, listener, options) { \
                  if (type !== 'update') return super.addEventListener(type, listener, options); \
                  const deliver = (update) => listener.call(this, new MessageEvent('update', {data: JSON.stringify(update)})); \
                  return super.addEventListener(type, (event) => { \
                    const update = JSON.parse(event.data), phase = update.patch.phase; \
                    if (!__hold || !__held.length && (phase === undefined || phase === 'running')) return listener.call(this, event); \
                    if (!__held.length && Object.values(update.append).some((rows) => rows.length)) { \
                      deliver({...update, patch: {...update.patch, phase: 'running'}}); \
                      __held.push(() => deliver({...update, reset: false, append: {}})); \
                    } else __held.push(() => listener.call(this, event)); }, options); } }; \
                window.__release = () => { __hold = false; for (const deliver of __held.splice(0)) deliver(); };"}),
        )
        .await?;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return __held.length > 0 && document.querySelectorAll('.kwic-row').length > 0", "the running search's lines").await?;
        observed(&driver, "return window.__probe.state.current.phase", json!("running"), "the held search").await?;
        driver.execute("document.querySelector('.kwic-row').click()", Vec::new()).await?;
        wait_for(&driver, "return !document.querySelector('#token-lab').hidden", "the open example").await?;
        driver.execute("__release()", Vec::new()).await?;
        wait_for(&driver, "return window.__probe.state.current.phase === 'complete'", "the completed search").await?;
        two_frames(&driver).await?;
        observed(
            &driver,
            "return [!document.querySelector('#token-lab').hidden, JSON.parse(new URLSearchParams(location.hash.slice(1)).get('view')).kind]",
            json!([true, "example"]),
            "the example after the search completes",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn an_address_for_a_changed_catalog_searches_only_once_the_reader_chooses()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let catalog = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(catalog.path(), COUNT_FIRST_CATALOG).unwrap();
    let served = serve_count_first(&["--patterns".as_ref(), catalog.path().as_os_str()]).await;
    let changed = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        changed.path(),
        format!("{COUNT_FIRST_CATALOG}verb := [upos=VERB].\n"),
    )
    .unwrap();
    let other = serve_count_first(&["--patterns".as_ref(), changed.path().as_os_str()]).await;
    let page = |served: &Served| served.base.trim_end_matches("frequencies").to_owned();
    let address = "const params = new URLSearchParams(location.hash.slice(1)); \
        return params.get('corpus') ? {source: params.get('source'), catalog: params.get('catalog'), run: params.get('run')} : null";
    let state = "return {notice: !document.querySelector('#catalog-notice').hidden, \
        searched: window.__probe?.state.current != null, \
        source: document.querySelector('#source').value}";
    let result: WebDriverResult<()> = async {
        driver.goto(page(&served)).await?;
        let linked = wait_for(&driver, address, "the question address").await?;
        assert_eq!(linked["source"], Value::Null, "{linked}");
        assert_eq!(linked["catalog"].as_str().unwrap().len(), 64, "{linked}");
        let hash = driver.execute("return location.hash", Vec::new()).await?.json().as_str().unwrap().to_owned();

        open(&driver, &format!("{}{hash}&run=1", page(&other)), "return window.__probe != null").await?;
        observed(
            &driver,
            state,
            json!({"notice": true, "searched": false, "source": format!("{COUNT_FIRST_CATALOG}verb := [upos=VERB].\n")}),
            "the changed catalog's notice",
        )
        .await?;
        observed(
            &driver,
            "return [document.querySelector('#catalog-notice').textContent, new URLSearchParams(location.hash.slice(1)).get('catalog')]",
            json!([
                "The server's patterns have changed since this link was made. A search with the current patterns may give different results.Search with the current patternsOpen the default question",
                linked["catalog"]
            ]),
            "the notice and the address that keeps the linked fingerprint",
        )
        .await?;
        driver.execute("document.querySelector('#language').click()", Vec::new()).await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('#catalog-notice p, #catalog-notice button')].map((node) => node.textContent)",
            json!([
                "このリンクを作ったときから、サーバーのパターンが変わっています。今のパターンで検索すると、リンクを作ったときと結果が異なることがあります。",
                "今のパターンで検索既定の問いを開く",
                "今のパターンで検索",
                "既定の問いを開く"
            ]),
            "the notice in Japanese",
        )
        .await?;
        driver.execute("document.querySelector('#language').click()", Vec::new()).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        observed(&driver, state, json!({"notice": true, "searched": false, "source": format!("{COUNT_FIRST_CATALOG}verb := [upos=VERB].\n")}), "no search while the notice waits").await?;

        driver.find(By::Css("#catalog-search")).await?.click().await?;
        wait_for(&driver, "return window.__probe.state.current != null", "the confirmed search").await?;
        observed(
            &driver,
            "return [document.querySelector('#catalog-notice').hidden, document.activeElement.id]",
            json!([true, "question-sentence"]),
            "the confirmed notice",
        )
        .await?;

        open(&driver, &format!("{}{hash}&run=1", page(&other)), "return window.__probe != null").await?;
        driver.find(By::Css("#catalog-default")).await?.click().await?;
        wait_for(
            &driver,
            "return window.__probe != null && document.querySelector('#catalog-notice').hidden && new URLSearchParams(location.hash.slice(1)).get('corpus') != null",
            "the default question",
        )
        .await?;
        let default = driver.execute(address, Vec::new()).await?.json().clone();
        assert_eq!(default["source"], Value::Null, "{default}");
        assert_eq!(default["run"], Value::Null, "{default}");
        assert_ne!(default["catalog"], linked["catalog"], "{default}");
        observed(&driver, "return window.__probe.state.current != null", json!(false), "nothing searched").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    other.server.abort();
    result
}

#[tokio::test]
async fn a_clamped_reading_option_is_announced() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-row').length > 0", "the search's lines").await?;
        driver
            .execute(
                "const input = document.querySelector('#context-size'); input.value = '40'; \
                 input.dispatchEvent(new Event('input')); input.dispatchEvent(new Event('change'));",
                Vec::new(),
            )
            .await?;
        observed(
            &driver,
            "const note = document.querySelector('#reading-clamp'); \
             return [document.querySelector('#context-size').value, note?.getAttribute('role') ?? null, note?.textContent ?? null]",
            json!(["24", "status", "Context is at most 24 tokens."]),
            "the clamped context",
        )
        .await?;
        driver
            .execute(
                "const input = document.querySelector('#context-size'); input.value = '8'; \
                 input.dispatchEvent(new Event('input')); input.dispatchEvent(new Event('change'));",
                Vec::new(),
            )
            .await?;
        observed(&driver, "return document.querySelector('#reading-clamp').textContent", json!(""), "an in-range context").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_question_source_editor_checks_the_source_against_the_selected_corpus()
-> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-row').length > 0", "the search's lines").await?;
        assert_eq!(
            driver.execute("return document.querySelector('.pattern-editor, link[href$=\"pattern-editor.css\"]')", Vec::new()).await?.json(),
            &Value::Null,
            "the editor loads only when the source editor opens"
        );
        driver.execute("document.querySelector('#edit-source').click()", Vec::new()).await?;
        wait_for(&driver, "return !!document.querySelector('#source-editor .cm-content')", "the pattern editor").await?;
        wait_for(&driver, "return document.querySelectorAll('#source-editor .cm-content [class*=pattern-]').length > 0", "highlighting").await?;
        driver
            .execute(
                "const source = document.querySelector('#source');                  source.value = 'pattern noun := [text in {猫, 本}].\\nsubject := [dep.rel=nsubj].\\n';                  source.dispatchEvent(new Event('input'));",
                Vec::new(),
            )
            .await?;
        wait_for(
            &driver,
            "return [...document.querySelectorAll('#source-editor .cm-lintRange-warning')].some((mark) => mark.textContent === 'dep.rel')",
            "the unavailable-attribute warning",
        )
        .await?;
        observed(
            &driver,
            "return [!document.querySelector('#draft-badge').hidden, document.querySelector('#source').value.includes('dep.rel')]",
            json!([true, true]),
            "the edit is a draft",
        )
        .await?;
        let content = driver.find(By::Css("#source-editor .cm-content")).await?;
        content.click().await?;
        content.send_keys(crate::MOD + Key::End).await?;
        content.send_keys("x := [up").await?;
        wait_for(
            &driver,
            "return [...document.querySelectorAll('.cm-tooltip-autocomplete li')].some((item) => item.textContent.startsWith('upos'))",
            "attribute completion",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_short_result_leaves_the_example_count_for_the_next_search() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        let root = served.base.trim_end_matches("frequencies");
        let query = serde_urlencoded::to_string([
            ("fresh", "1"),
            ("run", "1"),
            ("source", "bird := [text=鳥].\n"),
            ("raw", "@document.year <= 1953"),
        ])
        .unwrap();
        driver.goto(format!("{root}#{query}")).await?;
        select_question_kwic(&driver).await?;
        let shown = "return [document.querySelectorAll('.kwic-row').length, document.querySelector('#line-limit').value, \
                     new URLSearchParams(location.hash.slice(1)).get('lines') ?? '20', document.querySelector('#reading-clamp').textContent]";
        wait_for(&driver, "return document.querySelector('#status').textContent.length > 0 && document.querySelectorAll('.kwic-row').length === 1", "the one-line search").await?;
        two_frames(&driver).await?;
        observed(&driver, shown, json!([1, "20", "20", ""]), "the requested count after a short result").await?;
        driver
            .execute(
                "const where = document.querySelector('#where'); where.value = ''; where.dispatchEvent(new Event('input')); \
                 document.querySelector('#run').click();",
                Vec::new(),
            )
            .await?;
        wait_for(&driver, "return document.querySelectorAll('.kwic-row').length === 20", "twenty lines from the wider search").await?;
        observed(&driver, shown, json!([20, "20", "20", ""]), "the requested count after a wider search").await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_completed_search_over_the_preview_budget_pages_each_pattern() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let text = "猫".repeat(14 * 1024);
    let artifact = support::build_artifact_with_metadata(
        (0..30)
            .map(|document| {
                let sentence = vec![TokenInfo::default().with_orth(&text).with_lemma("cat")];
                ("doc.md", document, false, vec![sentence])
            })
            .collect(),
        Vec::new(),
        (0..30).map(|_| Box::default()).collect(),
    );
    let served = serve_artifact(
        artifact,
        CATALOG,
        |artifact, table| write_table(artifact, table, 30),
        &[],
    )
    .await;
    let result: WebDriverResult<()> = async {
        let root = served.base.trim_end_matches("frequencies");
        let query = serde_urlencoded::to_string([
            ("fresh", "1"),
            ("run", "1"),
            ("source", "first := [].\nsecond := [].\n"),
        ])
        .unwrap();
        driver.goto(format!("{root}#{query}")).await?;
        wait_for(
            &driver,
            "return document.querySelector('#status').textContent === 'Complete' && \
             document.querySelectorAll('.kwic-row').length > 0",
            "the initial concordance from the completed search",
        )
        .await?;
        driver.find(By::Css(".pattern-picker summary")).await?.click().await?;
        driver.find(By::Css(".pattern-picker [data-pattern='second']")).await?.click().await?;
        let second_page = "return document.querySelector('#status').textContent === 'Complete' && \
             document.querySelector('[data-pattern=second]')?.getAttribute('aria-pressed') === 'true' && \
             document.querySelector('.kwic-list')?.getAttribute('aria-busy') === 'false' && \
             document.querySelectorAll('.kwic-row').length > 0";
        wait_for(&driver, second_page, "the second pattern's concordance").await?;
        driver.refresh().await?;
        wait_for(&driver, second_page, "the same concordance after reload").await?;
        observed(
            &driver,
            "return [document.querySelector('#error').hidden, \
                document.querySelector('.result-header a[href$=\"/export\"]')?.getClientRects().length > 0]",
            json!([true, true]),
            "the completed search with its download",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn a_repeated_search_does_not_shift_the_page() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        for (width, height) in [(1280, 800), (390, 844)] {
            driver.set_window_rect(0, 0, width, height).await?;
            driver.goto("about:blank").await?;
            driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
            let settled = "return /Complete/.test(document.querySelector('#status').textContent) && document.querySelector('.kwic-list')?.getAttribute('aria-busy') === 'false' && document.querySelectorAll('.work-sample-list .kwic-row').length === 5";
            wait_for(&driver, settled, "the first search").await?;
            two_frames(&driver).await?;
            driver
                .execute(
                    "window.__shift = 0; window.__moved = []; \
                     new PerformanceObserver((list) => { for (const e of list.getEntries()) { __shift += e.value; __moved.push([Math.round(e.startTime), e.value, ...(e.sources || []).map((s) => [s.node?.id || s.node?.className, Math.round(s.previousRect.y), Math.round(s.currentRect.y), Math.round(s.previousRect.height), Math.round(s.currentRect.height)])]); } }).observe({type: 'layout-shift'}); \
                     window.__ran = document.querySelector('.result-panel').dataset.run; document.querySelector('#run').click();",
                    Vec::new(),
                )
                .await?;
            wait_for(
                &driver,
                &format!("return document.querySelector('.result-panel').dataset.run !== window.__ran && (() => {{ {settled} }})()"),
                "the repeated search",
            )
            .await?;
            two_frames(&driver).await?;
            let shift = driver.execute("return [__shift, __moved]", Vec::new()).await?.json().clone();
            assert!(shift[0].as_f64().unwrap() < 0.1, "{width}px: {shift}");
        }
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn the_result_list_is_one_tab_stop_crossed_with_arrow_keys() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver.goto(limited_question(&served.base, ENTRY_CATALOG, true)).await?;
        select_question_kwic(&driver).await?;
        wait_for(&driver, "return document.querySelector('.kwic-list').getAttribute('aria-busy') === 'false' && document.querySelectorAll('.kwic-row').length === 20", "twenty lines").await?;
        let focused = "const a = document.activeElement, items = [...document.querySelectorAll('.kwic-list > .kwic-row, .kwic-list > .document-heading > a')]; \
            return {at: items.indexOf(a), row: a.classList.contains('kwic-row'), last: items.length - 1, \
              stops: items.filter((item) => item.tabIndex >= 0).length, inside: !!a.closest('.kwic-list')}";
        let at = |driver: &WebDriver| {
            let driver = driver.clone();
            async move { WebDriverResult::Ok(driver.execute(focused, Vec::new()).await?.json().clone()) }
        };
        observed(
            &driver,
            "const rows = [...document.querySelectorAll('.kwic-row')]; \
             return rows.every((row) => row.tagName === 'A' && row.hasAttribute('href') && !row.hasAttribute('role') && row.getAttribute('aria-label').length > 0 && row.getAttribute('aria-expanded') === 'false')",
            json!(true),
            "the lines are labelled links",
        )
        .await?;
        let before = "const list = document.querySelector('.kwic-list'), all = [...document.querySelectorAll('a[href], button, input, select, summary, [tabindex]')] \
            .filter((e) => e.tabIndex >= 0 && !e.disabled && e.getClientRects().length && !list.contains(e) && (e.compareDocumentPosition(list) & Node.DOCUMENT_POSITION_FOLLOWING)); \
            all.at(-1).focus(); return document.activeElement === all.at(-1)";
        observed(&driver, before, json!(true), "focus on the control before the list").await?;
        driver.action_chain().send_keys(Key::Tab).perform().await?;
        let entered = at(&driver).await?;
        assert_eq!(entered["at"], 0, "{entered}");
        assert_eq!(entered["stops"], 1, "{entered}");
        driver.action_chain().send_keys(Key::Down).perform().await?;
        assert_eq!(at(&driver).await?["at"], 1);
        driver.action_chain().send_keys(Key::Down).send_keys(Key::Down).perform().await?;
        assert_eq!(at(&driver).await?["at"], 3);
        driver.action_chain().send_keys(Key::Up).perform().await?;
        let moved = at(&driver).await?;
        assert_eq!(moved["at"], 2, "{moved}");
        assert_eq!(moved["stops"], 1, "{moved}");
        driver.action_chain().send_keys(Key::End).perform().await?;
        let end = at(&driver).await?;
        assert_eq!(end["at"], end["last"], "{end}");
        driver.action_chain().send_keys(Key::Down).perform().await?;
        assert_eq!(at(&driver).await?["at"], end["last"], "Down stops at the last line");
        driver.action_chain().send_keys(Key::Home).perform().await?;
        assert_eq!(at(&driver).await?["at"], 0);
        driver.action_chain().send_keys(Key::Up).perform().await?;
        assert_eq!(at(&driver).await?["at"], 0, "Up stops at the first item");
        driver.action_chain().send_keys(Key::Down).perform().await?;
        let row = at(&driver).await?;
        assert_eq!(row["row"], true, "{row}");
        driver.action_chain().send_keys(Key::Tab).perform().await?;
        let left = at(&driver).await?;
        assert_eq!(left["inside"], false, "Tab leaves the list: {left}");
        observed(
            &driver,
            "const list = document.querySelector('.kwic-list'); return !!(list.compareDocumentPosition(document.activeElement) & Node.DOCUMENT_POSITION_FOLLOWING)",
            json!(true),
            "Tab moves to the control after the list",
        )
        .await?;
        driver.action_chain().key_down(Key::Shift).send_keys(Key::Tab).key_up(Key::Shift).perform().await?;
        assert_eq!(at(&driver).await?["at"], 1, "Shift+Tab returns to the line last focused");
        driver.action_chain().send_keys(Key::Enter).perform().await?;
        wait_for(&driver, "return !document.querySelector('#token-lab').hidden", "the example opened with Enter").await?;
        observed(
            &driver,
            "const rows = [...document.querySelectorAll('.kwic-row')]; return [rows[0].getAttribute('aria-expanded'), rows[0].tabIndex, rows.filter((row) => row.tabIndex >= 0).length]",
            json!(["true", 0, 1]),
            "the opened line keeps the list's one Tab stop",
        )
        .await?;
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn an_oversized_example_keeps_its_place_beside_readable_rows() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let lemma = "x".repeat(1024 * 1024 + 1);
    let artifact = support::build_artifact_with_metadata(
        (0..6)
            .map(|document| {
                let lemma = if document == 0 { lemma.as_str() } else { "cat" };
                let sentence = vec![TokenInfo::default().with_orth("cat").with_lemma(lemma)];
                ("doc.md", document, false, vec![sentence])
            })
            .collect(),
        Vec::new(),
        (0..6).map(|_| Box::default()).collect(),
    );
    let served = serve_artifact(
        artifact,
        CATALOG,
        |artifact, table| write_table(artifact, table, 6),
        &[],
    )
    .await;
    let result: WebDriverResult<()> = async {
        let root = served.base.trim_end_matches("frequencies");
        let query = serde_urlencoded::to_string([
            ("fresh", "1"),
            ("run", "1"),
            ("source", "large := [lemma~x | lemma=cat].\n"),
        ])
        .unwrap();
        driver.goto(format!("{root}#{query}")).await?;
        select_question_kwic(&driver).await?;
        wait_for(
            &driver,
            "return document.querySelector('#status').textContent === 'Complete' && \
             document.querySelector('.kwic-list')?.getAttribute('aria-busy') === 'false' && \
             document.querySelectorAll('.kwic-row').length === 6",
            "all six results in the completed concordance",
        )
        .await?;
        observed(
            &driver,
            "const rows = [...document.querySelectorAll('.kwic-row')]; \
             return [rows[0].querySelector('mark').textContent, rows[0].hasAttribute('href'), \
                rows[1].querySelector('mark').textContent, rows[1].hasAttribute('href'), \
                document.querySelector('#error').hidden]",
            json!(["Full example in JSONL download", false, "cat", true, true]),
            "an omitted preview beside an ordinary match",
        )
        .await?;
        let lemmas = driver
            .execute_async(
                "const done = arguments[arguments.length - 1]; \
                 fetch(document.querySelector('.result-header a[href$=\"/export\"]').href).then((r) => r.text()) \
                   .then((body) => done(body.trim().split('\\n').map((line) => JSON.parse(line)) \
                     .filter((record) => record.type === 'match') \
                     .map((record) => record.data.tokens[0].attributes.lemma.length)));",
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        assert_eq!(lemmas, json!([lemma.len(), 3, 3, 3, 3, 3]));
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

#[tokio::test]
async fn clicking_the_jsonl_link_saves_every_result_and_keeps_the_page() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let directory = tempfile::tempdir().unwrap();
    driver
        .cdp()
        .send_raw(
            "Browser.setDownloadBehavior",
            json!({"behavior": "allow", "downloadPath": directory.path()}),
        )
        .await?;
    let served = serve_entries().await;
    let result: WebDriverResult<()> = async {
        driver
            .goto(limited_question(&served.base, ENTRY_CATALOG, true))
            .await?;
        let link = ".result-header a[aria-label=\"Download all results as JSONL\"]";
        wait_for(
            &driver,
            &format!("return document.querySelector('{link}') ? true : null"),
            "the download link of the completed search",
        )
        .await?;
        let served_body = driver
            .execute_async(
                &format!(
                    "const done = arguments[arguments.length - 1]; \
                     fetch(document.querySelector('{link}').href).then((r) => r.text()).then(done);"
                ),
                Vec::new(),
            )
            .await?
            .json()
            .clone();
        let before = driver.current_url().await?;
        driver.find(By::Css(link)).await?.click().await?;
        let file = directory.path().join("matches.jsonl");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !file.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let saved = std::fs::read_to_string(&file)
            .map_err(|error| WebDriverError::Timeout(format!("matches.jsonl download: {error}")))?;
        assert_eq!(saved, served_body.as_str().unwrap());
        let records: Vec<Value> = saved
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let matches = records.iter().filter(|r| r["type"] == "match").count();
        assert!(matches > 0);
        assert_eq!(records.first().unwrap()["type"], "run");
        assert_eq!(records.last().unwrap()["type"], "complete");
        assert_eq!(
            records.last().unwrap()["data"]["counters"]["matches_found"],
            json!(matches)
        );
        assert_eq!(driver.windows().await?.len(), 1);
        assert_eq!(driver.current_url().await?, before);
        assert_eq!(
            driver
                .execute(
                    &format!("return !!document.querySelector('{link}')"),
                    Vec::new()
                )
                .await?
                .json(),
            &json!(true)
        );
        assert_no_page_errors(&driver).await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}

const KEYED_CATALOG: &str = "key: [$w.text]\npattern glass := (?<w> [text=ガラス]).\n";

/// Two sentences of one work whose first words share the match key ガラス:
/// one spelled in half-width katakana, one in full width.
fn variant_sentences() -> Vec<Vec<TokenInfo>> {
    let word = |surface: &str, text: &str| {
        TokenInfo::default()
            .with_orth(text)
            .with_surface(surface)
            .with_pos("NOUN")
    };
    vec![
        vec![
            word("ｶﾞﾗｽ", "ガラス"),
            word("の", "の"),
            word("靴", "靴"),
            word("。", "。"),
        ],
        vec![
            word("ガラス", "ガラス"),
            word("が", "が"),
            word("割れる", "割れる"),
            word("。", "。"),
        ],
    ]
}

/// The count table `ling summarize --write-table` writes for
/// `KEYED_CATALOG`: one entry, keyed by `text`, counted in both sentences.
fn write_keyed_table(artifact: &std::path::Path, destination: &std::path::Path) {
    let mut writer = CountTableWriter::create(
        destination,
        table_key(artifact, KEYED_CATALOG),
        vec!["glass".into()],
        vec![GroupSchema {
            kind: GroupKind::Key,
            names: vec!["$w.text".into()],
        }],
        &["ガラス"],
        &[DocumentTotals {
            document_id: 0,
            sentences: 2,
            tokens: 8,
        }],
    )
    .unwrap();
    writer.begin_entry(0, 0, &[0]).unwrap();
    writer.add_sentence(0, 0, 1).unwrap();
    writer.add_sentence(0, 1, 1).unwrap();
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
}

/// A table row keyed by `text` names the key; its lines, the reader and the
/// question page show each occurrence's exact surface.
#[tokio::test]
async fn a_folded_key_lists_and_reads_each_exact_surface() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let served = serve_artifact(
        support::build_artifact(vec![("work.txt", 0, false, variant_sentences())]),
        KEYED_CATALOG,
        write_keyed_table,
        &[],
    )
    .await;
    let result: WebDriverResult<()> = async {
        open(&driver, &format!("{}#pattern=glass", served.base), ROWS).await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('table.freq tbody tr button.open-entry')].map((b) => b.textContent.trim())",
            json!(["ガラス"]),
            "the row names the key",
        )
        .await?;
        driver
            .execute("document.querySelector('button.open-entry').click()", Vec::new())
            .await?;
        wait_for(&driver, ENTRY_READY, "the entry").await?;
        wait_for(
            &driver,
            "return document.querySelectorAll('.fq-kwic li').length === 2",
            "both lines",
        )
        .await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('.fq-kwic li')].map((li) => [li.querySelector('.kn').textContent, li.querySelector('.kr').textContent])",
            json!([["ｶﾞﾗｽ", "ｶﾞﾗｽの靴。"], ["ガラス", "ガラスが割れる。"]]),
            "each line's exact surface",
        )
        .await?;
        driver
            .find(By::Css(".fq-kwic li[data-i='0'] .kn"))
            .await?
            .click()
            .await?;
        wait_for(
            &driver,
            "return document.querySelector('[data-fq-reader]').open && !!document.querySelector('#document-reader-body mark')",
            "the reader",
        )
        .await?;
        observed(
            &driver,
            "return [...document.querySelectorAll('#document-reader-body mark')].map((m) => m.textContent).join('')",
            json!("ｶﾞﾗｽ"),
            "the reader marks the exact surface",
        )
        .await?;
        let inspect: String = driver
            .execute(
                "return document.querySelector(\".fq-kwic li[data-i='0'] .fq-inspect\").href",
                Vec::new(),
            )
            .await?
            .convert()?;
        driver.goto("about:blank").await?;
        driver.goto(&inspect).await?;
        wait_for(
            &driver,
            "const lab = document.querySelector('#token-lab'); return !!lab && !lab.hidden && !!document.querySelector('#focused-sentence mark')",
            "the inspected example",
        )
        .await?;
        observed(
            &driver,
            "return {sentence: document.querySelector('#focused-sentence').textContent, \
              mark: document.querySelector('#focused-sentence mark').textContent, \
              words: [...document.querySelectorAll('#token-ribbon button')].map((b) => b.textContent)}",
            json!({"sentence": "ｶﾞﾗｽの靴。", "mark": "ｶﾞﾗｽ", "words": ["ｶﾞﾗｽ", "の", "靴", "。"]}),
            "the question page shows the exact surface",
        )
        .await?;
        observed(&driver, PAGE_ERRORS, json!([]), "page errors").await
    }
    .await;
    driver.quit().await?;
    served.server.abort();
    result
}
