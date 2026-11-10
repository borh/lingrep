import { currentLocale, onLocaleChange } from "./i18n.js";
import { createConditionPicker } from "./condition-picker.js";
import { hashHolds, shortForms } from "./frequency.js";

const PAGE = 100;
const SCOPES = ["where", "versus"];
const CONDITIONS = { where: "w", versus: "v" };
const KEYS = ["corpus", "catalog", "pattern", "w", "v", "where", "versus"];

const NUM = { en: new Intl.NumberFormat("en-US"), ja: new Intl.NumberFormat("ja-JP") };
const esc = (text) =>
  String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const orList = (xs, word) => (xs.length <= 1 ? xs.join("") : `${xs.slice(0, -1).join(", ")} ${word} ${xs.at(-1)}`);
const jaJoin = (xs) => xs.join(xs.some((x) => x.includes("・")) ? "、" : "・");

const S = {
  en: {
    role: { pattern: "pattern", catalog: "catalog", where: "A", versus: "B" },
    name: (key) => key.replace(/_/g, " "),
    any: (name) => `any ${name}`,
    values: (w) => (w.m === "out" ? `other than ${orList(w.v, "and")}` : w.v.length <= 3 ? orList(w.v, "or") : `${w.v.length} values`),
    range: (w) => (w.lo != null && w.hi != null ? `${w.lo}–${w.hi}` : w.lo != null ? `${w.lo} or more` : `${w.hi} or less`),
    scope: (items) => (items.length ? `documents ${items.map((x) => (x.any ? `with ${x.html}` : `with ${esc(x.name)} ${x.html}`)).join(", ")}` : "all documents"),
    rawScope: "documents matching the clause below",
    sentence: (b) => `Compare ${b.P} in ${b.C} between ${b.A} and ${b.B}<span class="tail">.</span>`,
    add: "+ condition",
    addLabel: (scope) => `Add a condition to ${scope}`,
    scopeFigures: (docs, tokens, o, d) => `${docs} documents, ${tokens} tokens; ${o} occurrences in ${d} documents`,
    relation: {
      disjoint: () => "No document is in both scopes.",
      overlapping: (d, t) => `The scopes share ${d} documents (${t} tokens), which count on both sides: G² compares the scopes as given, not as independent samples.`,
      identical: () => "Both scopes admit the same documents, so every G² is 0.",
    },
    emptyScope: (scope) => `${scope} admits no tokens, so its rates and every G² are blank.`,
    caution: "G² is an exploratory score. It does not account for clustering within documents; compare each side’s document counts as well.",
    heads: { rank: "Rank", entry: "Entry", count: "Occurrences", pm: "Per million", docs: "Documents", ratio: "Rate ratio", g2: "G²" },
    ratio: (x, high, low) => `rate in ${high} is ${x}× the rate in ${low}`,
    only: (scope) => `only in ${scope}`,
    equal: "equal rates",
    entries: (n, shown) => `${shown} of ${n} entries, ranked by G²`,
    ranking: "Ranking entries…",
    more: (n) => `Show the next ${n}`,
    empty: "No entry occurs in either scope.",
    commandUnavailable: "A CLI command cannot be shown because the table, corpus and catalog paths have no usable common parent directory.",
    commandContext: "Run from the common parent directory of the table, corpus and catalog files.",
    copy: "Copy",
    copied: "Copied",
    edit: "Edit as clauses",
    apply: "Apply",
    cancel: "Cancel",
    blanks: "Use condition controls",
    reasons: {
      section: () => "@section cannot be answered here: a count table counts per document.",
      sentence: () => "@sentence keys and divisions cannot be answered here: a count table counts per document.",
      syntax: (r) => `The clause does not parse: ${r.message}`,
      unknown_key: (r) => `This corpus has no condition named “${r.key}”.`,
      unwritable: (r) => `“${r.value}” contains a double quote, which cannot be escaped in a scope clause.`,
      shape: (r) => `The condition on “${r.key}” does not fit its values.`,
      address: () => "The conditions in this URL cannot be read.",
    },
    inScope: (scope, text) => `${scope}: ${text}`,
    unavailable: (catalog, corpus) => `No count table is registered for ${catalog} over ${corpus}.`,
    unknownPattern: (pattern, catalog) => `${catalog} has no pattern “${pattern}”.`,
    noTables: "No count table is registered. Ask the server administrator to add one with ling serve --count-table.",
    failed: (status) => `Server response: ${status}.`,
  },
  ja: {
    role: { pattern: "パターン", catalog: "カタログ", where: "A", versus: "B" },
    name: (key) => key,
    any: (name) => name,
    values: (w) => (w.m === "out" ? `${jaJoin(w.v)}以外` : w.v.length <= 3 ? jaJoin(w.v) : `${w.v.length}件`),
    range: (w) => (w.lo != null && w.hi != null ? `${w.lo}–${w.hi}` : w.lo != null ? `${w.lo}以上` : `${w.hi}以下`),
    scope: (items) => {
      const parts = [];
      for (const x of items) {
        const last = parts.at(-1);
        if (x.any && last?.any) last.html.push(x.html);
        else parts.push({ any: x.any, html: [x.html], name: x.name });
      }
      return parts.length ? `${parts.map((p) => (p.any ? `${p.html.join("・")}を問わない` : `${esc(p.name)}が${p.html[0]}`)).join("、")}${parts.at(-1).any ? "" : "の"}文書` : "すべての文書";
    },
    rawScope: "下の条件式に合う文書",
    sentence: (b) => `${b.C}の${b.P}を、${b.A}と${b.B}で比べる<span class="tail">。</span>`,
    add: "＋ 条件",
    addLabel: (scope) => `${scope}に条件を追加`,
    scopeFigures: (docs, tokens, o, d) => `文書${docs}件、${tokens}語。文書${d}件に${o}件`,
    relation: {
      disjoint: () => "両方に含まれる文書はありません。",
      overlapping: (d, t) => `両方に文書${d}件（${t}語）が含まれ、両側で数えています。G²は指定どおりの二つの文書群を比べるもので、独立な標本の比較ではありません。`,
      identical: () => "二つの文書群は同じ文書からなるため、G²はすべて0です。",
    },
    emptyScope: (scope) => `${scope}に含まれる語がないため、その頻度とG²は空欄です。`,
    caution: "G²は探索のための指標です。文書内での用例の集中は計算に反映されません。両側の出現文書数もあわせて確認してください。",
    heads: { rank: "順位", entry: "項目", count: "出現数", pm: "100万語あたり", docs: "文書数", ratio: "頻度比", g2: "G²" },
    ratio: (x, high, low) => `${high}の頻度は${low}の${x}倍`,
    only: (scope) => `${scope}のみ`,
    equal: "頻度が同じ",
    entries: (n, shown) => `${n}項目中${shown}項目、G²の大きい順`,
    ranking: "順位を計算しています…",
    more: (n) => `次の${n}件を表示`,
    empty: "どちらの文書群にも項目がありません。",
    commandUnavailable: "入力パスを共通ディレクトリからの相対パスで表せないため、コピー用コマンドは表示できません。",
    commandContext: "テーブル・コーパス・カタログの各ファイルに共通する親ディレクトリで実行してください。",
    copy: "コピー",
    copied: "コピーしました",
    edit: "条件式として編集",
    apply: "適用",
    cancel: "取り消す",
    blanks: "条件の選択に戻す",
    reasons: {
      section: () => "@section はここでは扱えません。件数表は文書ごとの件数です。",
      sentence: () => "@sentence のキーと区分はここでは扱えません。件数表は文書ごとの件数です。",
      syntax: (r) => `条件式を解析できません：${r.message}`,
      unknown_key: (r) => `このコーパスに「${r.key}」という条件はありません。`,
      unwritable: (r) => `「${r.value}」は二重引用符を含むため条件式にできません。`,
      shape: (r) => `「${r.key}」の条件が値の種類に合いません。`,
      address: () => "このアドレスの条件を読み取れません。",
    },
    inScope: (scope, text) => `${scope}：${text}`,
    unavailable: (catalog, corpus) => `${corpus}の${catalog}には件数表が登録されていません。`,
    unknownPattern: (pattern, catalog) => `${catalog}にパターン「${pattern}」はありません。`,
    noTables: "件数表が登録されていません。サーバーの管理者に ling serve --count-table での登録を依頼してください。",
    failed: (status) => `サーバーの応答：${status}`,
  },
};

const L = () => S[currentLocale()] ?? S.en;
const num = (n) => (NUM[currentLocale()] ?? NUM.en).format(Number(n));

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

export const places = (xs) => {
  const min = Math.min(...xs.filter((x) => x > 0));
  return Number.isFinite(min) ? Math.max(0, Math.min(2, Math.ceil(-Math.log10(min)) + 1)) : 0;
};

async function api(path, params, signal) {
  const query = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== ""));
  const response = await fetch(`api/${path}?${query}`, { signal });
  if (!response.ok) {
    const reply = await response.json().catch(() => null);
    throw new Error(reply?.error ?? L().failed(response.status));
  }
  return response.json();
}

export function mountCompare(root) {
  const $ = (sel) => root.querySelector(sel);
  const sentenceEl = $("[data-cmp-sentence]"), scopesEl = $("[data-cmp-scopes]"), clauseEl = $("[data-cmp-clause]");
  const stateEl = $("[data-cmp-state]"), tableEl = $("[data-cmp-table]");
  const narrow = matchMedia("(max-width: 40rem)");
  narrow.addEventListener("change", () => renderTable());
  let meta = null, s = readState(location.hash), answer = null, gen = 0, editing = false;
  let inflight = null, paging = null;
  const cancel = () => {
    inflight?.abort();
    paging?.abort();
  };

  const pair = () => meta.pairs.find((p) => p.corpus === s.corpus && p.catalog === s.catalog);
  const dims = () => (pair() ? meta.dimensions[pair().corpus] ?? [] : []);
  const dim = (key) => dims().find((d) => d.key === key);
  const keyName = (key) => meta.labels?.[key]?.[currentLocale()] ?? L().name(key);
  const conds = (scope) => s[CONDITIONS[scope]];
  const shownKeys = (scope) => picker.shownKeys(scope);
  const encoded = (w) => (Object.keys(w).length ? JSON.stringify(w) : undefined);
  const picker = createConditionPicker({
    sentence: sentenceEl,
    view: (scope) => ({ dimensions: dims(), conditions: conds(scope), params: { corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, w: encoded(conds(scope)) }, revision: gen }),
    keyName,
    label: (scope, key) => key == null ? L().addLabel(L().role[scope]) : L().inScope(L().role[scope], keyName(key)),
    anchor: (scope, key) => sentenceEl.querySelector(key == null ? `[data-add][data-scope="${scope}"]` : `.pick[data-scope="${scope}"][data-key="${CSS.escape(key)}"]`),
    commit: (scope, key, w) => set({ [CONDITIONS[scope]]: { ...conds(scope), [key]: w } }),
    render: renderSentence,
    api,
  });
  const isAny = (d, w) => !w || (d.kind === "range" ? w.lo == null && w.hi == null : !w.v?.length);
  const conditionText = (d, w) => (isAny(d, w) ? L().any(keyName(d.key)) : d.kind === "range" ? L().range(w) : L().values(w));

  function resolve() {
    if (!meta.pairs.length) return { state: L().noTables };
    if (s.bad) return { state: L().reasons.address() };
    const first = meta.pairs[0];
    s.corpus ??= first.corpus;
    s.catalog ??= (meta.pairs.find((p) => p.corpus === s.corpus) ?? first).catalog;
    const p = pair();
    if (!p) return { state: L().unavailable(s.catalog, meta.pairs.find((x) => x.corpus === s.corpus)?.corpus_name ?? s.corpus) };
    s.pattern ??= p.patterns[0]?.name ?? null;
    if (!p.patterns.some((x) => x.name === s.pattern)) return { state: L().unknownPattern(s.pattern, s.catalog), sentence: true };
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
    const t = L();
    const swatch = `<span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span><span class="cmp-scope-name">${esc(t.role[scope])}</span>`;
    if (s[scope] != null) return `<span class="cmp-scope" data-scope="${scope}">${swatch}${esc(t.rawScope)}</span>`;
    const items = shownKeys(scope).map((key) => {
      const d = dim(key), w = conds(scope)[key];
      return {
        name: keyName(key),
        any: isAny(d, w),
        html: blank(keyName(key), `<button class="pick" data-scope="${scope}" data-key="${esc(key)}" aria-haspopup="dialog" aria-expanded="${isOpen(scope, key)}">${esc(conditionText(d, w))}</button>`),
      };
    });
    const free = shownKeys(scope).length < dims().length;
    const add = free ? `<button class="add" data-scope="${scope}" data-add aria-haspopup="menu" aria-expanded="${isOpen(scope, null)}" aria-label="${esc(t.addLabel(t.role[scope]))}">${esc(t.add)}</button>` : "";
    return `<span class="cmp-scope" data-scope="${scope}">${swatch}${t.scope(items)}${add}</span>`;
  }

  function renderSentence() {
    const t = L(), p = pair();
    const catalogs = meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, meta.pairs.some((y) => y.corpus !== x.corpus) ? `${x.catalog} (${x.corpus_name})` : x.catalog]);
    sentenceEl.innerHTML = t.sentence({
      P: blank(t.role.pattern, select("pattern", t.role.pattern, p.patterns.map((x) => [x.name, x.name]), s.pattern), "lead"),
      C: blank(t.role.catalog, select("catalog", t.role.catalog, catalogs, `${s.corpus}\n${s.catalog}`)),
      A: scopeHtml("where"),
      B: scopeHtml("versus"),
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
    const t = L();
    if (!answer) {
      scopesEl.innerHTML = "";
      return;
    }
    const line = (scope) => {
      const a = answer[scope];
      return `<li data-scope="${scope}"><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span><b>${esc(t.role[scope])}</b> ${esc(t.scopeFigures(num(a.documents), num(a.tokens), num(a.occurrences), num(a.works)))}</li>`;
    };
    const notes = [t.relation[answer.relation](num(answer.shared.documents), num(answer.shared.tokens))];
    for (const scope of SCOPES) if (!answer[scope].tokens) notes.push(t.emptyScope(t.role[scope]));
    notes.push(t.caution);
    scopesEl.innerHTML = `<ul class="cmp-figures">${SCOPES.map(line).join("")}</ul><p class="cmp-relation" data-relation="${esc(answer.relation)}">${notes.map(esc).join(" ")}</p>`;
  }

  function renderClause() {
    const t = L();
    if (editing) {
      const text = (scope) => esc(s[scope] ?? answer?.[scope]?.clause ?? "");
      clauseEl.innerHTML = `<div class="edit cmp-edit">${SCOPES.map((scope) => `<label><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span>${esc(t.role[scope])} <code>--${scope}</code><textarea data-c="${scope}" aria-label="--${scope}" spellcheck="false">${text(scope)}</textarea></label>`).join("")}<div class="actions"><button data-c="apply">${esc(t.apply)}</button><button data-c="cancel">${esc(t.cancel)}</button></div></div>`;
      return;
    }
    if (!answer) {
      clauseEl.innerHTML = "";
      return;
    }
    const command = answer.command;
    const raw = s.where != null || s.versus != null;
    const code = command ? esc(command).replace(/ (--where|--versus) ('(?:[^']|'\\'')*')/g, ' <span class="where">$1 $2</span>') : "";
    clauseEl.innerHTML = `<div class="clause">${command ? `<code data-c="code">${code}</code><button data-c="copy">${esc(t.copy)}</button>` : ""}<button data-c="edit">${esc(t.edit)}</button>${raw ? `<button data-c="blanks">${esc(t.blanks)}</button>` : ""}</div><p>${esc(command ? t.commandContext : t.commandUnavailable)}</p>`;
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
      e.target.textContent = L().copied;
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
    const t = L();
    const x = r.log2_ratio;
    const one = r.where.count === 0 ? "versus" : r.versus.count === 0 ? "where" : null;
    if (x == null && one == null) return '<td class="cmp-ratio"></td>';
    const toward = x == null ? one : x > 0 ? "where" : x < 0 ? "versus" : null;
    const width = x == null ? 50 : Math.max(0.5, (Math.abs(x) / extent) * 50);
    const factor = (high, low) => t.ratio(new Intl.NumberFormat(currentLocale() === "ja" ? "ja-JP" : "en-US", { maximumSignificantDigits: 3 }).format(2 ** Math.abs(x)), t.role[high], t.role[low]);
    const label = x == null ? t.only(t.role[one]) : x === 0 ? t.equal : x > 0 ? factor("where", "versus") : factor("versus", "where");
    const bar = toward ? `<i data-scope="${toward}"${x == null ? " data-only" : ""} style="width:${width.toFixed(1)}%"></i>` : "";
    return `<td class="cmp-ratio" title="${esc(label)}"><span class="cmp-axis" role="img" aria-label="${esc(label)}">${bar}</span></td>`;
  }

  function renderTable() {
    const t = L(), h = t.heads;
    if (!answer) {
      tableEl.innerHTML = "";
      return;
    }
    if (!answer.rows.length) {
      tableEl.innerHTML = `<p class="fq-empty">${esc(t.empty)}</p>`;
      return;
    }
    const finite = answer.rows.map((r) => Math.abs(r.log2_ratio ?? 0));
    const extent = Math.max(1, ...finite);
    const pms = SCOPES.map((scope) => places(answer.rows.map((r) => r[scope].per_million ?? 0)));
    const g2places = places(answer.rows.map((r) => r.g2 ?? 0));
    const fmt = (n, p) => (n == null ? "–" : new Intl.NumberFormat(currentLocale() === "ja" ? "ja-JP" : "en-US", { minimumFractionDigits: p, maximumFractionDigits: p }).format(n));
    const wide = !narrow.matches;
    const side = (r, scope, i) =>
      wide
        ? `<td class="fig r" data-scope="${scope}">${num(r[scope].count)}</td><td class="fig r"><b>${fmt(r[scope].per_million, pms[i])}</b></td><td class="fig r">${num(r[scope].works)}</td>`
        : `<td class="fig r" data-scope="${scope}"><b>${fmt(r[scope].per_million, pms[i])}</b></td>`;
    const group = (scope) => `<th colspan="${wide ? 3 : 1}" class="cmp-group" data-scope="${scope}"><span class="cmp-swatch" data-scope="${scope}" aria-hidden="true"></span>${esc(t.role[scope])}</th>`;
    const sub = wide ? `<th class="r">${esc(h.count)}</th><th class="r">${esc(h.pm)}</th><th class="r">${esc(h.docs)}</th>` : `<th class="r">${esc(h.pm)}</th>`;
    const names = (g) => (g.kind === "key" && g.fields.length > 1 ? g.fields.map((f) => f.name).join(" ") : "");
    const rows = answer.rows
      .map((r) => {
        const n = names(r.group);
        return `<tr><td class="fig r">${num(r.rank)}</td><td class="entry-cell">${esc(r.text)}${n ? `<small>${esc(n)}</small>` : ""}</td>${side(r, "where", 0)}${side(r, "versus", 1)}${ratioCell(r, extent)}<td class="fig r cmp-g2">${fmt(r.g2, g2places)}</td></tr>`;
      })
      .join("");
    tableEl.innerHTML = `<p class="cmp-count-line">${esc(t.entries(num(answer.entries), num(answer.rows.length)))}</p><div class="cmp-wrap"><table class="freq cmp-table"><thead><tr><th rowspan="2" class="r">${esc(h.rank)}</th><th rowspan="2">${esc(h.entry)}</th>${group("where")}${group("versus")}<th rowspan="2" class="cmp-ratio-head"><span data-scope="where">${esc(t.role.where)}</span>${esc(h.ratio)}<span data-scope="versus">${esc(t.role.versus)}</span></th><th rowspan="2" class="r">${esc(h.g2)}</th></tr><tr>${sub}${sub}</tr></thead><tbody>${rows}</tbody></table></div><button class="more"${answer.rows.length >= answer.entries ? " hidden" : ""}>${esc(t.more(PAGE))}</button>`;
  }
  tableEl.addEventListener("click", async (e) => {
    const button = e.target.closest(".more");
    if (!button || button.disabled || !answer) return;
    const mine = gen;
    button.disabled = true;
    paging = new AbortController();
    let more = null;
    try {
      more = await api("contrast", { ...params(), offset: answer.rows.length }, paging.signal);
    } catch (error) {
      if (mine === gen && error.name !== "AbortError") stateEl.textContent = error.message;
    }
    if (mine !== gen) return;
    if (more?.status !== "ok") {
      button.disabled = false;
      return;
    }
    answer.rows.push(...more.rows);
    renderTable();
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
      if (line) line.textContent = L().ranking;
      if (more) more.disabled = true;
    } else {
      scopesEl.innerHTML = `<p class="cmp-figures">${esc(L().ranking)}</p>`;
    }
    let reply;
    cancel();
    inflight = new AbortController();
    try {
      reply = await api("contrast", params(), inflight.signal);
    } catch (error) {
      if (mine === gen) {
        stateEl.textContent = error.name === "AbortError" ? "" : error.message;
        root.removeAttribute("aria-busy");
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
      const t = L();
      stateEl.textContent = reply.status === "unsupported" ? t.inScope(t.role[reply.scope], t.reasons[reply.reason]?.(reply) ?? reply.reason) : t.unavailable(s.catalog, s.corpus);
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
  onLocaleChange(() => {
    const r = resolve();
    stateEl.textContent = r.state ?? stateEl.textContent;
    if (r.sentence) renderSentence();
    renderScopes();
    renderClause();
    renderTable();
  });

  return (async () => {
    meta = await api("frequency/meta", {});
    await load();
  })();
}

const root = typeof document !== "undefined" && document.querySelector("[data-compare]");
if (root) mountCompare(root);
