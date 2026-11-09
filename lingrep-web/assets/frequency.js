import { currentLocale, onLocaleChange } from "./i18n.js";

const MANY = 30;
const PAGE = 100;
const LINES = 50;
// Milliseconds the pointer rests on a bar before its preview is requested.
const REST = 120;
const KEYS = ["corpus", "catalog", "pattern", "w", "where", "sort", "q", "entry"];

const NUM = { en: new Intl.NumberFormat("en-US"), ja: new Intl.NumberFormat("ja-JP") };
const esc = (text) =>
  String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

export const pctOf = (x, y) =>
  !y ? "0" : x && x / y < 0.01 ? "<1" : x < y && Math.round((x / y) * 100) === 100 ? (Math.floor((x / y) * 1000) / 10).toFixed(1) : String(Math.round((x / y) * 100));

const orList = (xs, word) => (xs.length <= 1 ? xs.join("") : `${xs.slice(0, -1).join(", ")} ${word} ${xs.at(-1)}`);
const jaJoin = (xs) => xs.join(xs.some((x) => x.includes("・")) ? "、" : "・");

const S = {
  en: {
    role: { pattern: "pattern", catalog: "catalog", order: "order", filter: "filter" },
    sorts: [["count", "most frequent first"], ["works", "most widespread first"], ["key", "in character-code order"]],
    name: (key) => key.replace(/_/g, " "),
    any: (name) => `any ${name}`,
    values: (w) => (w.m === "out" ? `other than ${orList(w.v, "and")}` : w.v.length <= 3 ? orList(w.v, "or") : `${w.v.length} values`),
    range: (w) => (w.lo != null && w.hi != null ? `${w.lo}–${w.hi}` : w.lo != null ? `${w.lo} or more` : `${w.hi} or less`),
    scope: (items) => (items.length ? `from documents ${items.map((x) => (x.any ? `with ${x.html}` : `with ${esc(x.name)} ${x.html}`)).join(", ")}` : ""),
    sentence: (b) => `Frequencies of ${b.P} in ${b.C}${b.scope ? `, ${b.scope}` : ""}; ${b.O}, containing ${b.Q}<span class="tail">.</span>`,
    rawScope: "where the clause below holds",
    add: "+ condition",
    addLabel: "Add a condition",
    figures: (e, o, d) => `${e} entries, ${o} occurrences, ${d} documents`,
    keeps: (x, y, p) => `${x} of ${y} (${p}%)`,
    missingField: (n, key) => `Among documents meeting the other conditions, ${n} ${n === "1" ? "document lacks" : "documents lack"} ${key} and ${n === "1" ? "is" : "are"} excluded.`,
    keepsEntries: (x, y, p) => `${x} of ${y} entries (${p}%)`,
    heads: ["Rank", "Entry", "Occurrences", "Documents", "Share of documents", "Per million tokens"],
    more: (n) => `Show the next ${n}`,
    empty: "No occurrence meets these conditions.",
    nomatch: (q) => `No entry contains “${q}”.`,
    placeholder: "anything",
    commandUnavailable: "A copied command is unavailable because the input paths cannot be represented below a shared directory.",
    commandContext: "Run from the common parent directory of the table, corpus and catalog files.",
    copy: "Copy",
    copied: "Copied",
    edit: "Edit as a clause",
    apply: "Apply",
    cancel: "Cancel",
    blanks: "Use the blanks",
    mode: ["Only these", "All except these"],
    anyButton: (name) => `Any ${name}`,
    findValue: "Find a value",
    topValues: (n, of) => `Top ${n} of ${of}`,
    foundValues: (n, of) => (n < of ? `Top ${n} of ${of} matches` : `${of} matches`),
    from: "From",
    to: "To",
    workbench: "Open in workbench",
    reasons: {
      section: () => "@section cannot be answered here: a count table counts per document.",
      sentence: () => "@sentence keys and divisions cannot be answered here: a count table counts per document.",
      syntax: (r) => `The clause does not parse: ${r.message}`,
      unknown_key: (r) => `This corpus has no condition named “${r.key}”.`,
      unwritable: (r) => `“${r.value}” contains a double quote, which a clause cannot hold.`,
      shape: (r) => `The condition on “${r.key}” does not fit its values.`,
      address: () => "The conditions in this URL cannot be read.",
      unknown_entry: () => "This entry does not occur in the table.",
    },
    back: (p, c) => `Frequencies of <b>${p}</b> in ${c}`,
    entryFigures: (o, d) => `${o} ${o === "1" ? "occurrence" : "occurrences"} in ${d} ${d === "1" ? "document" : "documents"}`,
    readout: (name, o, d) => `${name}: ${S.en.entryFigures(o, d)}`,
    noValue: "no value",
    moreValues: (n) => `${n} more values`,
    asTable: "Table",
    tableHeads: ["Value", "Occurrences", "Documents"],
    lines: "Lines",
    linesShown: (n, of) => `${n} of ${of}`,
    document: (id) => `document ${id}`,
    entryNeedsBlanks: "Return to the blanks to open an entry",
    unavailable: (catalog, corpus) => `No count table is registered for ${catalog} over ${corpus}.`,
    unknownPattern: (pattern, catalog) => `${catalog} has no pattern “${pattern}”.`,
    noTables: "No count table is registered. Start ling serve with --count-table.",
    failed: (status) => `Server response: ${status}.`,
  },
  ja: {
    role: { pattern: "パターン", catalog: "カタログ", order: "並び順", filter: "絞り込み" },
    sorts: [["count", "頻度の高い順"], ["works", "出現文書数の多い順"], ["key", "文字コード順"]],
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
      return parts.length ? `${parts.map((p) => (p.any ? `${p.html.join("・")}を問わない` : `${esc(p.name)}が${p.html[0]}`)).join("、")}${parts.at(-1).any ? "" : "の"}文書` : "";
    },
    sentence: (b) => `${b.C}${b.scope ? `のうち、${b.scope}における` : "における"}${b.P}の頻度。${b.O}、${b.Q}を含む項目<span class="tail">。</span>`,
    rawScope: "下の条件式に合う文書",
    add: "＋ 条件",
    addLabel: "条件を追加",
    figures: (e, o, d) => `${e}項目、${o}件、文書${d}件`,
    keeps: (x, y, p) => `${y}件中${x}件（${p}%）`,
    missingField: (n, key) => `他の条件を満たす文書のうち、${key}がない${n}件を除外しています。`,
    keepsEntries: (x, y, p) => `${y}項目中${x}項目（${p}%）`,
    heads: ["順位", "項目", "出現数", "文書数", "文書に占める割合", "100万語あたり"],
    more: (n) => `次の${n}件を表示`,
    empty: "条件に合う用例はありません。",
    nomatch: (q) => `「${q}」を含む項目はありません。`,
    placeholder: "任意の文字列",
    commandUnavailable: "入力パスを共通ディレクトリからの相対パスで表せないため、コピー用コマンドは表示できません。",
    commandContext: "テーブル・コーパス・カタログの各ファイルに共通する親ディレクトリで実行してください。",
    copy: "コピー",
    copied: "コピーしました",
    edit: "条件式として編集",
    apply: "適用",
    cancel: "取り消す",
    blanks: "空欄に戻す",
    mode: ["これらのみ", "これら以外"],
    anyButton: (name) => `すべての${name}`,
    findValue: "値を検索",
    topValues: (n, of) => `${of}種中、上位${n}種`,
    foundValues: (n, of) => (n < of ? `一致${of}種中、上位${n}種` : `一致${of}種`),
    from: "下限",
    to: "上限",
    workbench: "ワークベンチで開く",
    reasons: {
      section: () => "@section はここでは扱えません。件数表は文書ごとの件数です。",
      sentence: () => "@sentence のキーと区分はここでは扱えません。件数表は文書ごとの件数です。",
      syntax: (r) => `条件式を解析できません：${r.message}`,
      unknown_key: (r) => `このコーパスに「${r.key}」という条件はありません。`,
      unwritable: (r) => `「${r.value}」は二重引用符を含むため条件式にできません。`,
      shape: (r) => `「${r.key}」の条件が値の種類に合いません。`,
      address: () => "このアドレスの条件を読み取れません。",
      unknown_entry: () => "この項目は件数表にありません。",
    },
    back: (p, c) => `${c}における<b>${p}</b>の頻度`,
    entryFigures: (o, d) => `文書${d}件に${o}件`,
    readout: (name, o, d) => `${name}：文書${d}件に${o}件`,
    noValue: "値なし",
    moreValues: (n) => `ほか${n}種`,
    asTable: "表",
    tableHeads: ["値", "出現数", "文書数"],
    lines: "用例",
    linesShown: (n, of) => `${of}件中${n}件`,
    document: (id) => `文書${id}`,
    entryNeedsBlanks: "項目を開くには空欄に戻してください",
    unavailable: (catalog, corpus) => `${corpus}の${catalog}には件数表が登録されていません。`,
    unknownPattern: (pattern, catalog) => `${catalog}にパターン「${pattern}」はありません。`,
    noTables: "件数表が登録されていません。ling serve に --count-table を指定してください。",
    failed: (status) => `サーバーの応答：${status}`,
  },
};

const L = () => S[currentLocale()] ?? S.en;
const num = (n) => (NUM[currentLocale()] ?? NUM.en).format(Number(n));

export function readState(hash) {
  const h = new URLSearchParams(hash.replace(/^#/, ""));
  const s = { corpus: h.get("corpus"), catalog: h.get("catalog"), pattern: h.get("pattern"), w: {}, where: h.get("where"), sort: h.get("sort") || "count", q: h.get("q") || "", entry: h.get("entry"), bad: false };
  if (h.get("w")) {
    try {
      const w = JSON.parse(h.get("w"));
      if (w && typeof w === "object" && !Array.isArray(w)) s.w = w;
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
    if (v === null || v === undefined || v === "" || (k === "w" && !Object.keys(v).length) || (k === "sort" && v === "count")) continue;
    out.push([k, k === "w" ? JSON.stringify(v) : String(v)]);
  }
  return out;
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

/** A label's units: runs ending at 、, ・, a space, an ideographic space or “. ”, with each bracketed part one unit. */
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

/**
 * Shorter forms of each name, longest first. A form replaces whole units with
 * one “…”, keeps the brackets around a part whose inside it cuts, and is left
 * out when another name shares everything it keeps on that side of the “…”.
 */
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

async function api(path, params, signal) {
  const query = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== ""));
  const response = await fetch(`api/${path}?${query}`, { signal });
  if (!response.ok) {
    const reply = await response.json().catch(() => null);
    throw new Error(reply?.error ?? L().failed(response.status));
  }
  return response.json();
}

export function mountFrequency(root) {
  const $ = (sel) => root.querySelector(sel);
  const sentenceEl = $("[data-fq-sentence]"), figuresEl = $("[data-fq-figures]"), clauseEl = $("[data-fq-clause]");
  const sourceEl = $("[data-fq-source]"), stateEl = $("[data-fq-state]"), tableEl = $("[data-fq-table]"), entryEl = $("[data-fq-entry]");
  const pop = document.body.appendChild(Object.assign(document.createElement("div"), { className: "fq-pop", hidden: true }));
  let meta = null, s = readState(location.hash), answer = null, gen = 0, shown = 0;
  let openPop = null, pending = null, editing = false, find = "", values = null;
  let valueMatches = 0, selectedValues = [], valueRequest = 0, valueTimer;
  // The selected entry's profile and lines under the committed conditions,
  // and the rows each chart keeps while a preview shows other conditions.
  let profile = null, lined = null, plan = {}, preview = null, previewed = null, anchor = null, lastEntry = null;

  const pair = () => meta.pairs.find((p) => p.corpus === s.corpus && p.catalog === s.catalog);
  const dims = () => (pair() ? meta.dimensions[pair().corpus] ?? [] : []);
  const dim = (key) => dims().find((d) => d.key === key);
  const keyName = (key) => meta.labels?.[key]?.[currentLocale()] ?? L().name(key);
  const shownKeys = () => dims().map((d) => d.key).filter((k) => k in s.w || k === pending);
  const conditions = () => (Object.keys(s.w).length ? JSON.stringify(s.w) : undefined);

  function resolve() {
    if (!meta.pairs.length) return { state: L().noTables };
    if (s.bad) return { state: L().reasons.address() };
    const first = meta.pairs[0];
    s.corpus ??= first.corpus;
    s.catalog ??= (meta.pairs.find((p) => p.corpus === s.corpus) ?? first).catalog;
    const p = pair();
    if (!p) {
      const name = meta.pairs.find((x) => x.corpus === s.corpus)?.corpus_name ?? s.corpus;
      return { state: L().unavailable(s.catalog, name) };
    }
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

  const blank = (role, inner, cls = "") => `<span class="blank${cls ? ` ${cls}` : ""}"><span class="role">${role}</span>${inner}</span>`;
  const role = (name, key = "") => `<span class="nm" data-name="${esc(key || name)}">${esc(name)}</span><span class="n" data-share="${esc(key)}"></span>`;
  const select = (b, label, options, value) =>
    `<select data-b="${b}" aria-label="${esc(label)}">${options.map(([v, t]) => `<option value="${esc(v)}"${String(v) === String(value ?? "") ? " selected" : ""}>${esc(t)}</option>`).join("")}</select>`;

  function conditionText(d, w) {
    const t = L();
    if (!w || (d.kind === "range" ? w.lo == null && w.hi == null : !w.v?.length)) return t.any(keyName(d.key));
    return d.kind === "range" ? t.range(w) : t.values(w);
  }
  const isAny = (d, w) => !w || (d.kind === "range" ? w.lo == null && w.hi == null : !w.v?.length);

  function sentenceHtml() {
    const t = L(), p = pair();
    const catalogs = meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, meta.pairs.some((y) => y.corpus !== x.corpus) ? `${x.catalog} (${x.corpus_name})` : x.catalog]);
    const items = shownKeys().map((key) => {
      const d = dim(key), w = s.w[key];
      return {
        name: keyName(key),
        any: isAny(d, w),
        html: blank(role(keyName(key), key), `<button class="pick" data-b="${esc(key)}" aria-haspopup="dialog" aria-expanded="${openPop === key}">${esc(conditionText(d, w))}</button>`),
      };
    });
    const b = {
      P: blank(role(t.role.pattern), select("pattern", t.role.pattern, p.patterns.map((x) => [x.name, x.name]), s.pattern), "lead"),
      C: blank(role(t.role.catalog), select("catalog", t.role.catalog, catalogs, `${s.corpus}\n${s.catalog}`)),
      scope: s.where != null ? t.rawScope : t.scope(items),
      O: blank(role(t.role.order), select("sort", t.role.order, t.sorts, s.sort)),
      Q: blank(role(t.role.filter, "filter"), `<input data-b="q" type="search" placeholder="${esc(t.placeholder)}" aria-label="${esc(t.role.filter)}" value="${esc(s.q)}">`),
    };
    const free = s.where == null && shownKeys().length < dims().length;
    return t.sentence(b) + (free ? `<button class="add" data-b="add" aria-haspopup="menu" aria-expanded="${openPop === "add"}" aria-label="${esc(t.addLabel)}">${esc(t.add)}</button>` : "");
  }

  function renderSentence() {
    sentenceEl.innerHTML = sentenceHtml();
    annotate();
  }

  function annotate() {
    const t = L();
    for (const n of sentenceEl.querySelectorAll(".role .n")) {
      const key = n.dataset.share, roleEl = n.parentElement, pick = roleEl.parentElement.querySelector(".pick, input");
      let kept = null, title = null;
      if (answer && key === "filter" && s.q) {
        const p = pctOf(answer.matching, answer.entries);
        kept = p;
        title = t.keepsEntries(num(answer.matching), num(answer.entries), p);
        pick?.style.setProperty("--kept", `${answer.entries ? (answer.matching / answer.entries) * 100 : 0}%`);
      } else if (answer && key && answer.kept[key]) {
        const k = answer.kept[key], p = pctOf(k.kept, k.of);
        kept = p;
        title = t.keeps(num(k.kept), num(k.of), p);
        if (s.w[key]?.m === "out" && k.excluded_missing_field_documents) {
          title += ` ${t.missingField(num(k.excluded_missing_field_documents), keyName(key))}`;
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
    figuresEl.textContent = answer && !entering() ? L().figures(num(answer.entries), num(answer.occurrences), num(answer.documents_admitted)) : "";
  }

  function renderClause() {
    const t = L();
    if (editing) {
      clauseEl.innerHTML = `<div class="edit"><textarea data-c="text" aria-label="--where" spellcheck="false">${esc(s.where ?? answer?.clause ?? "")}</textarea><div class="actions"><button data-c="apply">${esc(t.apply)}</button><button data-c="cancel">${esc(t.cancel)}</button></div></div>`;
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
    clauseEl.innerHTML = `<div class="clause"><code data-c="code">${code}</code>${command ? `<button data-c="copy">${esc(t.copy)}</button>` : ""}<button data-c="edit">${esc(t.edit)}</button>${s.where != null ? `<button data-c="blanks">${esc(t.blanks)}</button>` : ""}</div>${source ? `<p>${esc(command ? t.commandContext : t.commandUnavailable)}</p>` : ""}`;
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
    sourceEl.innerHTML = `${pattern.source ? `<pre>${esc(pattern.source)}</pre>` : ""}${p.workbench ? `<a href="${esc(p.workbench)}">${esc(L().workbench)}</a>` : ""}`;
  }

  const keyNames = (group) => (group.kind === "key" && group.fields.length > 1 ? group.fields.map((f) => f.name).join(" ") : "");
  function rowHtml(r, i) {
    const docs = answer.documents_admitted, tokens = answer.tokens_admitted;
    const share = docs ? r.works / docs : 0;
    const pm = tokens ? (r.count * 1e6) / tokens : 0;
    const names = keyNames(r.group);
    return `<tr${s.where == null ? ` data-i="${i}"` : ""}><td class="fig r">${num(r.rank)}</td><td class="entry-cell">${s.where == null ? `<button class="open-entry" data-i="${i}">${esc(r.text)}</button>` : esc(r.text)}${names ? `<small>${esc(names)}</small>` : ""}</td><td class="fig r"><b>${num(r.count)}</b></td><td class="fig r">${num(r.works)}</td><td class="spread" title="${(share * 100).toFixed(1)}%"><span><i style="width:${Math.max(1, share * 100).toFixed(1)}%"></i></span></td><td class="fig r pm" data-v="${pm}"></td></tr>`;
  }
  function alignColumn(cells) {
    const xs = cells.map((c) => +c.dataset.v), min = Math.min(...xs.filter((x) => x > 0));
    const places = Number.isFinite(min) ? Math.max(0, Math.min(2, Math.ceil(-Math.log10(min)) + 1)) : 0;
    const format = new Intl.NumberFormat(currentLocale() === "ja" ? "ja-JP" : "en-US", { minimumFractionDigits: places, maximumFractionDigits: places });
    cells.forEach((c, i) => (c.textContent = format.format(xs[i])));
  }
  function renderTable() {
    const t = L();
    tableEl.hidden = entering();
    if (!answer || entering()) {
      tableEl.innerHTML = "";
      return;
    }
    if (!answer.rows.length) {
      tableEl.innerHTML = `<p class="fq-empty">${esc(s.q && answer.occurrences ? t.nomatch(s.q) : t.empty)}</p>`;
      return;
    }
    tableEl.innerHTML = `<table class="freq"><thead><tr>${t.heads.map((h, i) => `<th class="${[0, 2, 3, 5].includes(i) ? "r" : ""}">${esc(h)}</th>`).join("")}</tr></thead><tbody>${answer.rows.map((r, i) => rowHtml(r, i)).join("")}</tbody></table><button class="more"${shown >= answer.matching ? " hidden" : ""}>${esc(t.more(PAGE))}</button>`;
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
      set({ entry: JSON.stringify(answer.rows[+row.dataset.i].group) });
      return;
    }
    if (!e.target.closest(".more")) return;
    const mine = gen, more = await api("frequency", { ...params(), offset: shown });
    if (mine !== gen || more.status !== "ok") return;
    answer.rows.push(...more.rows);
    shown = answer.rows.length;
    renderTable();
  });

  const blankEl = (b) => sentenceEl.querySelector(`[data-b="${CSS.escape(b)}"]`);
  function placePop(anchor) {
    const r = anchor.getBoundingClientRect(), top = r.bottom + 6;
    pop.style.top = `${top}px`;
    pop.style.maxHeight = `${Math.max(160, innerHeight - top - 12)}px`;
    pop.style.left = `${Math.max(8, Math.min(r.left, innerWidth - pop.offsetWidth - 8))}px`;
  }
  function closePop() {
    openPop = null;
    values = null;
    find = "";
    pop.hidden = true;
    if (pending && !(pending in s.w)) {
      pending = null;
      renderSentence();
    } else sentenceEl.querySelectorAll("[aria-expanded]").forEach((b) => b.setAttribute("aria-expanded", "false"));
  }
  function options(key, list) {
    const chosen = new Set(s.w[key]?.v ?? []), max = Math.max(1, ...list.map(([, n]) => n));
    return list.map(([v, n]) => `<label><input type="checkbox" data-v="${esc(v)}"${chosen.has(v) ? " checked" : ""}><span>${esc(v)}</span><span class="b" style="width:${((n / max) * 100).toFixed(1)}%"></span><span class="v">${num(n)}</span></label>`).join("");
  }
  function valueList(key) {
    if (!values) return "";
    const t = L(), q = find.trim(), shown = new Set(values.map(([v]) => v));
    const list = q ? values : [...values, ...selectedValues.filter(([v]) => !shown.has(v))];
    const hint = valueMatches > values.length
      ? (q ? t.foundValues(num(values.length), num(valueMatches)) : t.topValues(num(values.length), num(valueMatches)))
      : "";
    return options(key, list) + (hint ? `<p class="hint">${esc(hint)}</p>` : "");
  }
  function popover() {
    const t = L();
    if (openPop === "add") {
      const free = dims().filter((d) => !shownKeys().includes(d.key));
      pop.innerHTML = `<ul class="addlist" role="menu">${free.map((d) => `<li><button role="menuitem" data-add="${esc(d.key)}">${esc(keyName(d.key))}</button></li>`).join("")}</ul>`;
      pop.setAttribute("aria-label", t.addLabel);
      pop.hidden = false;
      placePop(blankEl("add"));
      pop.querySelector("button")?.focus();
      return;
    }
    const d = openPop && dim(openPop);
    if (!d || !blankEl(openPop)) {
      pop.hidden = true;
      return;
    }
    const w = s.w[d.key];
    pop.setAttribute("aria-label", keyName(d.key));
    if (d.kind === "range") {
      pop.innerHTML = `<div class="head"><span></span><button class="any" data-any>${esc(t.anyButton(keyName(d.key)))}</button></div><div class="range"><label>${esc(t.from)} <input type="number" data-lo step="any" placeholder="${esc(d.min)}" value="${esc(w?.lo ?? "")}"></label><label>${esc(t.to)} <input type="number" data-hi step="any" placeholder="${esc(d.max)}" value="${esc(w?.hi ?? "")}"></label></div>`;
    } else {
      const out = w?.m === "out";
      pop.innerHTML = `<div class="head"><span class="mode" role="group"><button data-mode="in" aria-pressed="${!out}">${esc(t.mode[0])}</button><button data-mode="out" aria-pressed="${out}">${esc(t.mode[1])}</button></span><button class="any" data-any>${esc(t.anyButton(keyName(d.key)))}</button></div>${d.values > MANY ? `<input class="find" type="search" data-find placeholder="${esc(t.findValue)}" aria-label="${esc(t.findValue)}" value="${esc(find)}">` : ""}<div class="opts">${valueList(d.key)}</div>`;
    }
    pop.hidden = false;
    placePop(blankEl(d.key));
  }
  async function openCondition(key) {
    openPop = key;
    find = "";
    values = null;
    sentenceEl.querySelectorAll(".pick").forEach((b) => b.setAttribute("aria-expanded", String(b.dataset.b === key)));
    popover();
    if (dim(key)?.kind !== "categorical") return;
    await loadValues(key);
  }
  async function loadValues(key) {
    const mine = gen, needle = find.trim(), request = ++valueRequest;
    let r;
    try {
      r = await api("frequency/values", { corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, key, w: conditions(), q: needle, limit: MANY });
    } catch (error) {
      if (mine === gen && openPop === key && request === valueRequest && needle === find.trim()) pop.querySelector(".opts").textContent = error.message;
      return;
    }
    if (mine !== gen || openPop !== key || request !== valueRequest || needle !== find.trim() || r.status !== "ok") return;
    values = r.values;
    valueMatches = r.matching;
    selectedValues = r.selected;
    pop.querySelector(".opts").innerHTML = valueList(key);
  }
  pop.addEventListener("input", (e) => {
    if (e.target.matches("[data-find]")) {
      find = e.target.value;
      const key = openPop;
      values = null;
      pop.querySelector(".opts").replaceChildren();
      clearTimeout(valueTimer);
      valueTimer = setTimeout(() => { if (openPop === key) loadValues(key); }, 150);
    }
  });
  pop.addEventListener("change", (e) => {
    const key = openPop;
    if (e.target.matches("input[data-v]")) {
      const set_ = new Set(s.w[key]?.v ?? []);
      e.target.checked ? set_.add(e.target.dataset.v) : set_.delete(e.target.dataset.v);
      pending = null;
      set({ w: { ...s.w, [key]: { m: s.w[key]?.m === "out" ? "out" : "in", v: [...set_].sort() } } });
    } else if (e.target.matches("[data-lo], [data-hi]")) {
      const read = (sel) => {
        const v = pop.querySelector(sel).value;
        return v === "" || !Number.isFinite(+v) ? undefined : +v;
      };
      const w = { m: "range" }, lo = read("[data-lo]"), hi = read("[data-hi]");
      if (lo !== undefined) w.lo = lo;
      if (hi !== undefined) w.hi = hi;
      pending = null;
      set({ w: { ...s.w, [key]: w } });
    }
  });
  pop.addEventListener("click", (e) => {
    const add = e.target.closest("[data-add]");
    if (add) {
      e.stopPropagation();
      pending = add.dataset.add;
      pop.hidden = true;
      renderSentence();
      openCondition(pending);
      return;
    }
    if (e.target.closest("[data-any]")) {
      const key = openPop, d = dim(key);
      pending = null;
      closePop();
      set({ w: { ...s.w, [key]: d.kind === "range" ? { m: "range" } : { m: "in", v: [] } } });
      return;
    }
    const mode = e.target.closest("[data-mode]");
    if (mode) {
      const key = openPop;
      pending = null;
      set({ w: { ...s.w, [key]: { m: mode.dataset.mode, v: s.w[key]?.v ?? [] } } });
    }
  });

  sentenceEl.addEventListener("change", (e) => {
    const b = e.target.dataset.b;
    if (b === "catalog") {
      const [corpus, catalog] = e.target.value.split("\n");
      set({ corpus, catalog, pattern: null, w: corpus === s.corpus ? s.w : {}, q: "", entry: null });
    } else if (b === "pattern") set({ pattern: e.target.value, entry: null });
    else if (b === "sort") set({ sort: e.target.value });
  });
  let typing;
  sentenceEl.addEventListener("input", (e) => {
    if (e.target.dataset.b !== "q") return;
    clearTimeout(typing);
    typing = setTimeout(() => {
      s.q = e.target.value;
      persist();
      load({ keepSentence: true });
    }, 220);
  });
  sentenceEl.addEventListener("click", (e) => {
    const b = e.target.closest("button[data-b]")?.dataset.b;
    if (!b) return;
    e.stopPropagation();
    if (openPop === b) return closePop();
    if (b === "add") {
      openPop = "add";
      e.target.setAttribute("aria-expanded", "true");
      popover();
    } else openCondition(b);
  });
  document.addEventListener("click", (e) => {
    if (openPop && !pop.contains(e.target) && !e.target.closest(".pick, .add")) closePop();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (openPop) closePop();
    else if (entering() && !editing && !e.target.closest?.("input, textarea, select")) leaveEntry();
  });
  addEventListener("resize", () => {
    fitRoles();
    if (openPop) popover();
  });

  // The entry level: one row's profile per condition and its lines.
  const entering = () => s.entry != null && s.where == null;
  const groupText = (g) => (g.kind === "key" ? g.fields.map((f) => (f.kind === "list" ? f.values.join(",") : f.value)).join(" ") : g.value);
  const valueName = (v) => v ?? L().noValue;
  const ems = (text) => [...text].reduce((w, c) => w + (c.charCodeAt(0) > 0x2e7f ? 1 : 0.6), 0);
  const barsOf = (c) => [...c.bars, ...(c.none ? [{ ...c.none, value: null }] : [])];
  const span = (x) => (String(x.lo) === String(x.hi) ? String(x.lo) : `${x.lo}–${x.hi}`);
  const entryParams = (w) => ({ corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, entry: s.entry, w: Object.keys(w).length ? JSON.stringify(w) : undefined });
  const opened = new Set();
  let readout = null;

  // Rows each chart shows under the committed conditions: a preview fills at
  // most these and pads the rest, and the label column keeps its width.
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
    const t = L(), rows = plan[c.key] ?? { rows: 0, more: false, lw: 2 }, admitted = admits(c.key);
    const all = barsOf(c).slice(0, rows.rows), max = Math.max(1, ...all.map((b) => b.count));
    const forms = labelForms(all.map((b) => valueName(b.value)));
    const bar = (b, i) => {
      const name = valueName(b.value), none = b.value == null;
      return `<li><button data-bar data-v="${none ? "" : esc(b.value)}"${none ? " data-none" : ""} aria-pressed="${admitted(b.value)}" aria-label="${esc(t.readout(name, num(b.count), num(b.works)))}"><span class="name${none ? " none" : ""}" data-full="${esc(name)}" data-forms="${esc(JSON.stringify(forms[i]))}" aria-hidden="true"><span>${esc(name)}</span></span><span class="b"><i style="width:${((b.count / max) * 100).toFixed(1)}%"></i></span><span class="v" aria-hidden="true">${num(b.count)}</span></button></li>`;
    };
    const pad = '<li class="pad" aria-hidden="true"><span>&nbsp;</span></li>'.repeat(Math.max(0, rows.rows - all.length));
    const more = rows.more ? `<p class="fq-more-values">${c.distinct > c.bars.length ? esc(t.moreValues(num(c.distinct - c.bars.length))) : "&nbsp;"}</p>` : "";
    return `<ol class="fq-bars" style="--lw:${Math.min(rows.lw, 14).toFixed(1)}em">${all.map(bar).join("")}${pad}</ol>${more}`;
  }
  function columnsHtml(c) {
    const t = L(), d = dim(c.key), w = s.w[c.key], max = Math.max(1, ...c.columns.map((x) => x.count));
    const ranged = w?.m === "range" && (w.lo != null || w.hi != null);
    const inside = (x) => ranged && (w.lo == null || +x.lo >= w.lo) && (w.hi == null || +x.hi <= w.hi);
    const columns = c.columns
      .map((x) => (x.lo == null ? '<span class="gap"></span>' : `<button data-col data-lo="${esc(x.lo)}" data-hi="${esc(x.hi)}" aria-pressed="${inside(x)}" aria-label="${esc(t.readout(span(x), num(x.count), num(x.works)))}"><i style="height:${((x.count / max) * 100).toFixed(1)}%"></i></button>`))
      .join("");
    const none = c.none ? t.readout(t.noValue, num(c.none.count), num(c.none.works)) : "";
    return `<div class="fq-columns${c.columns.length > 60 ? " dense" : ""}">${columns}</div><div class="fq-axis" aria-hidden="true"><span>${d ? tick(d.min) : ""}</span><span>${d ? tick(d.max) : ""}</span></div><p class="fq-none">${none ? esc(none) : "&nbsp;"}</p>`;
  }
  const tick = (n) => (Number.isInteger(n) && Math.abs(n) < 10000 ? String(n) : num(n));
  const bodyHtml = (c) => (c.kind === "categorical" ? barsHtml(c) : columnsHtml(c));
  function chartTable(c) {
    const t = L();
    const rows =
      c.kind === "categorical"
        ? barsOf(c).map((b) => [valueName(b.value), b.count, b.works])
        : [...c.columns.filter((x) => x.lo != null && x.count).map((x) => [span(x), x.count, x.works]), ...(c.none ? [[t.noValue, c.none.count, c.none.works]] : [])];
    return `<table><thead><tr>${t.tableHeads.map((h, i) => `<th${i ? ' class="r"' : ""}>${esc(h)}</th>`).join("")}</tr></thead><tbody>${rows.map(([v, n, d]) => `<tr><td>${esc(v)}</td><td class="r">${num(n)}</td><td class="r">${num(d)}</td></tr>`).join("")}</tbody></table>`;
  }
  const chartHtml = (c) =>
    `<figure class="fq-chart" data-key="${esc(c.key)}"><figcaption>${esc(keyName(c.key))}</figcaption><p class="fq-readout" data-readout aria-live="polite">&nbsp;</p><div class="fq-chart-body" data-body>${bodyHtml(c)}</div><details${opened.has(c.key) ? " open" : ""}><summary>${esc(L().asTable)}</summary>${chartTable(c)}</details></figure>`;

  const lineHtml = (l) =>
    `<li><span class="kl"><span>${esc(l.left)}</span></span><span class="kr"><b class="kn">${esc(l.node)}</b>${esc(l.right)}</span><span class="src">${esc(L().document(l.document))}</span></li>`;
  function linesHtml(st, previewing) {
    const t = L();
    return `<h3>${esc(t.lines)} <span class="count">${esc(t.linesShown(num(st.lines.length), num(st.total)))}</span></h3>${st.total ? "" : `<p class="fq-empty">${esc(t.empty)}</p>`}<ol class="fq-kwic">${st.lines.map(lineHtml).join("")}</ol><button class="more" data-e="more"${previewing || st.lines.length >= st.total ? " hidden" : ""}>${esc(t.more(LINES))}</button>`;
  }
  const figuresText = (p) => L().entryFigures(num(p.count), num(p.works));

  // Shows each label's full name where it fits, otherwise its longest form
  // that fits. Every full name is written before any width is read, so a
  // chart where all fit costs one layout.
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

  // The chart's line naming the bar last pointed at, focused or tapped, in
  // full, with its counts as drawn now.
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
    const t = L(), back = new URLSearchParams(stateEntries({ ...s, entry: null }));
    const focused = document.activeElement?.closest?.("[data-bar], [data-col]");
    const at = focused && entryEl.contains(focused) ? [focused.closest(".fq-chart").dataset.key, focused.dataset.v ?? null, focused.dataset.lo ?? null] : null;
    plan = planOf(profile);
    const names = keyNames(profile.group);
    entryEl.innerHTML = `<a class="fq-back" href="#${esc(back)}" data-e="back"><span aria-hidden="true">← </span>${t.back(esc(s.pattern), esc(s.catalog))}</a>
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

  async function loadEntry(mine) {
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
      [p, l] = await Promise.all([api("frequency/profile", entryParams(s.w)), api("frequency/lines", { ...entryParams(s.w), limit: LINES })]);
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
      renderEntry(bad.status === "unsupported" ? L().reasons[bad.reason]?.(bad) ?? bad.reason : L().unavailable(s.catalog, s.corpus));
    } else {
      profile = { ...p, entry: s.entry };
      lined = { total: l.total, lines: l.lines };
      renderEntry();
    }
    renderClause();
    entryEl.removeAttribute("aria-busy");
  }

  // A preview shows the profile, figures and lines as if the pointed-at value
  // were the condition on its key; the sentence and the URL stay committed.
  // Charts a preview has drawn under other conditions than the committed ones.
  const painted = new Set();
  // Redraws one chart's bars, keeping keyboard focus on the same value.
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
    // The pointed-at chart counts under every condition but its own, which
    // the preview leaves as committed.
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
  // Stops the pending preview's requests; what it has drawn stays.
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
      try {
        const mine = gen, l = await api("frequency/lines", { ...entryParams(s.w), offset: lined.lines.length, limit: LINES });
        if (mine !== gen || l.status !== "ok") return;
        lined.lines.push(...l.lines);
        entryEl.querySelector("[data-e=lines]").innerHTML = linesHtml(lined, false);
      } finally {
        more.disabled = false;
      }
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

  const params = () => ({ corpus: s.corpus, catalog: s.catalog, pattern: s.pattern, w: s.where == null ? conditions() : undefined, where: s.where ?? undefined, sort: s.sort, q: s.q, limit: PAGE });

  function renderAll() {
    const r = resolve();
    stateEl.textContent = r.state ?? "";
    if (r.sentence) renderSentence();
    else sentenceEl.innerHTML = r.state && meta.pairs.length && !s.bad ? sentenceFallback() : "";
    renderFigures();
    renderClause();
    renderSource();
    renderTable();
    if (!entering() || profile?.entry !== s.entry) renderEntry();
    if (s.entry != null && s.where != null) stateEl.textContent = L().entryNeedsBlanks;
    return r;
  }
  function sentenceFallback() {
    const t = L();
    const catalogs = [[`${s.corpus}\n${s.catalog}`, s.catalog], ...meta.pairs.map((x) => [`${x.corpus}\n${x.catalog}`, `${x.catalog} (${x.corpus_name})`])];
    return blank(role(t.role.catalog), select("catalog", t.role.catalog, catalogs, `${s.corpus}\n${s.catalog}`));
  }

  async function load({ keepSentence = false } = {}) {
    const mine = ++gen;
    if (!keepSentence) answer = null;
    const r = keepSentence ? resolve() : renderAll();
    clearTimeout(valueTimer);
    if (openPop && dim(openPop)?.kind === "categorical") {
      values = null;
      pop.querySelector(".opts")?.replaceChildren();
      loadValues(openPop);
    }
    if (!r.query) return;
    const entryLoad = entering() && !keepSentence ? loadEntry(mine) : null;
    if (!entering()) {
      profile = null;
      lined = null;
    }
    let reply;
    try {
      reply = await api("frequency", params());
    } catch (error) {
      if (mine === gen) stateEl.textContent = error.message;
      return;
    }
    if (mine !== gen) return;
    if (reply.status === "ok") {
      answer = reply;
      shown = reply.rows.length;
      stateEl.textContent = "";
    } else {
      answer = null;
      stateEl.textContent = reply.status === "unsupported" ? L().reasons[reply.reason]?.(reply) ?? reply.reason : L().unavailable(s.catalog, s.corpus);
    }
    if (s.entry != null && s.where != null) stateEl.textContent = L().entryNeedsBlanks;
    if (keepSentence) annotate();
    else renderSentence();
    renderFigures();
    renderClause();
    renderTable();
    if (openPop) {
      const search = pop.querySelector("[data-find]"), focused = document.activeElement === search;
      const start = search?.selectionStart, end = search?.selectionEnd;
      popover();
      if (focused) {
        const next = pop.querySelector("[data-find]");
        next?.focus();
        next?.setSelectionRange(start, end);
      }
    }
    await entryLoad;
  }

  addEventListener("popstate", () => {
    s = readState(location.hash);
    closePop();
    editing = false;
    load();
  });
  onLocaleChange(() => {
    renderAll();
    renderEntry();
    if (answer) {
      renderSentence();
      renderFigures();
      renderClause();
      renderTable();
    }
  });

  return (async () => {
    meta = await api("frequency/meta", {});
    await load();
  })();
}

const root = typeof document !== "undefined" && document.querySelector("[data-frequency]");
if (root) mountFrequency(root);
