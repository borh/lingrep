import test from "node:test";
import assert from "node:assert/strict";
import { inspectHref, readState, stateEntries } from "../assets/frequency.js";
import { parseOccurrence, frequencyReturn } from "../question-ui/location.js";

test("a frequency line's handoff address names its occurrence, scope and return for the question page", () => {
  const s = readState(`#${new URLSearchParams({ corpus: "natsume", catalog: "class6.lx", pattern: "npv", entry: '{"kind":"key"}', k: JSON.stringify({ "$n.lemma": "坂", "$p.lemma": "を" }), w: JSON.stringify({ genre: { m: "in", v: ["novel"] } }) })}`);
  const back = `frequencies#${new URLSearchParams(stateEntries(s))}`;
  const line = { sentence: 41, start: 0, end: 2, roles: [{ name: "n", start: 0, end: 1 }, { name: "p", start: 1, end: 2 }, { name: "v", start: 3, end: 4 }] };
  const identity = { source: "npv := [].", clause: 'genre in {"novel"}', analysis: "a".repeat(64), fingerprint: "f".repeat(64) };
  const href = inspectHref(s, line, identity, back);
  assert.ok(href.startsWith("./#"));
  const params = new URLSearchParams(href.slice(3));
  assert.deepEqual(Object.fromEntries([...params].filter(([key]) => !["occurrence", "return"].includes(key))), {
    fresh: "1", run: "1", corpus: "natsume", analysis: identity.analysis, source: identity.source, raw: identity.clause,
  });
  assert.deepEqual(parseOccurrence(params.get("occurrence")), { pattern: "npv", sentence: 41, roles: line.roles, analysis: identity.analysis, fingerprint: identity.fingerprint });
  const returned = frequencyReturn(params.get("return"));
  assert.equal(returned, back);
  assert.deepEqual(readState(returned.slice("frequencies".length)), s);
  assert.equal(new URLSearchParams(inspectHref(s, line, { ...identity, clause: "" }, back).slice(3)).has("raw"), false);
});
