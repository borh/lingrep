// Client-side locale machinery for lingrep-web.
//
// Language model: the DOM as served is the English canonical. `localize`
// applies the active locale's dictionary over it and remembers the English
// text so toggling back to English is a restore, not a re-render. A missing
// or empty dictionary entry changes nothing — partial coverage degrades
// silently instead of damaging known-good server text.

import enDictionary from "./i18n/en.json" with { type: "json" };

const STORE_KEY = "lingrep-web:locale:v1";
const MODULE_URL = new URL(".", import.meta.url);

let current = "en";
// English strings ship statically: they are the default locale and the
// fallback for JS-created strings, so they cannot depend on a fetch, and
// under Node (js tests) there is no fetch to rely on.
let dictionaries = { en: enDictionary };
const listeners = [];

const supported = (locale) => locale === "ja" || locale === "en";

const save = (locale) => {
  try {
    globalThis.localStorage?.setItem(STORE_KEY, locale);
  } catch (_error) {
    // Preference persistence is non-blocking.
  }
};

const read = () => {
  try {
    const stored = globalThis.localStorage?.getItem(STORE_KEY);
    if (stored && supported(stored)) return stored;
  } catch (_error) {
    // Fall through to the browser hint.
  }
  const nav = (globalThis.navigator?.language || "").toLowerCase();
  return nav.startsWith("ja") ? "ja" : "en";
};

async function dictionary(locale) {
  if (dictionaries[locale]) return dictionaries[locale];
  const response = await fetch(new URL(`i18n/${locale}.json`, MODULE_URL));
  if (!response.ok) return (dictionaries[locale] = {});
  dictionaries[locale] = await response.json();
  return dictionaries[locale];
}

/** Lookup with `{param}` interpolation; null when the key is absent/empty. */
export function lookup(locale, key, params, dict) {
  const table = dict ?? dictionaries[locale];
  if (!table) return null;
  const entry = table[key];
  if (typeof entry !== "string" || entry === "") return null;
  if (!params) return entry;
  return entry.replace(/\{(\w+)\}/g, (m, name) =>
    params[name] !== undefined ? String(params[name]) : m,
  );
}

/** JS-side string lookup helper for the active locale. */
export function t(key, params = null) {
  return lookup(current, key, params) ?? lookup("en", key, params) ?? key;
}

export function currentLocale() {
  return current;
}

/** Subscribe to locale switches; subscribers re-localize their own strings. */
export function onLocaleChange(fn) {
  listeners.push(fn);
  return () => listeners.splice(listeners.indexOf(fn), 1);
}

/** Interpolation arguments ride as JSON in `data-i18n-args`. */
function readArgs(element) {
  const json = element.dataset.i18nArgs;
  if (!json) return null;
  try {
    return JSON.parse(json);
  } catch (_error) {
    return null;
  }
}

/**
 * Applies the active locale over `root` (element or document). Every slot is
 * opt-in: `data-i18n` rewrites textContent from the dictionary, restoring the
 * served English on toggle-back. Unknown keys and empty values are ignored —
 * they never mutate the DOM. The function is idempotent.
 */
export function localize(root = document, dict = undefined, locale = current) {
  const scoped = root.querySelectorAll
    ? root.querySelectorAll("[data-i18n]")
    : [];
  for (const element of scoped) {
    const key = element.dataset.i18n;
    const text =
      locale === "en"
        ? null
        : lookup(locale, key, readArgs(element), dict ?? dictionaries[locale]);
    if (locale !== "en" && text) {
      element.dataset.i18nSrc ??= element.textContent;
      element.textContent = text;
    } else if (locale === "en" && element.dataset.i18nSrc !== undefined) {
      element.textContent = element.dataset.i18nSrc;
    }
    for (const attr of ["title", "placeholder", "aria-label"]) {
      // data-i18n-title/-placeholder/-aria-label → camelCase slot name
      const slot = attr.replace(/^(.)|-(\w)/g, (_, first, dash) =>
        (first ?? dash).toUpperCase(),
      );
      const attrKey = element.dataset[`i18n${slot}`];
      if (!attrKey) continue;
      const english = element.getAttribute(attr);
      const saved = element.dataset[`i18nSrc${attr}`];
      if (locale !== "en") {
        const translated =
          dict ?? dictionaries[locale]
            ? lookup(locale, attrKey, readArgs(element), dict ?? dictionaries[locale])
            : null;
        if (translated) {
          if (saved === undefined) element.dataset[`i18nSrc${attr}`] = english ?? "";
          element.setAttribute(attr, translated);
        }
      } else if (saved !== undefined) {
        element.setAttribute(attr, saved);
      }
    }
  }
  // Dual-label spans (token inspection fields + groups) are CSS-gated by
  // `data-locale`; nothing to do here beyond stamping the attribute on the
  // element if it opted in explicitly.
}

async function apply(locale, { persist = true } = {}) {
  if (!supported(locale) || locale === current) return;
  if (locale !== "en") await dictionary(locale);
  current = locale;
  document.documentElement.dataset.locale = locale;
  document.documentElement.lang = locale === "ja" ? "ja" : "en";
  localize();
  if (persist) save(locale);
  for (const listener of listeners) listener(locale);
  updateToggle();
}

const toggle = () => document.querySelector("[data-locale-toggle]");

function updateToggle() {
  const button = toggle();
  if (!button) return;
  button.textContent = current === "ja" ? "English" : "日本語";
  button.setAttribute("aria-label", current === "ja" ? "Switch to English" : "日本語に切り替え");
}

/** Boots the machinery: restore, localize, route swaps, wire the toggle. */
export async function initI18n() {
  await apply(read(), { persist: false });
  document.addEventListener("htmx:afterSwap", (event) => {
    const target = event.detail?.target;
    if (target instanceof Element) localize(target);
    else if (target instanceof Document) localize(target);
  });
  document.addEventListener("htmx:oobAfterSwap", (event) => {
    const target = event.detail?.target;
    if (target instanceof Element) localize(target);
  });
  document.addEventListener("click", (event) => {
    const button = event.target.closest("[data-locale-toggle]");
    if (button) void apply(current === "ja" ? "en" : "ja");
  });
  updateToggle();
}

if (typeof document !== "undefined" && document.addEventListener) {
  document.addEventListener("DOMContentLoaded", () => {
    void initI18n();
  });
}
