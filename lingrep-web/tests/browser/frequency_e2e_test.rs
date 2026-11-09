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
