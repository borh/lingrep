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
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::{get, post};
use lingrep_web::{RequestStore, handlers};
use thirtyfour::prelude::*;
use tokio::net::TcpListener;

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <link rel="stylesheet" href="/assets/app.css">
  <link rel="stylesheet" href="/assets/editor/pattern-editor.css">
  <script src="/assets/vendor/htmx.min.js"></script>
  <script type="module" src="/assets/editor/pattern-editor.js"></script>
</head>
<body>
  <section class="corpus-context" data-corpus-digest="digest-a"></section>
  <div class="query-workspace">
    <div class="query-editor-column">
      <form hx-post="/api/search" hx-target="#status"
            data-pattern-submit="corpus-pattern" data-corpus-search>
        <label for="corpus-pattern">Pattern</label>
        <div id="corpus-editor" class="corpus-pattern-editor" data-editor-size>
          <textarea id="corpus-pattern" name="pattern" data-pattern-editor required></textarea>
        </div>
        <button type="button" data-editor-toggle aria-controls="corpus-editor"
                aria-expanded="false">Expand editor</button>
        <button type="submit">Search Corpus</button>
      </form>
    </div>
    <details class="query-library" data-query-library open>
      <summary>Queries <span data-query-edited hidden>Edited</span></summary>
      <div class="query-library-body">
        <div class="query-tabs" role="tablist" aria-label="Query library">
          <button type="button" role="tab" id="query-tab-recent"
                  aria-controls="query-panel" aria-selected="true"
                  data-query-tab="recent">Recent</button>
          <button type="button" role="tab" id="query-tab-saved"
                  aria-controls="query-panel" aria-selected="false" tabindex="-1"
                  data-query-tab="saved">Saved</button>
        </div>
        <div id="query-panel" role="tabpanel" aria-labelledby="query-tab-recent">
          <div class="query-library-actions">
            <button type="button" data-save-current>Save current</button>
            <button type="button" data-clear-recent>Clear recent history</button>
          </div>
          <div data-query-list></div>
          <button type="button" data-query-more hidden>Show more</button>
        </div>
        <p role="status" data-query-status></p>
      </div>
    </details>
  </div>
  <div id="status"></div>
  <script type="module" src="/assets/query-library.js"></script>
  <script type="module" src="/assets/editor-sizing.js"></script>
</body>
</html>"##;

async fn fixture(counter: Arc<AtomicUsize>) -> String {
    let app = Router::<RequestStore>::new()
        .route("/", get(|| async { Html(PAGE) }))
        .route(
            "/api/search",
            post(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Html("<p>searched</p>")
                }
            }),
        )
        .nest("/assets", handlers::results_assets::routes())
        .nest("/assets/editor", handlers::editor_assets::routes())
        .nest("/api/pattern", handlers::pattern_editor::routes())
        .with_state(RequestStore::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

// Mirrors PAGE but adds the third "Catalogs" tab and its list container, per
// the DOM contract `handlers::index::index_page` renders in production
// (`data-query-tab="catalogs"` / `#query-tab-catalogs` / `data-catalog-list` /
// `data-catalog-key`). Kept as a second minimal fixture rather than switching
// to the real templated index page, matching this file's existing convention
// of a hand-trimmed page that carries only the elements `query-library.js`
// touches.
const CATALOG_PAGE_TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <link rel="stylesheet" href="/assets/app.css">
  <link rel="stylesheet" href="/assets/editor/pattern-editor.css">
  <script src="/assets/vendor/htmx.min.js"></script>
  <script type="module" src="/assets/editor/pattern-editor.js"></script>
</head>
<body>
  <section class="corpus-context" data-corpus-digest="digest-a"></section>
  <div class="query-workspace">
    <div class="query-editor-column">
      <form hx-post="/api/search" hx-target="#status"
            data-pattern-submit="corpus-pattern" data-corpus-search>
        <label for="corpus-pattern">Pattern</label>
        <div id="corpus-editor" class="corpus-pattern-editor" data-editor-size>
          <textarea id="corpus-pattern" name="pattern" data-pattern-editor required></textarea>
        </div>
        <button type="button" data-editor-toggle aria-controls="corpus-editor"
                aria-expanded="false">Expand editor</button>
        <button type="submit">Search Corpus</button>
      </form>
    </div>
    <details class="query-library" data-query-library>
      <summary>Queries <span data-query-edited hidden>Edited</span></summary>
      <div class="query-library-body">
        <div class="query-tabs" role="tablist" aria-label="Query library">
          <button type="button" role="tab" id="query-tab-recent"
                  aria-controls="query-panel" aria-selected="true"
                  data-query-tab="recent">Recent</button>
          <button type="button" role="tab" id="query-tab-saved"
                  aria-controls="query-panel" aria-selected="false" tabindex="-1"
                  data-query-tab="saved">Saved</button>
          <button type="button" role="tab" id="query-tab-catalogs"
                  aria-controls="query-panel" aria-selected="false" tabindex="-1"
                  data-query-tab="catalogs">Catalogs</button>
        </div>
        <div id="query-panel" role="tabpanel" aria-labelledby="query-tab-recent">
          <div class="query-library-actions">
            <button type="button" data-save-current>Save current</button>
            <button type="button" data-clear-recent>Clear recent history</button>
          </div>
          <div data-query-list></div>
          <div class="catalog-list" data-catalog-list hidden>{{CATALOG_BUTTONS}}</div>
          <button type="button" data-query-more hidden>Show more</button>
        </div>
        <p role="status" data-query-status></p>
      </div>
    </details>
  </div>
  <div id="status"></div>
  <script type="module" src="/assets/query-library.js"></script>
  <script type="module" src="/assets/editor-sizing.js"></script>
</body>
</html>"##;

fn render_catalog_page(index: &lingrep_web::catalog_index::CatalogIndex) -> String {
    let buttons: String = index
        .keys()
        .map(|key| {
            format!(
                r#"<button type="button" class="catalog-entry" data-catalog-key="{key}">{key}</button>"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    CATALOG_PAGE_TEMPLATE.replace("{{CATALOG_BUTTONS}}", &buttons)
}

/// Same shape as [`fixture`], but serves the real catalog routes backed by a
/// [`lingrep_web::catalog_index::CatalogIndex`] discovered from `root`.
async fn fixture_with_catalogs(root: &std::path::Path) -> String {
    let index = Arc::new(lingrep_web::catalog_index::CatalogIndex::discover(root));
    let page = render_catalog_page(&index);
    let store = RequestStore::default().with_catalogs(Arc::clone(&index));
    let app = Router::<RequestStore>::new()
        .route(
            "/",
            get(move || {
                let page = page.clone();
                async move { Html(page) }
            }),
        )
        .nest("/assets", handlers::results_assets::routes())
        .nest("/assets/editor", handlers::editor_assets::routes())
        .nest("/api/pattern", handlers::pattern_editor::routes())
        .nest("/api", handlers::catalogs::routes())
        .with_state(store);
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

/// Read the first row's name without holding an element handle.
async fn row_name(driver: &WebDriver) -> WebDriverResult<String> {
    driver
        .execute(
            r#"const row=document.querySelector(".query-row-name");
               return row ? row.textContent.trim() : "";"#,
            vec![],
        )
        .await?
        .convert()
}

/// Click a selector at the element, after asserting it is what a user would hit.
///
/// Chromedriver's click is a coordinate click: it measures the element's box and
/// dispatches at that point. This page reflows after a click — restoring a query
/// loads the library and re-renders its rows — so the box can move between the
/// measurement and the dispatch, and the click then lands on the row instead of
/// the button. One visible status line, 19 px, is enough, and a loaded machine
/// starves the browser enough that the shift reliably arrives first — which is
/// how `query_library_uses_real_indexeddb_and_enhanced_editor` failed CI.
/// Dispatching in the same page turn as the hit test keeps
/// what this suite exists to catch — a control covered by an overlay, which fails
/// the hit test and is reported by name — without the coordinate race.
async fn click_selector(driver: &WebDriver, selector: &str) -> WebDriverResult<()> {
    let script = format!(
        r#"const target=document.querySelector({selector});
           if(!target) return "missing";
           target.scrollIntoView({{block:"nearest",inline:"nearest"}});
           const box=target.getBoundingClientRect();
           const x=box.x+box.width/2, y=box.y+box.height/2;
           const hit=document.elementFromPoint(x,y);
           if(!(hit===target||target.contains(hit))) {{
             return "covered:"+(hit?hit.tagName+"."+String(hit.className).slice(0,60):"null");
           }}
           for (const type of ["pointerdown","mousedown","pointerup","mouseup","click"]) {{
             target.dispatchEvent(new MouseEvent(type,{{bubbles:true,cancelable:true,view:window,clientX:x,clientY:y}}));
           }}
           return "clicked";"#,
        selector = js_string(selector),
    );
    let mut last = String::from("missing");
    for _ in 0..40 {
        let outcome: String = driver.execute(&script, vec![]).await?.convert()?;
        if outcome == "clicked" {
            return Ok(());
        }
        last = outcome;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{selector} never became clickable: {last}");
}

fn js_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

async fn wait_for_count(driver: &WebDriver, selector: &str, count: usize) -> WebDriverResult<()> {
    for _ in 0..100 {
        if driver.find_all(By::Css(selector)).await?.len() == count {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {count} elements matching {selector}");
}

async fn submit_source(driver: &WebDriver, source: &str) -> WebDriverResult<()> {
    driver
        .execute(
            r##"const t=document.querySelector("#corpus-pattern");
               t.value=arguments[0];
               t.dispatchEvent(new Event("input",{bubbles:true}));
               document.querySelector("[data-corpus-search]").requestSubmit();"##,
            vec![serde_json::Value::String(source.to_string())],
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn query_library_uses_real_indexeddb_and_enhanced_editor() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".to_string());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        return Ok(());
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let address = fixture(Arc::clone(&counter)).await;
    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new")?;
    caps.add_arg("--no-sandbox")?;
    let driver = WebDriver::new(&driver_url, caps).await?;
    driver.goto(&address).await?;

    eprintln!("query-library-e2e: wait for enhanced editor");
    wait_for_count(&driver, ".pattern-editor .cm-editor", 1).await?;
    let default_is_bounded: bool = driver
        .execute(
            r#"const editor=document.querySelector('[data-editor-size]');const height=editor.getBoundingClientRect().height;return height<=Math.min(384,innerHeight*.45)+1&&height>=192;"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        default_is_bounded,
        "editor default height escaped its bounds"
    );
    click_selector(&driver, "[data-editor-toggle]").await?;
    let expanded_is_bounded: bool = driver
        .execute(
            r#"const editor=document.querySelector('[data-editor-size]');const toggle=document.querySelector('[data-editor-toggle]');return toggle.getAttribute('aria-expanded')==='true'&&toggle.textContent==='Collapse editor'&&Math.abs(editor.getBoundingClientRect().height-innerHeight*.75)<=1;"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        expanded_is_bounded,
        "editor expansion state is inconsistent"
    );
    driver
        .execute(
            r#"localStorage.setItem('lingrep-web:editor-height:v1','300');localStorage.setItem('lingrep-web:editor-expanded:v1','false');"#,
            vec![],
        )
        .await?;
    driver.refresh().await?;
    wait_for_count(&driver, ".pattern-editor .cm-editor", 1).await?;
    let restored_height: bool = driver
        .execute(
            r#"const editor=document.querySelector('[data-editor-size]');return Math.abs(editor.getBoundingClientRect().height-300)<=1&&document.querySelector('[data-editor-toggle]').getAttribute('aria-expanded')==='false';"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(restored_height, "stored editor height was not restored");
    eprintln!("query-library-e2e: submit and dedupe");
    submit_source(&driver, "a := [upos=NOUN].").await?;
    wait_for_count(&driver, ".query-row", 1).await?;
    submit_source(&driver, "b := [upos=VERB].").await?;
    wait_for_count(&driver, ".query-row", 2).await?;
    submit_source(&driver, "a := [upos=NOUN].").await?;

    for _ in 0..100 {
        if counter.load(Ordering::SeqCst) == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Read names in one page turn. Element handles do not survive the library's
    // re-render, and under load the re-render lands between `find_all` and the
    // per-element `text()` calls, which is a `stale element reference` rather
    // than the assertion this test is about.
    let names: Vec<String> = driver
        .execute(
            r#"return [...document.querySelectorAll(".query-row-name")].map((row) => row.textContent.trim());"#,
            vec![],
        )
        .await?
        .convert()?;

    eprintln!("query-library-e2e: restore");
    let before_restore = counter.load(Ordering::SeqCst);
    click_selector(&driver, r#"[data-query-id] [data-query-action="restore"]"#).await?;
    let editor_source: String = driver
        .execute(
            r#"return document.querySelector(".cm-content").textContent;"#,
            vec![],
        )
        .await?
        .convert()?;

    eprintln!("query-library-e2e: save and switch projection");
    click_selector(&driver, r#"[data-query-action="save"]"#).await?;
    wait_for_count(&driver, ".query-inline-form", 1).await?;
    eprintln!("query-library-e2e: save form opened");
    driver
        .execute(
            r#"const input=document.querySelector(".query-inline-form input");
               input.value="Research query";
               input.dispatchEvent(new Event("input",{bubbles:true}));"#,
            vec![],
        )
        .await?;
    eprintln!("query-library-e2e: save name entered");
    click_selector(&driver, r#".query-inline-form button[type="submit"]"#).await?;
    wait_for_count(&driver, ".query-inline-form", 0).await?;
    let save_focus_retained: bool = driver
        .execute(
            "return document.querySelector('[data-query-library]').contains(document.activeElement)",
            vec![],
        )
        .await?
        .convert()?;
    assert!(save_focus_retained, "Save must retain focus in the panel");
    eprintln!("query-library-e2e: save submitted");
    click_selector(&driver, r#"[data-query-tab="saved"]"#).await?;
    eprintln!("query-library-e2e: saved tab selected");
    wait_for_count(&driver, ".query-row", 1).await?;
    eprintln!("query-library-e2e: saved row rendered");
    assert_eq!(
        row_name(&driver).await?,
        "Research query",
        "Saved name must be the one submitted"
    );

    eprintln!("query-library-e2e: reload persistence");
    driver.refresh().await?;
    wait_for_count(&driver, ".pattern-editor .cm-editor", 1).await?;
    eprintln!("query-library-e2e: mismatch confirmation");
    click_selector(&driver, r#"[data-query-tab="saved"]"#).await?;
    wait_for_count(&driver, ".query-row", 1).await?;
    assert_eq!(
        row_name(&driver).await?,
        "Research query",
        "Saved entries must survive navigation"
    );

    eprintln!("query-library-e2e: clear and undo");
    click_selector(&driver, r#"[data-query-tab="recent"]"#).await?;
    wait_for_count(&driver, ".query-row", 2).await?;
    driver
        .execute(
            r#"document.querySelector(".corpus-context").dataset.corpusDigest="digest-b";
               document.dispatchEvent(new CustomEvent("htmx:afterSwap",{
                 detail:{target:document.querySelector(".corpus-context")}
               }));"#,
            vec![],
        )
        .await?;
    wait_for_count(&driver, ".query-mismatch", 2).await?;
    click_selector(&driver, r#"[data-query-action="restore-run"]"#).await?;
    wait_for_count(&driver, r#"[data-query-action="confirm-run"]"#, 1).await?;
    assert_eq!(
        counter.load(Ordering::SeqCst),
        3,
        "known mismatch must wait for inline confirmation"
    );
    click_selector(&driver, r#"[data-query-action="confirm-run"]"#).await?;
    for _ in 0..100 {
        if counter.load(Ordering::SeqCst) == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    click_selector(&driver, "[data-clear-recent]").await?;
    wait_for_count(&driver, ".query-row", 0).await?;
    let clear_focus_retained: bool = driver
        .execute(
            "return document.querySelector('[data-query-library]').contains(document.activeElement)",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        clear_focus_retained,
        "Clear recent must retain focus in the panel"
    );
    click_selector(&driver, r#"[data-query-action="undo"]"#).await?;
    wait_for_count(&driver, ".query-row", 2).await?;

    eprintln!("query-library-e2e: keyboard and narrow layout");
    let recent_tab = driver.find(By::Css(r#"[data-query-tab="recent"]"#)).await?;
    recent_tab.send_keys(Key::Right).await?;
    let saved_selected: String = driver
        .execute(
            r#"const tab=document.querySelector('[data-query-tab="saved"]');
               return tab ? tab.getAttribute("aria-selected") : "";"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(saved_selected, "true");

    driver.set_window_rect(0, 0, 320, 780).await?;
    let overflow: i64 = driver
        .execute(
            "return document.documentElement.scrollWidth-window.innerWidth",
            vec![],
        )
        .await?
        .convert()?;
    let segmented_tabs_are_coherent: bool = driver
        .execute(
            r#"const tabs=document.querySelector('.query-tabs');const buttons=[...tabs.querySelectorAll('[role=tab]')];const selected=buttons.find((button)=>button.getAttribute('aria-selected')==='true');const idle=buttons.find((button)=>button!==selected);return getComputedStyle(tabs).display==='inline-flex'&&Math.abs(buttons[0].getBoundingClientRect().height-buttons[1].getBoundingClientRect().height)<=1&&getComputedStyle(selected).backgroundColor!==getComputedStyle(idle).backgroundColor&&getComputedStyle(document.querySelector('[data-query-more]')).display==='none';"#,
            vec![],
        )
        .await?
        .convert()?;

    driver.quit().await?;
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    assert_eq!(names, vec!["a", "b"]);
    assert_eq!(before_restore, 3, "Restore must not submit");
    assert!(editor_source.contains("a := [upos=NOUN]."));
    assert!(
        overflow <= 1,
        "query library overflows 320 px by {overflow}px"
    );
    assert!(
        segmented_tabs_are_coherent,
        "query tabs or semantic hiding lost their visual contract"
    );
    Ok(())
}

/// Thin synchronous wrapper around a running fixture server and a real
/// `WebDriver` session, used by the catalog-switcher tests below. Each method
/// blocks on `runtime` and panics on a WebDriver failure — acceptable here
/// under this file's test panic-deny exemption.
struct Harness {
    driver: WebDriver,
    runtime: tokio::runtime::Runtime,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn click(&self, selector: &str) {
        self.runtime.block_on(async {
            self.driver
                .find(By::Css(selector))
                .await
                .unwrap_or_else(|error| panic!("no element for {selector}: {error}"))
                .click()
                .await
                .unwrap_or_else(|error| panic!("could not click {selector}: {error}"));
        });
    }

    fn editor_text(&self) -> String {
        self.runtime.block_on(async {
            self.driver
                .execute(
                    r##"return document.querySelector("#corpus-pattern").value;"##,
                    vec![],
                )
                .await
                .unwrap()
                .convert()
                .unwrap()
        })
    }

    /// `initCatalogTab`'s click handler is async (it awaits a `fetch`), so
    /// reading `editor_text` immediately after `click` races it. Poll until
    /// the editor holds `expected` (or time out and return whatever the last
    /// read was, so a genuine mismatch still fails with a useful diff).
    fn wait_for_editor_text(&self, expected: &str) -> String {
        let mut last = self.editor_text();
        for _ in 0..100 {
            if last == expected {
                return last;
            }
            self.runtime.block_on(async {
                tokio::time::sleep(Duration::from_millis(50)).await;
            });
            last = self.editor_text();
        }
        last
    }

    fn set_editor_text(&self, text: &str) {
        self.runtime.block_on(async {
            self.driver
                .execute(
                    r##"const t=document.querySelector("#corpus-pattern");
                       t.value=arguments[0];
                       t.dispatchEvent(new Event("input",{bubbles:true}));"##,
                    vec![serde_json::Value::String(text.to_string())],
                )
                .await
                .unwrap();
        });
    }

    fn is_visible(&self, selector: &str) -> bool {
        self.runtime.block_on(async {
            let element = self
                .driver
                .find(By::Css(selector))
                .await
                .unwrap_or_else(|error| panic!("no element for {selector}: {error}"));
            element.is_displayed().await.unwrap()
        })
    }

    /// Catalog loading uses the same input-event path as a manual edit and has
    /// no confirmation modal. The edited marker must therefore remain visible
    /// after a load over unsaved changes.
    fn saw_unsaved_edit_prompt(&self) -> bool {
        self.is_visible("[data-query-edited]")
    }

    fn quit(self) {
        self.runtime.block_on(self.driver.quit()).unwrap();
    }
}

fn start_harness_with_catalogs() -> Option<Harness> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".to_string());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    if !runtime.block_on(driver_available(&driver_url)) {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        return None;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("alpha.lx"), "alpha := [].\n").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/beta.lx"), "beta := [].\n").unwrap();

    let address = runtime.block_on(fixture_with_catalogs(dir.path()));

    let mut caps = DesiredCapabilities::chrome();
    caps.add_arg("--headless=new").unwrap();
    caps.add_arg("--no-sandbox").unwrap();
    let driver = runtime.block_on(WebDriver::new(&driver_url, caps)).unwrap();
    runtime.block_on(driver.goto(&address)).unwrap();
    // `initCatalogTab` and the tab click listeners are only attached once
    // `initializeQueryLibrary`'s `await openQueryDatabase()` (and its first
    // `render()`) resolve, so wait for `render()`'s empty-state paragraph
    // rather than any element present in the static markup from page load.
    runtime
        .block_on(wait_for_count(&driver, "[data-query-list] p", 1))
        .unwrap();

    Some(Harness {
        driver,
        runtime,
        _dir: dir,
    })
}

/// Selecting a catalog replaces the editor buffer. Guarded by the same
/// chromedriver check the rest of this file uses.
#[test]
fn selecting_a_catalog_loads_it_into_the_editor() {
    let Some(harness) = start_harness_with_catalogs() else {
        eprintln!("chromedriver unavailable; skipping");
        return;
    };
    harness.click("[data-query-library] summary");
    harness.click("[data-query-tab=\"catalogs\"]");
    harness.click("button[data-catalog-key=\"alpha.lx\"]");
    let text = harness.wait_for_editor_text("alpha := [].\n");
    harness.quit();
    assert_eq!(text, "alpha := [].\n");
}

/// The unsaved-edit guard is the existing one: catalog loading runs through
/// `restoreSource`'s bubbling `input` event exactly like a manual edit does,
/// and never rewrites `anchorSource` itself. There is no confirmation dialog
/// in this implementation, so the observable proof that the load went through
/// the guard is that `[data-query-edited]`
/// stays visible across the load (the switch is never allowed to look like a
/// save), while the editor content still changes to the catalog's source.
#[test]
fn loading_a_catalog_over_unsaved_edits_keeps_the_edited_marker_visible() {
    let Some(harness) = start_harness_with_catalogs() else {
        eprintln!("chromedriver unavailable; skipping");
        return;
    };
    harness.set_editor_text("scratch := [].");
    harness.click("[data-query-library] summary");
    assert!(harness.is_visible("[data-query-edited]"));
    harness.click("[data-query-tab=\"catalogs\"]");
    harness.click("button[data-catalog-key=\"alpha.lx\"]");
    let text = harness.wait_for_editor_text("alpha := [].\n");
    let edited_after_load = harness.saw_unsaved_edit_prompt();
    harness.quit();
    assert_eq!(text, "alpha := [].\n", "the catalog load must still apply");
    assert!(
        edited_after_load,
        "the edited marker must stay visible: a catalog load must never make \
         a just-discarded unsaved edit look saved"
    );
}
