import test from "node:test";
import assert from "node:assert/strict";
import { valuesHint } from "../assets/condition-picker.js";

test("a value list that holds every value has no hint", () => {
  assert.equal(valuesHint("", 30, 30), "");
  assert.equal(valuesHint("nov", 4, 4), "");
});

test("a partial value list shows the listed and total counts", () => {
  assert.equal(valuesHint("", 30, 1200), "Top 30 of 1,200");
  assert.equal(valuesHint("nov", 30, 45), "Top 30 of 45 matches");
});

test("listed and total counts are compared numerically across digit lengths", () => {
  assert.equal(valuesHint("nov", 30, 100), "Top 30 of 100 matches");
  assert.equal(valuesHint("nov", 30, 1200), "Top 30 of 1,200 matches");
});
