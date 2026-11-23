import test from "node:test";
import assert from "node:assert/strict";
import { contextHtml, documentNames, entryFieldNames, inspectHref, keyFieldLabel, readState, stateEntries } from "../assets/frequency.js";
import { parseOccurrence, parseEntryLines, frequencyReturn } from "../question-ui/location.js";

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

test("a line's handoff carries its entry's words, total and lines, at the line's position", () => {
  const s = readState(`#${new URLSearchParams({ corpus: "natsume", catalog: "class6.lx", pattern: "npv", entry: '{"kind":"key"}' })}`);
  const roles = (n) => [{ name: "n", start: n, end: n + 1 }];
  const lines = [{ sentence: 3, document: 1, source_name: "Natsume_S_Meian.conllu", start: 0, end: 2, left: "x", right: "y", roles: roles(0) }, { sentence: 41, document: 5, start: 1, end: 3, left: "", right: "", roles: roles(2) }];
  const identity = { source: "npv := [].", analysis: "a".repeat(64), fingerprint: "f".repeat(64) };
  const href = inspectHref(s, lines[1], identity, "frequencies#x", { words: ["坂", "を", "上る"], total: 10, works: 6, lines, at: 1 });
  assert.deepEqual(parseEntryLines(new URLSearchParams(href.slice(3)).get("entry")), {
    words: ["坂", "を", "上る"], total: 10, works: 6, at: 1, names: { 1: "Natsume_S_Meian" },
    lines: [{ sentence: 3, document: 1, start: 0, end: 2, roles: roles(0) }, { sentence: 41, document: 5, start: 1, end: 3, roles: roles(2) }],
  });
  assert.equal(new URLSearchParams(inspectHref(s, lines[1], identity, "frequencies#x").slice(3)).get("entry"), null);
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

test("a line's context marks the words its where clause binds, by code point and escaped", () => {
  const right = "ゆっくり𠮟る<b>";
  const bound = [{ name: "v", side: "right", start: 4, end: 6 }, { name: "n", side: "left", start: 0, end: 1 }];
  assert.equal(contextHtml(right, bound, "right"), 'ゆっくり<mark class="kb" title="v">𠮟る</mark>&lt;b&gt;');
  assert.equal(contextHtml("坂", undefined, "left"), "坂");
});

test("Frequencies names values as the question page does, and glosses an entry's named fields", async () => {
  const { valueName, entryGlosses } = await import("../assets/frequency.js");
  const question = await import("../question-ui/dependency.js");
  const { readFileSync } = await import("node:fs");
  const names = JSON.parse(readFileSync(new URL("../../lingrep-core/src/fsm/label_names.json", import.meta.url), "utf8")).sets;
  for (const [attribute, value] of [["upos", "NOUN"], ["upos", "SPACE"], ["dep.rel", "nsubj:pass"], ["dep.rel", "obl:x"], ["dep.rel", "dobj"], ["xpos", "NN"], ["xpos", "名詞-普通名詞-一般"], ["lemma", "NOUN"]])
    for (const locale of ["en", "ja"]) assert.equal(valueName(names, attribute, value, locale), question.valueName(names, attribute, value, locale), `${attribute} ${value} ${locale}`);
  assert.deepEqual(entryGlosses({ kind: "key", fields: [{ kind: "scalar", name: "$v.unidic.goshu", value: "和" }] }, names, "en"), ["native Japanese word"]);
  const tags = { kind: "key", fields: [{ kind: "scalar", name: "xpos", value: "名詞-普通名詞-一般" }, { kind: "scalar", name: "upos", value: "NOUN" }] };
  assert.deepEqual(entryGlosses(tags, names, "ja"), ["名詞"]);
  assert.deepEqual(entryGlosses({ kind: "key", fields: [{ kind: "scalar", name: "$v.dep.rel", value: "obj" }] }, names, "en"), ["object"]);
  assert.deepEqual(entryGlosses({ kind: "surface", value: "NOUN" }, names, "en"), []);
});
