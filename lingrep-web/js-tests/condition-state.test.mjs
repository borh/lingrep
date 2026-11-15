import test from "node:test";
import assert from "node:assert/strict";
import { readState as frequencyState } from "../assets/frequency.js";
import { readState as compareState } from "../assets/compare.js";

const readers = [[frequencyState, "w"], [compareState, "w"], [compareState, "v"]];

test("malformed conditions are rejected before rendering", () => {
  for (const [read, key] of readers) {
    for (const value of [
      null, [], "text", 1,
      { author: null }, { author: [] }, { author: "ab" },
      { author: { m: "in", v: "ab" } },
      { author: { m: "out", v: [3] } },
      { author: { m: "in", v: [null] } },
      { author: { m: "in", v: null } },
      { year: { m: "range", lo: "1900" } },
      { year: { m: "range", hi: {} } },
      { author: { m: "unknown", v: [] } },
      { author: { v: ["a"] } },
    ]) {
      const state = read(`#${new URLSearchParams({ [key]: JSON.stringify(value) })}`);
      assert.equal(state.bad, true, `${key}: ${JSON.stringify(value)}`);
      assert.deepEqual(state[key], {});
    }
    assert.equal(read(`#${key}={"year":{"m":"range","lo":1e400}}`).bad, true);
  }
});

test("reading valid conditions preserves defaults, ranges, Unicode and unconsumed fields", () => {
  for (const [read, key] of readers) {
    for (const value of [
      {}, { author: { m: "in" } }, { author: { m: "out", v: [] } },
      { author: { m: "in", v: ["日本", "a & b"], future: { value: 1 } } },
      { year: { m: "range", lo: 1900, hi: 2026.5 } },
      { year: { m: "range", lo: null } },
    ]) {
      const state = read(`#${new URLSearchParams({ [key]: JSON.stringify(value) })}`);
      assert.equal(state.bad, false);
      assert.deepEqual(state[key], value);
    }
  }
});
