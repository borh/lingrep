import test from "node:test";
import assert from "node:assert/strict";
import { readState, stateEntries } from "../assets/compare.js";

/** A deterministic generator, so a failure names its seed. */
function random(seed) {
  let x = seed >>> 0 || 1;
  return (n) => {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    return (x >>> 0) % n;
  };
}

const TEXTS = ["", "novel", "a & b", "#?=%", "日本文学", "@document.ndc contains \"914\""];

function state(pick) {
  const text = () => TEXTS[pick(TEXTS.length)] || null;
  const conditions = () => {
    const w = {};
    for (const key of ["genre", "ndc", "year"]) {
      const kind = pick(4);
      if (kind === 1) w[key] = { m: "in", v: [TEXTS[1 + pick(TEXTS.length - 1)]] };
      else if (kind === 2) w[key] = { m: "out", v: [] };
      else if (kind === 3) w[key] = { m: "range", lo: pick(3000) };
    }
    return w;
  };
  return { corpus: text(), catalog: text(), pattern: text(), where: text(), versus: text(), w: conditions(), v: conditions(), bad: false };
}

test("an address written from a state reads back as that state", () => {
  for (let seed = 1; seed <= 500; seed++) {
    const s = state(random(seed));
    const hash = `#${new URLSearchParams(stateEntries(s))}`;
    assert.deepEqual(readState(hash), s, `seed ${seed}`);
  }
});

test("conditions that are not an object mark the address unreadable", () => {
  for (const raw of ["[]", "1", "{", "null"]) {
    assert.equal(readState(`#v=${encodeURIComponent(raw)}`).bad, true, raw);
  }
});
