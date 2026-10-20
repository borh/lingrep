import test from "node:test";
import assert from "node:assert/strict";

import {
  editorExpanded,
  parseEditorHeight,
  setToggleLabel,
} from "../assets/editor-sizing.js";

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
    setAttribute(name, value) { this.attributes[name] = value; } };
  const toggle = { querySelector: (selector) =>
    selector === "[data-i18n]" ? span : null };

  assert.equal(setToggleLabel(toggle, true), span);
  assert.equal(span.attributes["data-i18n"], "editor.collapse");
  assert.equal(span.textContent, "Collapse editor");

  setToggleLabel(toggle, false);
  assert.equal(span.attributes["data-i18n"], "editor.expand");
  assert.equal(span.textContent, "Expand editor");
});
