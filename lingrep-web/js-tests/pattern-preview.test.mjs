import assert from "node:assert/strict";
import test from "node:test";
import { patternPreview } from "../assets/editor/pattern-editor.js";

// The folded editor bar previews intent, never grammar boilerplate: catalogs
// conventionally start with `defaults` / `set` / `fragment` lines.
test("preview names the first pattern-kind definition", () => {
  assert.equal(
    patternPreview("defaults\nset VERBAL := {VERB}.\nfragment 動詞終止形 := [pos=VERB].\n動詞終止形 := [orth=る]."),
    "Pattern: 動詞終止形 (+1 more)"
  );
});

test("preview falls back to a lone bare definition", () => {
  assert.equal(patternPreview("predication := [pos=NOUN]."), "Pattern: predication");
});

test("preview uses the count when only grammar-level statements exist", () => {
  assert.equal(patternPreview("defaults\nset V := {VERB}.\n"), "Pattern: V");
});

test("preview degenerates to the label when nothing parses", () => {
  assert.equal(patternPreview("# only comments\n\n"), "Pattern");
  assert.equal(patternPreview(""), "Pattern");
});

test("non-ASCII names survive intact", () => {
  assert.equal(patternPreview("動詞終止形 := [pos=VERB & cform=終止形]."), "Pattern: 動詞終止形");
});
