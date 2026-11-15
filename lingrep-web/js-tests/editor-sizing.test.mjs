import test from "node:test";
import assert from "node:assert/strict";

import {
  editorExpanded,
  parseEditorHeight,
  relocalize,
  setToggleLabel,
} from "../assets/editor-sizing.js";
import { localize } from "../assets/i18n.js";

test("editor height clamps between 4.5rem and 90vh", () => {
  // The floor is the pinned band's four-line editor, not the ten-line default.
  assert.equal(parseEditorHeight("50", 1000, 16), 72);
  assert.equal(parseEditorHeight("100", 1000, 16), 100);
  assert.equal(parseEditorHeight("500", 1000, 16), 500);
  assert.equal(parseEditorHeight("2000", 1000, 16), 900);
  assert.equal(parseEditorHeight("garbage", 1000, 16), null);
  assert.equal(parseEditorHeight(null, 1000, 16), null);
});

test("expanded preference is strict", () => {
  assert.equal(editorExpanded("true"), true);
  assert.equal(editorExpanded("false"), false);
  assert.equal(editorExpanded("1"), false);
});

test("the expand toggle relabels its i18n span rather than replacing it", () => {
  // Writing the button's own textContent deleted the translated span, so the
  // label stopped being translated the first time it was pressed.
  const span = { attributes: {}, textContent: "Expand editor",
    setAttribute(name, value) { this.attributes[name] = value; },
    removeAttribute(name) { delete this.attributes[name]; } };
  const toggle = { querySelector: (selector) =>
    selector === "[data-i18n]" ? span : null };

  assert.equal(setToggleLabel(toggle, true), span);
  assert.equal(span.attributes["data-i18n"], "editor.collapse");
  assert.equal(span.textContent, "Collapse editor");

  setToggleLabel(toggle, false);
  assert.equal(span.attributes["data-i18n"], "editor.expand");
  assert.equal(span.textContent, "Expand editor");
});

test("a relabelled toggle is translated, and reverts to the English of its current label", () => {
  const ja = { "editor.expand": "エディタを拡大", "editor.collapse": "エディタを縮小" };
  const span = {
    dataset: { i18n: "editor.expand" },
    textContent: "Expand editor",
    getAttribute: () => null,
    setAttribute(name, value) { if (name === "data-i18n") this.dataset.i18n = value; },
    removeAttribute(name) { if (name === "data-i18n-src") delete this.dataset.i18nSrc; },
  };
  const toggle = {
    querySelector: () => span,
    querySelectorAll: () => [span],
  };
  span.parentElement = toggle;
  span.querySelectorAll = () => [];
  localize(toggle, ja, "ja");

  relocalize(setToggleLabel(toggle, true), (root) => localize(root, ja, "ja"));
  assert.equal(span.textContent, "エディタを縮小");

  localize(toggle, ja, "en");
  assert.equal(span.textContent, "Collapse editor");
});
