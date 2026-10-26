import { t, onLocaleChange } from "./i18n.js";
export const KWIC_PAGE_SIZES = Object.freeze([50, 100, 250, 500, 1000]);
export const DEFAULT_KWIC_PAGE_SIZE = 100;
const KWIC_PAGE_SIZE_KEY = "lingrep-web:kwic-page-size:v1";

// Per-pattern UI state (filters, page, scroll, selected hit), keyed by the
// authored pattern id and cleared whenever a new results revision arrives.
// Lets heavy users flip between patterns without losing their place.
const patternUiState = new Map();

export function parseKwicPageSize(value) {
  const parsed = Number(value);
  return KWIC_PAGE_SIZES.includes(parsed) ? parsed : DEFAULT_KWIC_PAGE_SIZE;
}

const kwicCollator = new Intl.Collator("ja");
const kwicCharacters = new Intl.Segmenter("ja", { granularity: "grapheme" });

export function sortKwicRows(rows, field, descending = false) {
  const keyed = rows.map(row => {
    let key = "";
    if (field === "section") {
      key = [...row.querySelectorAll(".kwic-section-label")]
        .map(label => label.textContent).sort(kwicCollator.compare).join(" / ");
    } else {
      const selector = { match: ".kwic-key", left: ".kwic-left-text", right: ".kwic-right" }[field];
      if (selector) key = row.querySelector(selector)?.textContent ?? "";
      if (field === "left") key = [...kwicCharacters.segment(key)].map(part => part.segment).reverse().join("");
    }
    return { row, key, ordinal: Number(row.dataset.hitOrdinal) };
  });
  keyed.sort((a, b) => {
    if (field === "section" && (!a.key !== !b.key)) return a.key ? -1 : 1;
    const order = field === "original" ? a.ordinal - b.ordinal : kwicCollator.compare(a.key, b.key);
    return (descending ? -order : order) || a.ordinal - b.ordinal;
  });
  return keyed.map(item => item.row);
}

export function pageProjection(rows, requestedPageIndex, pageSize) {
  const pageCount = Math.max(1, Math.ceil(rows.length / pageSize));
  const pageIndex = Math.min(
    Math.max(0, requestedPageIndex),
    pageCount - 1,
  );
  const start = rows.length === 0 ? 0 : pageIndex * pageSize;
  const end = Math.min(start + pageSize, rows.length);
  return {
    pageIndex,
    pageCount,
    start,
    end,
    rows: rows.slice(start, end),
  };
}

export function pageRangeLabel(
  start,
  end,
  filteredCount,
  surface = null,
  section,
  query = "",
  occurrence = "all",
) {
  const range = filteredCount === 0
    ? t("detail.hits_range_zero")
    : t("detail.hits_range", { start: start + 1, end, total: filteredCount });
  const scopes = [];
  if (occurrence === "standalone") scopes.push(t("detail.occurrence_scope_standalone"));
  if (occurrence === "chained") scopes.push(t("detail.occurrence_scope_chained"));
  scopes.push(
    surface === null
      ? t("detail.all_surfaces")
      : t("detail.surface", { name: surface }),
  );
  if (section !== undefined) {
    scopes.push(
      section === null
        ? t("detail.all_sections")
        : t("detail.section", { name: section }),
    );
  }
  const trimmed = query.trim();
  if (trimmed !== "") {
    scopes.push(t("detail.query", { query: trimmed }));
  }
  return `${range} · ${scopes.join(" · ")}`;
}

export function transitionPageState(state, action) {
  if (action.type === "filter") {
    return { ...state, pageIndex: 0 };
  }
  if (action.type === "page-size") {
    return { pageIndex: 0, pageSize: action.value };
  }
  const delta = action.type === "next"
    ? 1
    : action.type === "previous"
      ? -1
      : 0;
  return {
    ...state,
    pageIndex: Math.min(
      Math.max(0, state.pageIndex + delta),
      action.pageCount - 1,
    ),
  };
}

function loadKwicPageSize(storage = localStorage) {
  try {
    return parseKwicPageSize(storage.getItem(KWIC_PAGE_SIZE_KEY));
  } catch (_error) {
    return DEFAULT_KWIC_PAGE_SIZE;
  }
}

function saveKwicPageSize(value, storage = localStorage) {
  try {
    storage.setItem(KWIC_PAGE_SIZE_KEY, String(value));
  } catch (_error) {
    // Preference persistence is non-blocking.
  }
}

export function visibleForVariation(rowVariation, selectedVariation) {
  return selectedVariation === "all" || rowVariation === selectedVariation;
}

/** `rowInChain` is the row's `data-in-chain` text; `selected` is
 *  `standalone`, `chained`, or `all`. */
export function visibleForOccurrence(rowInChain, selected) {
  if (selected === "all") return true;
  return (rowInChain === "true") === (selected === "chained");
}

export function visibleForQuery(rowText, query = "") {
  const trimmed = query.trim();
  return trimmed === "" || searchMatches(rowText, trimmed);
}

/**
 * Shows the stated reason for an emptied concordance, or hides it.
 *
 * The variation list is corpus-exact and the concordance is the bounded
 * retained sample, so a surface with a positive corpus count can have no
 * retained row: pressing it empties the table while the count beside it still
 * says otherwise. The note names that population. A section-only selection
 * keeps the ordinary status line and no note of its own, and a surviving row
 * always clears the note.
 */
export function applyFilterEmptyNote(
  detail,
  visibleCount,
  surfaceSelected,
  sectionSelected,
  query = "",
) {
  const note = detail?.querySelector("[data-kwic-filter-empty]");
  if (!note) return;
  note.hidden = !(
    visibleCount === 0
    && surfaceSelected !== "all"
    && sectionSelected === "all"
    && query.trim() === ""
  );
}

/**
 * The occurrence counts on the selector are corpus-exact while the rows are
 * the retained sample, so a kind with a positive count can have no retained
 * row. Shown only when the occurrence selection alone emptied the table.
 */
export function applyOccurrenceEmptyNote(
  detail,
  visibleCount,
  occurrenceSelected,
  surfaceSelected,
  sectionSelected,
  query = "",
) {
  const note = detail?.querySelector("[data-kwic-occurrence-empty]");
  if (!note) return;
  note.hidden = !(
    visibleCount === 0
    && occurrenceSelected !== "all"
    && surfaceSelected === "all"
    && sectionSelected === "all"
    && query.trim() === ""
  );
}

export function filterStatus(visible, total, surface = null, section, query = "") {
  const count = visible === total
    ? t("detail.hits_all", { total })
    : t("detail.hits_partial", { visible, total });
  const scopes = [
    surface === null
      ? t("detail.all_surfaces")
      : t("detail.surface", { name: surface }),
  ];
  if (section !== undefined) {
    scopes.push(
      section === null
        ? t("detail.all_sections")
        : t("detail.section", { name: section }),
    );
  }
  const trimmed = query.trim();
  if (trimmed !== "") {
    scopes.push(t("detail.query", { query: trimmed }));
  }
  return `${count} · ${scopes.join(" · ")}`;
}

export function rankedEntryVisible(
  index,
  limit,
  expanded,
  queryMatches,
  searchActive = false,
) {
  if (searchActive) return queryMatches;
  return expanded || index < limit;
}

export function rankedTail(entries, limit) {
  const hidden = entries.slice(limit);
  return {
    entryCount: hidden.length,
    hitCount: hidden.reduce((sum, entry) => sum + entry.count, 0),
  };
}

export function namedMaximum(entries) {
  return Math.max(1, ...entries.map((entry) => Number(entry.count)));
}

/** The surface list's tail note. The section list has none: it is never
 *  truncated, being bounded by the corpus's own label inventory. */
export function rankedTailSummary(entryCount, hitCount, retainedCount) {
  const percentage = retainedCount === 0
    ? "0.0"
    : (hitCount * 100 / retainedCount).toFixed(1);
  return t("detail.tail_surface", {
    hits: hitCount,
    kinds: entryCount,
    pct: percentage,
  });
}

export function visibleForSection(rowSectionIds, selected) {
  if (selected === "all") return true;
  if (selected === "none") return rowSectionIds.length === 0;
  return rowSectionIds.includes(selected);
}

export function nextRowIndex(current, direction, visibility) {
  const visible = visibility
    .map((shown, index) => (shown ? index : -1))
    .filter((index) => index >= 0);
  if (visible.length === 0) return -1;
  const position = visible.indexOf(current);
  const start = position < 0 ? 0 : position;
  return visible[(start + direction + visible.length) % visible.length];
}

export function searchMatches(value, query) {
  return value.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase());
}

export function activationKey(key) {
  return key === "Enter" || key === " ";
}

export function compareCatalogRows(left, right, key, direction) {
  const sign = direction === "asc" ? 1 : -1;
  let order;
  if (key === "count") order = left.count - right.count;
  else if (key === "rate") order = left.rate - right.rate;
  else order = left.form.localeCompare(right.form, "ja");
  // Ties fall to the label, so a rate order is total and a re-sort under a
  // surface filter cannot shuffle equal rows about.
  if (order === 0 && key !== "form") {
    return left.form.localeCompare(right.form, "ja");
  }
  return order * sign;
}

/**
 * Tenths of one occurrence per million sentences.
 *
 * The same arithmetic as `pattern_detail.rs`'s `rate_tenths`, including its
 * half-up rounding, so a value recomputed here after a surface filter matches
 * the one the server rendered before it.
 */
export function rateTenths(count, universe) {
  if (!universe) return 0;
  return Math.floor((count * 10000000 + Math.floor(universe / 2)) / universe);
}

export function rateLabel(tenths) {
  return `${Math.floor(tenths / 10)}.${tenths % 10}`;
}

export function emptyHitState() {
  return {
    selectedOrdinal: null,
    expandedOrdinal: null,
    inspectorOrdinal: null,
  };
}

export function transitionHitState(state, action) {
  if (action.type === "filter-reset") {
    return { state: emptyHitState(), requestOrdinal: null };
  }
  if (action.type === "inspector-loaded") {
    return {
      state: { ...state, inspectorOrdinal: action.ordinal },
      requestOrdinal: null,
    };
  }
  if (action.type === "close-inspector") {
    return {
      state: { ...state, selectedOrdinal: null, inspectorOrdinal: null },
      requestOrdinal: null,
    };
  }
  if (action.type === "escape") {
    if (action.focusedOnExpansion && state.expandedOrdinal === action.ordinal) {
      return {
        state: { ...state, expandedOrdinal: null },
        requestOrdinal: null,
      };
    }
    return {
      state: { ...state, selectedOrdinal: null, inspectorOrdinal: null },
      requestOrdinal: null,
    };
  }
  if (action.type === "toggle-expansion") {
    return {
      state: {
        ...state,
        expandedOrdinal: state.expandedOrdinal === action.ordinal
          ? null
          : action.ordinal,
      },
      requestOrdinal: null,
    };
  }
  if (action.type === "select") {
    return {
      state: { ...state, selectedOrdinal: action.ordinal },
      requestOrdinal: action.ordinal,
    };
  }
  return { state, requestOrdinal: null };
}

export function isOverflowing(scrollWidth, clientWidth) {
  return scrollWidth > clientWidth + 1;
}

export function groupLabelNode(document, label, exactTotalLabel) {
  const heading = document.createElement("h3");
  const name = document.createElement("span");
  name.textContent = label;
  const count = document.createElement("span");
  count.className = "pattern-group-count";
  count.textContent = exactTotalLabel;
  heading.append(name, count);
  return heading;
}

export function groupCatalog(rows, field) {
  const groups = new Map();
  for (const row of rows) {
    const metadata = [...row.querySelectorAll("[data-group-field]")].find((item) => item.dataset.groupField === field);
    const key = metadata?.dataset.groupKey ?? "missing";
    const label = metadata?.dataset.groupLabel ?? "Unclassified";
    if (!groups.has(key)) groups.set(key, { key, label, exactTotal: 0, rows: [] });
    const group = groups.get(key);
    group.exactTotal += Number(row.dataset.exactTotal);
    group.rows.push(row);
  }
  return [...groups.values()].sort((a, b) => b.exactTotal - a.exactTotal || a.label.localeCompare(b.label, "ja"));
}

export function beginDetailRequest(target) {
  target.setAttribute("aria-busy", "true"); target.dataset.loading = "true";
  for (const child of target.children) child.inert = true;
}
export function finishDetailRequest(target) {
  target.setAttribute("aria-busy", "false"); delete target.dataset.loading;
  for (const child of target.children) child.inert = false;
}
export function failDetailRequest(target) {
  finishDetailRequest(target);
  let status = target.querySelector(":scope > .detail-load-status");
  if (!status) { status = document.createElement("p"); status.className = "detail-load-status alert alert-error"; target.prepend(status); }
  status.textContent = t("status.detail_failed");
}
export function beginCorpusRequest() {
  const status = document.getElementById("search-status");
  status.replaceChildren(Object.assign(document.createElement("p"), { textContent: t("status.submitting") }));
  document.getElementById("results")?.setAttribute("aria-busy", "true");
}
export function finishCorpusRequest(target) {
  target.setAttribute("aria-busy", "false");
  // A new revision invalidates every captured per-pattern place.
  patternUiState.clear();
}
export function failCorpusRequest() {
  const status = document.getElementById("search-status");
  status.replaceChildren(Object.assign(document.createElement("p"), { className: "alert alert-error", textContent: t("status.submit_failed") }));
  document.getElementById("results")?.setAttribute("aria-busy", "false");
}

function beginSampleRequest(form) {
  form.querySelector("[data-sample-status]").textContent = t("sample.running");
  document.getElementById("sample-results")?.setAttribute("aria-busy", "true");
}

function finishSampleRequest(target) {
  target.setAttribute("aria-busy", "false");
  const status = target.closest("[data-sample-workspace]")
    ?.querySelector("[data-sample-status]");
  if (status) status.textContent = "";
}

function failSampleRequest(form) {
  form.querySelector("[data-sample-status]").textContent = t("sample.run_failed");
  document.getElementById("sample-results")?.setAttribute("aria-busy", "false");
}

function renderGroupedCatalog(tbody, field) {
  const rows = [...tbody.querySelectorAll("tr[data-authored-index]")];
  const fragment = document.createDocumentFragment();
  for (const group of groupCatalog(rows, field)) {
    group.rows.sort((a, b) => Number(b.dataset.exactTotal) - Number(a.dataset.exactTotal) || Number(a.dataset.authoredIndex) - Number(b.dataset.authoredIndex));
    const header = document.createElement("tr");
    header.className = "pattern-group-header";
    const cell = document.createElement("th");
    // Two visible columns (pattern name, corpus matches); the metadata cell
    // is `hidden`, so spanning 3 would push the group count past the
    // right-hand count column.
    cell.colSpan = 2;
    const formatted = new Intl.NumberFormat().format(group.exactTotal);
    cell.append(groupLabelNode(document, group.label, formatted));
    header.append(cell);
    fragment.append(header, ...group.rows);
  }
  tbody.replaceChildren(fragment);
}

const hitControllers = new WeakMap();
const FILTER_GROUPS = {
  surface: {
    source: ".surface-filter", controls: ".surface-filter, [data-quick-variation]",
    value: "variation", quickValue: "quickVariation", label: "[lang=ja]",
  },
  section: {
    source: ".section-filter", controls: ".section-filter, [data-quick-section]",
    value: "section", quickValue: "quickSection", label: ".section-label",
  },
  occurrence: { source: ".occurrence-filter", controls: ".occurrence-filter", value: "occurrence" },
};

function filterValue(button, group) {
  return button.dataset[group.value] ?? button.dataset[group.quickValue];
}
const rowInteractiveSelector = [
  "button",
  "a",
  "input",
  "select",
  "textarea",
  "[data-capture-target]",
  "[data-copy-text]",
].join(",");

// The inspector is a persistent dock owned by the results explorer, not a
// table row: opening a hit never moves the KWIC list. The dock shell lives
// in results_explorer.html; rows target #inspector-body via htmx.
export function inspectorDock(root) {
  const explorer = root?.closest?.(".results-explorer") ?? null;
  if (!explorer) return null;
  const dock = explorer.querySelector("#inspector-dock");
  const body = explorer.querySelector("#inspector-body");
  if (!dock || !body) return null;
  return {
    explorer,
    dock,
    body,
    title: explorer.querySelector("[data-inspector-title]"),
  };
}

export function inspectorTitle(ordinal) {
  return t("hit.retained", { n: Number(ordinal) + 1 });
}

const WIDE_QUERY = "(min-width: 64rem)";

// Terminal SSE event names after which the stream is finished: the
// app.js "connection lost" notice is a transport-level aid and must not
// stack under (or behind) a terminal server message. Keyed on the event
// *name* (htmx:sseMessage's detail is the raw MessageEvent) rather than on
// markup, so every emitter of a terminal frame is covered — the revision-
// status error frame and the "Request not found" frame alike.
const FINISHED_SSE_EVENTS = new Set(["result", "error"]);

export function isFinishedSseEvent(eventName) {
  return FINISHED_SSE_EVENTS.has(eventName);
}

class HitRequest {
  #state = emptyHitState();
  #committedState = emptyHitState();
  #pendingOrdinal = null;

  get state() { return this.#state; }
  get pendingOrdinal() { return this.#pendingOrdinal; }

  transition(action) {
    const result = transitionHitState(this.#state, action);
    this.#state = result.state;
  }

  select(ordinal) {
    this.transition({ type: "select", ordinal });
    this.#pendingOrdinal = ordinal;
  }

  close() {
    this.transition({ type: "close-inspector" });
    this.#committedState = this.#state;
    this.#pendingOrdinal = null;
  }

  commit(ordinal) {
    if (ordinal !== this.#pendingOrdinal) return false;
    this.transition({ type: "inspector-loaded", ordinal });
    this.#committedState = this.#state;
    this.#pendingOrdinal = null;
    return true;
  }

  rollback(ordinal) {
    if (ordinal !== this.#pendingOrdinal) return false;
    this.#state = this.#committedState;
    this.#pendingOrdinal = null;
    return true;
  }

  showFailure(ordinal) { this.#state = { ...this.#state, selectedOrdinal: ordinal }; }
}

class KwicProjection {
  #grid;
  #filters;
  #pageIndex = 0;
  #pageSize;
  #filteredRows = [];

  constructor(grid) {
    this.#grid = grid;
    this.#pageSize = grid.matches("[data-kwic-pagination]")
      ? loadKwicPageSize()
      : Math.max(1, grid.querySelectorAll(".kwic-row").length);
    this.#filters = Object.fromEntries(Object.entries(FILTER_GROUPS).map(([kind, group]) => [
      kind,
      grid.closest(".pattern-detail")?.querySelector(`${group.source}[aria-pressed="true"]`)
        ?.dataset[group.value] ?? "all",
    ]));
  }

  get filters() { return this.#filters; }
  get pageIndex() { return this.#pageIndex; }
  get pageSize() { return this.#pageSize; }

  setFilter(kind, value) {
    this.#filters = { ...this.#filters, [kind]: value };
    this.resetPage();
  }

  resetPage() { this.#pageIndex = 0; }

  restore(filters, pageIndex) {
    this.#filters = { ...filters };
    this.#pageIndex = pageIndex;
  }

  changePage(action) {
    const pageCount = Math.max(1, Math.ceil(this.#filteredRows.length / this.#pageSize));
    const next = transitionPageState(
      { pageIndex: this.#pageIndex, pageSize: this.#pageSize },
      { pageCount, ...action },
    );
    this.#pageIndex = next.pageIndex;
    this.#pageSize = next.pageSize;
  }

  firstRow() { return this.#filteredRows[this.#pageIndex * this.#pageSize]; }

  project() {
    const grid = this.#grid;
    const detail = grid.closest(".pattern-detail");
    const { surface, section, occurrence } = this.#filters;
    const query = detail.querySelector("[data-kwic-search]")?.value.trim() ?? "";
    const originalRows = [...detail.querySelectorAll(".kwic-row")];
    const rows = sortKwicRows(originalRows,
      detail.querySelector("[data-kwic-sort]")?.value ?? "original",
      detail.querySelector("[data-kwic-sort-direction]")?.value === "descending");
    if (rows.some((row, index) => row !== originalRows[index])) grid.tBodies[0].append(...rows);
    this.#filteredRows = rows.filter(row => {
      const sectionIds = (row.dataset.sectionIds ?? "").split(" ").filter(Boolean);
      const text = query === "" ? "" : (row.querySelector(".kwic-sentence")?.textContent ?? "");
      return visibleForVariation(row.dataset.variation, surface)
        && visibleForSection(sectionIds, section)
        && visibleForOccurrence(row.dataset.inChain, occurrence)
        && visibleForQuery(text, query);
    });
    const page = pageProjection(this.#filteredRows, this.#pageIndex, this.#pageSize);
    this.#pageIndex = page.pageIndex;
    return { rows, filteredRows: this.#filteredRows, page, textQuery: query };
  }
}

class InspectorMount {
  #grid;
  #inlineRow = null;
  #committedContent = null;

  constructor(grid) { this.#grid = grid; }

  ensureInlineRow() {
    if (this.#inlineRow) return this.#inlineRow;
    const row = document.createElement("tr");
    row.className = "kwic-inspector-row";
    row.hidden = true;
    row.setAttribute("role", "row");
    const cell = document.createElement("td");
    cell.setAttribute("role", "gridcell");
    cell.colSpan = this.#grid.tHead?.rows[0]?.cells.length ?? 1;
    const header = document.createElement("header");
    header.className = "kwic-inspector-header";
    const title = document.createElement("h3");
    title.className = "kwic-inspector-title";
    const close = document.createElement("button");
    close.type = "button";
    close.className = "inspector-dock-close";
    close.dataset.inspectorClose = "";
    close.setAttribute("aria-label", "Close inspector");
    close.textContent = "×";
    header.append(title, close);
    cell.append(header);
    row.append(cell);
    this.#inlineRow = { row, cell, title };
    return this.#inlineRow;
  }

  title(ordinal) {
    const parts = inspectorDock(this.#grid);
    if (!parts) return;
    const label = inspectorTitle(ordinal);
    if (parts.title) parts.title.textContent = label;
    if (this.#inlineRow) this.#inlineRow.title.textContent = label;
  }

  open(selectedRow) {
    const parts = inspectorDock(selectedRow);
    if (!parts) return;
    const ordinal = selectedRow.dataset.hitOrdinal;
    if (window.matchMedia(WIDE_QUERY).matches) {
      this.#inlineRow?.row.remove();
      if (parts.body.parentElement !== parts.dock) parts.dock.append(parts.body);
      parts.dock.hidden = false;
      parts.explorer.classList.add("inspector-open");
    } else {
      parts.dock.hidden = true;
      parts.explorer.classList.remove("inspector-open");
      const inline = this.ensureInlineRow();
      selectedRow.after(inline.row);
      inline.row.hidden = false;
      if (parts.body.parentElement !== inline.cell) inline.cell.append(parts.body);
    }
    this.title(ordinal);
    parts.body.setAttribute("aria-label", `${inspectorTitle(ordinal)} details`);
  }

  close() {
    const parts = inspectorDock(this.#grid);
    if (parts) {
      if (parts.body.parentElement !== parts.dock) parts.dock.append(parts.body);
      parts.dock.hidden = true;
      parts.explorer.classList.remove("inspector-open");
      parts.body.setAttribute("aria-busy", "false");
    }
    this.#inlineRow?.row.remove();
  }

  clear() {
    this.#committedContent = null;
    this.close();
  }

  busy(selectedRow) {
    const parts = inspectorDock(selectedRow);
    if (!parts) return;
    if (!this.#committedContent && parts.body.querySelector(".hit-detail")) {
      this.#committedContent = document.createDocumentFragment();
      this.#committedContent.append(...parts.body.childNodes);
    }
    this.open(selectedRow);
    const loading = document.createElement("p");
    loading.className = "inspector-loading";
    loading.textContent = t("inspector.loading_hit");
    parts.body.replaceChildren(loading);
    parts.body.setAttribute("aria-busy", "true");
    parts.body.scrollTop = 0;
  }

  commit(target) {
    this.#committedContent = null;
    target.setAttribute("aria-busy", "false");
    for (const child of target.children) child.inert = false;
  }

  rollback(ordinal, failedRow) {
    const parts = inspectorDock(this.#grid);
    const committedRow = ordinal === null
      ? null
      : this.#grid.querySelector(`.kwic-row[data-hit-ordinal="${ordinal}"]`);
    const restored = committedRow && this.#committedContent;
    if (restored) {
      parts.body.replaceChildren(this.#committedContent);
      this.#committedContent = null;
      this.title(ordinal);
    } else {
      const message = document.createElement("p");
      message.className = "inline-inspector-error";
      message.textContent = t("inspector.load_failed");
      const retry = document.createElement("button");
      retry.type = "button";
      retry.className = "btn btn-secondary";
      retry.dataset.retryHit = failedRow.dataset.hitOrdinal;
      retry.textContent = t("inspector.retry_hit");
      parts.body.replaceChildren(message, retry);
      this.title(failedRow.dataset.hitOrdinal);
    }
    parts.body.setAttribute("aria-busy", "false");
    return Boolean(restored);
  }

  follow(row) {
    if (this.#inlineRow && !this.#inlineRow.row.hidden) row.after(this.#inlineRow.row);
  }

  dispose() {
    this.#committedContent = null;
    const parts = inspectorDock(this.#grid);
    if (parts && parts.body.parentElement !== parts.dock) parts.dock.append(parts.body);
    this.#inlineRow?.row.remove();
  }
}

function remountOpenInspectors() {
  for (const explorer of document.querySelectorAll(".results-explorer")) {
    const selectedRow = explorer.querySelector(
      '.kwic-row[aria-selected="true"]',
    );
    if (!selectedRow) continue;
    const grid = selectedRow.closest("[data-hit-grid]");
    const controller = grid ? hitControllers.get(grid) : null;
    if (controller) controller.mount.open(selectedRow);
  }
}

function hideInspectorChrome(root) {
  const parts = inspectorDock(root);
  if (!parts) return;
  parts.dock.hidden = true;
  parts.explorer.classList.remove("inspector-open");
}

export function rowOverflows(row) {
  const text = row.querySelector("[data-expandable-text]");
  if (!text) return false;
  const wasExpanded = row.dataset.expanded === "true";
  if (wasExpanded) row.dataset.expanded = "false";
  const overflows = [...text.children]
    .some((segment) => isOverflowing(segment.scrollWidth, segment.clientWidth));
  if (wasExpanded) row.dataset.expanded = "true";
  row.dataset.overflow = String(overflows);
  const button = row.querySelector("[data-expand-sentence]");
  if (overflows) {
    if (button) button.hidden = false;
  } else {
    if (button) button.hidden = true;
  }
  return overflows;
}

export function applyHitState(grid, state) {
  for (const row of grid.querySelectorAll(".kwic-row")) {
    const ordinal = row.dataset.hitOrdinal;
    const selected = ordinal === state.selectedOrdinal;
    const expanded = ordinal === state.expandedOrdinal;
    row.setAttribute("aria-selected", String(selected));
    row.dataset.expanded = String(expanded);
    const button = row.querySelector("[data-expand-sentence]");
    if (button) {
      const label = expanded ? t("detail.show_clipped_sentence") : t("detail.show_full_sentence");
      button.setAttribute("aria-expanded", String(expanded));
      button.setAttribute("aria-label", label);
      button.setAttribute("title", label);
    }
  }
}

function queueMeasurement(controller, row) {
  controller.measureQueue.add(row);
  if (controller.measureFrame !== null) return;
  controller.measureFrame = requestAnimationFrame(() => {
    controller.measureFrame = null;
    for (const queued of controller.measureQueue) rowOverflows(queued);
    controller.measureQueue.clear();
  });
}

function observeNearRows(grid, controller) {
  for (const row of [...grid.querySelectorAll(".kwic-row")].slice(0, 20)) {
    controller.nearRows.add(row);
    queueMeasurement(controller, row);
  }
  if (typeof IntersectionObserver !== "undefined") {
    controller.observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          controller.nearRows.add(entry.target);
          queueMeasurement(controller, entry.target);
        } else {
          controller.nearRows.delete(entry.target);
        }
      }
    }, { rootMargin: "200px 0px" });
    for (const row of grid.querySelectorAll(".kwic-row")) {
      controller.observer.observe(row);
    }
  }
  if (typeof ResizeObserver !== "undefined") {
    controller.resizeObserver = new ResizeObserver(() => {
      for (const row of controller.nearRows) queueMeasurement(controller, row);
      const focused = grid.querySelector(".kwic-row:focus");
      const selected = grid.querySelector('.kwic-row[aria-selected="true"]');
      if (focused) queueMeasurement(controller, focused);
      if (selected) queueMeasurement(controller, selected);
    });
    controller.resizeObserver.observe(grid);
  }
}

function controllerFor(grid) {
  let controller = hitControllers.get(grid);
  if (controller) return controller;
  controller = {
    grid,
    selection: new HitRequest(),
    projection: new KwicProjection(grid),
    mount: new InspectorMount(grid),
    observer: null,
    resizeObserver: null,
    nearRows: new Set(),
    measureQueue: new Set(),
    measureFrame: null,
  };
  hitControllers.set(grid, controller);
  grid.closest(".pattern-detail")?.classList.add("results-enhanced");
  observeNearRows(grid, controller);
  return controller;
}

// Filter the pattern catalog rows by name; when a query is active the group
// headers go away so their (global) counts aren't mistaken for filtered ones.
function applyRailFilter(input) {
  const rows = input.closest(".pattern-rail")
    ?.querySelectorAll("#pattern-rows > tr[data-authored-index]");
  if (!rows) return;
  const query = input.value.trim().toLocaleLowerCase();
  for (const row of rows) {
    const name = row.querySelector(".pattern-selector")?.textContent ?? "";
    row.style.display =
      query === "" || name.toLocaleLowerCase().includes(query) ? "" : "none";
  }
  for (const header of input.closest(".pattern-rail")
    .querySelectorAll("#pattern-rows > .pattern-group-header")) {
    header.style.display = query === "" ? "" : "none";
  }
}

/**
 * Publishes the sticky control strip's height, so the concordance's own column
 * headers pin directly under it instead of behind it.
 *
 * Two sticky elements in one region do not stack by themselves: each is offset
 * from the scrollport, not from the other. The strip wraps at narrow widths and
 * its filter status is rewritten as filters change, so the height is observed
 * rather than assumed.
 */
const kwicHeaderSize =
  typeof ResizeObserver === "undefined"
    ? null
    : new ResizeObserver((entries) => {
        for (const entry of entries) {
          document.documentElement.style.setProperty(
            "--kwic-head-top",
            // Floor, not round: a fractional height rounded up leaves a
            // sub-pixel gap between the two sticky elements, and a scrolling
            // row is visible through it.
            `${Math.floor(entry.target.getBoundingClientRect().height)}px`,
          );
        }
      });

function trackKwicHeader(root = document) {
  if (!kwicHeaderSize) return;
  const header =
    root.querySelector?.(".kwic-pane-header") ??
    document.querySelector(".kwic-pane-header");
  kwicHeaderSize.disconnect();
  if (!header) {
    document.documentElement.style.setProperty("--kwic-head-top", "0px");
    return;
  }
  kwicHeaderSize.observe(header);
}

/**
 * The views one run offers, in tab order. Concordance is first because it is
 * what the edit→run loop is about: the charts answer questions asked once per
 * run, and stacking them above the table put ~600px of them between the editor
 * and the first hit.
 */
export const DETAIL_VIEWS = ["concordance", "sections", "clusters", "definition", "chains"];

/** What the reader chose, or `null` while they have chosen nothing.
 *
 *  The distinction is load-bearing. The catalog-level panels are on the page
 *  before `#detail-body` has been filled, so at first paint the only available
 *  view is `sections`; a plain "current selection" would be captured by that
 *  and the concordance would never become the default it is meant to be.
 *  A choice survives a pattern swap deliberately — switching pattern while
 *  reading the section chart should not throw the reader back to the table —
 *  and is not persisted across runs. */
let chosenDetailView = null;

/**
 * Which views the selection may fall on, given what is on the page.
 *
 * Panels live in two places — the catalog-level charts beside `#detail-body`,
 * the per-pattern ones inside it — and a run may produce neither dendrogram,
 * no sections at all, or a pattern whose definition the catalog no longer
 * holds. A tab with no panel is hidden rather than selectable-and-empty.
 */
export function availableDetailViews(present) {
  return DETAIL_VIEWS.filter((name) => present.includes(name));
}

/**
 * The view to show, given what the reader chose and what is available.
 *
 * `chosen` is `null` until the reader picks one, and the default is preferred
 * over whatever happens to be present — otherwise a transiently unavailable
 * concordance, which is the state of every page before its first detail
 * arrives, would permanently demote it.
 */
export function resolveDetailView(chosen, available) {
  for (const candidate of [chosen, DETAIL_VIEWS[0]]) {
    if (candidate !== null && available.includes(candidate)) return candidate;
  }
  return available[0] ?? null;
}

function applyDetailView() {
  const tabs = document.querySelector("[data-detail-tabs]");
  if (!tabs) return;
  const panels = new Map(
    DETAIL_VIEWS.map((name) => [
      name,
      [...document.querySelectorAll(`[data-detail-view="${name}"]`)],
    ]),
  );
  const available = availableDetailViews(
    DETAIL_VIEWS.filter((name) => panels.get(name).length > 0),
  );
  const shown = resolveDetailView(chosenDetailView, available);
  // A strip offering one view is chrome, not navigation.
  tabs.hidden = available.length < 2;
  for (const tab of tabs.querySelectorAll("[data-detail-tab]")) {
    const name = tab.dataset.detailTab;
    const selected = name === shown;
    tab.hidden = !available.includes(name);
    tab.setAttribute("aria-selected", String(selected));
    tab.tabIndex = selected ? 0 : -1;
  }
  for (const [name, views] of panels) {
    for (const view of views) view.hidden = name !== shown;
  }
}

function selectDetailView(name, { focus = false } = {}) {
  chosenDetailView = name;
  applyDetailView();
  if (!focus) return;
  document
    .querySelector(`[data-detail-tab="${name}"]`)
    ?.focus();
}

function initializeHitGrids(root = document) {
  trackKwicHeader(root);
  applyDetailView();
  if (root.matches?.("[data-hit-grid]")) {
    controllerFor(root);
    applyRowFilters(root.closest(".pattern-detail"));
  }
  for (const grid of root.querySelectorAll?.("[data-hit-grid]") ?? []) {
    controllerFor(grid);
    applyRowFilters(grid.closest(".pattern-detail"));
  }
}

function activateRow(row, { forceRequest = false } = {}) {
  if (row.closest("[inert]")) return;
  const grid = row.closest("[data-hit-grid]");
  if (!grid) return;
  const controller = controllerFor(grid);
  if (
    !forceRequest
    && controller.selection.state.selectedOrdinal === row.dataset.hitOrdinal
  ) {
    clearHitSelection(row.closest(".pattern-detail"));
    return;
  }
  controller.selection.select(row.dataset.hitOrdinal);
  applyHitState(grid, controller.selection.state);
  controller.mount.busy(row);
  row.dispatchEvent(new CustomEvent("hit-select"));
}

function clearHitSelection(detail) {
  const grid = detail?.querySelector("[data-hit-grid]");
  if (!grid) {
    hideInspectorChrome(detail);
    return;
  }
  const controller = controllerFor(grid);
  if (controller.selection.pendingOrdinal !== null) {
    grid.querySelector(`.kwic-row[data-hit-ordinal="${controller.selection.pendingOrdinal}"]`)
      ?.dispatchEvent(new Event("htmx:abort"));
  }
  controller.selection.close();
  applyHitState(grid, controller.selection.state);
  controller.mount.clear();
}

function detailOf(node) {
  return (
    node.closest(".pattern-detail")
    ?? document.querySelector(".pattern-detail[data-pattern-id]")
  );
}

function setFilter(detail, kind, value) {
  if (!detail) return;
  const controller = controllerFor(detail.querySelector("[data-hit-grid]"));
  controller.projection.setFilter(kind, value);
  applyRowFilters(detail);
}

function applyRowFilters(detail) {
  if (!detail) return;
  const grid = detail.querySelector("[data-hit-grid]");
  if (!grid) return;
  const controller = controllerFor(grid);
  const { surface: surfaceSelected, section: sectionSelected, occurrence: occurrenceSelected } = controller.projection.filters;
  for (const [kind, group] of Object.entries(FILTER_GROUPS)) {
    for (const button of detail.querySelectorAll(group.controls)) {
      button.setAttribute("aria-pressed", String(filterValue(button, group) === controller.projection.filters[kind]));
    }
  }
  const surfaceButton = detail.querySelector('.surface-filter[aria-pressed="true"]');
  const sectionButton = detail.querySelector('.section-filter[aria-pressed="true"]');
  const hasSectionDimension = detail.querySelector(".section-distribution") !== null;
  const { rows, filteredRows, page, textQuery } = controller.projection.project();
  applyFilterEmptyNote(
    detail,
    filteredRows.length,
    surfaceSelected,
    sectionSelected,
    textQuery,
  );
  applyOccurrenceEmptyNote(
    detail,
    filteredRows.length,
    occurrenceSelected,
    surfaceSelected,
    sectionSelected,
    textQuery,
  );
  const visibleSet = new Set(page.rows);
  for (const row of rows) row.hidden = !visibleSet.has(row);
  const selectedRow = controller.selection.state.selectedOrdinal === null
    ? null
    : grid.querySelector(
      `.kwic-row[data-hit-ordinal="${controller.selection.state.selectedOrdinal}"]`,
    );
  if (selectedRow && !visibleSet.has(selectedRow)) clearHitSelection(detail);
  else if (selectedRow) controller.mount.follow(selectedRow);
  rows.forEach((row) => { row.tabIndex = -1; });
  if (page.rows[0]) page.rows[0].tabIndex = 0;
  for (const pager of detail.querySelectorAll("[data-kwic-pager]")) {
    pager.querySelector("[data-page-previous]").disabled = page.pageIndex === 0;
    pager.querySelector("[data-page-next]").disabled =
      page.pageIndex + 1 >= page.pageCount;
    pager.querySelector("[data-page-status]").textContent =
      t("detail.page", { page: page.pageIndex + 1, pages: page.pageCount });
    pager.querySelector("[data-page-size]").value = String(controller.projection.pageSize);
  }
  const surfaceLabel = surfaceSelected === "all"
    ? null
    : surfaceButton?.querySelector("[lang=ja]")?.textContent ?? "";
  const sectionLabel = !hasSectionDimension
    ? undefined
    : sectionSelected === "all"
      ? null
      : sectionButton?.querySelector(".section-label")?.textContent ?? "";
  const status = detail.querySelector(".kwic-filter-status");
  if (status) {
    status.textContent = pageRangeLabel(
      page.start,
      page.end,
      filteredRows.length,
      surfaceLabel,
      sectionLabel,
      textQuery,
      occurrenceSelected,
    );
  }
  const clearSurface = detail.querySelector("[data-clear-surface-filter]");
  if (clearSurface) {
    clearSurface.hidden = surfaceSelected === "all";
    clearSurface.textContent = clearSurface.hidden
      ? t("detail.clear_surface")
      : t("detail.clear_surface_value", { name: surfaceLabel });
  }
  const clearSection = detail.querySelector("[data-clear-section-filter]");
  if (clearSection) {
    clearSection.hidden = sectionSelected === "all";
    clearSection.textContent = clearSection.hidden
      ? t("detail.clear_section")
      : t("detail.clear_section_value", { name: sectionLabel });
  }
  const clearQuery = detail.querySelector("[data-clear-kwic-search]");
  if (clearQuery) {
    clearQuery.hidden = textQuery === "";
    clearQuery.textContent = clearQuery.hidden
      ? t("detail.clear_query")
      : t("detail.clear_query_value", { query: textQuery });
  }
  // The scope note says which population the panel is describing, and the
  // panel narrows to the selection wherever the run counted it.
  const scopeNote = detail.querySelector("[data-section-scope-note]");
  if (scopeNote) scopeNote.hidden = surfaceSelected === "all";
  narrowSectionsToSurface(detail, surfaceSelected);
  const filterActive =
    surfaceSelected !== "all"
    || sectionSelected !== "all"
    || textQuery !== "";
  const concordanceTab = document.querySelector('[data-detail-tab="concordance"]');
  if (concordanceTab) {
    if (filterActive) concordanceTab.dataset.filterActive = "true";
    else delete concordanceTab.dataset.filterActive;
  }
  const viewSurface = detail.querySelector('[data-view-filtered-concordance="surface"]');
  if (viewSurface) {
    viewSurface.hidden = surfaceSelected === "all";
    viewSurface.textContent = t("detail.view_filtered_concordance", { count: filteredRows.length });
  }
  const viewSection = detail.querySelector('[data-view-filtered-concordance="section"]');
  if (viewSection) {
    viewSection.hidden = sectionSelected === "all";
    viewSection.textContent = t("detail.view_filtered_concordance", { count: filteredRows.length });
  }
}

/**
 * Narrows the section panel to the selected surface.
 *
 * The counts are corpus-exact per (pattern, surface, section) and ship with
 * the fragment, positionally aligned with the rendered rows and the
 * unlabelled row last. The rates are recomputed here rather than shipped: a
 * section's universe is what it always was, so only the numerator moved.
 *
 * A pattern the run refused to cross-tabulate carries no blob, and the panel
 * then keeps the whole pattern's counts and says so.
 */
function narrowSectionsToSurface(detail, surfaceSelected) {
  const list = detail.querySelector('[data-ranked-list="section"]');
  const blob = detail.querySelector("[data-surface-sections]");
  if (!list || !blob) return;
  let matrix;
  try {
    matrix = JSON.parse(blob.textContent);
  } catch (_error) {
    return;
  }
  const counts = surfaceSelected === "all" ? null : matrix[surfaceSelected];
  for (const row of rankedRows(list)) {
    // `data-matrix-index`, not the row's position: a previous filter may have
    // re-ranked the list, and reading the matrix positionally then attaches
    // one section's counts to another's row and denominator.
    const position = Number(row.dataset.matrixIndex);
    // `data-full-count` is the invariant the server rendered; `data-count` is
    // what the panel is currently showing, which every other reader of this
    // list already treats as the live value.
    const count = counts
      ? (counts[position] ?? 0)
      : Number(row.dataset.fullCount);
    const universe = Number(row.dataset.universe);
    const tenths = rateTenths(count, universe);
    row.dataset.count = String(count);
    row.dataset.rateTenths = String(tenths);
    const countLabel = row.querySelector("[data-count-label]");
    if (countLabel) countLabel.textContent = String(count);
    const rateSlot = row.querySelector("[data-rate-label]");
    if (rateSlot) rateSlot.textContent = `${rateLabel(tenths)} ppm`;
  }
  // Re-ranked, because a distribution that is not in order is not one. The
  // list keeps whatever order the reader chose; a list with no control keeps
  // the order it was rendered in.
  reorderRankedList(list, rankedOrder(list));
}

function applyFilter(button, kind) {
  const detail = detailOf(button);
  if (!detail) return;
  const value = filterValue(button, FILTER_GROUPS[kind]);
  const selected = controllerFor(detail.querySelector("[data-hit-grid]")).projection.filters[kind];
  if (kind === "occurrence" && selected === value) return;
  setFilter(detail, kind, kind !== "occurrence" && selected === value ? "all" : value);
}

function applyRankedList(list) {
  const limit = Number(list.dataset.rankedLimit);
  const expanded = list.dataset.rankedExpanded === "true";
  const search = list.querySelector("[data-ranked-search]");
  const query = search?.value.trim() ?? "";
  const searchActive = query !== "";
  const entries = [...list.querySelectorAll("[data-ranked-entry]")];
  entries.forEach((entry, index) => {
    entry.dataset.rankedIndex = String(index);
    entry.hidden = !rankedEntryVisible(
      index,
      limit,
      expanded,
      searchMatches(entry.dataset.searchValue, query),
      searchActive,
    );
  });
  list.querySelector("[data-ranked-tail]")?.toggleAttribute(
    "hidden",
    expanded || searchActive,
  );
  list.querySelector("[data-ranked-toggle]")?.toggleAttribute("hidden", searchActive);
}

function updateRankedTail(list) {
  const entries = [...list.querySelectorAll("[data-ranked-entry]")];
  const limit = Number(list.dataset.rankedLimit);
  const tail = list.querySelector("[data-ranked-tail]");
  // Bars first, and unconditionally: a list short enough to have no tail still
  // has bars, and they still move when a surface filter narrows the counts.
  renderRankedBars(list);
  if (!tail) return;
  const hidden = rankedTail(
    entries.map((entry) => ({ count: Number(entry.dataset.count) })),
    limit,
  );
  tail.textContent = rankedTailSummary(
    hidden.entryCount,
    hidden.hitCount,
    Number(detailOf(list)?.dataset.retainedCount ?? 0),
  );
}

/**
 * Scales every bar in one ranked list against the largest value of whatever
 * that list is ranked on.
 *
 * The section panel is a distribution over sections, so its bar is the rate
 * and not the count: a section holding ten times the sentences collects ten
 * times the hits from a pattern with no preference at all, and a count bar
 * reports that rather than anything about the pattern.
 */
function renderRankedBars(list) {
  const rows = rankedRows(list);
  const maximum = namedMaximum(rows.map((row) => ({ count: rankedMetric(list, row) })));
  for (const row of rows) {
    const share = `${rankedMetric(list, row) * 100 / maximum}%`;
    const fill = row.querySelector(".surface-bar-fill");
    if (fill) fill.style.inlineSize = share;
  }
}

/** Every drawn row, the unlabelled section included. */
function rankedRows(list) {
  return [
    ...list.querySelectorAll("[data-ranked-entry]"),
    ...list.querySelectorAll(".section-none[data-count]"),
  ];
}

function rankedMetric(list, row) {
  return list.dataset.rankedList === "section"
    ? Number(row.dataset.rateTenths)
    : Number(row.dataset.count);
}

/** The order a list is in, whether or not it offers a control to change it. */
function rankedOrder(list) {
  const select = list.querySelector("[data-ranked-order]");
  if (select) return select.value;
  return list.dataset.rankedList === "section" ? "rate-desc" : "count-desc";
}

function reorderRankedList(list, value) {
  const body = list?.querySelector("ol");
  if (!body) return;
  const [key, direction] = value.split("-");
  const entries = [...body.querySelectorAll("[data-ranked-entry]")];
  const row = (entry) => ({
    count: Number(entry.dataset.count),
    rate: Number(entry.dataset.rateTenths),
    form: entry.dataset.searchValue,
  });
  const field = key === "name" ? "form" : key === "rate" ? "rate" : "count";
  entries.sort((left, right) =>
    compareCatalogRows(row(left), row(right), field, direction));
  // Before the tail summary, or before the unlabelled row when a list is short
  // enough to have no tail. `insertBefore(entry, null)` appends, which on a
  // short section list would move every ranked row past `(no section)` and
  // leave the unlabelled bucket at the top.
  const anchor = body.querySelector("[data-ranked-tail]")
    ?? body.querySelector(".section-none");
  for (const entry of entries) body.insertBefore(entry, anchor);
  applyRankedList(list);
  updateRankedTail(list);
}

function sortRankedList(select) {
  reorderRankedList(select.closest("[data-ranked-list]"), select.value);
}

function finishHitRequest(target) {
  const explorer = target.closest(".results-explorer");
  const grid = explorer?.querySelector("[data-hit-grid]");
  const rendered = target.querySelector(".hit-detail[data-reference-ordinal]");
  if (!grid || !rendered) return;
  const controller = controllerFor(grid);
  const ordinal = rendered.dataset.referenceOrdinal;
  if (!controller.selection.commit(ordinal)) return;
  controller.mount.commit(target);
  const firstMatchToken =
    rendered.querySelector(".kwic-token-ribbon .kwic-token-match")
    ?? rendered.querySelector(".kwic-token-ribbon .kwic-token");
  if (
    firstMatchToken
    && !rendered.querySelector('.kwic-token[aria-pressed="true"]')
  ) {
    firstMatchToken.click();
  }
}

function failHitRequest(row) {
  const grid = row.closest("[data-hit-grid]");
  const parts = inspectorDock(row);
  if (!grid || !parts) return;
  const controller = controllerFor(grid);
  if (!controller.selection.rollback(row.dataset.hitOrdinal)) return;
  applyHitState(grid, controller.selection.state);
  if (!controller.mount.rollback(controller.selection.state.inspectorOrdinal, row)) {
    controller.selection.showFailure(row.dataset.hitOrdinal);
    applyHitState(grid, controller.selection.state);
  }
}

function disposeHitGrid(grid) {
  const controller = hitControllers.get(grid);
  if (!controller) return;
  if (controller.selection.pendingOrdinal !== null) {
    grid.querySelector(`.kwic-row[data-hit-ordinal="${controller.selection.pendingOrdinal}"]`)
      ?.dispatchEvent(new Event("htmx:abort"));
  }
  controller.observer?.disconnect();
  controller.resizeObserver?.disconnect();
  if (controller.measureFrame !== null) cancelAnimationFrame(controller.measureFrame);
  controller.mount.dispose();
  hitControllers.delete(grid);
}

function capturePatternUi(detailBody) {
  const detail = detailBody?.querySelector(".pattern-detail[data-pattern-id]");
  if (!detail) return;
  const grid = detail.querySelector("[data-hit-grid]");
  const controller = grid ? hitControllers.get(grid) : null;
  patternUiState.set(detail.dataset.patternId, {
    filters: { ...controller?.projection.filters },
    query: detail.querySelector("[data-kwic-search]")?.value ?? "",
    sort: detail.querySelector("[data-kwic-sort]")?.value ?? "original",
    sortDirection: detail.querySelector("[data-kwic-sort-direction]")?.value ?? "ascending",
    pageIndex: controller?.projection.pageIndex ?? 0,
    selectedOrdinal: controller?.selection.state.selectedOrdinal ?? null,
    scrollY: window.scrollY,
  });
}

function restorePatternUi(detailBody) {
  const detail = detailBody.querySelector(".pattern-detail[data-pattern-id]");
  if (!detail) return;
  const saved = patternUiState.get(detail.dataset.patternId);
  if (!saved) {
    requestAnimationFrame(() =>
      (detail.closest(".detail-panel") ?? detail.querySelector(".kwic-pane-header"))
        ?.scrollIntoView({ block: "start" }),
    );
    return;
  }
  const grid = detail.querySelector("[data-hit-grid]");
  if (!grid) return;
  const controller = controllerFor(grid);
  controller.projection.restore(saved.filters, saved.pageIndex);
  const searchInput = detail.querySelector("[data-kwic-search]");
  if (searchInput) searchInput.value = saved.query ?? "";
  const sort = detail.querySelector("[data-kwic-sort]");
  const direction = detail.querySelector("[data-kwic-sort-direction]");
  if (sort) sort.value = saved.sort ?? "original";
  if (direction) direction.value = saved.sortDirection ?? "ascending";
  applyRowFilters(detail);
  requestAnimationFrame(() => window.scrollTo({ top: saved.scrollY }));
  if (saved.selectedOrdinal !== null) {
    const row = grid.querySelector(
      `.kwic-row[data-hit-ordinal="${CSS.escape(saved.selectedOrdinal)}"]`,
    );
    if (row && !row.hidden) activateRow(row, { forceRequest: true });
  }
}

function orientDendrogram(view, orientation) {
  const svg = view.querySelector(".section-dendrogram");
  const { width, height } = svg.dataset;
  const vertical = orientation === "vertical";
  svg.setAttribute("viewBox", `0 0 ${vertical ? height : width} ${vertical ? width : height}`);
  svg.setAttribute("width", vertical ? height : width);
  svg.setAttribute("height", vertical ? width : height);
  const geometry = svg.querySelector("[data-dendrogram-geometry]");
  if (vertical) geometry.setAttribute("transform", `translate(0 ${width}) rotate(-90)`);
  else geometry.removeAttribute("transform");
}

if (typeof document !== "undefined") {
  // Locale flip: re-render the strings this module owns (pager, filter
  // status, clear-filter chips, controller-driven inspector titles).
  onLocaleChange(() => {
    for (const grid of document.querySelectorAll("[data-hit-grid]")) {
      applyRowFilters(grid.closest(".pattern-detail"));
      const controller = hitControllers.get(grid);
      const ordinal = controller?.selection.state?.selectedOrdinal;
      if (ordinal !== null && ordinal !== undefined) {
        controller.mount.title(ordinal);
      }
    }
  });
  document.addEventListener("DOMContentLoaded", () => {
    initializeHitGrids();
    document.fonts?.ready.then(() => {
      for (const grid of document.querySelectorAll("[data-hit-grid]")) {
        const controller = controllerFor(grid);
        for (const row of controller.nearRows) queueMeasurement(controller, row);
      }
    });
  });
  document.addEventListener("htmx:load", (event) => initializeHitGrids(event.detail.elt));

  document.addEventListener("htmx:beforeRequest", (event) => {
    const form = event.detail.elt.closest?.("form[data-corpus-search]");
    if (form && event.detail.elt.closest("form") === form) beginCorpusRequest(form);
    const sampleForm = event.detail.elt.closest?.(".sample-form");
    if (sampleForm && event.detail.elt.closest("form") === sampleForm) {
      beginSampleRequest(sampleForm);
    }
    if (event.detail.elt.closest?.(".pattern-selector")) {
      const detailBody = document.getElementById("detail-body");
      capturePatternUi(detailBody);
      hideInspectorChrome(detailBody);
      detailBody?.querySelectorAll("[data-hit-grid]").forEach((grid) => {
        const controller = hitControllers.get(grid);
        if (controller) controller.mount.close();
        disposeHitGrid(grid);
      });
      if (detailBody) beginDetailRequest(detailBody);
    }
  });

  document.addEventListener("htmx:beforeSwap", (event) => {
    if (event.detail.target?.matches?.(".reading-body") && event.detail.xhr?.status >= 400) {
      event.detail.shouldSwap = true;
    }
  });

  document.addEventListener("htmx:sendError", (event) => {
    if (event.detail.target?.matches?.(".reading-body")) {
      event.detail.target.textContent = t("reading.failed");
    }
  });

  document.addEventListener("htmx:afterSwap", (event) => {
    if (event.detail.target?.id === "detail-body") {
      finishDetailRequest(event.detail.target);
      restorePatternUi(event.detail.target);
    }
    if (event.detail.target?.id === "sample-results") finishSampleRequest(event.detail.target);
    if (event.detail.target?.id === "results") finishCorpusRequest(event.detail.target);
    if (event.detail.target?.matches?.("#inspector-body")) finishHitRequest(event.detail.target);
    if (event.detail.target?.matches?.("[data-section-analysis]")) applyDetailView();
    initializeHitGrids(event.detail.target);
  });

  // The terminal "result" SSE event swaps #results OUT OF BAND (stream.rs), and
  // htmx fires only htmx:oobAfterSwap for that — never htmx:afterSwap with
  // #results as the target. So the handler above never saw the one swap that
  // ends a corpus search, and the busy state `beginCorpusRequest` set was never
  // cleared. That was invisible while nothing was drawn for `aria-busy`; it is
  // a bar that never stops once something is.
  document.addEventListener("htmx:oobAfterSwap", (event) => {
    if (event.detail?.target?.id === "results") finishCorpusRequest(event.detail.target);
  });

  for (const name of ["htmx:responseError", "htmx:sendError"]) {
    document.addEventListener(name, (event) => {
      if (event.detail.elt.closest?.("form[data-corpus-search]")) failCorpusRequest();
      const sampleForm = event.detail.elt.closest?.(".sample-form");
      if (sampleForm) failSampleRequest(sampleForm);
      const target = document.getElementById("detail-body");
      if (event.detail.elt.closest?.(".pattern-selector") && target) {
        failDetailRequest(target);
      }
      const row = event.detail.elt.closest?.(".kwic-row");
      if (row) failHitRequest(row);
    });
  }

  document.addEventListener("change", (event) => {
    if (event.target.matches("[data-dendrogram-orientation]")) {
      orientDendrogram(event.target.closest("[data-dendrogram-view]"), event.target.value);
    }
  });

  document.addEventListener("fullscreenchange", () => {
    for (const button of document.querySelectorAll("[data-dendrogram-fullscreen]")) {
      button.setAttribute("aria-pressed", String(
        document.fullscreenElement === button.closest("[data-dendrogram-view]"),
      ));
    }
  });

  document.addEventListener("click", (event) => {
    const fullscreen = event.target.closest("[data-dendrogram-fullscreen]");
    if (fullscreen) {
      const view = fullscreen.closest("[data-dendrogram-view]");
      const status = view.querySelector("[data-dendrogram-status]");
      status.textContent = "";
      (async () => {
        try {
          if (document.fullscreenElement === view) await document.exitFullscreen();
          else await view.requestFullscreen();
        } catch (_error) {
          status.textContent = t("results.cluster_fullscreen_unavailable");
        }
      })();
      return;
    }
    const reading = event.target.closest("[data-reading-open]");
    if (reading) {
      const dialog = reading.closest(".hit-detail")?.querySelector(".reading-dialog");
      if (dialog && !dialog.open) dialog.showModal();
      return;
    }
    const tab = event.target.closest("[data-detail-tab]");
    if (tab) {
      selectDetailView(tab.dataset.detailTab);
      return;
    }

    const viewFiltered = event.target.closest("[data-view-filtered-concordance]");
    if (viewFiltered) {
      selectDetailView("concordance", { focus: true });
      return;
    }

    const selectPattern = event.target.closest("[data-select-pattern]");
    if (selectPattern) {
      const patternId = selectPattern.dataset.selectPattern;
      const selector = document.querySelector(
        `.pattern-selector[hx-get$="/${CSS.escape(patternId)}"]`,
      );
      if (selector) {
        selector.click();
        selectDetailView("concordance");
      }
      return;
    }

    const copyDef = event.target.closest("[data-copy-definition]");
    if (copyDef) {
      const pane = copyDef.closest(".definition-pane");
      const code = pane?.querySelector("code");
      if (code && navigator.clipboard) {
        void navigator.clipboard.writeText(code.textContent ?? "").then(() => {
          const span = copyDef.querySelector("span") ?? copyDef;
          span.textContent = t("detail.copied_definition");
          setTimeout(() => {
            span.textContent = t("detail.copy_definition");
          }, 1500);
        });
      }
      return;
    }

    const jumpDef = event.target.closest("[data-jump-definition]");
    if (jumpDef) {
      const name = jumpDef.dataset.jumpDefinition;
      const textarea = document.getElementById("corpus-pattern");
      document.dispatchEvent(new CustomEvent("lingrep-web:query-restored"));
      if (textarea && name) {
        void import("./editor/pattern-editor.js")
          .then((mod) => mod.jumpToDefinitionByName(textarea, name))
          .catch(() => textarea.focus());
      }
      return;
    }

    const sampleToken = event.target.closest("[data-sample-token]");
    if (sampleToken) {
      const container = sampleToken.closest(".sample-results");
      if (!container) return;
      const tokenIndex = sampleToken.dataset.sampleToken;
      const wasPressed = sampleToken.getAttribute("aria-pressed") === "true";
      container.querySelectorAll("[data-sample-token]").forEach((btn) => {
        btn.setAttribute(
          "aria-pressed",
          String(!wasPressed && btn.dataset.sampleToken === tokenIndex),
        );
      });
      container.querySelectorAll(".sample-token-detail").forEach((panel) => {
        panel.hidden = wasPressed || panel.id !== `sample-token-${tokenIndex}`;
      });
      return;
    }

    const inspectorClose = event.target.closest("[data-inspector-close]");
    if (inspectorClose) {
      const explorer = inspectorClose.closest(".results-explorer");
      const selected = explorer?.querySelector(
        '.kwic-row[aria-selected="true"]',
      );
      clearHitSelection(explorer);
      // Return focus to the row that was inspected, if still present.
      selected?.focus?.();
      return;
    }

    const pageButton = event.target.closest(
      "[data-page-previous], [data-page-next]",
    );
    if (pageButton) {
      const detail = pageButton.closest(".pattern-detail");
      const grid = detail?.querySelector("[data-hit-grid]");
      if (!detail || !grid) return;
      const controller = controllerFor(grid);
      const action = pageButton.matches("[data-page-next]") ? "next" : "previous";
      controller.projection.changePage({ type: action });
      applyRowFilters(detail);
      controller.projection.firstRow()?.focus();
      return;
    }

    const selector = event.target.closest(".pattern-selector");
    if (selector) {
      document.querySelectorAll(".pattern-selector").forEach((item) =>
        item.setAttribute("aria-pressed", String(item === selector)));
      const label = selector.textContent.trim();
      document.querySelectorAll("[data-selected-pattern]").forEach((node) => {
        node.textContent = label;
      });
    }

    const tokenButton = event.target.closest(".kwic-token");
    if (tokenButton) {
      tokenButton.closest(".kwic-token-ribbon")?.querySelectorAll(".kwic-token").forEach((item) =>
        item.setAttribute("aria-pressed", String(item === tokenButton)));
      return;
    }

    const captureButton = event.target.closest("[data-capture-target]");
    if (captureButton) {
      const target = document.getElementById(captureButton.dataset.captureTarget);
      const pressed = captureButton.getAttribute("aria-pressed") !== "true";
      captureButton.setAttribute("aria-pressed", String(pressed));
      const inspection = target?.querySelector(".capture-inspection");
      if (inspection) inspection.hidden = !pressed;
      return;
    }

    const retry = event.target.closest("[data-retry-hit]");
    if (retry) {
      const grid = retry.closest(".pattern-detail")?.querySelector("[data-hit-grid]");
      const row = grid?.querySelector(`.kwic-row[data-hit-ordinal="${retry.dataset.retryHit}"]`);
      if (row) activateRow(row, { forceRequest: true });
      return;
    }
    const expansion = event.target.closest("[data-expand-sentence]");
    if (expansion) {
      const row = expansion.closest(".kwic-row");
      const grid = row.closest("[data-hit-grid]");
      const controller = controllerFor(grid);
      controller.selection.transition({
        type: "toggle-expansion",
        ordinal: row.dataset.hitOrdinal,
      });
      applyHitState(grid, controller.selection.state);
      expansion.focus();
      return;
    }

    const clearSurface = event.target.closest("[data-clear-surface-filter]");
    if (clearSurface) {
      setFilter(detailOf(clearSurface), "surface", "all");
      return;
    }
    const clearSection = event.target.closest("[data-clear-section-filter]");
    if (clearSection) {
      setFilter(detailOf(clearSection), "section", "all");
      return;
    }
    const clearKwicSearch = event.target.closest("[data-clear-kwic-search]");
    if (clearKwicSearch) {
      const detail = detailOf(clearKwicSearch);
      const input = detail?.querySelector("[data-kwic-search]");
      if (input) input.value = "";
      const grid = detail?.querySelector("[data-hit-grid]");
      if (grid) controllerFor(grid).projection.resetPage();
      applyRowFilters(detail);
      input?.focus();
      return;
    }

    const quickToggle = event.target.closest("[data-quick-filter-toggle]");
    if (quickToggle) {
      const detail = detailOf(quickToggle);
      const kind = quickToggle.dataset.quickFilterToggle;
      const drawer = detail?.querySelector(
        `[data-quick-filter-drawer="${CSS.escape(kind)}"]`,
      );
      if (drawer) {
        const opening = drawer.hidden;
        if (opening && !drawer.querySelector(".kwic-quick-chips")) {
          const wrap = document.createElement("div");
          wrap.className = "kwic-quick-chips";
          const group = FILTER_GROUPS[kind];
          const selected = controllerFor(detail.querySelector("[data-hit-grid]")).projection.filters[kind];
          for (const button of detail.querySelectorAll(group.source)) {
            const chip = document.createElement("button");
            chip.type = "button";
            chip.className = "kwic-quick-chip";
            chip.dataset[group.quickValue] = button.dataset[group.value];
            chip.setAttribute("aria-pressed", String(button.dataset[group.value] === selected));
            const label = document.createElement("span");
            if (kind === "surface") label.lang = "ja";
            label.textContent = button.querySelector(group.label)?.textContent ?? "";
            const count = document.createElement("small");
            count.textContent = button.querySelector(".ranked-count")?.textContent ?? "";
            chip.append(label, " ", count);
            wrap.append(chip);
          }
          drawer.replaceChildren(wrap);
        }
        for (const other of detail.querySelectorAll("[data-quick-filter-drawer]")) {
          other.hidden = true;
        }
        for (const btn of detail.querySelectorAll("[data-quick-filter-toggle]")) {
          btn.setAttribute("aria-expanded", "false");
        }
        drawer.hidden = !opening;
        quickToggle.setAttribute("aria-expanded", String(opening));
      }
      return;
    }

    for (const [kind, group] of Object.entries(FILTER_GROUPS)) {
      const button = event.target.closest(group.controls);
      if (button) {
        applyFilter(button, kind);
        return;
      }
    }
    const rankedToggle = event.target.closest("[data-ranked-toggle]");
    if (rankedToggle) {
      const list = rankedToggle.closest("[data-ranked-list]");
      const expanded = list.dataset.rankedExpanded !== "true";
      list.dataset.rankedExpanded = String(expanded);
      rankedToggle.setAttribute("aria-expanded", String(expanded));
      rankedToggle.textContent = expanded
        ? t("detail.show_leading", { n: list.dataset.rankedLimit })
        : t("detail.show_remaining", { n: list.querySelector(".ranked-tools")?.dataset.hiddenCount ?? "" }).trim();
      applyRankedList(list);
    }
    const row = event.target.closest(".kwic-row");
    if (row && !event.target.closest(rowInteractiveSelector)) activateRow(row);
  });

  document.addEventListener("input", (event) => {
    if (event.target.matches("[data-pattern-rail-filter]")) {
      applyRailFilter(event.target);
      return;
    }
    if (event.target.matches(".pattern-group-toggle, .pattern-group-field")) {
      const catalog = event.target.closest(".pattern-catalog");
      const toggle = catalog.querySelector(".pattern-group-toggle");
      const select = catalog.querySelector(".pattern-group-field");
      select.disabled = !toggle.checked;
      if (toggle.checked) {
        renderGroupedCatalog(catalog.querySelector("tbody"), select.value);
      } else {
        const body = catalog.querySelector("tbody");
        [...body.querySelectorAll("tr[data-authored-index]")]
          .sort((a, b) => Number(b.dataset.exactTotal) - Number(a.dataset.exactTotal)
            || Number(a.dataset.authoredIndex) - Number(b.dataset.authoredIndex))
          .forEach((row) => body.append(row));
        body.querySelectorAll(".pattern-group-header").forEach((row) => row.remove());
      }
      return;
    }
    if (event.target.matches("[data-ranked-search]")) {
      applyRankedList(event.target.closest("[data-ranked-list]"));
      return;
    }
    if (event.target.matches("[data-kwic-search]")) {
      const detail = detailOf(event.target);
      const grid = detail?.querySelector("[data-hit-grid]");
      if (detail && grid) {
        controllerFor(grid).projection.resetPage();
        applyRowFilters(detail);
      }
    }
  });

  document.addEventListener("change", (event) => {
    if (event.target.matches("[data-kwic-sort], [data-kwic-sort-direction]")) {
      const detail = detailOf(event.target);
      const grid = detail?.querySelector("[data-hit-grid]");
      if (grid) { controllerFor(grid).projection.resetPage(); applyRowFilters(detail); }
      return;
    }
    if (event.target.matches("[data-ranked-order]")) {
      sortRankedList(event.target);
      return;
    }
    if (event.target.matches("[data-page-size]")) {
      const detail = event.target.closest(".pattern-detail");
      const grid = detail?.querySelector("[data-hit-grid]");
      if (!detail || !grid) return;
      const controller = controllerFor(grid);
      const pageSize = parseKwicPageSize(event.target.value);
      controller.projection.changePage({ type: "page-size", value: pageSize });
      saveKwicPageSize(pageSize);
      applyRowFilters(detail);
    }
  });

  document.addEventListener("keydown", (event) => {
    if (event.target.closest?.("[data-site-help]")) return;
    const token = event.target.closest?.(".kwic-token");
    if (token && (event.key === "ArrowRight" || event.key === "ArrowLeft" || event.key === "Home" || event.key === "End")) {
      const ribbon = token.closest(".kwic-token-ribbon");
      const tokens = [...(ribbon?.querySelectorAll(".kwic-token") ?? [])];
      if (tokens.length > 0) {
        const at = tokens.indexOf(token);
        const next = event.key === "Home"
          ? 0
          : event.key === "End"
            ? tokens.length - 1
            : (at + (event.key === "ArrowRight" ? 1 : -1) + tokens.length) % tokens.length;
        event.preventDefault();
        tokens[next].focus();
        tokens[next].click();
      }
      return;
    }

    const tab = event.target.closest?.("[data-detail-tab]");
    if (tab) {
      const step = { ArrowRight: 1, ArrowLeft: -1, Home: 0, End: 0 }[event.key];
      if (step === undefined) return;
      const tabs = [
        ...tab.closest("[data-detail-tabs]").querySelectorAll("[data-detail-tab]"),
      ].filter((candidate) => !candidate.hidden);
      if (tabs.length === 0) return;
      const at = tabs.indexOf(tab);
      const next = event.key === "Home"
        ? 0
        : event.key === "End"
          ? tabs.length - 1
          : (at + step + tabs.length) % tabs.length;
      event.preventDefault();
      selectDetailView(tabs[next].dataset.detailTab, { focus: true });
      return;
    }

    const row = event.target.closest?.(".kwic-row");
    if (row) {
      const expansion = event.target.closest("[data-expand-sentence]");
      if (event.key === "Escape" && expansion) {
        event.preventDefault();
        const grid = row.closest("[data-hit-grid]");
        const controller = controllerFor(grid);
        controller.selection.transition({
          type: "escape",
          ordinal: row.dataset.hitOrdinal,
          focusedOnExpansion: true,
        });
        applyHitState(grid, controller.selection.state);
        expansion.focus();
        return;
      }
      if (event.target.closest(rowInteractiveSelector)) return;
      if (activationKey(event.key)) {
        event.preventDefault();
        activateRow(row);
        return;
      }
      if (event.key === "Escape") {
        const grid = row.closest("[data-hit-grid]");
        const controller = controllerFor(grid);
        controller.selection.transition({
          type: "escape",
          ordinal: row.dataset.hitOrdinal,
          focusedOnExpansion: false,
        });
        applyHitState(grid, controller.selection.state);
        clearHitSelection(row.closest(".pattern-detail"));
        return;
      }
      const rows = [...row.closest("[data-hit-grid]").querySelectorAll(".kwic-row")];
      const index = rows.indexOf(row);
      const visibility = rows.map((item) => !item.hidden);
      let target = -1;
      if (event.key === "ArrowDown" || event.key === "j") target = nextRowIndex(index, 1, visibility);
      if (event.key === "ArrowUp" || event.key === "k") target = nextRowIndex(index, -1, visibility);
      if (event.key === "Home") target = visibility.findIndex(Boolean);
      if (event.key === "End") target = visibility.findLastIndex(Boolean);
      if (target < 0) return;
      event.preventDefault();
      rows.forEach((item) => { item.tabIndex = -1; });
      rows[target].tabIndex = 0;
      rows[target].focus();
      return;
    }

    // Workbench-level keys: "/" focuses the pattern filter, "["/"]" cycle
    // through visible patterns, Escape closes the inspector dock when the
    // focus is anywhere else (rows handle their own Escape above).
    if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey) return;
    const editable = event.target.closest?.(
      "input, textarea, select, [contenteditable], .cm-editor",
    );
    if (event.key === "Escape") {
      if (editable) return;
      const selected = document.querySelector(
        '.kwic-row[aria-selected="true"]',
      );
      if (selected) {
        event.preventDefault();
        selected.focus();
        clearHitSelection(selected.closest(".pattern-detail"));
      }
      return;
    }
    if (editable) return;
    if (event.key === "/") {
      const filter = document.querySelector("[data-pattern-rail-filter]");
      if (filter) {
        event.preventDefault();
        filter.focus();
      }
      return;
    }
    if (event.key === "[" || event.key === "]") {
      const selectors = [...document.querySelectorAll(".pattern-selector")]
        .filter((item) => !item.closest("[hidden]")
          && item.closest("tr")?.style.display !== "none");
      if (selectors.length === 0) return;
      const current = selectors.findIndex(
        (item) => item.getAttribute("aria-pressed") === "true",
      );
      const direction = event.key === "]" ? 1 : -1;
      const next = current < 0
        ? (direction > 0 ? 0 : selectors.length - 1)
        : (current + direction + selectors.length) % selectors.length;
      event.preventDefault();
      selectors[next].click();
    }
  });

  if (typeof window.matchMedia === "function") {
    window
      .matchMedia(WIDE_QUERY)
      .addEventListener?.("change", remountOpenInspectors);
  }

  document.body?.addEventListener("htmx:sseMessage", (event) => {
    const was = event.detail;
    if (!isFinishedSseEvent(was?.type)) return;
    const stream = document.getElementById("search-stream");
    if (stream) stream.dataset.streamFinished = "true";
  });

  document.addEventListener("htmx:beforeCleanupElement", (event) => {
    const element = event.detail.elt;
    if (element.matches?.("[data-hit-grid]")) disposeHitGrid(element);
    for (const grid of element.querySelectorAll?.("[data-hit-grid]") ?? []) {
      disposeHitGrid(grid);
    }
  });
}
