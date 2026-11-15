import { t } from "./i18n.js";

export const esc = (text) =>
  String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** HTML for a dictionary string: the string is escaped, each `{name}` slot receives `parts[name]` as HTML. */
export const phrase = (key, parts) => esc(t(key)).replace(/\{(\w+)\}/g, (slot, name) => parts[name] ?? slot);

export const fullStop = () => `<span class="tail">${esc(t("views.full_stop"))}</span>`;

export const fieldName = (key) => key.replace(/_/g, t("views.key_word_separator"));

const LIST = {
  and: { narrow: ["views.list_separator", "views.list_and"], wide: ["views.list_separator_wide", "views.list_and_wide"] },
  or: { narrow: ["views.list_separator", "views.list_or"], wide: ["views.list_separator_wide", "views.list_or_wide"] },
};

function listed(xs, word) {
  if (xs.length <= 1) return xs.join("");
  const [separator, last] = LIST[word][xs.some((x) => String(x).includes(t("views.list_separator"))) ? "wide" : "narrow"];
  return `${xs.slice(0, -1).join(t(separator))}${t(last)}${xs.at(-1)}`;
}

export const isAny = (dimension, w) => !w || (dimension.kind === "range" ? w.lo == null && w.hi == null : !w.v?.length);

function rangeText(w) {
  if (w.lo != null && w.hi != null) return t("views.range_between", w);
  return w.lo != null ? t("views.range_from", w) : t("views.range_to", w);
}

function valuesText(w) {
  if (w.m === "out") return t("views.values_except", { values: listed(w.v, "and") });
  return w.v.length <= 3 ? listed(w.v, "or") : t("views.values_count", { n: w.v.length });
}

/** `name` is the display label for the dimension's key. */
export function conditionText(dimension, w, name) {
  if (isAny(dimension, w)) return t("views.any", { name });
  return dimension.kind === "range" ? rangeText(w) : valuesText(w);
}

/** HTML naming the documents that `items` admit, or "" without items. Each item is `{ name, any, html }`, `html` being its control. */
export function scopeDocuments(items) {
  const runs = [];
  for (const x of items) {
    const last = runs.at(-1);
    if (x.any && last?.any) last.html.push(x.html);
    else runs.push({ any: x.any, html: [x.html], name: x.name });
  }
  if (!runs.length) return "";
  const conditions = runs
    .map((r) => (r.any ? phrase("views.scope_any", { values: r.html.join(esc(t("views.scope_any_separator"))) }) : phrase("views.scope_is", { name: esc(r.name), value: r.html[0] })))
    .join(esc(t("views.scope_separator")));
  return phrase(runs.at(-1).any ? "views.scope_documents_any_last" : "views.scope_documents", { conditions });
}

const REASONS = {
  section: "views.reason_section",
  sentence: "views.reason_sentence",
  syntax: "views.reason_syntax",
  unknown_key: "views.reason_unknown_key",
  unwritable: "views.reason_unwritable",
  shape: "views.reason_shape",
  unknown_entry: "frequency.reason_unknown_entry",
};

/** Why the server answered `unsupported`; a reason without a string is shown as received. */
export const reasonText = (reply) => (Object.hasOwn(REASONS, reply.reason) ? t(REASONS[reply.reason], reply) : reply.reason);

/**
 * GET `api/<path>` and read the reply as JSON. Parameters that are undefined, null or "" are left out.
 * A failed status rejects with the reply's `error`, or with the status when the reply has none.
 */
export async function api(path, params, signal) {
  const query = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== ""));
  const response = await fetch(`api/${path}?${query}`, { signal });
  if (!response.ok) {
    const reply = await response.json().catch(() => null);
    throw new Error(reply?.error ?? t("views.failed", { status: response.status }));
  }
  return response.json();
}

/** Each `next()` call aborts the previous signal. */
export function latestRequest() {
  let controller = null;
  return {
    next() {
      controller?.abort();
      return (controller = new AbortController()).signal;
    },
    abort: () => controller?.abort(),
  };
}

/** The count-table metadata, or null once the failure's message is in `stateEl`. */
export async function loadMeta(stateEl) {
  try {
    return await api("frequency/meta", {});
  } catch (error) {
    stateEl.textContent = error.message;
    return null;
  }
}

/** "Show more" requests. `pending` holds from `next` until its request settles; `cancel` aborts it. */
export function pager(report) {
  let request = null;
  return {
    get pending() {
      return request !== null;
    },
    cancel: () => request?.abort(),
    /**
     * Fetches one page with `fetchPage(signal)`. While `current()` holds, an `ok` reply goes to `receive`
     * and a failure's message to `report`; a reply that arrives after `current()` turned false is dropped.
     * `redraw` runs when the request has settled.
     */
    async next(fetchPage, current, receive, redraw) {
      const mine = (request = new AbortController());
      try {
        const reply = await fetchPage(mine.signal);
        if (current() && reply.status === "ok") receive(reply);
      } catch (error) {
        if (current() && error.name !== "AbortError") report(error.message);
      }
      if (request !== mine) return;
      request = null;
      redraw();
    },
  };
}

/** Runs `draw`, then focuses the control in `container` that `selectorFor(focused)` names, restoring a text caret. */
export function redrawKeepingFocus(container, selectorFor, draw) {
  const held = container.contains(document.activeElement) ? document.activeElement : null;
  const selector = held ? selectorFor(held) : null;
  const caret = held?.selectionStart != null ? [held.selectionStart, held.selectionEnd, held.selectionDirection] : null;
  draw();
  const control = selector ? container.querySelector(selector) : null;
  control?.focus({ preventScroll: true });
  if (caret) control?.setSelectionRange(...caret);
}

/** Names the corpus `key` in the page's corpus line: by its display name, or by the key when no table counts it. Without a key the line is hidden. */
export function showCorpus(root, meta, key) {
  const line = root.querySelector("[data-corpus-line]");
  const name = meta.pairs.find((pair) => pair.corpus === key)?.corpus_name ?? key ?? "";
  line.querySelector(".corpus-context-name > :last-child").textContent = name;
  line.hidden = !name;
  root.querySelector("[data-overlaps-note]").hidden = !name;
}
