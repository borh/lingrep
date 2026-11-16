import test from "node:test";
import assert from "node:assert/strict";
import { documentNames, entryFieldNames, inspectHref, keyFieldLabel, readState, stateEntries } from "../assets/frequency.js";
import { parseOccurrence, frequencyReturn } from "../question-ui/location.js";

test("a frequency line's handoff address names its occurrence and document, and returns to the conditioned entry", () => {
  const s = readState(`#${new URLSearchParams({ corpus: "natsume", catalog: "class6.lx", pattern: "npv", entry: '{"kind":"key"}', k: JSON.stringify({ "$n.lemma": "坂", "$p.lemma": "を" }), w: JSON.stringify({ genre: { m: "in", v: ["novel"] } }) })}`);
  const back = `frequencies#${new URLSearchParams(stateEntries(s))}`;
  const line = { sentence: 41, document: 5, start: 0, end: 2, roles: [{ name: "n", start: 0, end: 1 }, { name: "p", start: 1, end: 2 }, { name: "v", start: 3, end: 4 }] };
  const identity = { source: "npv := [].", analysis: "a".repeat(64), fingerprint: "f".repeat(64) };
  const href = inspectHref(s, line, identity, back);
  assert.ok(href.startsWith("./#"));
  const params = new URLSearchParams(href.slice(3));
  assert.deepEqual(Object.fromEntries([...params].filter(([key]) => !["occurrence", "return"].includes(key))), {
    fresh: "1", run: "1", corpus: "natsume", analysis: identity.analysis, source: identity.source,
  });
  assert.deepEqual(parseOccurrence(params.get("occurrence")), { pattern: "npv", sentence: 41, document: 5, roles: line.roles, analysis: identity.analysis, fingerprint: identity.fingerprint });
  const returned = frequencyReturn(params.get("return"));
  assert.equal(returned, back);
  assert.deepEqual(readState(returned.slice("frequencies".length)), s);
});

test("an exact-key field is named by its capture and attribute in plain words", () => {
  assert.equal(keyFieldLabel("$n.lemma"), "n: lemma (dictionary form)");
  assert.equal(keyFieldLabel("$v.unidic.cform"), "v: unidic.cform");
  assert.equal(keyFieldLabel("upos"), "upos (part of speech)");
});

test("frequency lines name documents by source name unless one source name covers several documents", () => {
  const named = documentNames([
    { document: 0, source_name: "Natsume_S_Bocchan.conllu" },
    { document: 5, source_name: "Natsume_S_Maboroshinotate.conllu" },
    { document: 7, source_name: "doc.md" },
    { document: 8, source_name: "doc.md" },
  ]);
  assert.equal(named({ document: 5, source_name: "Natsume_S_Maboroshinotate.conllu" }), "Natsume_S_Maboroshinotate");
  assert.equal(named({ document: 8, source_name: "doc.md" }), "document 8");
  assert.equal(named({ document: 9 }), "document 9");
});

test("a multi-field entry lists its captures when every field is a capture's attribute", () => {
  assert.equal(entryFieldNames(["$n.lemma", "$p.lemma", "$v.lemma"]), "n · p · v");
  assert.equal(entryFieldNames(["lemma", "upos"]), "lemma (dictionary form) · upos (part of speech)");
});
