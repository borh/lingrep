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

const SLOT_ATTRIBUTES = ["title", "placeholder", "aria-label"];
const SLOTS = ["[data-export-link]", "[data-i18n]", ...SLOT_ATTRIBUTES.map((attr) => `[data-i18n-${attr}]`)].join(",");

/**
 * Localizes opt-in text and attributes in `root`, including `root` itself.
 * English values are restored when switching back; unknown or empty translations
 * leave their target unchanged. Export links receive a locale query parameter.
 * The function is idempotent.
 */
export function localize(root = document, dict = undefined, locale = current) {
  const table = dict ?? dictionaries[locale];
  const elements = root.querySelectorAll ? [...root.querySelectorAll(SLOTS)] : [];
  if (root.matches?.(SLOTS)) elements.unshift(root);
  for (const element of elements) {
    if (element.dataset.exportLink !== undefined) {
      const url = new URL(element.getAttribute("href"), document.baseURI);
      url.searchParams.set("locale", locale);
      if (element.href !== url.href) element.href = url.href;
    }
    const args = readArgs(element);
    const key = element.dataset.i18n;
    if (key && locale !== "en") {
      const text = lookup(locale, key, args, table);
      if (text) {
        element.dataset.i18nSrc ??=
          element.textContent === text
            ? (lookup("en", key, args) ?? text)
            : element.textContent;
        if (element.textContent !== text) element.textContent = text;
      }
    } else if (key && element.dataset.i18nSrc !== undefined) {
      element.textContent = element.dataset.i18nSrc;
    }
    for (const attr of SLOT_ATTRIBUTES) {
      // data-i18n-title/-placeholder/-aria-label → camelCase slot name
      const slot = attr.replace(/^(.)|-(\w)/g, (_, first, dash) =>
        (first ?? dash).toUpperCase(),
      );
      const attrKey = element.dataset[`i18n${slot}`];
      if (!attrKey) continue;
      const source = `i18nSrc${slot}`;
      const saved = element.dataset[source];
      if (locale !== "en") {
        const translated = table ? lookup(locale, attrKey, args, table) : null;
        if (translated) {
          if (saved === undefined) {
            const served = element.getAttribute(attr) ?? "";
            element.dataset[source] =
              served === translated ? (lookup("en", attrKey, args) ?? served) : served;
          }
          element.setAttribute(attr, translated);
        }
      } else if (saved !== undefined) {
        element.setAttribute(attr, saved);
      }
    }
  }
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

export async function initI18n() {
  await apply(read(), { persist: false });
  new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === "attributes") {
        delete record.target.dataset.i18nSrc;
        localize(record.target);
        continue;
      }
      for (const node of record.addedNodes) {
        if (node.nodeType === Node.ELEMENT_NODE) localize(node);
      }
    }
  }).observe(document.documentElement, {
    childList: true,
    subtree: true,
    attributeFilter: ["data-i18n"],
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
