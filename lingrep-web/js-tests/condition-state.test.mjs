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

test("exact key values round-trip through the frequency address and malformed ones are rejected", async () => {
  const { stateEntries } = await import("../assets/frequency.js");
  const k = { "$n.lemma": "坂", "$p.lemma": "を" };
  const state = frequencyState(`#${new URLSearchParams({ pattern: "npv", k: JSON.stringify(k) })}`);
  assert.equal(state.bad, false);
  assert.deepEqual(state.k, k);
  assert.deepEqual(frequencyState(`#${new URLSearchParams(stateEntries(state))}`).k, k);
  assert.equal(stateEntries({ ...state, k: {} }).some(([key]) => key === "k"), false);
  for (const value of [null, [], "坂", { "$n.lemma": 1 }, { "$n.lemma": ["坂"] }]) {
    const bad = frequencyState(`#${new URLSearchParams({ k: JSON.stringify(value) })}`);
    assert.equal(bad.bad, true, JSON.stringify(value));
    assert.deepEqual(bad.k, {});
  }
});
