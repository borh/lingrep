import { currentLocale, onLocaleChange, t } from "./i18n.js";
import { createConditionPicker } from "./condition-picker.js";
import { count as num, fixed, places, significant } from "./numbers.js";
import { hashHolds, shortForms } from "./frequency.js";
import { api, conditionText, esc, fieldName, fullStop, isAny, latestRequest, loadMeta, pager, phrase, reasonText, redrawKeepingFocus, scopeDocuments, showCorpus } from "./views.js";

const PAGE = 100;
const SCOPES = ["where", "versus"];
const CONDITIONS = { where: "w", versus: "v" };
const KEYS = ["corpus", "catalog", "pattern", "w", "v", "where", "versus"];

const ROLE = { where: "compare.role_where", versus: "compare.role_versus" };
const RELATION = { disjoint: "compare.relation_disjoint", overlapping: "compare.relation_overlapping", identical: "compare.relation_identical" };


export function readState(hash) {
  const h = new URLSearchParams(hash.replace(/^#/, ""));
  const s = { corpus: h.get("corpus"), catalog: h.get("catalog"), pattern: h.get("pattern"), where: h.get("where"), versus: h.get("versus"), w: {}, v: {}, bad: false };
  for (const k of ["w", "v"]) {
    if (!h.get(k)) continue;
    try {
      const w = JSON.parse(h.get(k));
      if (w && typeof w === "object" && !Array.isArray(w)) s[k] = w;
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
    if (v === null || v === undefined || v === "" || ((k === "w" || k === "v") && !Object.keys(v).length)) continue;
    out.push([k, k === "w" || k === "v" ? JSON.stringify(v) : String(v)]);
  }
  return out;
}

export function commandHtml(command) {
  let html = "", at = 0;
  for (const found of command.matchAll(/ (--where|--versus) '(?:[^']|'\\'')*'/g)) {
    html += `${esc(command.slice(at, found.index))} <span class="where">${esc(found[0].slice(1))}</span>`;
    at = found.index + found[0].length;
  }
  return html + esc(command.slice(at));
}

export function mountCompare(root) {
  const $ = (sel) => root.querySelector(sel);
  const sentenceEl = $("[data-cmp-sentence]"), scopesEl = $("[data-cmp-scopes]"), clauseEl = $("[data-cmp-clause]");
  const stateEl = $("[data-cmp-state]"), tableEl = $("[data-cmp-table]");
  const narrow = matchMedia("(max-width: 40rem)");
  narrow.addEventListener("change", () => renderTable());
  let meta = null, s = readState(location.hash), answer = null, gen = 0, editing = false;
  const inflight = latestRequest(), paging = pager((message) => (stateEl.textContent = message));
  const cancel = () => {
    inflight.abort();
    paging.cancel();
  };

  const pair = () => meta.pairs.find((p) => p.corpus === s.corpus && p.catalog === s.catalog);
  const dims = () => (pair() ? meta.dimensions[pair().corpus] ?? [] : []);
  const dim = (key) => dims().find((d) => d.key === key);
  const keyName = (key) => meta.labels?.[key]?.[currentLocale()] ?? fieldName(key);
  const conds = (scope) => s[CONDITIONS[scope]];
  const shownKeys = (scope) => picker.shownKeys(scope);
  const encoded = (w) => (Object.keys(w).length ? JSON.stringify(w) : undefined);
  const picker = createConditionPicker({
    sentence: sentenceEl,
    view: (scope) => ({ dimensions: dims(), conditions: conds(scope), params: { corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, w: encoded(conds(scope)) }, revision: gen }),
    keyName,
    label: (scope, key) => key == null ? t("compare.add_label", { scope: t(ROLE[scope]) }) : t("compare.in_scope", { scope: t(ROLE[scope]), text: keyName(key) }),
    anchor: (scope, key) => sentenceEl.querySelector(key == null ? `[data-add][data-scope="${scope}"]` : `.pick[data-scope="${scope}"][data-key="${CSS.escape(key)}"]`),
    commit: (scope, key, w) => set({ [CONDITIONS[scope]]: { ...conds(scope), [key]: w } }),
    render: renderSentence,
  });
  function resolve() {
    if (!meta.pairs.length) return { state: t("views.no_tables") };
    if (s.bad) return { state: t("views.reason_address") };
    const first = meta.pairs[0];
    s.corpus ??= first.corpus;
    s.catalog ??= (meta.pairs.find((p) => p.corpus === s.corpus) ?? first).catalog;
    const p = pair();
    if (!p) return { state: t("views.unavailable", { catalog: s.catalog, corpus: meta.pairs.find((x) => x.corpus === s.corpus)?.corpus_name ?? s.corpus }) };
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

  const blank = (role, inner, cls = "") => `<span class="blank${cls ? ` ${cls}` : ""}"><span class="role"><span class="nm">${esc(role)}</span></span>${inner}</span>`;
  const select = (b, label, options, value) =>
    `<select data-b="${b}" aria-label="${esc(label)}">${options.map(([v, t]) => `<option value="${esc(v)}"${String(v) === String(value ?? "") ? " selected" : ""}>${esc(t)}</option>`).join("")}</select>`;
  const isOpen = (scope, key) => picker.isOpen(key, scope);

  function scopeHtml(scope) {
    const swatch = `<span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span><span class="cmp-scope-name">${esc(t(ROLE[scope]))}</span>`;
    if (s[scope] != null) return `<span class="cmp-scope" data-scope="${scope}">${swatch}${esc(t("compare.raw_scope"))}</span>`;
    const items = shownKeys(scope).map((key) => {
      const d = dim(key), w = conds(scope)[key];
      return {
        name: keyName(key),
        any: isAny(d, w),
        html: blank(keyName(key), `<button class="pick" data-scope="${scope}" data-key="${esc(key)}" aria-haspopup="dialog" aria-expanded="${isOpen(scope, key)}">${esc(conditionText(d, w, keyName(key)))}</button>`),
      };
    });
    const free = shownKeys(scope).length < dims().length;
    const add = free ? `<button class="add" data-scope="${scope}" data-add aria-haspopup="menu" aria-expanded="${isOpen(scope, null)}" aria-label="${esc(t("compare.add_label", { scope: t(ROLE[scope]) }))}">${esc(t("views.add"))}</button>` : "";
    return `<span class="cmp-scope" data-scope="${scope}">${swatch}${scopeDocuments(items) || esc(t("compare.scope_all"))}${add}</span>`;
  }

  function renderSentence() {
    const p = pair(), pattern = t("views.role_pattern"), catalog = t("views.role_catalog");
    const catalogs = meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, meta.pairs.some((y) => y.corpus !== x.corpus) ? `${x.catalog} (${x.corpus_name})` : x.catalog]);
    const control = (held) => {
      if (held.dataset.b) return `[data-b="${held.dataset.b}"]`;
      if (held.hasAttribute("data-add")) return `[data-add][data-scope="${held.dataset.scope}"]`;
      return held.dataset.key != null ? `.pick[data-scope="${held.dataset.scope}"][data-key="${CSS.escape(held.dataset.key)}"]` : null;
    };
    redrawKeepingFocus(sentenceEl, control, () => {
      sentenceEl.innerHTML = phrase("compare.sentence", {
        pattern: blank(pattern, select("pattern", pattern, p.patterns.map((x) => [x.name, x.name]), s.pattern), "lead"),
        catalog: blank(catalog, select("catalog", catalog, catalogs, `${s.corpus}\n${s.catalog}`)),
        a: scopeHtml("where"),
        b: scopeHtml("versus"),
        stop: fullStop(),
      });
    });
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
      r.title = "";
    });
    const at = roles.map((r) => r.getBoundingClientRect()), edge = sentenceEl.getBoundingClientRect().right;
    roles.forEach((r, i) => {
      const a = at[i], next = at[i + 1];
      const limit = (next && Math.abs(next.top - a.top) < 2 && next.left > a.left ? next.left - 8 : edge) - a.left;
      if (r.getBoundingClientRect().width <= limit) return;
      r.title = full[i];
      for (const form of shortForms(full[i], full)) {
        names[i].textContent = form;
        if (r.getBoundingClientRect().width <= limit) return;
      }
      names[i].textContent = full[i];
      r.classList.add("cut");
      r.style.maxWidth = `${Math.max(0, Math.floor(limit))}px`;
    });
  }

  function renderScopes() {
    if (!answer) {
      scopesEl.innerHTML = "";
      return;
    }
    const line = (scope) => {
      const a = answer[scope];
      return `<li data-scope="${scope}"><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span><b>${esc(t(ROLE[scope]))}</b> ${esc(t("compare.scope_figures", { documents: num(a.documents), tokens: num(a.tokens), occurrences: num(a.occurrences), works: num(a.works) }))}</li>`;
    };
    const notes = [t(RELATION[answer.relation], { documents: num(answer.shared.documents), tokens: num(answer.shared.tokens) })];
    for (const scope of SCOPES) if (!answer[scope].tokens) notes.push(t("compare.empty_scope", { scope: t(ROLE[scope]) }));
    notes.push(t("compare.caution"));
    scopesEl.innerHTML = `<ul class="cmp-figures">${SCOPES.map(line).join("")}</ul><p class="cmp-relation" data-relation="${esc(answer.relation)}">${notes.map(esc).join(" ")}</p>`;
  }

  function renderClause() {
    if (editing) {
      const text = (scope) => esc(s[scope] ?? answer?.[scope]?.clause ?? "");
      clauseEl.innerHTML = `<div class="edit cmp-edit">${SCOPES.map((scope) => `<label><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span>${esc(t(ROLE[scope]))} <code>--${scope}</code><textarea data-c="${scope}" aria-label="--${scope}" spellcheck="false">${text(scope)}</textarea></label>`).join("")}<div class="actions"><button data-c="apply">${esc(t("views.apply"))}</button><button data-c="cancel">${esc(t("views.cancel"))}</button></div></div>`;
      return;
    }
    const raw = s.where != null || s.versus != null;
    if (!answer && !raw) {
      clauseEl.innerHTML = "";
      return;
    }
    const command = answer?.command;
    clauseEl.innerHTML = `<div class="clause">${command ? `<code data-c="code">${commandHtml(command)}</code><button data-c="copy">${esc(t("views.copy"))}</button>` : ""}<button data-c="edit">${esc(t("compare.edit"))}</button>${raw ? `<button data-c="blanks">${esc(t("views.use_controls"))}</button>` : ""}</div>${answer ? `<p>${esc(command ? t("views.command_context") : t("views.command_unavailable"))}</p>` : ""}`;
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
      const text = (scope) => clauseEl.querySelector(`textarea[data-c="${scope}"]`).value.trim() || null;
      const where = text("where"), versus = text("versus");
      editing = false;
      set({ where, versus, w: {}, v: {} });
    } else if (c === "blanks") set({ where: null, versus: null });
  });

  function ratioCell(r, extent) {
    const x = r.log2_ratio;
    const one = r.where.count === 0 ? "versus" : r.versus.count === 0 ? "where" : null;
    if (x == null && one == null) return '<td class="cmp-ratio"></td>';
    const toward = x == null ? one : x > 0 ? "where" : x < 0 ? "versus" : null;
    const width = x == null ? 50 : Math.max(0.5, (Math.abs(x) / extent) * 50);
    const factor = (high, low) => t("compare.ratio", { x: significant(2 ** Math.abs(x)), high: t(ROLE[high]), low: t(ROLE[low]) });
    const label = x == null ? t("compare.only", { scope: t(ROLE[one]) }) : x === 0 ? t("compare.equal") : x > 0 ? factor("where", "versus") : factor("versus", "where");
    const bar = toward ? `<i data-scope="${toward}"${x == null ? " data-only" : ""} style="width:${width.toFixed(1)}%"></i>` : "";
    return `<td class="cmp-ratio" title="${esc(label)}"><span class="cmp-axis" role="img" aria-label="${esc(label)}">${bar}</span></td>`;
  }

  function renderTable() {
    if (!answer) {
      tableEl.innerHTML = "";
      return;
    }
    if (!answer.rows.length) {
      tableEl.innerHTML = `<p class="fq-empty">${esc(t("compare.empty"))}</p>`;
      return;
    }
    const finite = answer.rows.map((r) => Math.abs(r.log2_ratio ?? 0));
    const extent = Math.max(1, ...finite);
    const pms = SCOPES.map((scope) => places(answer.rows.map((r) => r[scope].per_million ?? 0)));
    const g2places = places(answer.rows.map((r) => r.g2 ?? 0));
    const fmt = (n, p) => (n == null ? "–" : fixed(n, p));
    const wide = !narrow.matches;
    const side = (r, scope, i) =>
      wide
        ? `<td class="fig r" data-scope="${scope}">${num(r[scope].count)}</td><td class="fig r"><b>${fmt(r[scope].per_million, pms[i])}</b></td><td class="fig r">${num(r[scope].works)}</td>`
        : `<td class="fig r" data-scope="${scope}"><b>${fmt(r[scope].per_million, pms[i])}</b></td>`;
    const group = (scope) => `<th colspan="${wide ? 3 : 1}" class="cmp-group" data-scope="${scope}"><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span>${esc(t(ROLE[scope]))}</th>`;
    const sub = wide ? `<th class="r">${esc(t("views.head_occurrences"))}</th><th class="r">${esc(t("compare.head_per_million"))}</th><th class="r">${esc(t("views.head_documents"))}</th>` : `<th class="r">${esc(t("compare.head_per_million"))}</th>`;
    const names = (g) => (g.kind === "key" && g.fields.length > 1 ? g.fields.map((f) => f.name).join(" ") : "");
    const rows = answer.rows
      .map((r) => {
        const n = names(r.group);
        return `<tr><td class="fig r">${num(r.rank)}</td><td class="entry-cell">${esc(r.text)}${n ? `<small>${esc(n)}</small>` : ""}</td>${side(r, "where", 0)}${side(r, "versus", 1)}${ratioCell(r, extent)}<td class="fig r cmp-g2">${fmt(r.g2, g2places)}</td></tr>`;
      })
      .join("");
    tableEl.innerHTML = `<p class="cmp-count-line">${esc(t("compare.entries", { n: num(answer.entries), shown: num(answer.rows.length) }))}</p><div class="cmp-wrap"><table class="freq cmp-table"><thead><tr><th rowspan="2" class="r">${esc(t("views.head_rank"))}</th><th rowspan="2">${esc(t("views.head_entry"))}</th>${group("where")}${group("versus")}<th rowspan="2" class="cmp-ratio-head"><span data-scope="where">${esc(t(ROLE.where))}</span>${esc(t("compare.head_ratio"))}<span data-scope="versus">${esc(t(ROLE.versus))}</span></th><th rowspan="2" class="r">${esc(t("compare.head_g2"))}</th></tr><tr>${sub}${sub}</tr></thead><tbody>${rows}</tbody></table></div><button class="more"${answer.rows.length >= answer.entries ? " hidden" : ""}${paging.pending || root.hasAttribute("aria-busy") ? " disabled" : ""}>${esc(t("views.more", { n: PAGE }))}</button>`;
  }
  tableEl.addEventListener("click", async (e) => {
    const button = e.target.closest(".more");
    if (!button || button.disabled || !answer) return;
    const mine = gen;
    button.disabled = true;
    await paging.next(
      (signal) => api("contrast", { ...params(), offset: answer.rows.length }, signal),
      () => mine === gen,
      (more) => answer.rows.push(...more.rows),
      () => {
        if (mine === gen) renderTable();
      },
    );
  });

  sentenceEl.addEventListener("change", (e) => {
    const b = e.target.dataset.b;
    if (b === "catalog") {
      const [corpus, catalog] = e.target.value.split("\n");
      const same = corpus === s.corpus;
      set({ corpus, catalog, pattern: null, w: same ? s.w : {}, v: same ? s.v : {} });
    } else if (b === "pattern") set({ pattern: e.target.value });
  });
  sentenceEl.addEventListener("click", (e) => {
    const button = e.target.closest("button[data-scope]");
    if (!button) return;
    e.stopPropagation();
    const o = { scope: button.dataset.scope, key: button.hasAttribute("data-add") ? null : button.dataset.key };
    if (picker.isOpen(o.key, o.scope)) return picker.close();
    picker.open(o.key, o.scope);
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && picker.opened) picker.close();
  });
  addEventListener("resize", () => {
    fitRoles();
    picker.refresh();
  });

  const params = () => ({
    corpus: s.corpus,
    catalog: s.catalog,
    pattern: s.pattern,
    w: s.where == null ? encoded(s.w) : undefined,
    v: s.versus == null ? encoded(s.v) : undefined,
    where: s.where ?? undefined,
    versus: s.versus ?? undefined,
    limit: PAGE,
  });

  async function load() {
    const mine = ++gen;
    const r = resolve();
    showCorpus(root, meta, s.corpus);
    stateEl.textContent = r.state ?? "";
    if (r.sentence) renderSentence();
    else sentenceEl.innerHTML = "";
    if (!r.query) {
      cancel();
      answer = null;
      root.removeAttribute("aria-busy");
      renderScopes();
      renderClause();
      renderTable();
      return;
    }
    root.setAttribute("aria-busy", "true");
    if (answer) {
      const line = tableEl.querySelector(".cmp-count-line"), more = tableEl.querySelector(".more");
      if (line) line.textContent = t("compare.ranking");
      if (more) more.disabled = true;
    } else {
      scopesEl.innerHTML = `<p class="cmp-figures">${esc(t("compare.ranking"))}</p>`;
    }
    let reply;
    cancel();
    try {
      reply = await api("contrast", params(), inflight.next());
    } catch (error) {
      if (mine === gen) {
        answer = null;
        root.removeAttribute("aria-busy");
        renderScopes();
        renderClause();
        renderTable();
        stateEl.textContent = error.name === "AbortError" ? "" : error.message;
      }
      return;
    }
    if (mine !== gen) return;
    root.removeAttribute("aria-busy");
    if (reply.status === "ok") {
      answer = reply;
      stateEl.textContent = "";
    } else {
      answer = null;
      stateEl.textContent = reply.status === "unsupported" ? t("compare.in_scope", { scope: t(ROLE[reply.scope]), text: reasonText(reply) }) : t("views.unavailable", { catalog: s.catalog, corpus: s.corpus });
    }
    renderScopes();
    renderClause();
    renderTable();
    picker.refresh();
  }

  addEventListener("popstate", () => {
    s = readState(location.hash);
    picker.close();
    editing = false;
    load();
  });
  const relocalize = () => {
    const r = resolve();
    stateEl.textContent = r.state ?? stateEl.textContent;
    if (r.sentence) renderSentence();
    renderScopes();
    renderClause();
    renderTable();
  };

  return (async () => {
    meta = await loadMeta(stateEl);
    if (!meta) return;
    onLocaleChange(relocalize);
    await load();
  })();
}

const root = typeof document !== "undefined" && document.querySelector("[data-compare]");
if (root) mountCompare(root);
