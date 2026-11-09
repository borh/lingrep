import { currentLocale } from "./i18n.js";

const MANY = 30;
const NUM = { en: new Intl.NumberFormat("en-US"), ja: new Intl.NumberFormat("ja-JP") };
const num = (n) => (NUM[currentLocale()] ?? NUM.en).format(Number(n));
const esc = (text) =>
  String(text).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const S = {
  en: {
    mode: ["Only these", "All except these"],
    anyButton: (name) => `Any ${name}`,
    findValue: "Find a value",
    topValues: (n, of) => `Top ${n} of ${of}`,
    foundValues: (n, of) => (n < of ? `Top ${n} of ${of} matches` : `${of} matches`),
    from: "From",
    to: "To",
  },
  ja: {
    mode: ["これらのみ", "これら以外"],
    anyButton: (name) => `すべての${name}`,
    findValue: "値を検索",
    topValues: (n, of) => `${of}種中、上位${n}種`,
    foundValues: (n, of) => (n < of ? `一致${of}種中、上位${n}種` : `一致${of}種`),
    from: "下限",
    to: "上限",
  },
};
const L = () => S[currentLocale()] ?? S.en;

export function createConditionPicker({ sentence, view, keyName, label, anchor, commit, render, api }) {
  const pop = document.body.appendChild(Object.assign(document.createElement("div"), { className: "fq-pop", hidden: true }));
  let openPop = null, pending = null, find = "", values = null, valueMatches = 0, selectedValues = [], valueRequest = 0, valueTimer;
  const dims = (scope) => view(scope).dimensions;
  const conds = (scope) => view(scope).conditions;
  const dim = (key) => dims(openPop?.scope).find((d) => d.key === key);
  const shownKeys = (scope = null) => dims(scope).map((d) => d.key).filter((k) => k in conds(scope) || (pending?.scope === scope && pending.key === k));
  function placePop(anchor) {
    const r = anchor.getBoundingClientRect(), top = r.bottom + 6;
    pop.style.top = `${top}px`;
    pop.style.maxHeight = `${Math.max(160, innerHeight - top - 12)}px`;
    pop.style.left = `${Math.max(8, Math.min(r.left, innerWidth - pop.offsetWidth - 8))}px`;
  }
  const anchorOf = (o) => anchor(o.scope, o.key);
  function closePop() {
    openPop = null;
    values = null;
    find = "";
    pop.hidden = true;
    if (pending && !(pending.key in conds(pending.scope))) {
      pending = null;
      render();
    } else sentence.querySelectorAll("[aria-expanded]").forEach((b) => b.setAttribute("aria-expanded", "false"));
  }
  function options(w, list) {
    const chosen = new Set(w?.v ?? []), max = Math.max(1, ...list.map(([, n]) => n));
    return list.map(([v, n]) => `<label><input type="checkbox" data-v="${esc(v)}"${chosen.has(v) ? " checked" : ""}><span>${esc(v)}</span><span class="b" style="width:${((n / max) * 100).toFixed(1)}%"></span><span class="v">${num(n)}</span></label>`).join("");
  }
  function valueList() {
    if (!values) return "";
    const t = L(), q = find.trim(), seen = new Set(values.map(([v]) => v));
    const list = q ? values : [...values, ...selectedValues.filter(([v]) => !seen.has(v))];
    const hint = valueMatches > values.length ? (q ? t.foundValues(num(values.length), num(valueMatches)) : t.topValues(num(values.length), num(valueMatches))) : "";
    return options(conds(openPop.scope)[openPop.key], list) + (hint ? `<p class="hint">${esc(hint)}</p>` : "");
  }
  function popover() {
    const t = L();
    if (!openPop) return;
    const anchor = anchorOf(openPop);
    if (!anchor) {
      pop.hidden = true;
      return;
    }
    if (openPop.key == null) {
      const free = dims(openPop.scope).filter((d) => !shownKeys(openPop.scope).includes(d.key));
      pop.innerHTML = `<ul class="addlist" role="menu">${free.map((d) => `<li><button role="menuitem" data-add="${esc(d.key)}">${esc(keyName(d.key))}</button></li>`).join("")}</ul>`;
      pop.setAttribute("aria-label", label(openPop.scope, null));
      pop.hidden = false;
      placePop(anchor);
      pop.querySelector("button")?.focus();
      return;
    }
    const d = dim(openPop.key), w = conds(openPop.scope)[d.key];
    pop.setAttribute("aria-label", label(openPop.scope, d.key));
    if (d.kind === "range") {
      pop.innerHTML = `<div class="head"><span></span><button class="any" data-any>${esc(t.anyButton(keyName(d.key)))}</button></div><div class="range"><label>${esc(t.from)} <input type="number" data-lo step="any" placeholder="${esc(d.min)}" value="${esc(w?.lo ?? "")}"></label><label>${esc(t.to)} <input type="number" data-hi step="any" placeholder="${esc(d.max)}" value="${esc(w?.hi ?? "")}"></label></div>`;
    } else {
      const out = w?.m === "out";
      pop.innerHTML = `<div class="head"><span class="mode" role="group"><button data-mode="in" aria-pressed="${!out}">${esc(t.mode[0])}</button><button data-mode="out" aria-pressed="${out}">${esc(t.mode[1])}</button></span><button class="any" data-any>${esc(t.anyButton(keyName(d.key)))}</button></div>${d.values > MANY ? `<input class="find" type="search" data-find placeholder="${esc(t.findValue)}" aria-label="${esc(t.findValue)}" value="${esc(find)}">` : ""}<div class="opts">${valueList()}</div>`;
    }
    pop.hidden = false;
    placePop(anchor);
  }
  async function openCondition(o) {
    openPop = o;
    find = "";
    values = null;
    sentence.querySelectorAll("[aria-expanded]").forEach((b) => b.setAttribute("aria-expanded", "false"));
    anchorOf(o)?.setAttribute("aria-expanded", "true");
    popover();
    if (o.key != null && dim(o.key)?.kind === "categorical") await loadValues(o);
  }
  async function loadValues(o) {
    const current = view(o.scope), mine = current.revision, needle = find.trim(), request = ++valueRequest;
    let r;
    try {
      r = await api("frequency/values", { ...current.params, key: o.key, q: needle, limit: MANY });
    } catch (error) {
      if (mine === view(o.scope).revision && openPop === o && request === valueRequest && needle === find.trim()) pop.querySelector(".opts").textContent = error.message;
      return;
    }
    if (mine !== view(o.scope).revision || openPop !== o || request !== valueRequest || needle !== find.trim() || r.status !== "ok") return;
    values = r.values;
    valueMatches = r.matching;
    selectedValues = r.selected;
    pop.querySelector(".opts").innerHTML = valueList();
  }
  const setCondition = (scope, key, w) => {
    pending = null;
    commit(scope, key, w);
  };
  pop.addEventListener("input", (e) => {
    if (!e.target.matches("[data-find]")) return;
    find = e.target.value;
    const o = openPop;
    values = null;
    pop.querySelector(".opts").replaceChildren();
    clearTimeout(valueTimer);
    valueTimer = setTimeout(() => {
      if (openPop === o) loadValues(o);
    }, 150);
  });
  pop.addEventListener("change", (e) => {
    const { scope, key } = openPop ?? {};
    if (e.target.matches("input[data-v]")) {
      const w = conds(scope)[key], chosen = new Set(w?.v ?? []);
      e.target.checked ? chosen.add(e.target.dataset.v) : chosen.delete(e.target.dataset.v);
      setCondition(scope, key, { m: w?.m === "out" ? "out" : "in", v: [...chosen].sort() });
    } else if (e.target.matches("[data-lo], [data-hi]")) {
      const read = (sel) => {
        const v = pop.querySelector(sel).value;
        return v === "" || !Number.isFinite(+v) ? undefined : +v;
      };
      const w = { m: "range" }, lo = read("[data-lo]"), hi = read("[data-hi]");
      if (lo !== undefined) w.lo = lo;
      if (hi !== undefined) w.hi = hi;
      setCondition(scope, key, w);
    }
  });
  pop.addEventListener("click", (e) => {
    const add = e.target.closest("[data-add]");
    if (add && openPop) {
      e.stopPropagation();
      pending = { scope: openPop.scope, key: add.dataset.add };
      pop.hidden = true;
      render();
      openCondition(pending);
      return;
    }
    if (e.target.closest("[data-any]")) {
      const { scope, key } = openPop, d = dim(key);
      pending = null;
      closePop();
      setCondition(scope, key, d.kind === "range" ? { m: "range" } : { m: "in", v: [] });
      return;
    }
    const mode = e.target.closest("[data-mode]");
    if (mode) {
      const { scope, key } = openPop;
      setCondition(scope, key, { m: mode.dataset.mode, v: conds(scope)[key]?.v ?? [] });
    }
  });

  document.addEventListener("click", (e) => {
    if (openPop && !pop.contains(e.target) && !e.target.closest(".pick, .add")) closePop();
  });

  return {
    shownKeys,
    isOpen: (key, scope = null) => openPop?.scope === scope && openPop.key === key,
    get opened() { return openPop !== null; },
    open: (key, scope = null) => openCondition({ scope, key }),
    close: closePop,
    refresh({ keepSearchFocus = false } = {}) {
      if (!openPop) return;
      const search = pop.querySelector("[data-find]"), focused = keepSearchFocus && document.activeElement === search;
      const start = search?.selectionStart, end = search?.selectionEnd;
      popover();
      if (focused) {
        const next = pop.querySelector("[data-find]");
        next?.focus();
        next?.setSelectionRange(start, end);
      }
    },
    reloadValues() {
      clearTimeout(valueTimer);
      if (openPop && dim(openPop.key)?.kind === "categorical") {
        values = null;
        pop.querySelector(".opts")?.replaceChildren();
        loadValues(openPop);
      }
    },
  };
}
