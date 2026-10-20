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
//! Deterministic browser layout checks for the compact results workspace.

#![allow(clippy::result_large_err)]

use std::{fmt::Write as _, time::Duration};

use axum::{
    Router,
    http::header,
    response::{Html, IntoResponse},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use thirtyfour::extensions::cdp::ChromeDevTools;
use thirtyfour::prelude::*;
use tokio::{net::TcpListener, task::JoinHandle};

struct FixtureServer {
    address: String,
    task: JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutMetrics {
    document_overflow: f64,
    container_overflow: f64,
    row_height: f64,
    sentence_height: f64,
    line_height: f64,
    measured_rows: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DensityMetrics {
    document_overflow: f64,
    sentence_lines: f64,
    icon_text: String,
    icon_before: String,
    label: Option<String>,
    title: String,
    expanded: String,
    token_columns: usize,
    token_overflow: f64,
    sections_display: String,
    section_lines: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KwicEdgeMetrics {
    text: String,
    prefix_visible: bool,
    suffix_visible: bool,
    key_center_delta: f64,
    right_prefix_visible: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyAlignment {
    ordinal: String,
    center_delta: f64,
    document_overflow: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InteractionMetrics {
    expansion_p50: f64,
    expansion_p95: f64,
    filter_p50: f64,
    filter_p95: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExplorerDesktop {
    display: String,
    rail_position: String,
    summary_display: String,
    heading_display: String,
    details_open: bool,
    list_visible: bool,
    side_by_side: bool,
    overflow: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChromeColors {
    prefers_dark: bool,
    tooltip_background: String,
    tooltip_text: String,
    secondary_text: String,
    panel_background: String,
    panel_text: String,
    panel_top_border: String,
    panel_bottom_border: String,
    tooltip_background_token: String,
    tooltip_text_token: String,
    secondary_text_token: String,
    border_token: String,
    contrast: f64,
    search_input_background: String,
    search_input_text: String,
    search_input_border: String,
    search_button_background_image: String,
    search_button_border: String,
    search_input_contrast: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PopupColors {
    background: String,
    text: String,
    secondary: Option<String>,
    background_token: String,
    text_token: String,
    secondary_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletionColors {
    popup: PopupColors,
    selected_background: String,
    selected_text: String,
    selection_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpandedMetrics {
    text: String,
    mask_image: String,
    webkit_mask_image: String,
    prefix_visible: bool,
    suffix_visible: bool,
}

#[derive(Debug, Deserialize)]
struct HoverPoint {
    x: i64,
    y: i64,
}

fn fixture_html() -> String {
    let long_context = "長い科学文の文脈".repeat(5);
    let sentinel_left = concat!(
        "PREFIX_SENTINEL・研究ABC・",
        "中間文脈・中間文脈・中間文脈・中間文脈・中間文脈・",
        "中間文脈・中間文脈・中間文脈・中間文脈・中間文脈・",
        "MATCH_SIDE_SUFFIX…",
    );
    let sentinel_right = concat!(
        "RIGHT_SIDE_PREFIX・",
        "後続文脈・後続文脈・後続文脈・後続文脈・後続文脈・",
        "後続文脈・後続文脈・後続文脈・後続文脈・後続文脈・",
        "RIGHT_END_SENTINEL",
    );
    let wide_key = concat!(
        "非常に長い一致表現非常に長い一致表現非常に長い一致表現",
        "非常に長い一致表現非常に長い一致表現非常に長い一致表現",
    );
    let mut rows = String::with_capacity(1_500_000);
    for ordinal in 0..1_000 {
        let (left, key, right, variation) = match ordinal {
            0 => (sentinel_left, "一致表現", sentinel_right, 0),
            1 => (sentinel_left, "鍵", "短い右文脈。", 0),
            2 => ("短い左", "例", "mixed ABC-123 後続文脈です。", 1),
            3 => ("左", wide_key, "右", 1),
            _ => (
                long_context.as_str(),
                "一致表現",
                long_context.as_str(),
                ordinal % 2,
            ),
        };
        write!(
            rows,
            r#"<tr class="kwic-row" role="row" aria-selected="false" tabindex="{}" data-hit-ordinal="{}" data-variation="{}" data-expandable-candidate><td class="kwic-ordinal">{}</td><td class="kwic-sentence-cell"><span class="kwic-sentence" data-expandable-text lang="ja"><span class="kwic-left"><span class="kwic-left-text">{}</span></span><mark class="kwic-key">{}</mark><span class="kwic-right">{}</span></span><button type="button" class="kwic-expand btn btn-compact" data-expand-sentence hidden aria-expanded="false" aria-label="Show full sentence" title="Show full sentence"></button></td></tr>"#,
            if ordinal == 0 { 0 } else { -1 },
            ordinal,
            variation,
            ordinal + 1,
            left,
            key,
            right,
        )
        .expect("writing to String cannot fail");
    }
    let tokens = (0..200)
        .map(|index| format!(r#"<button class="kwic-token">語{index}</button>"#))
        .collect::<String>();
    let density = r#"<div class="corpus-context-sections"><details class="corpus-analysis-scope"><summary><span>Sections:</span> <span class="scope-count">28</span></summary><div class="section-scope-form"><div class="scope-options"><span class="corpus-section">section_00 <small>0.1k</small></span><span class="corpus-section">section_01 <small>7.1k</small></span><span class="corpus-section">section_02 <small>14.1k</small></span><span class="corpus-section">section_03 <small>21.1k</small></span><span class="corpus-section">section_04 <small>28.1k</small></span><span class="corpus-section">section_05 <small>35.1k</small></span><span class="corpus-section">section_06 <small>42.1k</small></span><span class="corpus-section">section_07 <small>49.1k</small></span><span class="corpus-section">section_08 <small>56.1k</small></span><span class="corpus-section">section_09 <small>63.1k</small></span><span class="corpus-section">section_10 <small>70.1k</small></span><span class="corpus-section">section_11 <small>77.1k</small></span><span class="corpus-section">section_12 <small>84.1k</small></span><span class="corpus-section">section_13 <small>91.1k</small></span><span class="corpus-section">section_14 <small>98.1k</small></span><span class="corpus-section">section_15 <small>105.1k</small></span><span class="corpus-section">section_16 <small>112.1k</small></span><span class="corpus-section">section_17 <small>119.1k</small></span><span class="corpus-section">section_18 <small>126.1k</small></span><span class="corpus-section">section_19 <small>133.1k</small></span><span class="corpus-section">data <small>3.4k</small></span><span class="corpus-section">unknown <small>1.6k</small></span><span class="corpus-section">appendix <small>989</small></span><span class="corpus-section">future_work <small>552</small></span><span class="corpus-section">hypotheses <small>435</small></span><span class="corpus-section">quote <small>31</small></span><span class="corpus-section">subtitle <small>7</small></span><span class="corpus-section">title <small>1</small></span></div></div></details></div>
<section class="token-inspection">
  <section class="token-field-group">
    <h4>Lexical</h4>
    <dl class="token-field-grid">
      <div class="token-field"><dt>Orthography</dt><dd>食べる</dd></div>
      <div class="token-field"><dt>Lemma</dt><dd>非常に長い日本語形態素値改行安全性確認用文字列非常に長い日本語形態素値改行安全性確認用文字列</dd></div>
      <div class="token-field"><dt>Pronunciation</dt><dd>タベル</dd></div>
      <div class="token-field"><dt>Part of speech</dt><dd>VERB</dd></div>
    </dl>
  </section>
  <details class="unavailable-fields"><summary>Show unavailable fields</summary><dl><dt>Accent</dt><dd>Unavailable</dd></dl></details>
</section>"#;

    format!(
        r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/assets/app.css"></head><body><main><section class="results-explorer"><div class="pattern-rail"></div><section class="pattern-detail" data-retained-count="1000" data-pattern-id="0"><div class="pattern-detail-summary"><h2>fixture</h2><p>1000 corpus matches</p></div><button type="button" class="surface-filter" data-variation="1" aria-pressed="false"><span lang="ja">variation one</span></button><section class="fixture-controls"><div class="kwic-token-ribbon">{tokens}</div><button class="hit-capture-item" data-capture-target="fixture-capture">capture {}</button><div id="fixture-capture"><section class="capture-inspection" hidden>detail</section></div></section>{density}<div class="kwic-region"><header class="kwic-pane-header"><div class="kwic-filter-bar"><span class="kwic-pane-pattern">fixture</span><p class="kwic-filter-status" role="status" aria-live="polite"></p><button type="button" class="filter-chip" data-clear-surface-filter hidden>Clear surface filter</button></div><nav class="kwic-pager" data-kwic-pager><button class="btn btn-secondary" data-page-previous>Previous</button><span data-page-status></span><button class="btn btn-secondary" data-page-next>Next</button><label>Rows per page<select data-page-size><option>50</option><option selected>100</option><option>250</option><option>500</option><option>1000</option></select></label></nav></header><div class="kwic-container"><table class="kwic-concordance" role="grid" data-hit-grid data-kwic-pagination><thead><tr><th>#</th><th>Sentence</th></tr></thead><tbody>{rows}</tbody></table></div></div></section><aside class="inspector-dock" id="inspector-dock" hidden aria-label="Hit inspector"><header class="inspector-dock-header"><h2 class="inspector-dock-title" data-inspector-title>Hit inspector</h2><button type="button" class="inspector-dock-close" data-inspector-close aria-label="Close inspector">×</button></header><div id="inspector-body" class="inspector-dock-body" aria-live="polite" aria-busy="false"></div></aside></section></main><script>window.hitSelectCount=0;window.layoutInitializedAt=-1;const measurementObserver=new MutationObserver((records)=>{{if(records.some((record)=>record.attributeName==='data-overflow')){{window.layoutInitializedAt=performance.now();measurementObserver.disconnect();}}}});measurementObserver.observe(document.querySelector('[data-hit-grid]'),{{attributes:true,subtree:true,attributeFilter:['data-overflow']}});for(const row of document.querySelectorAll('.kwic-row')){{row.addEventListener('hit-select',()=>{{window.hitSelectCount+=1;const inspector=document.getElementById('inspector-body');inspector.innerHTML=`<article class="hit-detail" data-reference-ordinal="${{row.dataset.hitOrdinal}}"><p>Retained hit</p></article>`;document.dispatchEvent(new CustomEvent('htmx:afterSwap',{{detail:{{target:inspector}}}}));}});}}</script><script type="module" src="/assets/results-explorer.js"></script></body></html>"#,
        "unbroken".repeat(80),
    )
}

fn filter_fixture_html() -> String {
    // methods has 2 members (r0,r1) → index 0; results has 2 (r1,r3) → index 1
    // (tie broken by label, "methods" < "results"). r2 is in no section.
    let rows = concat!(
        r#"<tr class="kwic-row" aria-selected="false" data-hit-ordinal="0" data-variation="0" data-section-ids="0 "><td>1</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a0</span></td></tr>"#,
        r#"<tr class="kwic-row" aria-selected="false" data-hit-ordinal="1" data-variation="1" data-section-ids="0 1 "><td>2</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a1</span></td></tr>"#,
        r#"<tr class="kwic-row" aria-selected="false" data-hit-ordinal="2" data-variation="0" data-section-ids=""><td>3</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a2</span></td></tr>"#,
        r#"<tr class="kwic-row" aria-selected="false" data-hit-ordinal="3" data-variation="1" data-section-ids="1 "><td>4</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a3</span></td></tr>"#,
    );
    format!(
        r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><link rel="stylesheet" href="/assets/app.css"></head><body><main><section class="pattern-detail" data-retained-count="4">
<button type="button" class="surface-filter" data-variation="0" aria-pressed="false"><span lang="ja">v0</span></button>
<button type="button" class="surface-filter" data-variation="1" aria-pressed="false"><span lang="ja">v1</span></button>
<section class="section-distribution"><h3>Section distribution — corpus</h3>
<p class="section-scope-note" data-section-scope-note hidden role="status">These counts and rates are for the selected surface.</p>
<script type="application/json" data-surface-sections>{{"0":[5,250,9],"1":[35,50,1]}}</script>
<div class="ranked-filter" data-ranked-list="section" data-ranked-limit="2" data-ranked-expanded="false">
<ol class="section-overview">
<li data-ranked-entry="section" data-ranked-index="0" data-search-value="methods" data-count="40" data-full-count="40" data-universe="100" data-rate-tenths="4000000" data-matrix-index="0" data-section-identity="0"><button type="button" class="section-filter" data-section="0" aria-pressed="false"><span class="section-label">methods</span><span class="surface-bar" aria-hidden="true"><span class="surface-bar-fill" style="inline-size:100%"></span></span><span class="ranked-rate" data-rate-label>400000.0 ppm</span><span class="ranked-count" data-count-label>40</span></button></li>
<li data-ranked-entry="section" data-ranked-index="1" data-search-value="results" data-count="300" data-full-count="300" data-universe="1000" data-rate-tenths="3000000" data-matrix-index="1" data-section-identity="1"><button type="button" class="section-filter" data-section="1" aria-pressed="false"><span class="section-label">results</span><span class="surface-bar" aria-hidden="true"><span class="surface-bar-fill" style="inline-size:75%"></span></span><span class="ranked-rate" data-rate-label>300000.0 ppm</span><span class="ranked-count" data-count-label>300</span></button></li>
<li class="section-none" data-matrix-index="2" data-count="10" data-full-count="10" data-universe="100" data-rate-tenths="1000000"><button type="button" class="section-filter" data-section="none" aria-pressed="false"><span class="section-label">(no section)</span><span class="surface-bar" aria-hidden="true"><span class="surface-bar-fill" style="inline-size:25%"></span></span><span class="ranked-rate" data-rate-label>100000.0 ppm</span><span class="ranked-count" data-count-label>10</span></button></li>
</ol></div></section>
<div class="kwic-filter-bar"><p class="kwic-filter-status" role="status" aria-live="polite">4 retained hits · All surfaces · All sections</p><button type="button" class="filter-chip" data-clear-surface-filter hidden>Clear surface filter</button><button type="button" class="filter-chip" data-clear-section-filter hidden>Clear section filter</button></div>
<div class="kwic-container"><table class="kwic-concordance" data-hit-grid><thead><tr><th>#</th><th>Sentence</th></tr></thead><tbody>{rows}</tbody></table></div>
</section></main><script type="module" src="/assets/results-explorer.js"></script></body></html>"#
    )
}

/// The query band over a tall page. Only the band and the stylesheet: what is
/// under test is that the band is a sticky element the results scroll beneath,
/// which is a property of the CSS and of every ancestor's overflow.
fn band_fixture_html() -> &'static str {
    r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<link rel="stylesheet" href="/assets/app.css"></head><body><main>
<div class="query-band" data-query-band>
  <div class="band-head"><p class="corpus-context-name">Corpus: fixture</p>
  <div id="search-status"><p>Results are current.</p></div></div>
  <div class="query-workspace" data-query-workspace><div class="query-editor-column">
    <div id="corpus-editor" class="corpus-pattern-editor"><textarea rows="4"></textarea></div>
    <div class="permalink-controls">
      <button type="submit" class="btn btn-primary">Search Corpus</button>
      <label class="retained-control">Hits kept <select name="retained"><option value="1000" selected>1000</option></select></label>
      <span class="compound-control-group">
        <label class="retained-control"><input type="checkbox" name="derive_compounds" value="true"> Derive contiguous compound items</label>
        <small class="compound-help">Creates an independent derived pattern for every chain of adjacent matches.</small>
        <label class="retained-control compound-category-control">Compound category <select name="compound_category"><option value="compound">Compound item</option><option value="source">Source categories</option></select></label>
      </span>
      <button type="button" class="btn btn-secondary copy-permalink">Copy link</button>
      <button type="button" class="btn btn-secondary editor-size-toggle">Expand editor</button>
      <button type="button" class="btn btn-secondary editor-fold-toggle">Fold editor <kbd>Ctrl+E</kbd></button>
      <button type="button" class="editor-help-open">Syntax and shortcuts</button>
    </div>
  </div></div>
</div>
<div id="results" style="block-size: 4000px">tall results</div>
</main></body></html>"#
}

/// Mirrors the production explorer's shape for the tab set: one tab strip,
/// catalog-level panels beside `#detail-body`, and per-pattern panels inside it.
/// The dendrogram tab has no panel here, which the controller hides.
fn tabs_fixture_html() -> &'static str {
    r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/assets/app.css"></head><body><main>
<section class="results-explorer" aria-label="Corpus search results">
  <div class="pattern-rail">
    <details class="pattern-catalog" open><summary>Patterns</summary>
      <table id="pattern-table"><tbody><tr data-authored-index="0"><td><button type="button" class="pattern-selector" aria-pressed="true">p</button></td><td>3</td></tr></tbody></table>
    </details>
  </div>
  <div class="detail-panel">
    <div class="detail-tabs" role="tablist" aria-label="Views of this run" data-detail-tabs>
      <button type="button" role="tab" data-detail-tab="concordance" aria-selected="true">Concordance</button>
      <button type="button" role="tab" data-detail-tab="sections" aria-selected="false" tabindex="-1">Sections</button>
      <button type="button" role="tab" data-detail-tab="clusters" aria-selected="false" tabindex="-1">Clusters</button>
      <button type="button" role="tab" data-detail-tab="definition" aria-selected="false" tabindex="-1">Definition</button>
    </div>
    <section id="detail-body">
      <section class="pattern-detail" data-retained-count="3" data-pattern-id="0">
        <div class="pattern-detail-summary"><h2>p</h2><p>3 corpus matches</p></div>
        <section class="detail-view definition-band" data-detail-view="definition" data-definition-band>
          <h3>Definition</h3>
          <pre class="definition-source"><code>p := [].</code></pre>
          <section class="surface-distribution"><div class="definition-pane-header"><h3>Surface variations — corpus</h3><button type="button" class="btn btn-compact btn-secondary" data-view-filtered-concordance="surface" hidden></button></div>
          <div class="ranked-filter" data-ranked-list="surface" data-ranked-limit="1" data-ranked-expanded="false">
          <div class="ranked-tools"><label class="ranked-search-label"><span>Search surfaces</span> <input type="search" data-ranked-search="surface"></label></div>
          <ol class="surface-overview">
            <li data-ranked-entry="surface" data-ranked-index="0" data-search-value="v0" data-count="2"><button type="button" class="surface-filter" data-variation="0" aria-pressed="false"><span lang="ja">v0</span><span class="surface-bar"><span class="surface-bar-fill"></span></span><span class="ranked-count">2</span></button></li>
            <li data-ranked-entry="surface" data-ranked-index="1" data-search-value="v1" data-count="1"><button type="button" class="surface-filter" data-variation="1" aria-pressed="false"><span lang="ja">v1</span><span class="surface-bar"><span class="surface-bar-fill"></span></span><span class="ranked-count">1</span></button></li>
          </ol></div></section>
        </section>
        <section class="detail-view section-distribution" data-detail-view="sections"><div class="definition-pane-header"><h3>Section distribution — corpus</h3><button type="button" class="btn btn-compact btn-secondary" data-view-filtered-concordance="section" hidden></button></div>
          <div class="ranked-filter" data-ranked-list="section" data-ranked-limit="12" data-ranked-expanded="true">
            <div class="ranked-tools"><label class="ranked-search-label"><span>Search sections</span> <input type="search" data-ranked-search="section"></label><label class="ranked-order-label"><span>Order</span> <select data-ranked-order><option value="rate-desc">Rate ↓</option><option value="count-desc">Frequency ↓</option><option value="name-asc">Name ↑</option></select></label></div>
            <ol class="section-overview">
              <li data-ranked-entry="section" data-ranked-index="0" data-search-value="a" data-count="9" data-full-count="9" data-universe="100" data-rate-tenths="900" data-section-identity="0" data-matrix-index="0"><button type="button" class="section-filter" data-section="0" aria-pressed="false"><span class="section-label">a</span><span class="surface-bar" aria-hidden="true"><span class="surface-bar-fill"></span></span><span class="ranked-rate" data-rate-label>90.0 ppm</span><span class="ranked-count" data-count-label>9</span></button></li>
              <li data-ranked-entry="section" data-ranked-index="1" data-search-value="a-very-long-section-label-indeed" data-count="4" data-full-count="4" data-universe="100" data-rate-tenths="400" data-section-identity="1" data-matrix-index="1"><button type="button" class="section-filter" data-section="1" aria-pressed="false"><span class="section-label">a-very-long-section-label-indeed</span><span class="surface-bar" aria-hidden="true"><span class="surface-bar-fill"></span></span><span class="ranked-rate" data-rate-label>40.0 ppm</span><span class="ranked-count" data-count-label>4</span></button></li>
            </ol>
          </div>
        </section>
        <div class="detail-view" data-detail-view="concordance">
          <div class="kwic-region">
            <header class="kwic-pane-header"><div class="kwic-filter-bar"><span class="kwic-pane-pattern">p</span><p class="kwic-filter-status" role="status" aria-live="polite"></p><button type="button" class="filter-chip" data-clear-surface-filter hidden>Clear surface filter</button><button type="button" class="btn btn-compact btn-secondary kwic-quick-toggle" data-quick-filter-toggle="surface" aria-expanded="false">Filter by surface (2)</button></div><nav class="kwic-pager" data-kwic-pager><button class="btn btn-secondary" data-page-previous>Previous</button><span data-page-status></span><button class="btn btn-secondary" data-page-next>Next</button><label>Rows per page<select data-page-size><option>50</option><option selected>100</option></select></label></nav><div class="kwic-quick-drawer" data-quick-filter-drawer="surface" hidden></div></header>
            <div class="kwic-container"><table class="kwic-concordance" role="grid" data-hit-grid data-kwic-pagination><thead><tr><th>#</th><th>Sentence</th></tr></thead><tbody>
              <tr class="kwic-row" role="row" aria-selected="false" tabindex="0" data-hit-ordinal="0" data-variation="0"><td>1</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a0</span></td></tr>
              <tr class="kwic-row" role="row" aria-selected="false" tabindex="-1" data-hit-ordinal="1" data-variation="0"><td>2</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a1</span></td></tr>
              <tr class="kwic-row" role="row" aria-selected="false" tabindex="-1" data-hit-ordinal="2" data-variation="1"><td>3</td><td class="kwic-sentence-cell"><span class="kwic-sentence" lang="ja">a2</span></td></tr>
            </tbody></table></div>
          </div>
        </div>
      </section>
    </section>
    <section class="detail-view section-comparison-band" data-detail-view="sections" data-section-comparison><h3>Section distribution — all patterns</h3><table class="section-comparison"><tbody><tr><th>p</th><td class="comparison-cell">3</td></tr></tbody></table></section>
  </div>
</section></main><script type="module" src="/assets/results-explorer.js"></script></body></html>"#
}

fn explorer_fixture_html() -> String {
    // Mirrors production markup: rail wrapper + desktop heading + <details open>
    // + mobile summary + detail pane with a tall filler so the page scrolls.
    // Loads the real results-explorer.js so label-sync is exercised.
    r#"<!doctype html><html lang="ja"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/assets/app.css"></head><body><main>
<section class="results-explorer" aria-label="Corpus search results">
  <div class="pattern-rail">
    <h2 class="pattern-catalog-heading"><span data-selected-pattern>Patterns</span></h2>
    <details class="pattern-catalog" open>
      <summary><span data-selected-pattern>Patterns</span></summary>
      <label class="visually-hidden" for="pattern-rail-filter">Filter patterns by name</label>
      <input type="search" id="pattern-rail-filter" class="pattern-rail-filter" data-pattern-rail-filter placeholder="Filter patterns…" autocomplete="off" spellcheck="false">
      <table id="pattern-table"><tbody id="pattern-rows">
        <tr data-authored-index="0"><td><button type="button" class="pattern-selector" aria-pressed="true">させる</button></td><td>128</td></tr>
        <tr data-authored-index="1"><td><button type="button" class="pattern-selector" aria-pressed="false">される</button></td><td>40</td></tr>
      </tbody></table>
    </details>
  </div>
  <div class="detail-panel"><section id="detail-body"><h2>させる</h2><section class="pattern-detail"><section class="detail-view definition-band" data-detail-view="definition"><ul data-ranked-list="surface"><li data-ranked-entry="surface" data-search-value="させる"></li></ul></section></section><div style="height:2000px">filler</div></section></div>
  <aside class="inspector-dock" id="inspector-dock" hidden aria-label="Hit inspector"><header class="inspector-dock-header"><h2 class="inspector-dock-title" data-inspector-title>Hit inspector</h2><button type="button" class="inspector-dock-close" data-inspector-close aria-label="Close inspector">×</button></header><div id="inspector-body" class="inspector-dock-body" aria-live="polite" aria-busy="false"></div></aside>
</section>
</main><script type="module" src="/assets/results-explorer.js"></script></body></html>"#.to_string()
}

/// The document the editor fixture loads.
///
/// It must stay **valid** lingrep patterns, which `the_editor_fixture_document_is_valid_pattern`
/// enforces. The editor's lint source returns local tree-sitter diagnostics and
/// stops before the server round trip whenever the document does not parse, so
/// an invalid fixture silently disables every server-derived behaviour a test
/// might assert on — which is exactly what it did here until a test finally
/// looked.
///
/// Two cursor positions are load bearing for
/// `editor_chrome_follows_light_and_dark_palette`, both counted back from the
/// end: `End-9` lands inside the `same_as(…)` argument list (signature help),
/// and `End-5` lands after `by=o` (value completion). Keep the trailing
/// `, by=text).` intact when editing this.
const EDITOR_FIXTURE_SOURCE: &str = "fixture := (?<x> [upos=VERB]) same_as($x, by=text).";

fn editor_fixture_html() -> String {
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="/assets/app.css"><link rel="stylesheet" href="/assets/editor/pattern-editor.css"></head><body><label for="editor-fixture">Pattern</label><textarea id="editor-fixture" data-pattern-editor>{}</textarea><button id="blur-target" type="button">Outside editor</button><script type="module" src="/assets/editor/pattern-editor.js"></script></body></html>"#,
        EDITOR_FIXTURE_SOURCE
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}

/// Needs no browser: the fixture's document is checked by the real compiler.
#[test]
fn the_editor_fixture_document_is_valid_pattern() {
    if let Err(error) = lingrep_web::patterns::compile_web_pattern(EDITOR_FIXTURE_SOURCE) {
        panic!(
            "the editor fixture document must be valid lingrep patterns:\n{}",
            lingrep_core::fsm::render_pattern_error(Some(EDITOR_FIXTURE_SOURCE), &error)
        );
    }
}

/// A fixture with a fragment and two references to it, for navigation.
///
/// `F` is declared at offset 9; the two `@F` references sit at 31/32 and
/// 34/35. The document is valid lingrep patterns so no local syntax diagnostic fires.
fn navigation_fixture_html() -> &'static str {
    r#"<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="/assets/app.css"><link rel="stylesheet" href="/assets/editor/pattern-editor.css"></head><body><label for="nav-fixture">Pattern</label><textarea id="nav-fixture" data-pattern-editor>fragment F := [upos=NOUN].
p := @F @F.</textarea><script type="module" src="/assets/editor/pattern-editor.js"></script></body></html>"#
}

async fn web_tree_sitter_wasm() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/wasm")],
        include_bytes!("../../assets/editor/vendor/web-tree-sitter.wasm").as_slice(),
    )
}

async fn tree_sitter_lingrep_wasm() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/wasm")],
        include_bytes!("../../assets/editor/vendor/tree-sitter-lingrep.wasm").as_slice(),
    )
}

/// How the fixture's `/api/pattern/format` route answers.
///
/// Defaults to [`FormatMode::Echo`] because a reply that *changes* the document
/// rewrites whichever fixture is under test — `formatCurrent` dispatches a
/// full-document replacement whenever the returned source differs — and the
/// editor fixtures pin cursor offsets against their exact source text.
/// Reformatting them shifts those offsets and breaks navigation and panel
/// placement. Only a test that is itself about formatting wants [`Real`].
///
/// [`Real`]: FormatMode::Real
#[derive(Clone, Copy, PartialEq, Eq)]
enum FormatMode {
    /// Echo the submitted source unchanged.
    Echo,
    /// Run the production formatter.
    Real,
}

async fn fixture_server() -> FixtureServer {
    fixture_server_with(FormatMode::Echo).await
}

async fn fixture_server_with(format_mode: FormatMode) -> FixtureServer {
    let html = fixture_html();
    let filter_html = filter_fixture_html();
    let app = Router::new()
        .route(
            "/",
            get(move || {
                let html = html.clone();
                async move { Html(html) }
            }),
        )
        .route(
            "/filter",
            get(move || {
                let html = filter_html.clone();
                async move { Html(html) }
            }),
        )
        .route("/provenance", get(|| async { Html(provenance_fixture_html()) }))
        .route("/band", get(|| async { Html(band_fixture_html()) }))
        .route("/tabs", get(|| async { Html(tabs_fixture_html()) }))
        .route("/explorer", get(|| async { Html(explorer_fixture_html()) }))
        .route("/editor", get(|| async { Html(editor_fixture_html()) }))
        .route(
            "/navigation",
            get(|| async { Html(navigation_fixture_html()) }),
        )
        .route(
            "/api/pattern/check",
            post(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    r#"{"status":"valid","stats":{"definitions":1,"expressions":2,"references":0,"depth":1,"nfa_states":3,"transitions":4},"diagnostics":[{"code":"UnusedFragment","category":"lint","severity":"warning","message":"A fragment is never referenced.","location":{"source_name":"lingrep-web.lx","line":1,"column":1,"byte_range":{"start":0,"end":1}}}]}"#,
                )
            }),
        )
        .route(
            "/api/pattern/format",
            post(move |body: String| async move {
                // Answers per `FormatMode`; see its docs for why echoing is
                // the default and why only a formatting test opts into `Real`.
                let source = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|value| value["source"].as_str().map(str::to_owned))
                    .unwrap_or_default();
                let body = match format_mode {
                    FormatMode::Echo => {
                        json!({"status": "formatted", "source": source, "changed": false})
                    }
                    FormatMode::Real => match lingrep_web::patterns::format_web_pattern(&source) {
                        Ok(output) => json!({
                            "status": "formatted",
                            "source": output.source,
                            "changed": output.changed,
                        }),
                        Err(_) => json!({"status": "invalid", "diagnostics": []}),
                    },
                };
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    body.to_string(),
                )
            }),
        )
        .route(
            "/assets/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../assets/app.css"),
                )
            }),
        )
        .route(
            "/assets/results-explorer.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/results-explorer.js"),
                )
            }),
        )
        .route(
            "/assets/i18n.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/i18n.js"),
                )
            }),
        )
        .route(
            "/assets/i18n/en.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    include_str!("../../assets/i18n/en.json"),
                )
            }),
        )
        .route(
            "/assets/i18n/ja.json",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                    include_str!("../../assets/i18n/ja.json"),
                )
            }),
        )
        .route(
            "/assets/editor/pattern-editor.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("../../assets/editor/pattern-editor.css"),
                )
            }),
        )
        .route(
            "/assets/editor/pattern-editor.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/editor/pattern-editor.js"),
                )
            }),
        )
        .route(
            "/assets/editor/vendor/web-tree-sitter.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/editor/vendor/web-tree-sitter.js"),
                )
            }),
        )
        .route(
            "/assets/editor/vendor/web-tree-sitter.wasm",
            get(web_tree_sitter_wasm),
        )
        .route(
            "/assets/editor/vendor/tree-sitter-lingrep.wasm",
            get(tree_sitter_lingrep_wasm),
        )
        .route(
            "/assets/editor/vendor/codemirror.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("../../assets/editor/vendor/codemirror.js"),
                )
            }),
        )
        .route(
            "/assets/editor/vendor/highlights.scm",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    include_str!("../../assets/editor/vendor/highlights.scm"),
                )
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FixtureServer { address, task }
}

async fn driver_available(url: &str) -> bool {
    let address = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    tokio::net::TcpStream::connect(address).await.is_ok()
}

async fn wait_for_selector(driver: &WebDriver, selector: &str) -> WebDriverResult<()> {
    for _ in 0..100 {
        let exists: bool = driver
            .execute(
                "return document.querySelector(arguments[0]) !== null",
                vec![serde_json::to_value(selector)?],
            )
            .await?
            .convert()?;
        if exists {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(WebDriverError::Timeout(format!(
        "waiting for selector {selector}"
    )))
}

async fn wait_for_enhanced_editor(driver: &WebDriver) -> WebDriverResult<()> {
    for _ in 0..100 {
        let state: String = driver
            .execute(
                r#"
const textarea = document.querySelector('textarea[data-pattern-editor]');
if (document.querySelector('.pattern-editor .cm-editor')) return 'ready';
if (textarea?.dataset.lxEnhancement === 'unavailable') {
  return `unavailable: ${textarea.dataset.lxEnhancementError ?? 'unknown error'}`;
}
return 'pending';
"#,
                vec![],
            )
            .await?
            .convert()?;
        if state == "ready" {
            return Ok(());
        }
        assert!(
            !state.starts_with("unavailable:"),
            "lingrep patterns enhancement failed: {state}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(WebDriverError::Timeout(
        "waiting for enhanced lingrep patterns editor".into(),
    ))
}

async fn emulate_color_scheme(tools: &ChromeDevTools, scheme: &str) -> WebDriverResult<()> {
    tools
        .execute_cdp_with_params(
            "Emulation.setEmulatedMedia",
            json!({
                "features": [{
                    "name": "prefers-color-scheme",
                    "value": scheme
                }]
            }),
        )
        .await?;
    Ok(())
}

/// The panel narrows to the selected surface and says which population it is
/// describing while a surface filter is active. `hidden` is a property here,
/// not an attribute: `applyRowFilters` assigns it, which removes the attribute
/// when false.
async fn scope_note_hidden(driver: &WebDriver) -> WebDriverResult<bool> {
    driver
        .execute(
            "return document.querySelector('[data-section-scope-note]').hidden",
            vec![],
        )
        .await?
        .convert()
}

async fn dom_attribute(
    driver: &WebDriver,
    selector: &str,
    attribute: &str,
) -> WebDriverResult<Option<String>> {
    let value: String = driver
        .execute(
            "return document.querySelector(arguments[0])?.getAttribute(arguments[1]) ?? '__MISSING__'",
            vec![
                serde_json::to_value(selector)?,
                serde_json::to_value(attribute)?,
            ],
        )
        .await?
        .convert()?;
    Ok((value != "__MISSING__").then_some(value))
}

async fn wait_for_measurement(driver: &WebDriver, selector: &str) -> WebDriverResult<()> {
    tokio::time::sleep(Duration::from_millis(250)).await;
    if dom_attribute(driver, selector, "data-overflow")
        .await?
        .is_some()
    {
        return Ok(());
    }
    Err(WebDriverError::Timeout("overflow measurement".into()))
}

async fn wait_for_initialization(driver: &WebDriver) -> WebDriverResult<f64> {
    for _ in 0..100 {
        let elapsed: f64 = driver
            .execute("return window.layoutInitializedAt ?? -1", vec![])
            .await?
            .convert()?;
        if elapsed >= 0.0 {
            return Ok(elapsed);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(WebDriverError::Timeout("layout initialization".into()))
}

#[tokio::test]
async fn compact_workspace_never_scrolls_horizontally_and_discloses_in_place() -> WebDriverResult<()>
{
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }

    eprintln!("layout-e2e: starting fixture");
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    eprintln!("layout-e2e: creating browser session");
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    eprintln!("layout-e2e: loading fixture");
    let mut initialization_samples = Vec::with_capacity(10);
    for _ in 0..10 {
        driver.goto(&fixture.address).await?;
        initialization_samples.push(wait_for_initialization(&driver).await?);
    }
    initialization_samples.sort_by(f64::total_cmp);
    let initialization_p50 = initialization_samples[5];
    let initialization_p95 = initialization_samples[9];
    eprintln!(
        "rows=1000 initialization_ms_p50={initialization_p50:.3} initialization_ms_p95={initialization_p95:.3}"
    );

    let visible: usize = driver
        .execute(
            "return [...document.querySelectorAll('.kwic-row')].filter((row) => !row.hidden).length",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(visible, 100);
    assert_eq!(
        driver
            .find(By::Css("[data-kwic-pager] [data-page-status]"))
            .await?
            .text()
            .await?,
        "Page 1 of 10"
    );
    let previous_disabled: (bool, String, String, String, String, String) = driver
        .execute(
            r#"const button=document.querySelector('[data-page-previous]');const style=getComputedStyle(button);return [button.disabled,style.color,style.borderColor,style.backgroundColor,style.opacity,style.cursor];"#,
            vec![],
        )
        .await?
        .convert()?;
    driver
        .execute(
            "for(let i=0;i<9;i++)document.querySelector('[data-page-next]').click()",
            vec![],
        )
        .await?;
    let next_disabled: (bool, String, String, String, String, String) = driver
        .execute(
            r#"const button=document.querySelector('[data-page-next]');const style=getComputedStyle(button);return [button.disabled,style.color,style.borderColor,style.backgroundColor,style.opacity,style.cursor];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(previous_disabled, next_disabled);
    assert!(previous_disabled.0);
    assert_eq!(previous_disabled.5, "default");
    assert!(previous_disabled.4.parse::<f64>().unwrap() < 1.0);
    driver
        .execute(
            "for(let i=0;i<9;i++)document.querySelector('[data-page-previous]').click()",
            vec![],
        )
        .await?;

    let first_row = driver
        .find(By::Css(".kwic-row[data-hit-ordinal='0']"))
        .await?;
    first_row.click().await?;
    first_row.click().await?;
    let toggled_closed: bool = driver
        .execute(
            "return window.hitSelectCount===1&&!document.querySelector('.kwic-row[aria-selected=true]')&&!document.querySelector('.kwic-inspector-row')&&document.getElementById('inspector-dock').hidden===true",
            vec![],
        )
        .await?
        .convert()?;
    assert!(toggled_closed, "same-row activation did not close locally");
    first_row.click().await?;
    driver
        .find(By::Css("[data-inspector-close]"))
        .await?
        .click()
        .await?;
    let dock_closed: bool = driver
        .execute(
            "await new Promise((resolve)=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));return window.hitSelectCount===2&&!document.querySelector('.kwic-row[aria-selected=true]')&&!document.querySelector('.kwic-inspector-row')&&document.getElementById('inspector-dock').hidden===true",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        dock_closed,
        "closing the inspector left the selection behind"
    );

    driver
        .find(By::Css("[data-kwic-pager] [data-page-next]"))
        .await?
        .click()
        .await?;
    let page_two: (usize, usize, String, String) = driver
        .execute(
            r#"const rows=[...document.querySelectorAll('.kwic-row')].filter((row)=>!row.hidden);return [Number(rows[0].dataset.hitOrdinal),Number(rows.at(-1).dataset.hitOrdinal),rows[0].getAttribute('tabindex'),document.activeElement===rows[0]?'focused':''];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(page_two, (100, 199, "0".into(), "focused".into()));
    assert_eq!(
        driver
            .find(By::Css("[data-kwic-pager] [data-page-status]"))
            .await?
            .text()
            .await?,
        "Page 2 of 10"
    );
    driver
        .find(By::Css("[data-kwic-pager] [data-page-previous]"))
        .await?
        .click()
        .await?;
    driver
        .find(By::Css(".kwic-row[data-hit-ordinal='0']"))
        .await?
        .click()
        .await?;
    driver
        .execute(
            r#"document.querySelector(".kwic-row[data-hit-ordinal='1'] [data-expand-sentence]").click();"#,
            vec![],
        )
        .await?;
    driver
        .find(By::Css("[data-kwic-pager] [data-page-next]"))
        .await?
        .click()
        .await?;
    let off_page_state_is_independent: bool = driver
        .execute(
            r#"const expanded=document.querySelector(".kwic-row[data-hit-ordinal='1']");return !document.querySelector('.kwic-row[aria-selected=true]')&&document.getElementById('inspector-dock').hidden===true&&expanded.dataset.expanded==='true'&&expanded.hidden;"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        off_page_state_is_independent,
        "page navigation braided inspection and sentence expansion"
    );
    // JS click: the sticky pane header can cover WebDriver's chosen click
    // point for controls in the distribution band.
    driver
        .execute(
            "document.querySelector(\".surface-filter[data-variation='1']\").click()",
            vec![],
        )
        .await?;
    let filtered_page_reset: bool = driver
        .execute(
            r#"const visible=[...document.querySelectorAll('.kwic-row')].filter((row)=>!row.hidden);return document.querySelector('[data-page-status]').textContent==='Page 1 of 5'&&visible.length===100&&visible[0].dataset.hitOrdinal==='2';"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        filtered_page_reset,
        "filtering from a later page did not reset the projection"
    );
    driver
        .execute(
            "document.querySelector(\".surface-filter[data-variation='1']\").click()",
            vec![],
        )
        .await?;
    let expanded_survived_return: bool = driver
        .execute(
            r#"return document.querySelector(".kwic-row[data-hit-ordinal='1']").dataset.expanded==='true';"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        expanded_survived_return,
        "returning to page one lost independent expansion"
    );
    driver
        .execute(
            r#"const select=document.querySelector('[data-page-size]');select.value='1000';select.dispatchEvent(new Event('change',{bubbles:true}));"#,
            vec![],
        )
        .await?;
    let visible: usize = driver
        .execute(
            "return [...document.querySelectorAll('.kwic-row')].filter((row) => !row.hidden).length",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(visible, 1_000);
    driver.refresh().await?;
    wait_for_initialization(&driver).await?;
    let persisted_visible: usize = driver
        .execute(
            "return [...document.querySelectorAll('.kwic-row')].filter((row) => !row.hidden).length",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(persisted_visible, 1_000);

    eprintln!("layout-e2e: finding first row");
    let first = driver
        .find(By::Css(".kwic-row[data-hit-ordinal='0']"))
        .await?;
    eprintln!("layout-e2e: waiting for measurement");
    wait_for_measurement(&driver, ".kwic-row[data-hit-ordinal='0']").await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );

    for width in [320, 768, 1_440] {
        driver.set_window_rect(0, 0, width, 900).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let metrics: LayoutMetrics = driver
            .execute(
                r#"const row=document.querySelector('.kwic-row');const sentence=row.querySelector('.kwic-sentence');const container=document.querySelector('.kwic-container');const style=getComputedStyle(sentence);const lineHeight=parseFloat(style.lineHeight)||parseFloat(style.fontSize)*1.2;return {documentOverflow:document.documentElement.scrollWidth-document.documentElement.clientWidth,containerOverflow:Math.max(0,container.getBoundingClientRect().right-document.documentElement.clientWidth),rowHeight:row.getBoundingClientRect().height,sentenceHeight:sentence.getBoundingClientRect().height,lineHeight,measuredRows:document.querySelectorAll('.kwic-row[data-overflow]').length};"#,
                vec![],
            )
            .await?
            .convert()?;
        eprintln!(
            "viewport={width} document_overflow_px={} container_overflow_px={} collapsed_row_px={} sentence_px={} line_px={} measured_rows={}",
            metrics.document_overflow,
            metrics.container_overflow,
            metrics.row_height,
            metrics.sentence_height,
            metrics.line_height,
            metrics.measured_rows
        );
        assert!(
            metrics.document_overflow <= 1.0,
            "horizontal overflow at {width}px"
        );
        assert!(
            metrics.container_overflow <= 1.0,
            "KWIC container escaped the {width}px viewport by {}px",
            metrics.container_overflow
        );
        assert!(
            metrics.sentence_height <= metrics.line_height * 1.5,
            "row wrapped at {width}px"
        );
        assert!(
            metrics.measured_rows < 1_000,
            "overflow measurement became eager"
        );

        // Sections are held inside a compact popover disclosure rather than a
        // wide horizontally scrolling row in the top band.
        let sections_scope: (bool, usize) = driver
            .execute(
                "const det=document.querySelector('.corpus-analysis-scope');const chips=det.querySelectorAll('.corpus-section');return [!det.open,chips.length];",
                vec![],
            )
            .await?
            .convert()?;
        assert!(
            sections_scope.0,
            "the sections popover is collapsed by default at {width}px"
        );
        assert_eq!(
            sections_scope.1, 28,
            "every section is present in the popover at {width}px"
        );
        assert!(
            driver
                .find_all(By::Css(".corpus-section-row"))
                .await?
                .is_empty(),
            "the horizontal scrolling section row must be gone at {width}px"
        );
    }

    // The pane header pins to the viewport top while the long KWIC table
    // scrolls beneath it, so filters and paging stay within reach.
    //
    // Scrolled by the region rather than by a fixed offset: the strip's
    // containing block is `.kwic-region`, so it cannot pin before the region
    // reaches the top — which is the point of `vibrato-pipe-csb`, and at 800px
    // the region has not.
    let pane_header_pin: (String, f64) = driver
        .execute(
            "window.scrollTo(0, window.scrollY + document.querySelector('.kwic-region').getBoundingClientRect().top + 800);const h=document.querySelector('.kwic-pane-header');return [getComputedStyle(h).position,h.getBoundingClientRect().top];",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(pane_header_pin.0, "sticky", "pane header must be sticky");
    assert!(
        pane_header_pin.1 <= 1.0,
        "pane header did not stay pinned while scrolling: top={}px",
        pane_header_pin.1
    );
    driver.execute("window.scrollTo(0,0)", vec![]).await?;

    let density: DensityMetrics = driver
        .execute(
            r#"
const row = document.querySelector('.kwic-row[data-hit-ordinal="0"]');
const sentence = row.querySelector('.kwic-sentence');
const sentenceStyle = getComputedStyle(sentence);
const lineHeight =
  parseFloat(sentenceStyle.lineHeight) ||
  parseFloat(sentenceStyle.fontSize) * 1.2;
const button = row.querySelector('[data-expand-sentence]');
const grid = document.querySelector('.token-field-grid');
const field = grid.querySelector('.token-field');
const sections = document.querySelector('.corpus-context-sections');
const sectionsStyle = getComputedStyle(sections);
const sectionsLineHeight =
  parseFloat(sectionsStyle.lineHeight) ||
  parseFloat(sectionsStyle.fontSize) * 1.2;
return {
  documentOverflow:
    document.documentElement.scrollWidth -
    document.documentElement.clientWidth,
  sentenceLines:
    sentence.getBoundingClientRect().height / lineHeight,
  iconText: button.textContent,
  iconBefore: getComputedStyle(button, '::before').content,
  label: button.getAttribute('aria-label'),
  title: button.title,
  expanded: button.getAttribute('aria-expanded'),
  tokenColumns:
    getComputedStyle(grid).gridTemplateColumns.split(' ').length,
  tokenOverflow:
    field.scrollWidth - field.clientWidth,
  sectionsDisplay:
    sectionsStyle.display,
  sectionLines:
    sections.getBoundingClientRect().height / sectionsLineHeight
};
"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        density.document_overflow <= 1.0,
        "document overflow with every section listed: {}",
        density.document_overflow
    );
    assert!(density.sentence_lines <= 1.5);
    assert_eq!(density.icon_text, "");
    assert_eq!(density.icon_before, "\"▾\"");
    assert_eq!(density.label.as_deref(), Some("Show full sentence"));
    assert_eq!(density.title, "Show full sentence");
    assert_eq!(density.expanded, "false");
    assert!(
        density.token_columns >= 3,
        "desktop token inspection should use at least three columns"
    );
    assert!(density.token_overflow <= 1.0);
    assert_eq!(density.sections_display, "flex");
    // Devices.js line-height inference on flex rows measures the labelled
    // chip row at ~1.77×; the contract is "one scrollable chip row", not a
    // single typographic line.
    assert!(
        density.section_lines <= 2.0,
        "Sections row must stay a single scrollable row: {}",
        density.section_lines
    );

    driver.set_window_rect(0, 0, 1_440, 900).await?;
    for (container_width, expected_columns) in
        [(1_100, 4_u64), (800, 3_u64), (600, 2_u64), (350, 1_u64)]
    {
        let columns: u64 = driver
            .execute(
                r#"
const inspection = document.querySelector('.token-inspection');
inspection.style.width = `${arguments[0]}px`;
await new Promise((resolve) =>
  requestAnimationFrame(() => requestAnimationFrame(resolve))
);
return getComputedStyle(
  inspection.querySelector('.token-field-grid')
).gridTemplateColumns.split(' ').length;
"#,
                vec![container_width.into()],
            )
            .await?
            .convert()?;
        assert_eq!(columns, expected_columns);
    }
    driver
        .execute(
            "document.querySelector('.token-inspection').style.width=''",
            vec![],
        )
        .await?;

    driver.set_window_rect(0, 0, 768, 900).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let edges: KwicEdgeMetrics = driver
        .execute(
            r#"
const row = document.querySelector('.kwic-row[data-hit-ordinal="0"]');
const left = row.querySelector('.kwic-left');
const text = row.querySelector('.kwic-left-text');
const key = row.querySelector('.kwic-key');
const right = row.querySelector('.kwic-right');
const rangeFor = (node, needle) => {
  const start = node.textContent.indexOf(needle);
  const range = new Range();
  range.setStart(node.firstChild, start);
  range.setEnd(node.firstChild, start + needle.length);
  return range.getBoundingClientRect();
};
const inside = (rect, clip) =>
  rect.left >= clip.left - 1 && rect.right <= clip.right + 1;
const leftClip = left.getBoundingClientRect();
const sentence = row.querySelector('.kwic-sentence').getBoundingClientRect();
const keyRect = key.getBoundingClientRect();
const rightClip = right.getBoundingClientRect();
return {
  text: text.textContent,
  prefixVisible: inside(rangeFor(text, 'PREFIX_SENTINEL'), leftClip),
  suffixVisible: inside(rangeFor(text, 'MATCH_SIDE_SUFFIX…'), leftClip),
  keyCenterDelta: Math.abs(
    (keyRect.left + keyRect.right) / 2 -
    (sentence.left + sentence.right) / 2
  ),
  rightPrefixVisible: inside(
    rangeFor(right, 'RIGHT_SIDE_PREFIX'),
    rightClip
  )
};
"#,
            vec![],
        )
        .await?
        .convert()?;
    let expected_left = concat!(
        "PREFIX_SENTINEL・研究ABC・",
        "中間文脈・中間文脈・中間文脈・中間文脈・中間文脈・",
        "中間文脈・中間文脈・中間文脈・中間文脈・中間文脈・",
        "MATCH_SIDE_SUFFIX…",
    );
    assert_eq!(edges.text, expected_left);
    assert!(!edges.prefix_visible, "early left context must be clipped");
    assert!(
        edges.suffix_visible,
        "match-side left suffix must be visible"
    );
    assert!(edges.key_center_delta <= 1.0, "match moved off center");
    assert!(
        edges.right_prefix_visible,
        "match-side right prefix was clipped"
    );
    let alignments: Vec<KeyAlignment> = driver
        .execute(
            r#"return [...document.querySelectorAll(".kwic-row:not([hidden])")].slice(0,4).map((row)=>{const cell=row.querySelector(".kwic-sentence-cell").getBoundingClientRect();const key=row.querySelector(".kwic-key").getBoundingClientRect();return {ordinal:row.dataset.hitOrdinal,centerDelta:Math.abs((key.left+key.width/2)-(cell.left+cell.width/2)),documentOverflow:document.documentElement.scrollWidth-document.documentElement.clientWidth};});"#,
            vec![],
        )
        .await?
        .convert()?;
    for alignment in alignments {
        assert!(
            alignment.center_delta <= 1.0,
            "row {} key moved off center by {}px",
            alignment.ordinal,
            alignment.center_delta
        );
        assert!(
            alignment.document_overflow <= 1.0,
            "row {} caused horizontal overflow",
            alignment.ordinal
        );
    }

    driver
        .execute(
            "document.querySelector('.kwic-token').click();document.querySelector('[data-capture-target]').click()",
            vec![],
        )
        .await?;
    let untouched_count: u64 = driver
        .execute("return window.hitSelectCount", vec![])
        .await?
        .convert()?;
    assert_eq!(untouched_count, 0, "inspector controls selected a row");

    // Width 768 (narrow): the hit detail mounts inline, directly below the
    // tapped row; the dock shell stays hidden and the table gains one row.
    first.click().await?;
    let inline_inspector: bool = driver
        .execute(
            r#"const selected=document.querySelector('.kwic-row[aria-selected="true"]');const inline=document.querySelector('.kwic-inspector-row');const cell=inline?.querySelector('td');const dock=document.getElementById('inspector-dock');return Boolean(selected&&inline&&cell&&selected.nextElementSibling===inline&&document.querySelectorAll('.kwic-inspector-row').length===1&&cell.colSpan===document.querySelector('[data-hit-grid] thead tr').cells.length&&cell.querySelector('.hit-detail[data-reference-ordinal="0"]')&&cell.querySelector('#inspector-body')&&dock.hidden===true);"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        inline_inspector,
        "narrow inspector must sit immediately below the tapped KWIC row"
    );
    // Wide viewports: the same inspector re-homes into the sticky dock
    // column, and rail/dock/pane-header all track while the table scrolls.
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let dock_wide: (String, bool, bool) = driver
        .execute(
            r#"const dock=document.getElementById('inspector-dock');return [getComputedStyle(dock).position,document.querySelector('.results-explorer').classList.contains('inspector-open'),Boolean(dock.querySelector('.hit-detail[data-reference-ordinal="0"]'))];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(
        dock_wide.0, "sticky",
        "wide inspector dock must be a sticky grid column"
    );
    assert!(dock_wide.1, "explorer must carry inspector-open while open");
    assert!(dock_wide.2, "open inspector re-homed into the wide dock");
    let tracking: (f64, f64, f64, f64) = driver
        .execute(
            r#"document.querySelector('.kwic-row[data-hit-ordinal="40"]').scrollIntoView({block:'center'});return [document.querySelector('.pattern-rail').getBoundingClientRect().top,document.getElementById('inspector-dock').getBoundingClientRect().top,document.querySelector('.kwic-pane-header').getBoundingClientRect().top,document.documentElement.scrollWidth-document.documentElement.clientWidth];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        tracking.0 <= 9.0,
        "rail must track the viewport: {}px",
        tracking.0
    );
    assert!(
        tracking.1 <= 9.0,
        "dock must track the viewport: {}px",
        tracking.1
    );
    assert!(
        tracking.2 <= 1.0,
        "pane header must pin at top: {}px",
        tracking.2
    );
    assert!(tracking.3 <= 1.0, "no horizontal overflow while docked");
    driver.execute("window.scrollTo(0,0)", vec![]).await?;
    driver.set_window_rect(0, 0, 768, 900).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The remaining steps exercise sentence expansion; close the inspector.
    driver
        .find(By::Css("[data-inspector-close]"))
        .await?
        .click()
        .await?;
    let inspector_closed: bool = driver
        .execute(
            "return !document.querySelector('.kwic-inspector-row')&&!document.querySelector('.kwic-row[aria-selected=true]')&&document.getElementById('inspector-dock').hidden===true",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        inspector_closed,
        "closing the inspector must leave no chrome behind"
    );
    let expand = driver
        .find(By::Css(
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
        ))
        .await?;
    expand.click().await?;
    let expanded: ExpandedMetrics = driver
        .execute(
            r#"
const row = document.querySelector('.kwic-row[data-hit-ordinal="0"]');
const left = row.querySelector('.kwic-left');
const text = row.querySelector('.kwic-left-text');
const rangeFor = (needle) => {
  const start = text.textContent.indexOf(needle);
  const range = new Range();
  range.setStart(text.firstChild, start);
  range.setEnd(text.firstChild, start + needle.length);
  return range.getBoundingClientRect();
};
const inside = (rect, box) =>
  rect.left >= box.left - 1 && rect.right <= box.right + 1;
const box = left.getBoundingClientRect();
const style = getComputedStyle(left);
return {
  text: row.querySelector('.kwic-sentence').textContent,
  maskImage: style.maskImage,
  webkitMaskImage: style.webkitMaskImage,
  prefixVisible: inside(rangeFor('PREFIX_SENTINEL'), box),
  suffixVisible: inside(rangeFor('MATCH_SIDE_SUFFIX…'), box)
};
"#,
            vec![],
        )
        .await?
        .convert()?;
    let expected_right = concat!(
        "RIGHT_SIDE_PREFIX・",
        "後続文脈・後続文脈・後続文脈・後続文脈・後続文脈・",
        "後続文脈・後続文脈・後続文脈・後続文脈・後続文脈・",
        "RIGHT_END_SENTINEL",
    );
    assert_eq!(
        expanded.text,
        format!("{expected_left}一致表現{expected_right}")
    );
    assert_eq!(expanded.mask_image, "none");
    assert_eq!(expanded.webkit_mask_image, "none");
    assert!(expanded.prefix_visible, "expanded prefix stayed clipped");
    assert!(expanded.suffix_visible, "expanded suffix stayed clipped");
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("true")
    );
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-label"
        )
        .await?
        .as_deref(),
        Some("Show clipped sentence")
    );
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "title"
        )
        .await?
        .as_deref(),
        Some("Show clipped sentence")
    );
    let expanded_icon: String = driver
        .execute(
            r#"return getComputedStyle(
  document.querySelector(
    ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]"
  ),
  "::before"
).content"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(expanded_icon, "\"▴\"");
    expand.click().await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );
    let hit_select_count: u64 = driver
        .execute("return window.hitSelectCount", vec![])
        .await?
        .convert()?;
    assert_eq!(
        hit_select_count, 1,
        "disclosure-only click repeated the request"
    );

    first.send_keys(Key::Enter).await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );
    first.send_keys(Key::Escape).await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );
    assert_eq!(
        dom_attribute(&driver, ".kwic-row[data-hit-ordinal='0']", "aria-selected")
            .await?
            .as_deref(),
        Some("false")
    );

    expand.click().await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("true")
    );

    let second = driver
        .find(By::Css(".kwic-row[data-hit-ordinal='1']"))
        .await?;
    second.click().await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='0'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("true")
    );
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='1'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );

    driver
        .find(By::Css(".kwic-row[data-hit-ordinal='2']"))
        .await?;
    wait_for_measurement(&driver, ".kwic-row[data-hit-ordinal='2']").await?;
    assert_eq!(
        dom_attribute(
            &driver,
            ".kwic-row[data-hit-ordinal='2'] [data-expand-sentence]",
            "aria-expanded"
        )
        .await?
        .as_deref(),
        Some("false")
    );

    let interaction: InteractionMetrics = driver
        .execute(
            r#"const percentile=(values,p)=>{values.sort((a,b)=>a-b);return values[Math.floor(values.length*p)]};const expand=document.querySelector('.kwic-row[data-hit-ordinal="0"] [data-expand-sentence]');const variation=document.querySelector('.surface-filter[data-variation="1"]');const expansion=[];const filter=[];for(let run=0;run<30;run++){let start=performance.now();expand.click();expansion.push(performance.now()-start);expand.click();start=performance.now();variation.click();filter.push(performance.now()-start);variation.click();if(document.querySelector('.surface-filter[aria-pressed="true"]'))throw new Error('surface reset failed')}return {expansionP50:percentile(expansion,.50),expansionP95:percentile(expansion,.95),filterP50:percentile(filter,.50),filterP95:percentile(filter,.95)};"#,
            vec![],
        )
        .await?
        .convert()?;
    eprintln!(
        "rows=1000 expansion_ms_p50={:.3} expansion_ms_p95={:.3} filter_ms_p50={:.3} filter_ms_p95={:.3}",
        interaction.expansion_p50,
        interaction.expansion_p95,
        interaction.filter_p50,
        interaction.filter_p95
    );

    driver.quit().await
}

#[tokio::test]
async fn editor_chrome_follows_light_and_dark_palette() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
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
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    let tools = ChromeDevTools::new(driver.handle.clone());

    for scheme in ["dark", "light"] {
        eprintln!("editor-chrome-e2e: apply {scheme} scheme");
        emulate_color_scheme(&tools, scheme).await?;
        driver.goto(format!("{}/editor", fixture.address)).await?;
        wait_for_enhanced_editor(&driver).await?;
        eprintln!("editor-chrome-e2e: enhanced editor ready");
        let content = driver.find(By::Css(".cm-content")).await?;
        content.click().await?;
        content.send_keys(Key::End).await?;
        for _ in 0..9 {
            content.send_keys(Key::Left).await?;
        }
        wait_for_selector(&driver, ".cm-pattern-signature").await?;
        eprintln!("editor-chrome-e2e: signature ready");

        content.send_keys(Key::Control + "f").await?;
        wait_for_selector(&driver, ".cm-panels").await?;
        eprintln!("editor-chrome-e2e: panel ready");

        let colors: ChromeColors = driver
            .execute(
                r#"
const tooltip = document.querySelector('.cm-pattern-signature').closest('.cm-tooltip');
const secondary = tooltip.querySelector('.cm-pattern-signature-doc');
const panel = document.querySelector('.cm-panels');
const editor = document.querySelector('.cm-editor');
const style = (node) => getComputedStyle(node);
const resolveToken = (name) => {
  const probe = document.createElement('span');
  probe.style.color = `var(${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
};
const rgb = (value) => value.match(/\d+(?:\.\d+)?/g).slice(0, 3).map(Number);
const luminance = (value) => {
  const channels = rgb(value).map((channel) => {
    const normalized = channel / 255;
    return normalized <= 0.04045
      ? normalized / 12.92
      : ((normalized + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
};
const background = style(tooltip).backgroundColor;
const foreground = style(tooltip).color;
const topPanelProbe = document.createElement('div');
topPanelProbe.className = 'cm-panels cm-panels-top';
editor.append(topPanelProbe);
const topPanelBorder = style(topPanelProbe).borderBottomColor;
topPanelProbe.remove();
const bottomPanelProbe = document.createElement('div');
bottomPanelProbe.className = 'cm-panels cm-panels-bottom';
editor.append(bottomPanelProbe);
const bottomPanelBorder = style(bottomPanelProbe).borderTopColor;
bottomPanelProbe.remove();
const lighter = Math.max(luminance(background), luminance(foreground));
const darker = Math.min(luminance(background), luminance(foreground));
return {
  prefersDark: matchMedia('(prefers-color-scheme: dark)').matches,
  tooltipBackground: background,
  tooltipText: foreground,
  secondaryText: style(secondary).color,
  panelBackground: style(panel).backgroundColor,
  panelText: style(panel).color,
  panelTopBorder: topPanelBorder,
  panelBottomBorder: bottomPanelBorder,
  searchInputBackground: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    return search ? style(search.querySelector('input')).backgroundColor : '';
  })(),
  searchInputText: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    return search ? style(search.querySelector('input')).color : '';
  })(),
  searchInputBorder: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    return search ? style(search.querySelector('input')).borderColor : '';
  })(),
  searchButtonBackgroundImage: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    return search ? style(search.querySelector('button:not([name=close])')).backgroundImage : 'none';
  })(),
  searchButtonBorder: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    return search ? style(search.querySelector('button:not([name=close])')).borderColor : '';
  })(),
  searchInputContrast: (() => {
    const search = document.querySelector('.cm-panel.cm-search');
    if (!search) return 0;
    const bg = luminance(style(search.querySelector('input')).backgroundColor);
    const fg = luminance(style(search.querySelector('input')).color);
    return (Math.max(bg, fg) + 0.05) / (Math.min(bg, fg) + 0.05);
  })(),
  tooltipBackgroundToken: resolveToken('--pattern-tooltip-bg'),
  tooltipTextToken: resolveToken('--pattern-mirror-text'),
  secondaryTextToken: resolveToken('--pattern-status-text'),
  borderToken: resolveToken('--pattern-mirror-border'),
  contrast: (lighter + 0.05) / (darker + 0.05)
};
"#,
                vec![],
            )
            .await?
            .convert()?;

        assert_eq!(
            colors.prefers_dark,
            scheme == "dark",
            "CDP did not apply the requested {scheme} media preference"
        );
        let (expected_background, expected_text, expected_secondary, expected_border) = match scheme
        {
            "dark" => (
                "rgb(35, 37, 43)",
                "rgb(209, 213, 219)",
                "rgb(154, 160, 166)",
                "rgb(75, 85, 99)",
            ),
            "light" => (
                "rgb(255, 255, 255)",
                "rgb(31, 41, 55)",
                "rgb(75, 85, 99)",
                "rgb(156, 163, 175)",
            ),
            _ => unreachable!(),
        };
        assert_eq!(colors.tooltip_background_token, expected_background);
        assert_eq!(colors.tooltip_text_token, expected_text);
        assert_eq!(colors.secondary_text_token, expected_secondary);
        assert_eq!(colors.border_token, expected_border);
        assert_eq!(
            colors.tooltip_background, colors.tooltip_background_token,
            "{scheme} tooltip background escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.tooltip_text, colors.tooltip_text_token,
            "{scheme} tooltip text escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.secondary_text, colors.secondary_text_token,
            "{scheme} secondary text escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.panel_background, colors.tooltip_background_token,
            "{scheme} panel background escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.panel_text, colors.tooltip_text_token,
            "{scheme} panel text escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.panel_top_border, colors.border_token,
            "{scheme} top-panel border escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.panel_bottom_border, colors.border_token,
            "{scheme} bottom-panel border escaped the lingrep patterns palette"
        );
        assert!(
            colors.contrast >= 4.5,
            "{scheme} tooltip contrast was {}",
            colors.contrast
        );

        // Search panel (Ctrl+F): the search input must take the scheme-flipping
        // palette — CM's baseTheme hardcodes *light* internals otherwise
        // (white bg under light-gray mirror text; the user's bug).
        assert_eq!(
            colors.search_input_background, colors.tooltip_background_token,
            "{scheme} search input background escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.search_input_text, colors.tooltip_text_token,
            "{scheme} search input text escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.search_input_border, colors.border_token,
            "{scheme} search input border escaped the lingrep patterns palette"
        );
        assert_eq!(
            colors.search_button_background_image, "none",
            "{scheme} search button kept CM's hardcoded light gradient"
        );
        assert_eq!(
            colors.search_button_border, colors.border_token,
            "{scheme} search button border escaped the lingrep patterns palette"
        );
        assert!(
            colors.search_input_contrast >= 4.5,
            "{scheme} search input contrast was {}",
            colors.search_input_contrast
        );

        driver
            .action_chain()
            .send_keys(Key::Escape)
            .perform()
            .await?;
        // `send_keys` focuses the contenteditable directly. A pointer click at
        // the element's center is flaky here because the intentionally open
        // signature tooltip may cover that coordinate.
        content.send_keys(Key::End).await?;
        for _ in 0..5 {
            content.send_keys(Key::Left).await?;
        }
        content.send_keys(Key::Control + Key::Space).await?;
        wait_for_selector(&driver, ".cm-tooltip-autocomplete").await?;
        wait_for_selector(&driver, ".cm-completionInfo").await?;
        eprintln!("editor-chrome-e2e: completion ready");
        let completion: CompletionColors = driver
            .execute(
                r#"
const popup = document.querySelector('.cm-tooltip-autocomplete');
const info = popup.querySelector('.cm-completionInfo');
const selected = popup.querySelector('li[aria-selected]');
const resolveToken = (name) => {
  const probe = document.createElement('span');
  probe.style.color = `var(${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
};
const resolveBackgroundToken = (name) => {
  const probe = document.createElement('span');
  probe.style.backgroundColor = `var(${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).backgroundColor;
  probe.remove();
  return color;
};
return {
  popup: {
    background: getComputedStyle(popup).backgroundColor,
    text: getComputedStyle(popup).color,
    secondary: getComputedStyle(info).color,
    backgroundToken: resolveToken('--pattern-tooltip-bg'),
    textToken: resolveToken('--pattern-mirror-text'),
    secondaryToken: resolveToken('--pattern-mirror-text')
  },
  selectedBackground: getComputedStyle(selected).backgroundColor,
  selectedText: getComputedStyle(selected).color,
  selectionToken: resolveBackgroundToken('--pattern-selection')
};
"#,
                vec![],
            )
            .await?
            .convert()?;
        assert_eq!(
            completion.popup.background, completion.popup.background_token,
            "{scheme} completion background escaped the lingrep patterns palette"
        );
        assert_eq!(
            completion.popup.text, completion.popup.text_token,
            "{scheme} completion text escaped the lingrep patterns palette"
        );
        assert_eq!(
            completion.popup.secondary.as_deref(),
            Some(completion.popup.secondary_token.as_str()),
            "{scheme} completion information escaped the lingrep patterns palette"
        );
        assert_eq!(
            completion.selected_background, completion.selection_token,
            "{scheme} selected completion escaped the lingrep patterns selection palette"
        );
        assert_eq!(
            completion.selected_text, completion.popup.text_token,
            "{scheme} selected completion text escaped the lingrep patterns palette"
        );

        driver
            .action_chain()
            .send_keys(Key::Escape)
            .perform()
            .await?;
        let hover_point: HoverPoint = driver
            .execute(
                r#"
const root = document.querySelector('.cm-content');
const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
let node;
while ((node = walker.nextNode())) {
  const start = node.data.indexOf('pos');
  if (start < 0) continue;
  const range = new Range();
  range.setStart(node, start);
  range.setEnd(node, start + 3);
  const rect = range.getBoundingClientRect();
  return {x: Math.round((rect.left + rect.right) / 2), y: Math.round((rect.top + rect.bottom) / 2)};
}
throw new Error('pos token not found');
"#,
                vec![],
            )
            .await?
            .convert()?;
        driver
            .action_chain()
            .move_to(hover_point.x, hover_point.y)
            .perform()
            .await?;
        wait_for_selector(&driver, ".cm-pattern-hover").await?;
        eprintln!("editor-chrome-e2e: hover ready");
        let hover: PopupColors = driver
            .execute(
                r#"
const content = document.querySelector('.cm-pattern-hover');
const popup = content.closest('.cm-tooltip');
const detail = content.querySelector('div');
const resolveToken = (name) => {
  const probe = document.createElement('span');
  probe.style.color = `var(${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
};
return {
  background: getComputedStyle(popup).backgroundColor,
  text: getComputedStyle(popup).color,
  secondary: getComputedStyle(detail).color,
  backgroundToken: resolveToken('--pattern-tooltip-bg'),
  textToken: resolveToken('--pattern-mirror-text'),
  secondaryToken: resolveToken('--pattern-status-text')
};
"#,
                vec![],
            )
            .await?
            .convert()?;
        assert_eq!(
            hover.background, hover.background_token,
            "{scheme} hover background escaped the lingrep patterns palette"
        );
        assert_eq!(
            hover.text, hover.text_token,
            "{scheme} hover text escaped the lingrep patterns palette"
        );
        assert_eq!(
            hover.secondary.as_deref(),
            Some(hover.secondary_token.as_str()),
            "{scheme} hover detail escaped the lingrep patterns palette"
        );
    }

    driver.quit().await?;
    Ok(())
}

async fn row_hidden(driver: &WebDriver, ordinal: u32) -> WebDriverResult<bool> {
    let hidden: bool = driver
        .execute(
            "return document.querySelector(`.kwic-row[data-hit-ordinal='${arguments[0]}']`).hidden",
            vec![serde_json::to_value(ordinal)?],
        )
        .await?
        .convert()?;
    Ok(hidden)
}

#[tokio::test]
async fn section_and_surface_filters_compose_and_persist() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.goto(format!("{}/filter", fixture.address)).await?;
    for _ in 0..100 {
        if driver
            .find(By::Css(".kwic-filter-status"))
            .await?
            .text()
            .await?
            .starts_with("1–4 of 4")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 1. Select surface v1 → rows r1, r3 remain (variation 1); r0, r2 hidden.
    driver
        .find(By::Css(".surface-filter[data-variation='1']"))
        .await?
        .click()
        .await?;
    assert!(row_hidden(&driver, 0).await?);
    assert!(!row_hidden(&driver, 1).await?);
    assert!(row_hidden(&driver, 2).await?);
    assert!(!row_hidden(&driver, 3).await?);
    assert!(
        !scope_note_hidden(&driver).await?,
        "a surface filter narrows the section counts too, and the panel must \
         say which population it is describing (vibrato-pipe-9wp)"
    );

    // 2. Also select section methods (index 0) → only r1 (v1 ∩ methods) remains.
    driver
        .find(By::Css(".section-filter[data-section='0']"))
        .await?
        .click()
        .await?;
    assert!(!row_hidden(&driver, 1).await?, "the intersection row stays");
    assert!(
        row_hidden(&driver, 0).await?,
        "r0 (methods but variation 0) stays hidden — surface excludes it"
    );
    assert!(
        row_hidden(&driver, 2).await?,
        "r2 (no section) stays hidden — section excludes it"
    );
    assert!(
        row_hidden(&driver, 3).await?,
        "v1 but not methods is hidden"
    );
    // both group selections are pressed simultaneously
    assert_eq!(
        dom_attribute(
            &driver,
            ".surface-filter[data-variation='1']",
            "aria-pressed"
        )
        .await?
        .as_deref(),
        Some("true")
    );
    assert_eq!(
        dom_attribute(&driver, ".section-filter[data-section='0']", "aria-pressed")
            .await?
            .as_deref(),
        Some("true")
    );
    // status names both scopes
    let status: String = driver
        .execute(
            "return document.querySelector('.kwic-filter-status').textContent",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        status.contains("Surface “v1”"),
        "status names the surface: {status}"
    );
    assert!(
        status.contains("Section “methods”"),
        "status names the section: {status}"
    );

    // 3. Reset the surface to All → section methods PERSISTS → r0, r1 remain.
    driver
        .find(By::Css(".surface-filter[data-variation='1']"))
        .await?
        .click()
        .await?;
    assert!(
        !row_hidden(&driver, 0).await?,
        "r0 (methods) returns under all surfaces"
    );
    assert!(!row_hidden(&driver, 1).await?, "r1 (methods) stays");
    assert!(
        row_hidden(&driver, 2).await?,
        "r2 (no section) stays hidden under methods"
    );
    assert!(
        row_hidden(&driver, 3).await?,
        "r3 (results only) stays hidden under methods"
    );
    assert_eq!(
        dom_attribute(&driver, ".section-filter[data-section='0']", "aria-pressed")
            .await?
            .as_deref(),
        Some("true"),
        "the section selection was not lost when the surface reset"
    );
    assert!(
        driver
            .find_all(By::Css(".surface-filter[aria-pressed='true']"))
            .await?
            .is_empty(),
        "the surface group has no pressed entry in its All state"
    );
    assert_eq!(
        dom_attribute(
            &driver,
            ".surface-filter[data-variation='1']",
            "aria-pressed"
        )
        .await?
        .as_deref(),
        Some("false"),
        "the previously selected v1 surface is released"
    );
    assert!(
        scope_note_hidden(&driver).await?,
        "with no surface filter the counts describe exactly what the heading \
         says, so the qualification must go away"
    );

    // 4. Select "(no section)" → only r2 (empty membership) remains.
    driver
        .find(By::Css(".section-filter[data-section='none']"))
        .await?
        .click()
        .await?;
    assert!(row_hidden(&driver, 0).await?);
    assert!(row_hidden(&driver, 1).await?);
    assert!(
        !row_hidden(&driver, 2).await?,
        "only the no-section row shows"
    );
    assert!(row_hidden(&driver, 3).await?);
    // Final-state contract: the (no section) button is pressed, the earlier
    // methods button is released, the surface group is back on "All surfaces",
    // and the status reflects all surfaces · section "(no section)".
    assert_eq!(
        dom_attribute(
            &driver,
            ".section-filter[data-section='none']",
            "aria-pressed"
        )
        .await?
        .as_deref(),
        Some("true"),
        "the (no section) button is pressed"
    );
    assert_eq!(
        dom_attribute(&driver, ".section-filter[data-section='0']", "aria-pressed")
            .await?
            .as_deref(),
        Some("false"),
        "the methods button is released within its group"
    );
    assert!(
        driver
            .find_all(By::Css(".surface-filter[aria-pressed='true']"))
            .await?
            .is_empty(),
        "the surface group stays unfiltered"
    );
    let final_status: String = driver
        .execute(
            "return document.querySelector('.kwic-filter-status').textContent",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        final_status.contains("All surfaces · Section “(no section)”"),
        "status reads all surfaces · section \"(no section)\": {final_status}"
    );

    driver.quit().await
}

#[tokio::test]
async fn results_explorer_is_master_detail_and_discloses_honestly() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
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
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.goto(format!("{}/explorer", fixture.address)).await?;

    // --- Desktop (>= 64rem): two-column, sticky rail, heading shown, toggle hidden ---
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let desktop: ExplorerDesktop = driver
        .execute(
            r#"const ex=document.querySelector('.results-explorer');const rail=document.querySelector('.pattern-rail');const det=document.querySelector('.detail-panel');const sum=document.querySelector('.pattern-catalog > summary');const head=document.querySelector('.pattern-catalog-heading');const cat=document.querySelector('.pattern-catalog');const list=document.querySelector('#pattern-table');const rr=rail.getBoundingClientRect();const dr=det.getBoundingClientRect();return {display:getComputedStyle(ex).display,railPosition:getComputedStyle(rail).position,summaryDisplay:getComputedStyle(sum).display,headingDisplay:getComputedStyle(head).display,detailsOpen:cat.open,listVisible:list.getBoundingClientRect().height>0,sideBySide:rr.right<=dr.left+1&&rr.top<dr.bottom&&dr.top<rr.bottom,overflow:document.documentElement.scrollWidth-document.documentElement.clientWidth};"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(
        desktop.display, "grid",
        "desktop results-explorer must be a grid"
    );
    assert_eq!(desktop.rail_position, "sticky", "rail must be sticky");
    assert_eq!(
        desktop.summary_display, "none",
        "desktop hides the disclosure toggle"
    );
    assert_ne!(
        desktop.heading_display, "none",
        "desktop shows the catalog heading"
    );
    assert!(desktop.details_open, "catalog stays open on desktop");
    assert!(
        desktop.list_visible,
        "pattern list visible — open agrees with visibility"
    );
    assert!(desktop.side_by_side, "rail sits left of the detail pane");
    assert!(desktop.overflow <= 1.0, "no horizontal overflow at 1440px");

    // Label sync: selecting a pattern updates BOTH labels (heading + summary).
    let labels: Vec<String> = driver
        .execute(
            r#"document.querySelectorAll('.pattern-selector')[1].click();return [...document.querySelectorAll('[data-selected-pattern]')].map(n=>n.textContent.trim());"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(
        labels,
        vec!["される".to_string(), "される".to_string()],
        "both labels sync on pattern selection"
    );

    // The rail filter narrows the catalog by pattern name; clearing restores.
    let filtered: (bool, bool) = driver
        .execute(
            r#"const input=document.getElementById('pattern-rail-filter');input.value='させる';input.dispatchEvent(new Event('input',{bubbles:true}));const rows=[...document.querySelectorAll('#pattern-table tr[data-authored-index]')];return [rows[0].style.display!=='none',rows[1].style.display==='none'];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(filtered.0, "matching pattern stays visible");
    assert!(filtered.1, "non-matching pattern is hidden");
    let variation_untouched: bool = driver
        .execute(
            r#"return !document.querySelector('[data-ranked-entry="surface"]').hidden;"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        variation_untouched,
        "pattern-name filtering does not filter variation entries"
    );
    driver
        .execute(
            r#"const input=document.getElementById('pattern-rail-filter');input.value='';input.dispatchEvent(new Event('input',{bubbles:true}));"#,
            vec![],
        )
        .await?;
    let restored: bool = driver
        .execute(
            r#"return [...document.querySelectorAll('#pattern-table tr[data-authored-index]')].every((row)=>row.style.display!=='none');"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(restored, "clearing the filter restores every pattern row");

    // --- Mobile (< 64rem): stacked, heading hidden, native operable disclosure ---
    driver.set_window_rect(0, 0, 320, 780).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (mobile_display, mobile_summary, mobile_heading): (String, String, String) = driver
        .execute(
            r#"return [getComputedStyle(document.querySelector('.results-explorer')).display,getComputedStyle(document.querySelector('.pattern-catalog > summary')).display,getComputedStyle(document.querySelector('.pattern-catalog-heading')).display];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(mobile_display, "block", "mobile stacks (not grid)");
    assert_ne!(mobile_summary, "none", "mobile shows the disclosure toggle");
    assert_eq!(mobile_heading, "none", "mobile hides the desktop heading");

    // Honesty invariant both directions: `open` agrees with whether the list is
    // actually rendered. Use checkVisibility() (not getBoundingClientRect) because
    // a closed <details> hides content via content-visibility, which still reports
    // a layout box. A short settle lets content-visibility apply after the toggle.
    let read = r#"const c=document.querySelector('.pattern-catalog');return [c.open, document.querySelector('#pattern-table').checkVisibility()];"#;
    let click = r#"document.querySelector('.pattern-catalog > summary').click();"#;
    let (o0, v0): (bool, bool) = driver.execute(read, vec![]).await?.convert()?;
    assert!(
        o0 && v0,
        "starts open and visible on mobile (open={o0} visible={v0})"
    );
    driver.execute(click, vec![]).await?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (o1, v1): (bool, bool) = driver.execute(read, vec![]).await?.convert()?;
    assert!(
        !o1 && !v1,
        "collapsing hides the list (open={o1} visible={v1})"
    );
    driver.execute(click, vec![]).await?;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (o2, v2): (bool, bool) = driver.execute(read, vec![]).await?.convert()?;
    assert!(
        o2 && v2,
        "expanding shows the list again (open={o2} visible={v2})"
    );

    driver.quit().await
}

/// A warning must reach CodeMirror's lint layer, not merely the status line.
///
/// This is the assertion that fails if the valid path starts discarding
/// diagnostics again — the bug this feature exists to fix. The unit tests
/// cover the mapping; only a real editor proves the mapped diagnostics are
/// actually handed to the linter.
#[tokio::test]
async fn a_warning_on_a_valid_pattern_reaches_the_lint_gutter() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
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
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;

    driver.goto(format!("{}/editor", fixture.address)).await?;
    wait_for_enhanced_editor(&driver).await?;

    // The check runs on blur; the fixture has a button outside the editor.
    driver.find(By::Css(".cm-content")).await?.click().await?;
    driver.find(By::Css("#blur-target")).await?.click().await?;

    // CodeMirror 6 marks the gutter with `cm-lint-marker-warning` and the
    // inline range with `cm-lintRange-warning`; accept either.
    wait_for_selector(&driver, ".cm-lint-marker-warning, .cm-lintRange-warning").await?;

    let status_text = driver
        .find(By::Css(".pattern-editor-status"))
        .await?
        .text()
        .await?;
    assert!(
        status_text.contains("1 warning"),
        "status line must report the warning: {status_text}"
    );
    assert!(
        status_text.contains("1 definition"),
        "status line must report the stats: {status_text}"
    );

    let invalid = dom_attribute(&driver, "textarea[data-pattern-editor]", "aria-invalid").await?;
    assert_eq!(
        invalid.as_deref(),
        Some("false"),
        "a warning must not mark the pattern invalid"
    );

    driver.quit().await
}

/// Navigation must work in a real editor, not just in the resolver.
///
/// `loadGrammar` building the navigation query, the state field carrying it,
/// and the keymap registration are all invisible to the unit tests — a
/// mistyped key name passes every one of them.
#[tokio::test]
async fn editor_navigates_from_a_reference_to_its_definition_and_between_references()
-> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
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
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;

    driver
        .goto(format!("{}/navigation", fixture.address))
        .await?;
    wait_for_enhanced_editor(&driver).await?;

    // The check writes the same status line the navigation messages use; let
    // it land before asserting on anything navigation put there.
    wait_for_settled_check(&driver).await?;

    let content = driver.find(By::Css(".cm-content")).await?;
    content.click().await?;
    // Put the caret inside the first `@F` (document offset 32). Home then six
    // Rights on line 2 is stable in a way a click coordinate is not.
    content.send_keys(Key::Control + Key::End).await?;
    content.send_keys(Key::Home).await?;
    for _ in 0..6 {
        content.send_keys(Key::Right).await?;
    }

    content.send_keys(Key::F12).await?;
    let status_text = status_line(&driver).await?;
    assert!(
        status_text.contains("fragment F, 2 references"),
        "status line after F12: {status_text}"
    );
    let selected: String = driver
        .execute("return window.getSelection().toString();", vec![])
        .await?
        .convert()?;
    assert_eq!(selected, "F", "F12 must select the definition identifier");

    content.send_keys(Key::F4).await?;
    let after_first = status_line(&driver).await?;
    assert!(
        after_first.contains("Reference 1 of 2"),
        "status line after F4: {after_first}"
    );

    content.send_keys(Key::F4).await?;
    let after_second = status_line(&driver).await?;
    assert!(
        after_second.contains("Reference 2 of 2"),
        "status line after a second F4: {after_second}"
    );

    // Wrapping is the resolver's job, but prove the wiring round-trips it.
    content.send_keys(Key::F4).await?;
    let wrapped = status_line(&driver).await?;
    assert!(
        wrapped.contains("Reference 1 of 2"),
        "F4 must wrap back to the first reference: {wrapped}"
    );

    driver.quit().await
}

async fn status_line(driver: &WebDriver) -> WebDriverResult<String> {
    driver
        .find(By::Css(".pattern-editor-status"))
        .await?
        .text()
        .await
}

/// Block until the initial compile check has written the status line.
///
/// The status line is one shared slot: navigation messages and check results
/// both land there, last writer wins. The check is asynchronous, so a
/// navigation message written before it completes gets overwritten. Cursor
/// movement does not change the document and therefore triggers no further
/// check, so waiting for the first one to settle makes the rest of a
/// navigation scenario deterministic.
async fn wait_for_settled_check(driver: &WebDriver) -> WebDriverResult<()> {
    for _ in 0..200 {
        if status_line(driver).await?.contains("Pattern is valid") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(WebDriverError::Timeout(
        "waiting for the initial lingrep patterns check to settle".into(),
    ))
}

/// Format-on-blur and the plain-mode fallback, rehomed from the retired
/// external-server `e2e_test` module.
///
/// Neither behavior is covered anywhere else: `pattern-editor-plain` appears in no
/// other test in this workspace, and the formatter's blur trigger is invisible
/// to the unit tests, which call it directly. The large-source path matters
/// because it *downgrades* the editor — the enhancement is dropped, and the
/// only thing keeping the pattern readable is that CodeMirror stays visible
/// while the source textarea stays hidden. Get that backwards and a long
/// pattern becomes an unstyled textarea.
#[tokio::test]
async fn editor_formats_on_blur_and_keeps_large_sources_readable() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "LINGREP_WEB_LAYOUT_REQUIRE_DRIVER is set but no Chromedriver at {driver_url}"
        );
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }

    // The one caller that wants the production formatter: everything else
    // echoes, so that reformatting cannot shift the cursor offsets the other
    // editor fixtures pin.
    let fixture = fixture_server_with(FormatMode::Real).await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;

    driver.goto(format!("{}/editor", fixture.address)).await?;
    wait_for_enhanced_editor(&driver).await?;
    wait_for_settled_check(&driver).await?;

    // Replace the fixture's source with an unformatted pattern, then blur.
    // The fixture's button exists precisely so the blur has somewhere to go.
    let content = driver.find(By::Css(".cm-content")).await?;
    content.click().await?;
    content.send_keys(Key::Control + "a").await?;
    content.send_keys("x:=[upos=VERB].").await?;
    driver.find(By::Css("#blur-target")).await?.click().await?;

    let editor = driver.find(By::Id("editor-fixture")).await?;
    let mut formatted = None;
    for _ in 0..50 {
        formatted = editor.prop("value").await?;
        if formatted.as_deref() == Some("x := [upos=VERB].\n") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let status = status_line(&driver).await?;
    assert_eq!(
        formatted.as_deref(),
        Some("x := [upos=VERB].\n"),
        "blur must write the formatted source back to the textarea: {status}"
    );

    // Past the enhancement ceiling the editor degrades to plain mode. The
    // source textarea must STAY hidden: CodeMirror is the visible surface.
    driver
        .execute(
            "const e=document.getElementById('editor-fixture'); \
             e.value='a'.repeat(65537); \
             e.dispatchEvent(new Event('input',{bubbles:true}));",
            vec![],
        )
        .await?;
    wait_for_selector(&driver, ".pattern-editor.pattern-editor-plain").await?;
    assert!(
        !editor.is_displayed().await?,
        "the source textarea is never the visible surface, even in plain mode"
    );
    assert!(
        driver
            .find(By::Css(".cm-editor"))
            .await?
            .is_displayed()
            .await?,
        "plain mode must keep the pattern readable in CodeMirror"
    );

    driver.quit().await
}

/// The band is pinned and the results scroll under it, rather than the editor
/// pushing the results down. Measured before this landed: at 1440x900 the first
/// result sat at 503px folded and 1010px with the editor open, so editing put
/// every result off-screen.
///
/// The specific hazard this guards is in the stylesheet's own header comment:
/// an `overflow` other than `visible` or `clip` on any ancestor silently turns
/// every `position: sticky` on the page into ordinary flow.
#[tokio::test]
async fn the_query_band_stays_in_view_while_the_results_scroll() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    driver.goto(format!("{}/band", fixture.address)).await?;
    wait_for_selector(&driver, "[data-query-band]").await?;

    let geometry = |driver: WebDriver| async move {
        let reading: String = driver
            .execute(
                r#"const band = document.querySelector('[data-query-band]').getBoundingClientRect();
                   return [Math.round(band.top), Math.round(band.height),
                           Math.round(window.scrollY)].join(':')"#,
                vec![],
            )
            .await?
            .convert()?;
        Ok::<_, WebDriverError>(reading)
    };

    let resting = geometry(driver.clone()).await?;
    let height = resting
        .split(':')
        .nth(1)
        .and_then(|value| value.parse::<i64>().ok())
        .expect("the band has a height");
    assert!(
        (120..220).contains(&height),
        "the band is sized for the edit loop, not for authoring a catalog: {height}px"
    );

    let (category_hidden_initial, controls_single_row): (bool, bool) = driver
        .execute(
            r#"const cat = document.querySelector('.compound-category-control');
               const items = [...document.querySelectorAll('.permalink-controls > button, .permalink-controls > label, .permalink-controls > .compound-control-group')];
               const top = items[0].getBoundingClientRect().top;
               return [getComputedStyle(cat).display === 'none', items.every((el) => Math.abs(el.getBoundingClientRect().top - top) <= 8)];"#,
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        category_hidden_initial,
        "compound category is hidden until derive_compounds is checked"
    );
    assert!(
        controls_single_row,
        "the default controls strip fits on one row at 1440px"
    );

    driver
        .find(By::Css("input[name='derive_compounds']"))
        .await?
        .click()
        .await?;
    let category_shown: bool = driver
        .execute(
            "return getComputedStyle(document.querySelector('.compound-category-control')).display !== 'none'",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        category_shown,
        "checking derive_compounds reveals the compound category selector"
    );
    driver
        .find(By::Css("input[name='derive_compounds']"))
        .await?
        .click()
        .await?;

    driver.execute("window.scrollTo(0, 900)", vec![]).await?;
    let scrolled = geometry(driver.clone()).await?;
    assert_eq!(
        scrolled,
        format!("0:{height}:900"),
        "the band is pinned to the top of the viewport while the page scrolls under it"
    );

    // Below the breakpoint it is an ordinary block again: a band pinned to a
    // phone viewport costs more of it than it saves.
    driver.set_window_rect(0, 0, 800, 900).await?;
    driver.execute("window.scrollTo(0, 900)", vec![]).await?;
    let narrow: String = driver
        .execute(
            "return String(Math.round(document.querySelector('[data-query-band]').getBoundingClientRect().top))",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        narrow.parse::<i64>().expect("a number") < 0,
        "the band scrolls away below 64rem, and did not: {narrow}"
    );
    driver.quit().await
}

/// The concordance's controls stick to the concordance and to nothing above
/// it. They used to be a child of the whole panel, so they pinned across the
/// definition and distribution bands too — 153px of header over a table a
/// screen and a half below, and the charts passing underneath it
/// (`vibrato-pipe-csb`).
///
/// The pattern's name and counts are its identity rather than a control, so
/// they stay at the top of the panel and scroll away.
#[tokio::test]
async fn the_concordance_controls_stick_to_the_concordance_and_nothing_above_it()
-> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    driver.goto(fixture.address.clone()).await?;
    wait_for_selector(&driver, ".kwic-pane-header").await?;
    wait_for_selector(&driver, ".kwic-row").await?;

    let reading: (String, f64, f64, f64, String, f64) = driver
        .execute(
            r#"window.scrollTo(0, window.scrollY
                 + document.querySelector('.kwic-region').getBoundingClientRect().top + 800);
               const box = (sel) => document.querySelector(sel).getBoundingClientRect();
               const strip = box('.kwic-pane-header');
               return [getComputedStyle(document.querySelector('.kwic-pane-header')).position,
                       strip.top, strip.bottom,
                       box('.kwic-concordance thead th').top,
                       getComputedStyle(document.querySelector('.pattern-detail-summary')).position,
                       box('.kwic-region').top];"#,
            vec![],
        )
        .await?
        .convert()?;
    let (strip_position, strip_top, strip_bottom, head_top, summary_position, region_top) = reading;

    assert_eq!(strip_position, "sticky");
    assert!(
        strip_top >= region_top - 1.0,
        "the strip escaped the region it operates: strip {strip_top}px, region {region_top}px"
    );
    // Contiguous, not merely both sticky: a fractional gap between the two is a
    // scrolling row visible through the header.
    assert!(
        (head_top - strip_bottom).abs() <= 1.0,
        "the column headers do not sit against the strip: strip ends {strip_bottom}px, headers start {head_top}px"
    );
    assert_ne!(
        summary_position, "sticky",
        "the pattern's name and counts are identity, not controls, and scroll away"
    );
    driver.quit().await
}

/// The `data-detail-view` names currently displayed, sorted and comma-joined.
async fn displayed_views(driver: &WebDriver) -> WebDriverResult<String> {
    driver
        .execute(
            "return [...document.querySelectorAll('[data-detail-view]')]\
               .filter((view) => !view.hidden)\
               .map((view) => view.dataset.detailView).sort().join(',');",
            vec![],
        )
        .await?
        .convert()
}

/// One tab set over every view of a run, with searchable surface variations in
/// the Definition tab and filters still applied to the concordance.
///
/// Three contracts at once, because they are one design: the views are
/// mutually exclusive so the concordance sits directly under the query band; a
/// tab with no panel is hidden rather than offered and empty; and surface
/// variations are searchable beside the pattern definition instead of beneath
/// the catalog list.
#[tokio::test]
async fn detail_views_are_exclusive_and_surface_search_lives_with_the_definition()
-> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    driver.goto(format!("{}/tabs", fixture.address)).await?;
    wait_for_selector(&driver, ".definition-band [data-ranked-search=surface]").await?;

    let placement: (usize, usize) = driver
        .execute(
            "return [document.querySelectorAll('.definition-band .surface-filter').length,\
             document.querySelectorAll('.pattern-rail .surface-filter').length];",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(placement, (2, 0), "variations belong to the Definition tab");

    // Exactly one view is displayed, and it is the concordance.
    assert_eq!(displayed_views(&driver).await?, "concordance");

    // The clusters tab has no panel in this run, so it is hidden rather than
    // selectable and empty.
    let hidden_tabs: String = driver
        .execute(
            "return [...document.querySelectorAll('[data-detail-tab]')]\
               .filter((tab) => tab.hidden).map((tab) => tab.dataset.detailTab).join(',');",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(hidden_tabs, "clusters");

    // Sections is two panels — the pattern's own chart and the all-patterns
    // table — and selecting it shows both and nothing else.
    driver
        .find(By::Css("[data-detail-tab=\"sections\"]"))
        .await?
        .click()
        .await?;
    assert_eq!(displayed_views(&driver).await?, "sections,sections");

    driver
        .find(By::Css("[data-detail-tab=\"concordance\"]"))
        .await?
        .click()
        .await?;

    // The section list's controls and its plot area are one figure and share
    // one width. The list carried a `max-width` the tools did not, so Search
    // and Order stretched across the whole panel while the bars stopped well
    // short of them. The list is also never truncated, so there is no toggle.
    driver
        .find(By::Css("[data-detail-tab=\"sections\"]"))
        .await?
        .click()
        .await?;
    let ranked: (f64, f64, usize, usize) = driver
        .execute(
            "const filter=document.querySelector('[data-ranked-list=\"section\"]');\
             return [filter.querySelector('.ranked-tools').getBoundingClientRect().width,\
             filter.querySelector('.section-overview').getBoundingClientRect().width,\
             filter.querySelectorAll('[data-ranked-toggle], [data-ranked-tail]').length,\
             [...filter.querySelectorAll('[data-ranked-entry]')].filter((e) => e.hidden).length];",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        ranked.0 <= ranked.1 + 1.0,
        "the tools row must not outrun the chart it controls: {} vs {}",
        ranked.0,
        ranked.1
    );
    assert_eq!(ranked.2, 0, "the section list has no tail and no toggle");
    assert_eq!(ranked.3, 0, "every section row is drawn");

    driver
        .find(By::Css("[data-detail-tab=\"definition\"]"))
        .await?
        .click()
        .await?;
    assert_eq!(displayed_views(&driver).await?, "definition");
    driver
        .execute(
            "const search=document.querySelector('.definition-band [data-ranked-search=surface]');search.value='v1';search.dispatchEvent(new Event('input',{bubbles:true}));",
            vec![],
        )
        .await?;
    let search_results: String = driver
        .execute(
            "return [...document.querySelectorAll('.definition-band [data-ranked-entry=surface]')].map((entry)=>String(entry.hidden)).join(',');",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(
        search_results, "true,false",
        "search exposes the matching variation"
    );
    driver
        .find(By::Css(
            ".definition-band .surface-filter[data-variation=\"1\"]",
        ))
        .await?
        .click()
        .await?;
    let (filter_badge_active, view_button_shown): (bool, String) = driver
        .execute(
            "const tab=document.querySelector('[data-detail-tab=\"concordance\"]');\
             const btn=document.querySelector('[data-view-filtered-concordance=\"surface\"]');\
             return [tab.dataset.filterActive==='true' && !btn.hidden, btn.textContent];",
            vec![],
        )
        .await?
        .convert()?;
    assert!(
        filter_badge_active,
        "selecting a surface marks the Concordance tab as filtered and reveals the jump button"
    );
    assert!(
        view_button_shown.contains("View 1 matching hits in Concordance"),
        "the jump button names the matching hit count: {view_button_shown}"
    );
    driver
        .find(By::Css("[data-view-filtered-concordance=\"surface\"]"))
        .await?
        .click()
        .await?;
    assert_eq!(displayed_views(&driver).await?, "concordance");
    let filtered: (usize, String, bool) = driver
        .execute(
            "return [document.querySelectorAll('.kwic-row:not([hidden])').length,\
             document.querySelector('.kwic-filter-status').textContent,\
             !document.querySelector('[data-clear-surface-filter]').hidden];",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(filtered.0, 1, "only the selected surface's row stays");
    assert!(
        filtered.1.contains("v1"),
        "the status names it: {}",
        filtered.1
    );
    assert!(filtered.2, "the clear chip must appear");

    // Clearing it from the concordance's strip releases the Definition tab's
    // pressed state.
    driver
        .find(By::Css("[data-clear-surface-filter]"))
        .await?
        .click()
        .await?;
    let cleared: (usize, usize) = driver
        .execute(
            "return [document.querySelectorAll('.kwic-row:not([hidden])').length,\
             document.querySelectorAll('.surface-filter[aria-pressed=\"true\"]').length];",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(cleared, (3, 0));

    // Quick-filter drawer in the Concordance header lets a researcher filter by
    // surface directly inside Concordance without switching to Definition.
    driver
        .find(By::Css("[data-quick-filter-toggle=\"surface\"]"))
        .await?
        .click()
        .await?;
    driver
        .find(By::Css("[data-quick-variation=\"1\"]"))
        .await?
        .click()
        .await?;
    let quick_filtered: (usize, bool) = driver
        .execute(
            "return [document.querySelectorAll('.kwic-row:not([hidden])').length,\
             document.querySelector('.surface-filter[data-variation=\"1\"]').getAttribute('aria-pressed')==='true'];",
            vec![],
        )
        .await?
        .convert()?;
    assert_eq!(quick_filtered, (1, true));
    driver.quit().await
}

/// Bars are comparable only against a shared baseline and a shared scale, and
/// the track is the scale. Every row is its own grid, so a content-sized column
/// gave each row its own track and the right-hand edge came out jagged.
#[tokio::test]
async fn every_bar_in_a_ranked_list_shares_one_track() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.set_window_rect(0, 0, 1_440, 900).await?;
    driver.goto(format!("{}/filter", fixture.address)).await?;
    wait_for_selector(&driver, ".section-overview .surface-bar").await?;

    // The three rows differ in label length, rate width and count width — the
    // three things that used to move the track.
    let tracks: String = driver
        .execute(
            r#"return [...document.querySelectorAll('.section-overview .surface-bar')]
                 .map((bar) => { const box = bar.getBoundingClientRect();
                   return `${Math.round(box.left)},${Math.round(box.right)}`; })
                 .join(' ')"#,
            vec![],
        )
        .await?
        .convert()?;
    let mut edges = tracks.split(' ');
    let first = edges
        .next()
        .expect("the fixture draws three bars")
        .to_string();
    assert_eq!(tracks.split(' ').count(), 3);
    for edge in edges {
        assert_eq!(edge, first, "every bar shares one track: {tracks}");
    }
    driver.quit().await
}

/// Selecting a surface narrows the section distribution to that surface's own
/// corpus-exact counts, and the panel re-ranks on the rate.
///
/// The second selection is the load-bearing half. The first re-ranks the list,
/// so a client reading the shipped matrix by DOM position would then attach
/// one section's counts to another's row and denominator — which is how this
/// was first written, and what `data-matrix-index` exists to prevent.
#[tokio::test]
async fn a_selected_surface_narrows_the_section_distribution() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        eprintln!("SKIP: layout Chromedriver is unavailable at {driver_url}");
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    capabilities.add_arg("--disable-gpu")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver.goto(format!("{}/filter", fixture.address)).await?;
    for _ in 0..100 {
        if driver
            .find(By::Css(".kwic-filter-status"))
            .await?
            .text()
            .await?
            .starts_with("1–4 of 4")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // `label:count:rate` in drawn order, so one assertion covers the counts,
    // the recomputed rates and the ranking at once.
    let panel = |driver: WebDriver| async move {
        let rows: String = driver
            .execute(
                r#"return [...document.querySelectorAll('.section-distribution li')].map((row) =>
                     [row.querySelector('.section-label').textContent,
                      row.dataset.count,
                      row.querySelector('[data-rate-label]').textContent,
                      row.querySelector('.surface-bar-fill').style.inlineSize].join(':')).join(' | ')"#,
                vec![],
            )
            .await?
            .convert()?;
        Ok::<_, WebDriverError>((driver, rows))
    };

    let (driver, before) = panel(driver).await?;
    assert_eq!(
        before,
        "methods:40:400000.0 ppm:100% | results:300:300000.0 ppm:75% | (no section):10:100000.0 ppm:25%",
        "the unfiltered panel ranks on the rate, not the count"
    );

    // Surface 0: 5 of 100 in methods, 250 of 1000 in results, 9 of 100
    // unlabelled — so `results` leads on rate despite `methods` having been
    // first, and the list re-ranks.
    driver
        .execute(
            "document.querySelector(\".surface-filter[data-variation='0']\").click()",
            vec![],
        )
        .await?;
    let (driver, first) = panel(driver).await?;
    assert_eq!(
        first,
        "results:250:250000.0 ppm:100% | methods:5:50000.0 ppm:20% | (no section):9:90000.0 ppm:36%",
        "the panel did not narrow to the selected surface"
    );

    // Surface 1, from the re-ranked list.
    driver
        .execute(
            "document.querySelector(\".surface-filter[data-variation='1']\").click()",
            vec![],
        )
        .await?;
    let (driver, second) = panel(driver).await?;
    assert_eq!(
        second,
        "methods:35:350000.0 ppm:100% | results:50:50000.0 ppm:14.2857% | (no section):1:10000.0 ppm:2.85714%",
        "the second selection read the matrix by position rather than by \
         `data-matrix-index`, so the counts landed on the wrong rows"
    );

    // Releasing the filter restores exactly what the server rendered.
    driver
        .execute(
            "document.querySelector(\".surface-filter[data-variation='1']\").click()",
            vec![],
        )
        .await?;
    let (driver, restored) = panel(driver).await?;
    assert_eq!(
        restored, before,
        "clearing the filter did not restore the panel"
    );
    assert!(
        scope_note_hidden(&driver).await?,
        "with no surface selected the panel describes the whole pattern again"
    );

    driver.quit().await?;
    drop(fixture);
    Ok(())
}

fn provenance_fixture_html() -> String {
    use askama::Template;
    use lingrep_web::{
        provenance_view::{HitProvenance, ProvenanceDivision, metadata_fields},
        templates::HitDetailFragment,
    };
    let mut html = String::from(
        r#"<!doctype html><html><head><link rel="stylesheet" href="/assets/app.css"></head><body>"#,
    );
    for ordinal in 0..2 {
        html.push_str(&HitDetailFragment {
            reference_ordinal: ordinal, workbench_id: "wb", revision: 1, pattern_key: 0,
            id_prefix: format!("provenance-{ordinal}"), tokens: Vec::new(), capture_lanes: Vec::new(), captures: Vec::new(), graph_bindings: Vec::new(),
            provenance: Some(HitProvenance {
                source: "fixture.jsonl".into(),
                document: metadata_fields(Some(r#"{"speaker":"document"}"#)),
                divisions: Box::new([ProvenanceDivision {
                    source_id: None, id:0,kind:"paragraph".into(),parent_id:None,fields:metadata_fields(Some(r#"{"speaker":"paragraph"}"#)) }]),
                sentence: if ordinal == 0 { metadata_fields(Some(r#"{"speaker":"sentence","<img src=x onerror=alert(1)>":"<script>window.injected=true</script>"}"#)) } else { Box::new([]) },
                sections: Box::new([]),
            }),
        }.render().unwrap());
    }
    html.push_str("</body></html>");
    html
}

#[tokio::test]
async fn provenance_grains_render_separately_and_escape_metadata() -> WebDriverResult<()> {
    let driver_url = std::env::var("LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9516".into());
    if !driver_available(&driver_url).await {
        assert!(
            std::env::var("LINGREP_WEB_LAYOUT_REQUIRE_DRIVER").is_err(),
            "required Chromedriver unavailable"
        );
        return Ok(());
    }
    let fixture = fixture_server().await;
    let mut capabilities = DesiredCapabilities::chrome();
    capabilities.add_arg("--headless")?;
    capabilities.add_arg("--no-sandbox")?;
    capabilities.add_arg("--disable-dev-shm-usage")?;
    let driver = WebDriver::new(&driver_url, capabilities).await?;
    driver
        .goto(format!("{}/provenance", fixture.address))
        .await?;
    let groups = driver.find_all(By::Css(".provenance-group")).await?;
    assert_eq!(groups.len(), 5);
    for (index, expected) in [
        "Document",
        "Division paragraph",
        "Sentence",
        "Document",
        "Division paragraph",
    ]
    .iter()
    .enumerate()
    {
        assert!(groups[index].text().await?.starts_with(expected));
        assert_eq!(groups[index].css_value("display").await?, "block");
        let fields = groups[index].find(By::Css("dl")).await?;
        assert_eq!(fields.css_value("display").await?, "grid");
        let field = fields.find(By::Css(".provenance-field")).await?;
        assert_eq!(field.css_value("display").await?, "grid");
    }
    assert!(
        groups[2]
            .text()
            .await?
            .contains("<script>window.injected=true</script>")
    );
    assert!(driver.find_all(By::Css("img, script")).await?.is_empty());
    driver.quit().await?;
    Ok(())
}
