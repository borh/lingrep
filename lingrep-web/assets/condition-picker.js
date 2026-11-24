import { t } from "./i18n.js";
import { count as num } from "./numbers.js";
import { api, esc } from "./views.js";

const MANY = 30;
const CONTROLS = ["data-v", "data-mode", "data-lo", "data-hi", "data-find", "data-any", "data-add"];
/** The line under a value list that holds `shown` of `matching` values; "" when it holds them all. */
export function valuesHint(q, shown, matching) {
  if (matching <= shown) return "";
  return t(q ? "picker.found_values" : "picker.top_values", { n: num(shown), of: num(matching) });
}

export function createConditionPicker({ sentence, view, keyName, label, anchor, commit, render }) {
  const pop = Object.assign(document.createElement("div"), { className: "fq-pop", hidden: true });
  pop.id = "condition-picker";
  pop.setAttribute("role", "dialog");
  sentence.after(pop);
  let openPop = null, drawn = null, pending = null, find = "", values = null, valueMatches = 0, selectedValues = [], valueRequest = 0, valueTimer;
  const dims = (scope) => view(scope).dimensions;
  const conds = (scope) => view(scope).conditions;
  const dim = (key) => dims(openPop?.scope).find((d) => d.key === key);
  const shownKeys = (scope = null) => dims(scope).map((d) => d.key).filter((k) => k in conds(scope) || (pending?.scope === scope && pending.key === k));
  function placePop(anchor) {
    const viewport = window.visualViewport;
    const left = viewport?.offsetLeft ?? 0, top = viewport?.offsetTop ?? 0;
    const width = viewport?.width ?? innerWidth, height = viewport?.height ?? innerHeight;
    const minLeft = left + 8, minTop = top + 8, maxBottom = top + height - 8;
    const r = anchor.getBoundingClientRect();
    pop.style.maxWidth = `${Math.max(0, width - 16)}px`;
    const desiredHeight = pop.scrollHeight + pop.offsetHeight - pop.clientHeight;
    const belowTop = Math.max(minTop, r.bottom + 6), aboveBottom = Math.min(maxBottom, r.top - 6);
    const below = Math.max(0, maxBottom - belowTop), above = Math.max(0, aboveBottom - minTop);
    const aboveAnchor = desiredHeight > below && above > below;
    const room = aboveAnchor ? above : below;
    const fitBeside = room >= Math.min(desiredHeight, 120);
    pop.style.maxHeight = `${fitBeside ? room : Math.max(0, height - 16)}px`;
    pop.style.top = `${fitBeside ? (aboveAnchor ? aboveBottom - pop.offsetHeight : belowTop) : minTop}px`;
    pop.style.left = `${Math.max(minLeft, Math.min(r.left, left + width - pop.offsetWidth - 8))}px`;
  }
  const anchorOf = (o) => anchor(o.scope, o.key);
  let placementFrame = 0;
  function schedulePlacement() {
    if (pop.hidden || placementFrame) return;
    placementFrame = requestAnimationFrame(() => {
      placementFrame = 0;
      const trigger = openPop && anchorOf(openPop);
      if (trigger && !pop.hidden) placePop(trigger);
    });
  }
  window.addEventListener("scroll", schedulePlacement, { passive: true });
  window.visualViewport?.addEventListener("scroll", schedulePlacement);
  window.visualViewport?.addEventListener("resize", schedulePlacement);
  function closePop() {
    const o = openPop, active = document.activeElement;
    const refocus = o && (pop.contains(active) || active === document.body);
    openPop = null;
    drawn = null;
    values = null;
    find = "";
    pop.hidden = true;
    if (pending && !(pending.key in conds(pending.scope))) {
      pending = null;
      render();
    } else sentence.querySelectorAll("[aria-expanded]").forEach((b) => b.setAttribute("aria-expanded", "false"));
    if (refocus) (anchorOf(o) ?? anchor(o.scope, null))?.focus({ preventScroll: true });
  }
  function controls() {
    const f = document.activeElement, attr = pop.contains(f) ? CONTROLS.find((a) => f.hasAttribute(a)) : null;
    return {
      focus: attr && `[${attr}="${CSS.escape(f.getAttribute(attr))}"]`,
      caret: attr === "data-find" ? [f.selectionStart, f.selectionEnd] : null,
      top: pop.scrollTop,
    };
  }
  function restore(c) {
    const f = c.focus && pop.querySelector(c.focus);
    f?.focus({ preventScroll: true });
    if (c.caret) f?.setSelectionRange(...c.caret);
    pop.scrollTop = c.top;
  }
  function options(w, list) {
    const chosen = new Set(w?.v ?? []), max = Math.max(1, ...list.map(([, n]) => n));
    return list.map(([v, n]) => `<label><input type="checkbox" data-v="${esc(v)}"${chosen.has(v) ? " checked" : ""}><span>${esc(v)}</span><span class="b" style="width:${((n / max) * 100).toFixed(1)}%"></span><span class="v">${num(n)}</span></label>`).join("");
  }
  function valueList() {
    if (!values) return "";
    const q = find.trim(), seen = new Set(values.map(([v]) => v));
    const list = q ? values : [...values, ...selectedValues.filter(([v]) => !seen.has(v))];
    const hint = valuesHint(q, values.length, valueMatches);
    return options(conds(openPop.scope)[openPop.key], list) + (hint ? `<p class="hint">${esc(hint)}</p>` : "");
  }
  function popover() {
    if (!openPop) return;
    const anchor = anchorOf(openPop);
    if (!anchor) {
      pop.hidden = true;
      return;
    }
    const kept = drawn === openPop ? controls() : null;
    if (openPop.key == null) {
      const free = dims(openPop.scope).filter((d) => !shownKeys(openPop.scope).includes(d.key));
      pop.innerHTML = `<ul class="addlist">${free.map((d) => `<li><button type="button" data-add="${esc(d.key)}">${esc(keyName(d.key))}</button></li>`).join("")}</ul>`;
      pop.setAttribute("aria-label", label(openPop.scope, null));
      pop.hidden = false;
      placePop(anchor);
      drawn = openPop;
      if (kept) restore(kept);
      else pop.querySelector("button")?.focus({ preventScroll: true });
      return;
    }
    const d = dim(openPop.key), w = conds(openPop.scope)[d.key];
    pop.setAttribute("aria-label", label(openPop.scope, d.key));
    if (d.kind === "range") {
      if (drawn !== openPop) pop.innerHTML = `<div class="head"><span></span><button class="any" data-any>${esc(t("picker.any", { name: keyName(d.key) }))}</button></div><div class="range"><label>${esc(t("picker.from"))} <input type="number" data-lo step="any" placeholder="${esc(d.min)}" value="${esc(w?.lo ?? "")}"></label><label>${esc(t("picker.to"))} <input type="number" data-hi step="any" placeholder="${esc(d.max)}" value="${esc(w?.hi ?? "")}"></label></div>`;
    } else {
      const out = w?.m === "out";
      pop.innerHTML = `<div class="head"><span class="mode" role="group"><button data-mode="in" aria-pressed="${!out}">${esc(t("picker.mode_in"))}</button><button data-mode="out" aria-pressed="${out}">${esc(t("picker.mode_out"))}</button></span><button class="any" data-any>${esc(t("picker.any", { name: keyName(d.key) }))}</button></div>${d.values > MANY ? `<input class="find" type="search" data-find placeholder="${esc(t("picker.find_value"))}" aria-label="${esc(t("picker.find_value"))}" value="${esc(find)}">` : ""}<div class="opts">${valueList()}</div>`;
    }
    pop.hidden = false;
    placePop(anchor);
    drawn = openPop;
    if (kept) restore(kept);
  }
  async function openCondition(o) {
    openPop = o;
    find = "";
    values = null;
    sentence.querySelectorAll("[aria-expanded]").forEach((b) => b.setAttribute("aria-expanded", "false"));
    anchorOf(o)?.setAttribute("aria-expanded", "true");
    popover();
    if (o.key != null) (pop.querySelector("[data-find], [data-lo]") ?? pop.querySelector("button"))?.focus({ preventScroll: true });
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
    if (mine !== view(o.scope).revision || openPop !== o || request !== valueRequest || needle !== find.trim()) return;
    const kept = controls();
    if (r.status === "ok") [values, valueMatches, selectedValues] = [r.values, r.matching, r.selected];
    else values = null;
    pop.querySelector(".opts").innerHTML = valueList();
    schedulePlacement();
    restore(kept);
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
    if (!openPop) return;
    const { scope, key } = openPop;
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
    refresh() {
      if (openPop) popover();
    },
    reloadValues() {
      clearTimeout(valueTimer);
      if (openPop && dim(openPop.key)?.kind === "categorical") loadValues(openPop);
    },
  };
}
