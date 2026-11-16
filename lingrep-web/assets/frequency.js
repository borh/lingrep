import { currentLocale, onLocaleChange, t } from "./i18n.js";
import { createConditionPicker } from "./condition-picker.js";
import { count as num, fixed, places } from "./numbers.js";
import { api, conditionText, esc, fieldName, fullStop, isAny, latestRequest, loadMeta, pager, phrase, reasonText, redrawKeepingFocus, scopeDocuments, showCorpus } from "./views.js";

const PAGE = 100;
const LINES = 50;
const REST = 120;
const KEYS = ["corpus", "catalog", "pattern", "w", "where", "sort", "q", "k", "entry", "read", "scope"];

export const pctOf = (x, y) =>
  !y ? "0" : x && x / y < 0.01 ? "<1" : x < y && Math.round((x / y) * 100) === 100 ? (Math.floor((x / y) * 1000) / 10).toFixed(1) : String(Math.round((x / y) * 100));

const SORTS = [["count", "frequency.sort_count"], ["works", "frequency.sort_works"], ["key", "frequency.sort_key"]];
const HEADS = ["views.head_rank", "views.head_entry", "views.head_occurrences", "views.head_documents", "frequency.head_share", "frequency.head_per_million"];
const TABLE_HEADS = ["frequency.head_value", "views.head_occurrences", "views.head_documents"];


export function validConditions(value) {
  const record = (x) => x !== null && typeof x === "object" && !Array.isArray(x);
  const bound = (x) => x == null || (typeof x === "number" && Number.isFinite(x));
  return record(value) && Object.values(value).every((condition) =>
    record(condition) && ["in", "out", "range"].includes(condition.m)
    && (!Object.hasOwn(condition, "v") || (Array.isArray(condition.v) && condition.v.every((x) => typeof x === "string")))
    && bound(condition.lo) && bound(condition.hi));
}

export function readState(hash) {
  const h = new URLSearchParams(hash.replace(/^#/, ""));
  const s = { corpus: h.get("corpus"), catalog: h.get("catalog"), pattern: h.get("pattern"), w: {}, where: h.get("where"), sort: h.get("sort") || "count", q: h.get("q") || "", k: {}, entry: h.get("entry"), read: h.get("read"), scope: h.get("scope"), bad: false };
  if (h.get("k")) {
    try {
      const k = JSON.parse(h.get("k"));
      if (k !== null && typeof k === "object" && !Array.isArray(k) && Object.values(k).every((v) => typeof v === "string")) s.k = k;
      else s.bad = true;
    } catch {
      s.bad = true;
    }
  }
  if (h.get("w")) {
    try {
      const w = JSON.parse(h.get("w"));
      if (validConditions(w)) s.w = w;
      else s.bad = true;
    } catch {
      s.bad = true;
    }
  }
  return s;
}

export function stateEntries(s) {
  const out = [];
  for (const k of KEYS) {
    const v = s[k];
    if (v === null || v === undefined || v === "" || ((k === "w" || k === "k") && !Object.keys(v).length) || (k === "sort" && v === "count")) continue;
    out.push([k, k === "w" || k === "k" ? JSON.stringify(v) : String(v)]);
  }
  return out;
}

/** The question page address that runs `s.pattern` of `source` over `line`'s document and opens `line`'s occurrence; `back` is the frequency view address to return to. */
export function inspectHref(s, line, { source, analysis, fingerprint }, back) {
  const occurrence = { pattern: s.pattern, sentence: line.sentence, document: line.document, roles: line.roles, analysis, fingerprint };
  return `./#${new URLSearchParams({ fresh: "1", run: "1", corpus: s.corpus, analysis, source, occurrence: JSON.stringify(occurrence), return: back })}`;
}

export const hashHolds = (hash, entries) => {
  const sorted = (p) => JSON.stringify([...p].sort());
  return sorted(new URLSearchParams(hash.replace(/^#/, ""))) === sorted(new URLSearchParams(entries));
};

export function shortForms(name, siblings = []) {
  const units = name.match(/[^_ ・、]+[_ ・、]*/g) ?? [name];
  const forms = [];
  for (let keep = units.length - 1; keep >= 1; keep--) {
    const head = units.slice(0, keep).join("").replace(/[_ ・、]+$/, "");
    const tail = units.slice(units.length - keep).join("");
    if (!siblings.some((other) => other !== name && other.startsWith(head))) forms.push(`${head}…`);
    if (!siblings.some((other) => other !== name && other.endsWith(tail))) forms.push(`…${tail}`);
  }
  return forms;
}

const OPEN = "（([［「", CLOSE = "）)]］」";

function units(name) {
  const stack = [[]];
  let cur = "";
  const flush = () => {
    if (cur) stack.at(-1).push(cur);
    cur = "";
  };
  for (let i = 0; i < name.length; i++) {
    const c = name[i];
    if (OPEN.includes(c)) {
      flush();
      const group = [];
      group.open = c;
      stack.push(group);
    } else if (CLOSE.includes(c) && stack.length > 1) {
      flush();
      const group = stack.pop();
      stack.at(-1).push({ open: group.open, items: [...group], close: c });
    } else {
      cur += c;
      if (c === "." && name[i + 1] === " ") {
        cur += " ";
        i++;
        flush();
      } else if ("、・ 　".includes(c)) flush();
    }
  }
  flush();
  while (stack.length > 1) {
    const group = stack.pop();
    stack.at(-1).push(group.open, ...group);
  }
  return stack[0];
}
const unitText = (ns) => ns.map((n) => (typeof n === "string" ? n : n.open + unitText(n.items) + n.close)).join("");
const trimEnd = (text) => text.replace(/[、・.,;:\s]+$/u, "");
function* tails(ns) {
  for (let k = ns.length - 1; k >= 1; k--) {
    const last = ns[k], head = unitText(ns.slice(0, k));
    if (typeof last !== "string") for (const inner of tails(last.items)) yield head + last.open + inner + last.close;
    yield `${trimEnd(head)}…`;
  }
}

/** Returns shorter labels longest first, replacing whole units with “…” and excluding forms whose retained prefix or suffix is shared by another name. */
export function labelForms(names) {
  return names.map((name, i) => {
    const ns = units(name), others = names.filter((_, j) => j !== i && names[j] !== name), out = [];
    for (let k = 1; k < ns.length; k++) {
      const right = unitText(ns.slice(k));
      if (!others.some((o) => o.endsWith(right))) out.push(`…${right}`);
    }
    for (const form of tails(ns)) {
      const left = form.slice(0, form.indexOf("…"));
      if (left && !others.some((o) => o.startsWith(left))) out.push(form);
    }
    return [...new Set(out)].sort((a, b) => b.length - a.length);
  });
}

export function mountFrequency(root) {
  const $ = (sel) => root.querySelector(sel);
  const sentenceEl = $("[data-fq-sentence]"), figuresEl = $("[data-fq-figures]"), clauseEl = $("[data-fq-clause]");
  const sourceEl = $("[data-fq-source]"), stateEl = $("[data-fq-state]"), tableEl = $("[data-fq-table]"), entryEl = $("[data-fq-entry]");
  let meta = null, s = readState(location.hash), answer = null, gen = 0, shown = 0;
  let editing = false;
  let queryMessage = null;
  const inflight = latestRequest(), paging = pager((message) => {
    queryMessage = () => message;
    stateEl.textContent = message;
  });
  let profile = null, lined = null, plan = {}, preview = null, previewed = null, anchor = null, lastEntry = null, depth = 0;

  const pair = () => meta.pairs.find((p) => p.corpus === s.corpus && p.catalog === s.catalog);
  const dims = () => (pair() ? meta.dimensions[pair().corpus] ?? [] : []);
  const dim = (key) => dims().find((d) => d.key === key);
  const keyName = (key) => meta.labels?.[key]?.[currentLocale()] ?? fieldName(key);
  const shownKeys = () => picker.shownKeys();
  const conditions = () => (Object.keys(s.w).length ? JSON.stringify(s.w) : undefined);
  const picker = createConditionPicker({
    sentence: sentenceEl,
    view: () => ({ dimensions: dims(), conditions: s.w, params: { corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, w: conditions() }, revision: gen }),
    keyName,
    label: (_scope, key) => key == null ? t("frequency.add_label") : keyName(key),
    anchor: (_scope, key) => sentenceEl.querySelector(`[data-b="${CSS.escape(key ?? "add")}"]`),
    commit: (_scope, key, w) => set({ w: { ...s.w, [key]: w } }),
    render: renderSentence,
  });

  function resolve() {
    if (!meta.pairs.length) return { state: t("views.no_tables") };
    if (s.bad) return { state: t("views.reason_address") };
    const first = meta.pairs[0];
    s.corpus ??= first.corpus;
    s.catalog ??= (meta.pairs.find((p) => p.corpus === s.corpus) ?? first).catalog;
    const p = pair();
    if (!p) {
      const name = meta.pairs.find((x) => x.corpus === s.corpus)?.corpus_name ?? s.corpus;
      return { state: t("views.unavailable", { catalog: s.catalog, corpus: name }) };
    }
    s.pattern ??= p.patterns[0]?.name ?? null;
    if (!p.patterns.some((x) => x.name === s.pattern)) return { state: t("views.unknown_pattern", { pattern: s.pattern, catalog: s.catalog }), sentence: true };
    return { sentence: true, query: true };
  }

  function persist() {
    const entries = stateEntries(s);
    if (!hashHolds(location.hash, entries)) history.pushState(null, "", `#${new URLSearchParams(entries)}`);
  }

  function set(patch) {
    Object.assign(s, patch);
    persist();
    load();
  }

  const blank = (role, inner, cls = "") => `<span class="blank${cls ? ` ${cls}` : ""}"><span class="role">${role}</span>${inner}</span>`;
  const role = (name, key = "") => `<span class="nm" data-name="${esc(key || name)}">${esc(name)}</span><span class="n" data-share="${esc(key)}"></span>`;
  const select = (b, label, options, value) =>
    `<select data-b="${b}" aria-label="${esc(label)}">${options.map(([v, t]) => `<option value="${esc(v)}"${String(v) === String(value ?? "") ? " selected" : ""}>${esc(t)}</option>`).join("")}</select>`;

  function sentenceHtml() {
    const p = pair();
    const catalogs = meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, meta.pairs.some((y) => y.corpus !== x.corpus) ? `${x.catalog} (${x.corpus_name})` : x.catalog]);
    const items = shownKeys().map((key) => {
      const d = dim(key), w = s.w[key];
      return {
        name: keyName(key),
        any: isAny(d, w),
        html: blank(role(keyName(key), key), `<button class="pick" data-b="${esc(key)}" aria-haspopup="dialog" aria-expanded="${picker.isOpen(key)}">${esc(conditionText(d, w, keyName(key)))}</button>`),
      };
    });
    const roles = { pattern: t("views.role_pattern"), catalog: t("views.role_catalog"), order: t("frequency.role_order"), filter: t("frequency.role_filter") };
    const documents = scopeDocuments(items);
    const scope = s.where != null ? esc(t("frequency.raw_scope")) : documents && phrase("frequency.scope_from", { documents });
    const free = s.where == null && shownKeys().length < dims().length;
    return (
      phrase(scope ? "frequency.sentence_scoped" : "frequency.sentence", {
        pattern: blank(role(roles.pattern), select("pattern", roles.pattern, p.patterns.map((x) => [x.name, x.name]), s.pattern), "lead"),
        catalog: blank(role(roles.catalog), select("catalog", roles.catalog, catalogs, `${s.corpus}\n${s.catalog}`)),
        scope,
        order: blank(role(roles.order), select("sort", roles.order, SORTS.map(([value, key]) => [value, t(key)]), s.sort)),
        filter: blank(role(roles.filter, "filter"), `<input data-b="q" type="search" placeholder="${esc(t("frequency.placeholder"))}" aria-label="${esc(roles.filter)}" value="${esc(s.q)}">`),
        stop: fullStop(),
      }) + keyFieldsHtml() + (free ? `<button class="add" data-b="add" aria-haspopup="menu" aria-expanded="${picker.isOpen(null)}" aria-label="${esc(t("frequency.add_label"))}">${esc(t("views.add"))}</button>` : "")
    );
  }

  const keyFields = () => pair()?.patterns.find((x) => x.name === s.pattern)?.key_fields ?? [];
  const keyEquals = () => Object.fromEntries(Object.entries(s.k).filter(([, v]) => v !== ""));
  /** One exact-match input per field of the pattern's compound key. */
  function keyFieldsHtml() {
    const fields = keyFields();
    if (fields.length < 2) return "";
    const label = t("frequency.key_equals");
    return `<span class="fq-key" role="group" aria-label="${esc(label)}"><span class="fq-key-label">${esc(label)}</span>${fields.map((f) => `<label><code>${esc(f)}</code> = <input data-k="${esc(f)}" type="search" size="6" value="${esc(s.k[f] ?? "")}" placeholder="${esc(t("frequency.key_any"))}"></label>`).join("")}</span>`;
  }

  function renderSentence() {
    redrawKeepingFocus(sentenceEl, (held) => (held.dataset.b ? `[data-b="${CSS.escape(held.dataset.b)}"]` : null), () => (sentenceEl.innerHTML = sentenceHtml()));
    annotate();
  }

  function annotate() {
    for (const n of sentenceEl.querySelectorAll(".role .n")) {
      const key = n.dataset.share, roleEl = n.parentElement, pick = roleEl.parentElement.querySelector(".pick, input");
      let kept = null, title = null;
      if (answer && key === "filter" && s.q) {
        const p = pctOf(answer.matching, answer.entries);
        kept = p;
        title = t("frequency.keeps_entries", { kept: num(answer.matching), of: num(answer.entries), pct: p });
        pick?.style.setProperty("--kept", `${answer.entries ? (answer.matching / answer.entries) * 100 : 0}%`);
      } else if (answer && key && answer.kept[key]) {
        const k = answer.kept[key], p = pctOf(k.kept, k.of);
        kept = p;
        title = t("frequency.keeps", { kept: num(k.kept), of: num(k.of), pct: p });
        if (s.w[key]?.m === "out" && k.excluded_missing_field_documents) {
          const n = num(k.excluded_missing_field_documents);
          title += ` ${t(n === "1" ? "frequency.missing_field_one" : "frequency.missing_field_other", { n, key: keyName(key) })}`;
        }
        pick?.style.setProperty("--kept", `${k.of ? (k.kept / k.of) * 100 : 0}%`);
      } else pick?.style.removeProperty("--kept");
      n.textContent = kept == null ? "" : `${kept}%`;
      if (title) roleEl.dataset.full = title;
      else delete roleEl.dataset.full;
    }
    fitRoles();
  }

  function fitRoles() {
    const roles = [...sentenceEl.querySelectorAll(".blank > .role")];
    const names = roles.map((r) => r.querySelector(".nm"));
    const full = names.map((n) => n.dataset.label ?? (n.dataset.label = n.textContent));
    roles.forEach((r, i) => {
      names[i].textContent = full[i];
      r.classList.remove("cut");
      r.style.maxWidth = "";
      r.title = r.dataset.full ?? "";
    });
    const at = roles.map((r) => r.getBoundingClientRect()), edge = sentenceEl.getBoundingClientRect().right;
    roles.forEach((r, i) => {
      const a = at[i], next = at[i + 1];
      const limit = (next && Math.abs(next.top - a.top) < 2 && next.left > a.left ? next.left - 8 : edge) - a.left;
      if (r.getBoundingClientRect().width <= limit) return;
      r.title ||= full[i];
      for (const form of shortForms(full[i], full)) {
        names[i].textContent = form;
        if (r.getBoundingClientRect().width <= limit) return;
      }
      names[i].textContent = full[i];
      r.classList.add("cut");
      r.style.maxWidth = `${Math.max(0, Math.floor(limit))}px`;
    });
  }

  function renderFigures() {
    if (!answer || entering()) figuresEl.textContent = "";
    else if (answer.subset) figuresEl.textContent = t("frequency.subset_figures", { key: Object.entries(keyEquals()).map(([f, v]) => `${f} = ${v}`).join(", "), entries: num(answer.subset.entries), occurrences: num(answer.subset.occurrences), works: num(answer.subset.works), total: num(answer.occurrences), documents: num(answer.documents_admitted) });
    else figuresEl.textContent = t("frequency.figures", { entries: num(answer.entries), occurrences: num(answer.occurrences), documents: num(answer.documents_admitted) });
  }

  function renderClause() {
    if (editing) {
      clauseEl.innerHTML = `<div class="edit"><textarea data-c="text" aria-label="--where" spellcheck="false">${esc(s.where ?? answer?.clause ?? "")}</textarea><div class="actions"><button data-c="apply">${esc(t("views.apply"))}</button><button data-c="cancel">${esc(t("views.cancel"))}</button></div></div>`;
      return;
    }
    const source = entering() ? profile : answer;
    if (!source && s.where == null) {
      clauseEl.innerHTML = "";
      return;
    }
    const command = source?.command ?? "";
    const where = source?.clause ?? s.where ?? "";
    const code = command
      ? (() => {
          const at = where ? command.indexOf(" --where ") : -1;
          if (at < 0) return esc(command);
          const end = command.indexOf(" --corpus ", at);
          return `${esc(command.slice(0, at))}<span class="where">${esc(command.slice(at, end))}</span>${esc(command.slice(end))}`;
        })()
      : esc(where);
    clauseEl.innerHTML = `<div class="clause"><code data-c="code">${code}</code>${command ? `<button data-c="copy">${esc(t("views.copy"))}</button>` : ""}<button data-c="edit">${esc(t("frequency.edit"))}</button>${s.where != null ? `<button data-c="blanks">${esc(t("views.use_controls"))}</button>` : ""}</div>${source ? `<p>${esc(command ? t("views.command_context") : t("views.command_unavailable"))}</p>` : ""}`;
  }
  clauseEl.addEventListener("click", async (e) => {
    const c = e.target.closest("[data-c]")?.dataset.c;
    if (c === "copy") {
      const code = clauseEl.querySelector("[data-c=code]");
      try {
        await navigator.clipboard.writeText(code.textContent);
      } catch {
        getSelection().selectAllChildren(code);
      }
      e.target.textContent = t("views.copied");
    } else if (c === "edit") {
      editing = true;
      renderClause();
      clauseEl.querySelector("textarea").focus();
    } else if (c === "cancel") {
      editing = false;
      renderClause();
    } else if (c === "apply") {
      const text = clauseEl.querySelector("textarea").value.trim();
      editing = false;
      set({ where: text || null, w: {} });
    } else if (c === "blanks") set({ where: null });
  });

  function renderSource() {
    const p = pair(), pattern = p?.patterns.find((x) => x.name === s.pattern);
    if (!pattern) {
      sourceEl.innerHTML = "";
      return;
    }
    sourceEl.innerHTML = `${pattern.source ? `<pre>${esc(pattern.source)}</pre>` : ""}${p.workbench ? `<a href="${esc(p.workbench)}">${esc(t("frequency.workbench"))}</a>` : ""}`;
  }

  const keyNames = (group) => (group.kind === "key" && group.fields.length > 1 ? group.fields.map((f) => f.name).join(" ") : "");
  function rowHtml(r, i) {
    const docs = answer.documents_admitted, tokens = answer.tokens_admitted;
    const share = docs ? r.works / docs : 0;
    const pm = tokens ? (r.count * 1e6) / tokens : 0;
    const names = keyNames(r.group);
    const subset = answer.subset ? `<td class="fig r" title="${esc(`${num(r.count)} / ${num(answer.subset.occurrences)}`)}">${answer.subset.occurrences ? ((r.count / answer.subset.occurrences) * 100).toFixed(1) : "0.0"}%</td>` : "";
    return `<tr${s.where == null ? ` data-i="${i}"` : ""}><td class="fig r">${num(r.rank)}</td><td class="entry-cell">${s.where == null ? `<button class="open-entry" data-i="${i}">${esc(r.text)}</button>` : esc(r.text)}${names ? `<small>${esc(names)}</small>` : ""}</td><td class="fig r"><b>${num(r.count)}</b></td>${subset}<td class="fig r">${num(r.works)}</td><td class="spread" title="${(share * 100).toFixed(1)}%"><span><i style="width:${Math.max(1, share * 100).toFixed(1)}%"></i></span></td><td class="fig r pm" data-v="${pm}"></td></tr>`;
  }
  function alignColumn(cells) {
    const xs = cells.map((c) => +c.dataset.v), digits = places(xs);
    cells.forEach((c, i) => (c.textContent = fixed(xs[i], digits)));
  }
  function renderTable() {
    tableEl.hidden = entering();
    if (!answer || entering()) {
      tableEl.innerHTML = "";
      return;
    }
    if (!answer.rows.length) {
      tableEl.innerHTML = `<p class="fq-empty">${esc(answer.subset && answer.occurrences ? t("frequency.no_key_match") : s.q && answer.occurrences ? t("frequency.no_match", { q: s.q }) : t("frequency.empty"))}</p>`;
      return;
    }
    const heads = answer.subset ? [...HEADS.slice(0, 3), "frequency.head_subset_share", ...HEADS.slice(3)] : HEADS;
    tableEl.innerHTML = `<table class="freq"><thead><tr>${heads.map((h) => `<th class="${h === "views.head_entry" || h === "frequency.head_share" ? "" : "r"}">${esc(t(h))}</th>`).join("")}</tr></thead><tbody>${answer.rows.map((r, i) => rowHtml(r, i)).join("")}</tbody></table><button class="more"${shown >= answer.matching ? " hidden" : ""}${paging.pending ? " disabled" : ""}>${esc(t("views.more", { n: PAGE }))}</button>`;
    alignColumn([...tableEl.querySelectorAll("td.pm")]);
    if (lastEntry) {
      const i = answer.rows.findIndex((r) => JSON.stringify(r.group) === lastEntry);
      lastEntry = null;
      tableEl.querySelector(`button[data-i="${i}"]`)?.focus();
    }
  }
  tableEl.addEventListener("click", async (e) => {
    const row = e.target.closest("tr[data-i]");
    if (row && answer) {
      depth = shown;
      set({ entry: JSON.stringify(answer.rows[+row.dataset.i].group) });
      return;
    }
    const button = e.target.closest(".more");
    if (!button) return;
    button.disabled = true;
    const into = answer;
    await paging.next(
      (signal) => api("frequency", { ...params(), offset: shown }, signal),
      () => into === answer,
      (more) => {
        into.rows.push(...more.rows);
        shown = into.rows.length;
      },
      renderTable,
    );
  });

  sentenceEl.addEventListener("change", (e) => {
    const b = e.target.dataset.b;
    if (b === "catalog") {
      const [corpus, catalog] = e.target.value.split("\n");
      set({ corpus, catalog, pattern: null, w: corpus === s.corpus ? s.w : {}, q: "", k: {}, entry: null });
    } else if (b === "pattern") set({ pattern: e.target.value, k: {}, entry: null });
    else if (b === "sort") set({ sort: e.target.value });
  });
  let typing;
  sentenceEl.addEventListener("input", (e) => {
    const field = e.target.dataset.k;
    if (field != null) {
      const k = { ...s.k };
      if (e.target.value) k[field] = e.target.value;
      else delete k[field];
      s.k = k;
    } else if (e.target.dataset.b === "q") s.q = e.target.value;
    else return;
    clearTimeout(typing);
    typing = setTimeout(() => {
      persist();
      load({ keepSentence: true });
    }, 220);
  });
  sentenceEl.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-b]")?.dataset.b;
    if (!b) return;
    e.stopPropagation();
    if (picker.isOpen(b === "add" ? null : b)) return picker.close();
    picker.open(b === "add" ? null : b);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || document.querySelector("dialog:modal")) return;
    if (picker.opened) picker.close();
    else if (entering() && !editing && !e.target.closest?.("input, textarea, select")) leaveEntry();
  });
  addEventListener("resize", () => {
    fitRoles();
    picker.refresh();
  });

  const entering = () => s.entry != null && s.where == null;
  const groupText = (g) => (g.kind === "key" ? g.fields.map((f) => (f.kind === "list" ? f.values.join(",") : f.value)).join(" ") : g.value);
  const valueName = (v) => v ?? t("frequency.no_value");
  const ems = (text) => [...text].reduce((w, c) => w + (c.charCodeAt(0) > 0x2e7f ? 1 : 0.6), 0);
  const barsOf = (c) => [...c.bars, ...(c.none ? [{ ...c.none, value: null }] : [])];
  const span = (x) => (String(x.lo) === String(x.hi) ? String(x.lo) : `${x.lo}–${x.hi}`);
  const entryParams = (w) => ({ corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, entry: s.entry, w: Object.keys(w).length ? JSON.stringify(w) : undefined });
  const opened = new Set();
  let readout = null;

  function planOf(p) {
    return Object.fromEntries(
      p.charts.map((c) => [c.key, c.kind === "categorical" ? { rows: barsOf(c).length, more: c.distinct > c.bars.length, lw: Math.max(2, ...barsOf(c).map((b) => ems(valueName(b.value)))) } : {}]),
    );
  }
  function admits(key) {
    const w = s.w[key];
    if (!w?.v?.length) return () => false;
    return w.m === "out" ? (v) => v != null && !w.v.includes(v) : (v) => w.v.includes(v);
  }
  function barsHtml(c) {
    const rows = plan[c.key] ?? { rows: 0, more: false, lw: 2 }, admitted = admits(c.key);
    const all = barsOf(c).slice(0, rows.rows), max = Math.max(1, ...all.map((b) => b.count));
    const forms = labelForms(all.map((b) => valueName(b.value)));
    const bar = (b, i) => {
      const name = valueName(b.value), none = b.value == null;
      return `<li><button data-bar data-v="${none ? "" : esc(b.value)}"${none ? " data-none" : ""} aria-pressed="${admitted(b.value)}" aria-label="${esc(readoutText(name, b))}"><span class="name${none ? " none" : ""}" data-full="${esc(name)}" data-forms="${esc(JSON.stringify(forms[i]))}" aria-hidden="true"><span>${esc(name)}</span></span><span class="b"><i style="width:${((b.count / max) * 100).toFixed(1)}%"></i></span><span class="v" aria-hidden="true">${num(b.count)}</span></button></li>`;
    };
    const pad = '<li class="pad" aria-hidden="true"><span>&nbsp;</span></li>'.repeat(Math.max(0, rows.rows - all.length));
    const more = rows.more ? `<p class="fq-more-values">${c.distinct > c.bars.length ? esc(t("frequency.more_values", { n: num(c.distinct - c.bars.length) })) : "&nbsp;"}</p>` : "";
    return `<ol class="fq-bars" style="--lw:${Math.min(rows.lw, 14).toFixed(1)}em">${all.map(bar).join("")}${pad}</ol>${more}`;
  }
  function columnsHtml(c) {
    const d = dim(c.key), w = s.w[c.key], max = Math.max(1, ...c.columns.map((x) => x.count));
    const ranged = w?.m === "range" && (w.lo != null || w.hi != null);
    const inside = (x) => ranged && (w.lo == null || +x.lo >= w.lo) && (w.hi == null || +x.hi <= w.hi);
    const columns = c.columns
      .map((x) => (x.lo == null ? '<span class="gap"></span>' : `<button data-col data-lo="${esc(x.lo)}" data-hi="${esc(x.hi)}" aria-pressed="${inside(x)}" aria-label="${esc(readoutText(span(x), x))}"><i style="height:${((x.count / max) * 100).toFixed(1)}%"></i></button>`))
      .join("");
    const none = c.none ? readoutText(t("frequency.no_value"), c.none) : "";
    return `<div class="fq-columns${c.columns.length > 60 ? " dense" : ""}">${columns}</div><div class="fq-axis" aria-hidden="true"><span>${d ? tick(d.min) : ""}</span><span>${d ? tick(d.max) : ""}</span></div><p class="fq-none">${none ? esc(none) : "&nbsp;"}</p>`;
  }
  const tick = (n) => (Number.isInteger(n) && Math.abs(n) < 10000 ? String(n) : num(n));
  const bodyHtml = (c) => (c.kind === "categorical" ? barsHtml(c) : columnsHtml(c));
  function chartTable(c) {
    const rows =
      c.kind === "categorical"
        ? barsOf(c).map((b) => [valueName(b.value), b.count, b.works])
        : [...c.columns.filter((x) => x.lo != null && x.count).map((x) => [span(x), x.count, x.works]), ...(c.none ? [[t("frequency.no_value"), c.none.count, c.none.works]] : [])];
    return `<table><thead><tr>${TABLE_HEADS.map((h, i) => `<th${i ? ' class="r"' : ""}>${esc(t(h))}</th>`).join("")}</tr></thead><tbody>${rows.map(([v, n, d]) => `<tr><td>${esc(v)}</td><td class="r">${num(n)}</td><td class="r">${num(d)}</td></tr>`).join("")}</tbody></table>`;
  }
  const chartHtml = (c) =>
    `<figure class="fq-chart" data-key="${esc(c.key)}"><figcaption>${esc(keyName(c.key))}</figcaption>${dim(c.key)?.list ? `<p class="fq-multivalue-note">${esc(t("frequency.multivalue_note"))}</p>` : ""}<p class="fq-readout" data-readout aria-live="polite">&nbsp;</p><div class="fq-chart-body" data-body>${bodyHtml(c)}</div><details${opened.has(c.key) ? " open" : ""}><summary>${esc(t("frequency.as_table"))}</summary>${chartTable(c)}</details></figure>`;

  const lineHtml = (st) => {
    const source = pair()?.source, back = `frequencies#${new URLSearchParams(stateEntries({ ...s, read: null, scope: null }))}`;
    const identity = { source, analysis: st.analysis_identity, fingerprint: st.catalog_fingerprint };
    return (l, i) =>
      `<li data-i="${i}" data-sentence="${l.sentence}" data-start="${l.start}" data-end="${l.end}"><span class="kl"><span>${esc(l.left)}</span></span><span class="kr"><button type="button" class="kn" aria-haspopup="dialog">${esc(l.node)}</button>${esc(l.right)}</span><span class="src"><span>${esc(t("frequency.document", { id: l.document }))}</span>${source && st.analysis_identity && l.roles ? ` <a class="fq-inspect" href="${esc(inspectHref(s, l, identity, back))}">${esc(t("frequency.inspect"))}</a>` : ""}</span></li>`;
  };
  function linesHtml(st, previewing) {
    return `<h3>${esc(t("frequency.lines"))} <span class="count">${esc(t("frequency.lines_shown", { n: num(st.lines.length), of: num(st.total) }))}</span></h3>${st.total ? "" : `<p class="fq-empty">${esc(t("frequency.empty"))}</p>`}<ol class="fq-kwic">${st.lines.map(lineHtml(st)).join("")}</ol><button class="more" data-e="more"${previewing || st.lines.length >= st.total ? " hidden" : ""}${paging.pending ? " disabled" : ""}>${esc(t("views.more", { n: LINES }))}</button>`;
  }
  function figuresText(p) {
    const o = num(p.count), d = num(p.works);
    return t("frequency.entry_figures", {
      occurrences: t(o === "1" ? "frequency.occurrences_one" : "frequency.occurrences_other", { n: o }),
      documents: t(d === "1" ? "frequency.documents_one" : "frequency.documents_other", { n: d }),
    });
  }
  const readoutText = (name, p) => t("frequency.readout", { name, figures: figuresText(p) });

  function fitLabels(scope = entryEl) {
    const names = [...scope.querySelectorAll(".fq-bars .name")];
    for (const n of names) {
      n.classList.remove("cut");
      n.firstChild.textContent = n.dataset.full;
    }
    const over = names.filter((n) => n.firstChild.scrollWidth > n.firstChild.clientWidth);
    for (const n of over) {
      const shown = n.firstChild, all = JSON.parse(n.dataset.forms);
      const fit = all.find((form) => {
        shown.textContent = form;
        return shown.scrollWidth <= shown.clientWidth;
      });
      if (fit === undefined) {
        shown.textContent = all.at(-1) ?? n.dataset.full;
        n.classList.add("cut");
      }
    }
  }
  new ResizeObserver(() => fitLabels()).observe(entryEl);

  function showReadout() {
    if (!readout) return;
    const chart = entryEl.querySelector(`.fq-chart[data-key="${CSS.escape(readout.key)}"]`);
    const bar = chart?.querySelector(readout.v != null ? `[data-bar][data-v="${CSS.escape(readout.v)}"]` : readout.lo != null ? `[data-col][data-lo="${CSS.escape(readout.lo)}"]` : "[data-none]");
    if (bar) chart.querySelector("[data-readout]").textContent = bar.getAttribute("aria-label");
  }

  function renderEntry(failure = "") {
    entryEl.hidden = !entering();
    entryEl.classList.remove("previewing");
    painted.clear();
    if (!entering()) {
      entryEl.innerHTML = "";
      return;
    }
    if (failure || !profile) {
      entryEl.innerHTML = failure ? `<p class="fq-state">${esc(failure)}</p>` : "";
      return;
    }
    const back = new URLSearchParams(stateEntries({ ...s, entry: null }));
    const focused = document.activeElement?.closest?.("[data-bar], [data-col]");
    const at = focused && entryEl.contains(focused) ? [focused.closest(".fq-chart").dataset.key, focused.dataset.v ?? null, focused.dataset.lo ?? null] : null;
    plan = planOf(profile);
    const names = keyNames(profile.group);
    entryEl.innerHTML = `<a class="fq-back" href="#${esc(back)}" data-e="back"><span aria-hidden="true">← </span>${phrase("frequency.back", { pattern: `<b>${esc(s.pattern)}</b>`, catalog: esc(s.catalog) })}</a>
      <h2 class="fq-entry-name">${esc(groupText(profile.group))}${names ? `<small>${esc(names)}</small>` : ""}</h2>
      <p class="fq-entry-figures" data-e="figures">${esc(figuresText(profile))}</p>
      <div class="fq-profile" data-e="profile">${profile.charts.map(chartHtml).join("")}</div>
      <section class="fq-lines" data-e="lines">${linesHtml(lined, false)}</section>`;
    fitLabels();
    showReadout();
    if (at) {
      const chart = entryEl.querySelector(`.fq-chart[data-key="${CSS.escape(at[0])}"]`);
      const sel = at[1] != null ? `[data-bar][data-v="${CSS.escape(at[1])}"]` : `[data-col][data-lo="${CSS.escape(at[2])}"]`;
      chart?.querySelector(sel)?.focus({ preventScroll: true });
    }
  }

  async function loadEntry(mine, signal) {
    entryEl.setAttribute("aria-busy", "true");
    if (profile?.entry !== s.entry) {
      profile = null;
      lined = null;
      readout = null;
      opened.clear();
      entryEl.innerHTML = "";
    }
    let p, l;
    try {
      [p, l] = await Promise.all([api("frequency/profile", entryParams(s.w), signal), api("frequency/lines", { ...entryParams(s.w), limit: LINES }, signal)]);
    } catch (error) {
      if (mine === gen) {
        renderEntry(error.message);
        entryEl.removeAttribute("aria-busy");
      }
      return;
    }
    if (mine !== gen) return;
    cancelPreview();
    preview = null;
    previewed = null;
    const bad = [p, l].find((x) => x.status !== "ok");
    if (bad) {
      profile = null;
      lined = null;
      renderEntry(bad.status === "unsupported" ? reasonText(bad) : t("views.unavailable", { catalog: s.catalog, corpus: s.corpus }));
    } else {
      profile = { ...p, entry: s.entry };
      lined = { total: l.total, lines: l.lines, analysis_identity: l.analysis_identity, catalog_fingerprint: l.catalog_fingerprint };
      renderEntry();
    }
    renderClause();
    entryEl.removeAttribute("aria-busy");
  }

  const painted = new Set();
  function paintChart(c) {
    const body = entryEl.querySelector(`.fq-chart[data-key="${CSS.escape(c.key)}"] [data-body]`);
    if (!body) return;
    const f = document.activeElement, at = body.contains(f) ? [f.hasAttribute("data-none"), f.dataset.v, f.dataset.lo] : null;
    body.innerHTML = bodyHtml(c);
    fitLabels(body);
    if (at) body.querySelector(at[0] ? "[data-none]" : at[1] != null ? `[data-bar][data-v="${CSS.escape(at[1])}"]` : `[data-col][data-lo="${CSS.escape(at[2])}"]`)?.focus({ preventScroll: true });
  }
  const committedChart = (key) => profile?.charts.find((c) => c.key === key);

  function startPreview(key, w) {
    const id = JSON.stringify([key, w]);
    if (preview?.id === id) return;
    if (JSON.stringify(s.w[key] ?? null) === JSON.stringify(w)) return endPreview();
    const linesEl = entryEl.querySelector("[data-e=lines]");
    if (!preview && linesEl) linesEl.style.minHeight = `${linesEl.offsetHeight}px`;
    cancelPreview();
    const controller = new AbortController();
    preview = { id, key, controller };
    if (painted.delete(key) && committedChart(key)) {
      paintChart(committedChart(key));
      showReadout();
    }
    const mine = gen, cond = { ...s.w, [key]: w }, keep = lined.lines.length, signal = controller.signal;
    preview.timer = setTimeout(() => Promise.all([api("frequency/profile", entryParams(cond), signal), keep ? api("frequency/lines", { ...entryParams(cond), limit: keep }, signal) : null]).then(
      ([p, l]) => {
        if (mine !== gen || preview?.id !== id || p.status !== "ok" || (l && l.status !== "ok")) return;
        previewed = id;
        for (const c of p.charts) {
          if (c.key === key) continue;
          paintChart(c);
          painted.add(c.key);
        }
        entryEl.querySelector("[data-e=figures]").textContent = figuresText(p);
        if (linesEl) linesEl.innerHTML = linesHtml({ total: p.count, lines: l?.lines.slice(0, keep) ?? [] }, true);
        entryEl.classList.add("previewing");
      },
      () => {},
    ), REST);
  }
  function cancelPreview() {
    clearTimeout(preview?.timer);
    preview?.controller.abort();
  }
  function endPreview() {
    if (!preview) return;
    const shown = previewed;
    cancelPreview();
    preview = null;
    previewed = null;
    entryEl.classList.remove("previewing");
    const linesEl = entryEl.querySelector("[data-e=lines]");
    if (shown && profile) {
      for (const key of painted) if (committedChart(key)) paintChart(committedChart(key));
      entryEl.querySelector("[data-e=figures]").textContent = figuresText(profile);
      if (linesEl) linesEl.innerHTML = linesHtml(lined, false);
    }
    painted.clear();
    if (linesEl) linesEl.style.minHeight = "";
  }
  const remember = (b) => (readout = { key: b.closest(".fq-chart").dataset.key, v: b.hasAttribute("data-none") ? null : b.dataset.v ?? null, lo: b.dataset.lo ?? null });
  function point(b) {
    const chart = b.closest(".fq-chart"), key = chart.dataset.key;
    remember(b);
    for (const el of entryEl.querySelectorAll("[data-readout]")) el.innerHTML = "&nbsp;";
    showReadout();
    if (b.hasAttribute("data-none")) endPreview();
    else if (b.hasAttribute("data-bar")) startPreview(key, { m: "in", v: [b.dataset.v] });
    else startPreview(key, { m: "range", lo: +b.dataset.lo, hi: +b.dataset.hi });
  }
  const pointed = (e) => e.target.closest?.("[data-bar], [data-col]");
  entryEl.addEventListener("pointerover", (e) => {
    const b = pointed(e);
    if (b) point(b);
    else if (!e.target.closest?.("[data-body]") && !entryEl.querySelector("[data-e=profile]")?.contains(document.activeElement)) endPreview();
  });
  entryEl.addEventListener("pointerleave", () => {
    if (!entryEl.querySelector("[data-e=profile]")?.contains(document.activeElement)) endPreview();
  });
  entryEl.addEventListener("focusin", (e) => {
    const b = pointed(e);
    if (b) point(b);
  });
  entryEl.addEventListener("focusout", (e) => {
    const profileEl = entryEl.querySelector("[data-e=profile]");
    if (!profileEl?.contains(e.relatedTarget) && !profileEl?.matches(":hover")) endPreview();
  });
  entryEl.addEventListener("keydown", (e) => {
    const b = pointed(e);
    if (!b || !["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(e.key)) return;
    const all = [...b.closest("[data-body]").querySelectorAll("[data-bar], [data-col]")], i = all.indexOf(b);
    all[Math.max(0, Math.min(all.length - 1, i + (e.key === "ArrowDown" || e.key === "ArrowRight" ? 1 : -1)))].focus();
    e.preventDefault();
  });
  entryEl.addEventListener("toggle", (e) => {
    const key = e.target.closest?.(".fq-chart")?.dataset.key;
    if (key) e.target.open ? opened.add(key) : opened.delete(key);
  }, true);
  // The reader shows a line's occurrence in its document. `read` names the
  // occurrence as `sentence.start.end` and `scope` its paragraph scope, so
  // the address reopens the reader; opening from a line adds a history entry,
  // which Back removes. The lines stay drawn behind the modal dialog, which
  // keeps their scroll and pages; closing it focuses the line it was opened
  // from.
  const readerEl = root.querySelector("[data-fq-reader]"), readerBody = readerEl.querySelector(".reading-body");
  // `readerEntry` is "pushed" while the current history entry is the one
  // opening the reader added, and "popping" from a close until the Back that
  // removes that entry is traversed; a line opened meanwhile waits in `queued`.
  let readerEntry = null, queued = null, shownLine = null;
  function showReader() {
    const found = /^(\d+)\.(\d+)\.(\d+)$/.exec(s.read ?? "");
    if (!found || !s.corpus) return;
    shownLine = found;
    readerEl.querySelector("[data-fq-reader-title]").textContent = t("frequency.reader_title");
    readerEl.querySelector("[data-fq-return]").textContent = `← ${t("frequency.reader_return")}`;
    readerBody.innerHTML = `<p>${esc(t("frequency.reader_loading"))}</p>`;
    if (!readerEl.open) readerEl.showModal();
    const [, sentence, start, end] = found, scope = s.scope === "paragraph" ? "paragraph" : "document";
    htmx.trigger(readerBody, "htmx:abort");
    htmx.ajax("GET", `api/read/${encodeURIComponent(s.corpus)}/${sentence}/${start}/${end}/${scope}`, { source: readerBody, target: readerBody, swap: "innerHTML" });
  }
  function openReader(line) {
    if (readerEntry === "popping") {
      queued = line;
      return;
    }
    const { sentence, start, end } = line.dataset;
    s.read = `${sentence}.${start}.${end}`;
    s.scope = null;
    const address = `#${new URLSearchParams(stateEntries(s))}`;
    if (readerEntry === "pushed") history.replaceState(null, "", address);
    else history.pushState(null, "", address);
    readerEntry = "pushed";
    showReader();
  }
  readerBody.addEventListener("htmx:beforeRequest", (e) => {
    const scope = /\/(paragraph|document)(?:\?|$)/.exec(e.detail.pathInfo?.requestPath ?? "")?.[1];
    if (!scope || !s.read) return;
    s.scope = scope === "paragraph" ? scope : null;
    history.replaceState(history.state, "", `#${new URLSearchParams(stateEntries(s))}`);
  });
  readerEl.querySelector("[data-fq-return]").addEventListener("click", () => readerEl.close());
  readerEl.addEventListener("close", () => {
    if (readerEl.open) return;
    htmx.trigger(readerBody, "htmx:abort");
    readerBody.replaceChildren();
    if (s.read != null) {
      s.read = null;
      s.scope = null;
      if (readerEntry === "pushed") {
        readerEntry = "popping";
        history.back();
      } else history.replaceState(history.state, "", `#${new URLSearchParams(stateEntries(s))}`);
    }
    if (!shownLine) return;
    const [, sentence, start, end] = shownLine;
    shownLine = null;
    entryEl.querySelector(`.fq-kwic li[data-sentence="${sentence}"][data-start="${start}"][data-end="${end}"] .kn`)?.focus({ preventScroll: true });
  });

  function leaveEntry() {
    lastEntry = s.entry;
    set({ entry: null });
  }
  entryEl.addEventListener("click", async (e) => {
    if (e.target.closest("[data-e=back]")) {
      e.preventDefault();
      leaveEntry();
      return;
    }
    const more = e.target.closest("[data-e=more]");
    if (more) {
      more.disabled = true;
      const into = lined;
      await paging.next(
        (signal) => api("frequency/lines", { ...entryParams(s.w), offset: into.lines.length, limit: LINES }, signal),
        () => into === lined,
        (l) => into.lines.push(...l.lines),
        () => {
          const linesEl = entryEl.querySelector("[data-e=lines]");
          if (lined && linesEl) linesEl.innerHTML = linesHtml(lined, false);
        },
      );
      return;
    }
    if (e.target.closest(".fq-inspect")) return;
    const line = e.target.closest(".fq-kwic li[data-sentence]");
    if (line && !entryEl.classList.contains("previewing") && getSelection().isCollapsed) {
      openReader(line);
      return;
    }
    const b = pointed(e);
    if (b) remember(b);
    if (!b || b.hasAttribute("data-none")) return;
    const key = b.closest(".fq-chart").dataset.key, w = s.w[key];
    let next;
    if (b.hasAttribute("data-bar")) {
      const chosen = new Set(w?.v ?? []), v = b.dataset.v;
      chosen.has(v) ? chosen.delete(v) : chosen.add(v);
      next = { m: w?.m === "out" ? "out" : "in", v: [...chosen].sort() };
    } else {
      const lo = +b.dataset.lo, hi = +b.dataset.hi;
      if (e.shiftKey && anchor?.key === key) next = { m: "range", lo: Math.min(anchor.lo, lo), hi: Math.max(anchor.hi, hi) };
      else next = w?.lo === lo && w?.hi === hi ? { m: "range" } : { m: "range", lo, hi };
      anchor = { key, lo, hi };
    }
    cancelPreview();
    preview = null;
    previewed = null;
    set({ w: { ...s.w, [key]: next } });
  });

  const params = () => ({ corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, w: s.where == null ? conditions() : undefined, where: s.where ?? undefined, sort: s.sort, q: s.q, k: Object.keys(keyEquals()).length ? JSON.stringify(keyEquals()) : undefined, limit: PAGE });

  function renderAll() {
    const r = resolve();
    stateEl.textContent = r.state ?? queryMessage?.() ?? "";
    if (r.sentence) renderSentence();
    else sentenceEl.innerHTML = r.state && meta.pairs.length && !s.bad ? sentenceFallback() : "";
    renderFigures();
    renderClause();
    renderSource();
    renderTable();
    if (!entering() || profile?.entry !== s.entry) renderEntry();
    if (s.entry != null && s.where != null) stateEl.textContent = t("frequency.entry_needs_controls");
    return r;
  }
  function sentenceFallback() {
    const catalog = t("views.role_catalog");
    const catalogs = [[`${s.corpus}\n${s.catalog}`, s.catalog], ...meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, `${x.catalog} (${x.corpus_name})`])];
    return blank(role(catalog), select("catalog", catalog, catalogs, `${s.corpus}\n${s.catalog}`));
  }

  async function load({ keepSentence = false } = {}) {
    const mine = ++gen;
    const signal = inflight.next();
    queryMessage = null;
    paging.cancel();
    if (!keepSentence) answer = null;
    const r = keepSentence ? resolve() : renderAll();
    showCorpus(root, meta, s.corpus);
    picker.reloadValues();
    if (!r.query) return;
    const entryLoad = entering() && (!keepSentence || entryEl.hasAttribute("aria-busy")) ? loadEntry(mine, signal) : null;
    if (!entering()) {
      profile = null;
      lined = null;
    }
    let reply;
    try {
      reply = await api("frequency", { ...params(), limit: lastEntry ? Math.max(PAGE, depth) : PAGE }, signal);
    } catch (error) {
      if (mine === gen) {
        queryMessage = () => error.message;
        stateEl.textContent = queryMessage();
      }
      return;
    }
    if (mine !== gen) return;
    if (reply.status === "ok") {
      answer = reply;
      shown = reply.rows.length;
      stateEl.textContent = s.entry != null && s.where != null ? t("frequency.entry_needs_controls") : "";
    } else {
      answer = null;
      queryMessage = () => reply.status === "unsupported" ? reasonText(reply) : t("views.unavailable", { catalog: s.catalog, corpus: s.corpus });
      stateEl.textContent = queryMessage();
    }
    if (keepSentence) annotate();
    else renderSentence();
    renderFigures();
    renderClause();
    renderTable();
    picker.refresh();
    await entryLoad;
  }

  addEventListener("popstate", async () => {
    if (readerEntry === "popping") {
      readerEntry = null;
      const line = queued;
      queued = null;
      if (line) openReader(line);
      return;
    }
    readerEntry = null;
    const next = readState(location.hash);
    const view = (state) => JSON.stringify(stateEntries({ ...state, read: null, scope: null }));
    const reread = view(next) === view(s);
    if (entering() && next.entry == null) lastEntry = s.entry;
    s = next;
    if (readerEl.open && s.read == null) readerEl.close();
    if (reread) {
      if (s.read != null) showReader();
      return;
    }
    picker.close();
    editing = false;
    await load();
    if (s.read != null) showReader();
  });
  const relocalize = () => {
    renderAll();
    renderEntry();
    if (answer) {
      renderSentence();
      renderFigures();
      renderClause();
      renderTable();
    }
  };

  return (async () => {
    meta = await loadMeta(stateEl);
    if (!meta) return;
    onLocaleChange(relocalize);
    await load();
    if (s.read != null) showReader();
  })();
}

const root = typeof document !== "undefined" && document.querySelector("[data-frequency]");
if (root) mountFrequency(root);
