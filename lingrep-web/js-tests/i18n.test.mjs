import test from "node:test";
import assert from "node:assert/strict";
import { lookup, localize } from "../assets/i18n.js";

/** Minimal element stand-in matching the localize contract. */
function node({ key = null, src = "Hello", title = null } = {}) {
  const state = {
    textContent: src,
    dataset: new Proxy(
      {},
      {
        set(target, name, value) {
          if (name.includes("-")) throw new SyntaxError(`invalid name ${name}`);
          target[name] = value;
          return true;
        },
      },
    ),
    attrs: {},
  };
  if (key) state.dataset.i18n = key;
  if (title !== null) {
    state.dataset.i18nTitle = title;
    state.attrs.title = title;
  }
  return {
    get textContent() {
      return state.textContent;
    },
    set textContent(v) {
      state.textContent = v;
    },
    dataset: state.dataset,
    matches: (selector) => carries(state.dataset, selector),
    getAttribute: (name) => state.attrs[name] ?? null,
    setAttribute: (name, value) => {
      state.attrs[name] = value;
    },
  };
}

function carries(dataset, selector) {
  return selector.split(",").some((part) => {
    const name = part.trim().slice("[data-".length, -1);
    return name.replace(/-(\w)/g, (_, letter) => letter.toUpperCase()) in dataset;
  });
}

function root(...elements) {
  return {
    querySelectorAll: (selector) =>
      elements.filter((element) => element.matches(selector)),
  };
}

const ja = {
  "greeting.hi": "こんにちは",
  "status.scanning": "スキャン中: {sentences}文 / {tokens}トークン…",
  "empty.value": "", // deliberately empty: must never clear the node
};

test("missing keys never mutate the node", () => {
  const n = node({ key: "nope.not.there" });
  localize(root(n), ja, "ja");
  assert.equal(n.textContent, "Hello");
});

test("empty dictionary values never clear the node", () => {
  const n = node({ key: "empty.value" });
  localize(root(n), ja, "ja");
  assert.equal(n.textContent, "Hello");
});

test("a present key rewrites and memoizes the English source", () => {
  const n = node({ key: "greeting.hi" });
  localize(root(n), ja, "ja");
  assert.equal(n.textContent, "こんにちは");
  assert.equal(n.dataset.i18nSrc, "Hello");
});

test("switching back to English restores the served text", () => {
  const n = node({ key: "greeting.hi" });
  localize(root(n), ja, "ja");
  localize(root(n), ja, "en");
  assert.equal(n.textContent, "Hello");
});

test("params interpolate into {placeholders} from data-i18n-args", () => {
  const n = node({ key: "status.scanning" });
  n.dataset.i18nArgs = JSON.stringify({ sentences: 7040, tokens: 91509 });
  localize(root(n), ja, "ja");
  assert.equal(n.textContent, "スキャン中: 7040文 / 91509トークン…");
});

test("title attributes restore and rewrite", () => {
  const n = node({ key: null, title: "Existing title" });
  n.dataset.i18nTitle = "greeting.hi";
  localize(root(n), ja, "ja");
  assert.equal(n.getAttribute("title"), "こんにちは");
  localize(root(n), ja, "en");
  assert.equal(n.getAttribute("title"), "Existing title");
});

test("lookup returns null for absent and empty entries", () => {
  assert.equal(lookup("ja", "missing", null, ja), null);
  assert.equal(lookup("ja", "empty.value", null, ja), null);
  assert.equal(lookup("en", "greeting.hi", null, { "greeting.hi": "hi en" }), "hi en");
});

test("an element that carries only an attribute slot is localized", () => {
  const n = node({ title: "Existing title" });
  n.dataset.i18nTitle = "greeting.hi";
  localize(root(n), ja, "ja");
  assert.equal(n.getAttribute("title"), "こんにちは");
  assert.equal(n.textContent, "Hello");
});

test("the root element's own slots are localized", () => {
  const n = node({ key: "greeting.hi" });
  n.querySelectorAll = () => [];
  localize(n, ja, "ja");
  assert.equal(n.textContent, "こんにちは");
});

test("an aria-label slot rewrites and restores", () => {
  const n = node();
  n.setAttribute("aria-label", "Corpus");
  n.dataset.i18nAriaLabel = "greeting.hi";
  localize(root(n), ja, "ja");
  assert.equal(n.getAttribute("aria-label"), "こんにちは");
  localize(root(n), ja, "en");
  assert.equal(n.getAttribute("aria-label"), "Corpus");
});

test("text a script wrote in the active locale restores to the English entry", () => {
  const n = node({ key: "nav.help", src: "ヘルプ" });
  localize(root(n), { "nav.help": "ヘルプ" }, "ja");
  localize(root(n), { "nav.help": "ヘルプ" }, "en");
  assert.equal(n.textContent, "Help");
});

test("an attribute a script wrote in the active locale restores to the English entry", () => {
  const n = node();
  n.setAttribute("aria-label", "ヘルプ");
  n.dataset.i18nAriaLabel = "nav.help";
  localize(root(n), { "nav.help": "ヘルプ" }, "ja");
  localize(root(n), { "nav.help": "ヘルプ" }, "en");
  assert.equal(n.getAttribute("aria-label"), "Help");
});
