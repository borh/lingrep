#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use std::ffi::OsStr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::token_match::TokenInfo;
use lingrep_corpus::{
    CorpusArtifact, CountTableWriter, DocumentTotals, GroupKind, GroupSchema, ReadingSidecar,
    ReadingSidecarKey,
};
use serde_json::json;
use thirtyfour::prelude::*;
use tokio::net::TcpListener;

use crate::frequency_e2e_test::{
    assert_no_page_errors, driver, record_errors, table_key, two_frames, wait_for,
};

#[allow(dead_code, clippy::duplicate_mod)]
#[path = "../suite/v6_support.rs"]
mod support;

const CATALOG: &str = "pattern cat := [text=猫] [text=が].\n";

/// A paper whose 猫が occurrences fill more than one page of lines, under
/// headings, in paragraphs, a list and a table, in words the test
/// dictionary segments. Its first occurrence crosses into bold text, and
/// paragraphs without one after them make it more than one reader page.
fn paper() -> String {
    let subjects = ["犬", "鳥", "私", "彼"];
    let verbs = ["走る", "眠る", "進む", "食べる"];
    let mut text = String::from("# 猫の研究\n\n研究の文です。\n\n猫**が鳴く**。\n\n");
    for index in 0..80 {
        if index % 10 == 0 {
            text.push_str(&format!("## 第{index}節\n\n"));
        }
        text.push_str(&format!(
            "{}が{}。\n猫が鳴く。\n\n",
            subjects[index % 4],
            verbs[index / 4 % 4]
        ));
    }
    text.push_str("- 猫が眠る。\n- 犬が走る。\n\n| 名 | 値 |\n|---|---|\n| 猫 | 一 |\n\n");
    for _ in 0..3000 {
        text.push_str("犬が走る。\n\n");
    }
    text
}

/// Writes the count table `ling summarize --write-table` writes for
/// `CATALOG`, from each document's sentences as their written words.
fn write_table(artifact: &Path, destination: &Path, documents: &[Vec<Vec<String>>]) {
    let totals = documents
        .iter()
        .enumerate()
        .map(|(document, sentences)| DocumentTotals {
            document_id: document as u64,
            sentences: sentences.len() as u64,
            tokens: sentences.iter().map(Vec::len).sum::<usize>() as u64,
        })
        .collect::<Vec<_>>();
    let mut writer = CountTableWriter::create(
        destination,
        table_key(artifact, CATALOG),
        vec!["cat".into()],
        vec![GroupSchema {
            kind: GroupKind::Surface,
            names: Vec::new(),
        }],
        &["猫が"],
        &totals,
    )
    .unwrap();
    writer.begin_entry(0, 0, &[0]).unwrap();
    let mut ordinal = 0u64;
    for (document, sentences) in documents.iter().enumerate() {
        for words in sentences {
            let count = words
                .windows(2)
                .filter(|pair| pair[0] == "猫" && pair[1] == "が")
                .count() as u64;
            if count > 0 {
                writer
                    .add_sentence(document as u32, ordinal, count)
                    .unwrap();
            }
            ordinal += 1;
        }
    }
    writer
        .finish(&CancellationFlag::new())
        .unwrap()
        .publish()
        .unwrap();
}

/// Each document's sentences of the Markdown artifact, as written words.
fn markdown_words(fixture: &support::MarkdownFixture) -> Vec<Vec<Vec<String>>> {
    let artifact = CorpusArtifact::open(&fixture.artifact).unwrap();
    let sidecar = ReadingSidecar::open(
        &fixture.sidecar,
        &ReadingSidecarKey::for_artifact(&artifact),
    )
    .unwrap();
    let source = artifact.sentence_source().unwrap();
    (0..sidecar.document_count())
        .map(|document| {
            sidecar
                .document(DocumentId::new(document))
                .unwrap()
                .sentences
                .map(|sentence| {
                    source
                        .sentence_words_bounded(sentence, usize::MAX)
                        .unwrap()
                        .unwrap()
                })
                .collect()
        })
        .collect()
}

/// A plaintext work: sentences with no headings or paragraphs.
fn plain_words() -> Vec<Vec<Vec<String>>> {
    vec![
        (0..6)
            .map(|sentence| {
                let middle = if sentence % 2 == 0 { "猫" } else { "犬" };
                ["その", middle, "が", "眠る", "。"]
                    .map(str::to_owned)
                    .to_vec()
            })
            .collect(),
    ]
}

struct Served {
    base: String,
    statuses: Arc<Mutex<Vec<String>>>,
    server: tokio::task::JoinHandle<()>,
}

/// Serves the Markdown corpus as `default` and the plaintext corpus as
/// `plain`, each with its count table, and the Markdown sidecar when
/// `sidecar` is set.
async fn serve(
    markdown: &support::MarkdownFixture,
    plain: &support::ArtifactFixture,
    sidecar: bool,
) -> Served {
    let directory = markdown.directory.path();
    let root = directory.join("patterns");
    if !root.exists() {
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("cat.lx"), CATALOG).unwrap();
        write_table(
            &markdown.artifact,
            &directory.join("papers.lingtable"),
            &markdown_words(markdown),
        );
        write_table(
            &plain.path,
            &directory.join("plain.lingtable"),
            &plain_words(),
        );
    }
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let plain_corpus = format!("plain={}", plain.path.display());
    let papers = directory.join("papers.lingtable");
    let plain_table = directory.join("plain.lingtable");
    let mut argv: Vec<&OsStr> = vec![
        "lingrep-web".as_ref(),
        "--dict-path".as_ref(),
        dictionary.path().as_os_str(),
        "--input".as_ref(),
        markdown.artifact.as_os_str(),
        "--corpus".as_ref(),
        plain_corpus.as_ref(),
        "--pattern-root".as_ref(),
        root.as_os_str(),
        "--count-table".as_ref(),
        papers.as_os_str(),
        "--count-table".as_ref(),
        plain_table.as_os_str(),
        "--base-path".as_ref(),
        "/sub".as_ref(),
    ];
    if sidecar {
        argv.extend(["--reading-sidecar".as_ref(), markdown.sidecar.as_os_str()]);
    }
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
    }
}

/// The address of 猫が's entry level in `corpus`.
fn entry(served: &Served, corpus: &str) -> String {
    format!(
        "{}#corpus={corpus}&catalog=cat.lx&pattern=cat&entry=%7B%22kind%22%3A%22surface%22%2C%22value%22%3A%22%E7%8C%AB%E3%81%8C%22%7D",
        served.base
    )
}

const LINES_READY: &str = "const e = document.querySelector('[data-fq-entry]'), n = document.querySelectorAll('.fq-kwic li').length; \
    return !e.hasAttribute('aria-busy') && n > 0 && n";

/// Clicks `part` of line `index` of the entry level with a layout-shift
/// observer running, and waits until the reader holds `ready`.
async fn open_line(
    driver: &WebDriver,
    index: usize,
    part: &str,
    ready: &str,
) -> WebDriverResult<()> {
    two_frames(driver).await?;
    driver
        .execute(
            "window.__shift = 0; window.__shifted = []; new PerformanceObserver((list) => { for (const e of list.getEntries()) { __shift += e.value; __shifted.push(...e.sources.map((s) => s.node?.outerHTML?.slice(0, 120))); } }).observe({type: 'layout-shift'});",
            Vec::new(),
        )
        .await?;
    driver
        .find(By::Css(format!(".fq-kwic li[data-i='{index}'] {part}")))
        .await?
        .click()
        .await?;
    wait_for(
        driver,
        &format!("return document.querySelector('[data-fq-reader]').open && !!document.querySelector('#document-reader-body {ready}')"),
        ready,
    )
    .await?;
    two_frames(driver).await
}

/// The reader's occurrence: its text, its background against the KWIC
/// match token's, whether it lies inside the reader's view, and the
/// layout shift since `open_line`.
const OCCURRENCE: &str = "const body = document.querySelector('#document-reader-body'), marks = [...body.querySelectorAll('mark')]; \
    const probe = document.createElement('span'); probe.style.background = 'var(--match-bg)'; document.body.append(probe); \
    const token = getComputedStyle(probe).backgroundColor; probe.remove(); \
    const box = marks[0].getBoundingClientRect(), view = body.getBoundingClientRect(); \
    return {text: marks.map((m) => m.textContent).join(''), background: getComputedStyle(marks[0]).backgroundColor, token, \
      inView: box.top >= view.top && box.bottom <= view.bottom, shift: __shift, shifted: __shifted}";

/// The reader's state: whether it is open, the `read` and `scope` the
/// address holds, the focused line and the window's scroll.
const READER_STATE: &str = "const h = new URLSearchParams(location.hash.slice(1)); \
    return {open: document.querySelector('[data-fq-reader]').open, read: h.get('read'), scope: h.get('scope'), entry: h.has('entry'), \
      focused: document.activeElement.closest('li')?.dataset.i ?? null, y: scrollY, lines: document.querySelectorAll('.fq-kwic li').length}";

#[tokio::test]
async fn a_line_opens_its_document_and_returning_restores_the_lines() -> WebDriverResult<()> {
    let Some(driver) = driver().await? else {
        return Ok(());
    };
    let text = paper();
    let markdown =
        support::markdown_fixture(&[("a.md", &text), ("b.md", "# 別\n\n犬が鳴く。\n")]).await;
    let plain = support::build_artifact(vec![(
        "work.txt",
        0,
        false,
        plain_words()[0]
            .iter()
            .map(|words| {
                words
                    .iter()
                    .map(|word| TokenInfo::default().with_orth(word))
                    .collect()
            })
            .collect(),
    )]);
    let served = serve(&markdown, &plain, true).await;

    driver.goto(entry(&served, "default")).await?;
    let first = wait_for(&driver, LINES_READY, "the lines").await?;
    assert_eq!(first, json!(50));
    driver.find(By::Css("[data-e=more]")).await?.click().await?;
    let lines = wait_for(
        &driver,
        "return document.querySelectorAll('.fq-kwic li').length > 50 && document.querySelectorAll('.fq-kwic li').length",
        "the second page of lines",
    )
    .await?;
    let before = driver
        .execute(
            "const line = document.querySelector(\".fq-kwic li[data-i='60']\"); line.scrollIntoView({block: 'center'}); \
             return {y: scrollY, node: line.querySelector('.kn').textContent, read: `${line.dataset.sentence}.${line.dataset.start}.${line.dataset.end}`}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert!(before["y"].as_f64().unwrap() > 0.0, "{before}");

    // The rendered document opens without moving anything, at the
    // occurrence, marked with the KWIC match token, and the address names it.
    open_line(&driver, 60, ".kl", "[data-doc-reader][data-marked]").await?;
    let shown = driver.execute(OCCURRENCE, Vec::new()).await?.json().clone();
    assert_eq!(shown["text"], before["node"], "{shown}");
    assert_eq!(shown["background"], shown["token"], "{shown}");
    assert_eq!(shown["inView"], true, "{shown}");
    assert_eq!(shown["shift"], json!(0), "{shown}");
    let opened = driver
        .execute(READER_STATE, Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(opened["read"], before["read"], "{opened}");
    assert_eq!(opened["scope"], json!(null), "{opened}");
    let structure = driver
        .execute(
            "const text = document.querySelector('#document-reader-body .doc-reader-text'); \
             return {h1: text.querySelectorAll('h1').length, h2: text.querySelectorAll('h2').length, p: text.querySelectorAll('p').length, \
               controls: document.querySelectorAll('#document-reader-body [data-reading-scope]').length}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(structure["h1"], json!(1), "{structure}");
    assert!(structure["h2"].as_u64().unwrap() >= 2, "{structure}");
    assert!(structure["p"].as_u64().unwrap() >= 20, "{structure}");
    assert_eq!(structure["controls"], json!(2), "{structure}");

    // Later text that fails to load says so on its control.
    driver
        .execute(
            "const more = document.querySelector('#document-reader-body [data-reader-later]'), failing = more.cloneNode(true); \
             failing.setAttribute('hx-get', more.getAttribute('hx-get').replace(/blocks_from=\\d+/, 'blocks_from=999999')); \
             more.replaceWith(failing); htmx.process(failing); failing.click();",
            Vec::new(),
        )
        .await?;
    let failed = wait_for(
        &driver,
        "const more = document.querySelector('#document-reader-body [data-reader-later][data-failed]'); return more && more.textContent",
        "the failed Later text control",
    )
    .await?;
    assert_eq!(
        failed,
        json!("The text could not load. Select to try again.")
    );
    let refused = std::mem::take(&mut *served.statuses.lock().unwrap());
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0].starts_with("400 Bad Request /sub/api/read/default/"),
        "{refused:?}"
    );

    // Returning closes the reader with the lines, their scroll and the
    // opened line's focus as they were, and takes the reader out of the
    // address.
    driver
        .find(By::Css("[data-fq-return]"))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return !document.querySelector('#document-reader-body').firstChild && !location.hash.includes('read=')",
        "the lines again",
    )
    .await?;
    let back = driver
        .execute(READER_STATE, Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(
        back,
        json!({"open": false, "read": null, "scope": null, "entry": true, "focused": "60", "y": before["y"], "lines": lines})
    );

    // Escape also returns to the lines, not to the table.
    open_line(&driver, 3, ".kn", "[data-doc-reader][data-marked]").await?;
    driver
        .find(By::Css("body"))
        .await?
        .send_keys(Key::Escape)
        .await?;
    let escaped = wait_for(
        &driver,
        "return !document.querySelector('#document-reader-body').firstChild && !location.hash.includes('read=') && \
          [document.activeElement.closest('li')?.dataset.i ?? null, location.hash.includes('entry='), !!document.querySelector('[data-fq-entry]:not([hidden])')]",
        "the lines after Escape",
    )
    .await?;
    assert_eq!(escaped, json!(["3", true, true]));

    // The browser's Back closes the reader and keeps the lines.
    open_line(&driver, 0, ".kn", "[data-doc-reader][data-marked]").await?;
    let y = driver
        .execute("return scrollY", Vec::new())
        .await?
        .json()
        .clone();
    let crossing = driver
        .execute(
            "const marks = [...document.querySelectorAll('#document-reader-body mark')]; \
             return {texts: marks.map((m) => m.textContent), bold: marks.map((m) => m.parentElement.tagName === 'STRONG')}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(
        crossing,
        json!({"texts": ["猫", "が"], "bold": [false, true]}),
        "a match across inline markup is marked in each text node"
    );
    driver.back().await?;
    wait_for(
        &driver,
        "return !document.querySelector('[data-fq-reader]').open && !document.querySelector('#document-reader-body').firstChild",
        "the lines after Back",
    )
    .await?;
    let backed = driver
        .execute(READER_STATE, Vec::new())
        .await?
        .json()
        .clone();
    assert_eq!(
        backed,
        json!({"open": false, "read": null, "scope": null, "entry": true, "focused": "0", "y": y, "lines": lines})
    );

    // A line opened before the reader's close event, or before the Back that
    // the close starts, opens that line, and Back from it returns to the lines.
    for opening in [
        "document.querySelector('[data-fq-return]').click(); open();",
        "reader.addEventListener('close', open, {once: true}); document.querySelector('[data-fq-return]').click();",
    ] {
        open_line(&driver, 3, ".kn", "[data-doc-reader][data-marked]").await?;
        driver
            .execute(
                format!(
                    "const reader = document.querySelector('[data-fq-reader]'); \
                     const open = () => document.querySelector(\".fq-kwic li[data-i='0'] .kn\").click(); {opening}"
                ),
                Vec::new(),
            )
            .await?;
        // Long enough for a pending Back to be traversed.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let reopened = wait_for(
            &driver,
            "const l = document.querySelector(\".fq-kwic li[data-i='0']\").dataset; \
             return !!document.querySelector('#document-reader-body [data-doc-reader][data-marked]') && \
               [document.querySelector('[data-fq-reader]').open, new URLSearchParams(location.hash.slice(1)).get('read') === `${l.sentence}.${l.start}.${l.end}`]",
            "line 0 after closing line 3",
        )
        .await?;
        assert_eq!(reopened, json!([true, true]), "{opening}");
        driver.back().await?;
        let lines_again = wait_for(
            &driver,
            "return !document.querySelector('[data-fq-reader]').open && !location.hash.includes('read=') && location.hash.includes('entry=')",
            "the lines after Back",
        )
        .await?;
        assert_eq!(lines_again, json!(true), "{opening}");
    }

    // A reader closed before its document arrives stays empty.
    driver
        .execute(
            "window.__settled = 0; document.addEventListener('htmx:afterRequest', () => __settled++); \
             document.querySelector(\".fq-kwic li[data-i='3'] .kn\").click(); document.querySelector('[data-fq-return]').click();",
            Vec::new(),
        )
        .await?;
    wait_for(
        &driver,
        "return __settled > 0",
        "the closed reader's request",
    )
    .await?;
    two_frames(&driver).await?;
    let emptied = driver
        .execute(
            "return [document.querySelector('[data-fq-reader]').open, document.querySelector('#document-reader-body').childElementCount, location.hash.includes('read=')]",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(emptied, json!([false, 0, false]));

    // An address that names an occurrence reopens the reader on load, and
    // on reload, in the scope it names.
    let address = format!(
        "{}&read={}",
        entry(&served, "default"),
        before["read"].as_str().unwrap()
    );
    driver.goto("about:blank").await?;
    driver.goto(&address).await?;
    let deep = wait_for(
        &driver,
        "const marks = document.querySelectorAll('#document-reader-body [data-doc-reader][data-marked] mark'); \
         return document.querySelector('[data-fq-reader]').open && marks.length > 0 && [...marks].map((m) => m.textContent).join('')",
        "the reader the address names",
    )
    .await?;
    assert_eq!(deep, before["node"]);
    driver
        .find(By::Css(
            "#document-reader-body [data-reading-scope='paragraph']",
        ))
        .await?
        .click()
        .await?;
    wait_for(
        &driver,
        "return new URLSearchParams(location.hash.slice(1)).get('scope') === 'paragraph' && !!document.querySelector('#document-reader-body [data-reading-page] mark')",
        "the paragraph scope in the address",
    )
    .await?;
    driver.refresh().await?;
    let reloaded = wait_for(
        &driver,
        "const body = document.querySelector('#document-reader-body'); \
         return document.querySelector('[data-fq-reader]').open && !!body.querySelector('[data-reading-page] mark') && \
           {rendered: !!body.querySelector('[data-doc-reader]'), pressed: body.querySelector('[data-reading-scope=paragraph]').getAttribute('aria-pressed')}",
        "the reader after reload",
    )
    .await?;
    assert_eq!(reloaded, json!({"rendered": false, "pressed": "true"}));
    driver
        .find(By::Css("[data-fq-return]"))
        .await?
        .click()
        .await?;
    let closed = wait_for(
        &driver,
        "return !document.querySelector('[data-fq-reader]').open && !location.hash.includes('read=') && !location.hash.includes('scope=') && \
          document.querySelectorAll('.fq-kwic li').length > 0",
        "the lines after closing the addressed reader",
    )
    .await?;
    assert_eq!(closed, json!(true));

    // A plaintext work has no sidecar: its sentences are one run, with the
    // occurrence marked with the same token.
    driver.goto("about:blank").await?;
    driver.goto(entry(&served, "plain")).await?;
    assert_eq!(
        wait_for(&driver, LINES_READY, "the plain lines").await?,
        json!(3)
    );
    open_line(&driver, 1, ".kn", "[data-reading-page] mark").await?;
    let shown = driver.execute(OCCURRENCE, Vec::new()).await?.json().clone();
    assert_eq!(shown["text"], "猫が", "{shown}");
    assert_eq!(shown["background"], shown["token"], "{shown}");
    assert_eq!(shown["shift"], json!(0), "{shown}");
    let run = driver
        .execute(
            "const body = document.querySelector('#document-reader-body'); \
             return {runs: body.querySelectorAll('.reading-text').length, sentences: body.querySelectorAll('.reading-text [data-reading-sentence]').length, \
               headings: body.querySelectorAll('.reading-text :is(h1, h2, h3, h4), [data-reading-section]').length, rendered: !!body.querySelector('[data-doc-reader]')}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(
        run,
        json!({"runs": 1, "sentences": 4, "headings": 0, "rendered": false})
    );

    assert_no_page_errors(&driver).await?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    served.server.abort();

    // Without the sidecar, the Markdown corpus keeps the sentence reader.
    let served = serve(&markdown, &plain, false).await;
    driver.goto(entry(&served, "default")).await?;
    wait_for(&driver, LINES_READY, "the lines without a sidecar").await?;
    open_line(&driver, 1, ".kn", "[data-reading-page] mark").await?;
    let fallback = driver
        .execute(
            "const body = document.querySelector('#document-reader-body'); \
             return {rendered: !!body.querySelector('[data-doc-reader]'), mark: body.querySelector('mark').textContent}",
            Vec::new(),
        )
        .await?
        .json()
        .clone();
    assert_eq!(fallback, json!({"rendered": false, "mark": "猫が"}));
    assert_no_page_errors(&driver).await?;
    assert_eq!(*served.statuses.lock().unwrap(), Vec::<String>::new());
    driver.quit().await?;
    served.server.abort();
    Ok(())
}
