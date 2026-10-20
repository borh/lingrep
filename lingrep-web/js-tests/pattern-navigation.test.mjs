import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { PATTERN_NAVIGATION_QUERY, patternNavigationIndex } from "../assets/editor/pattern-editor.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const vendor = path.join(here, "../assets/editor/vendor");
const repo = path.join(here, "../..");

async function navigateSource(source) {
  const { Parser, Language, Query } = await import(path.join(vendor, "web-tree-sitter.js"));
  await Parser.init({ locateFile: () => path.join(vendor, "web-tree-sitter.wasm") });
  const language = await Language.load(path.join(vendor, "tree-sitter-lingrep.wasm"));
  const parser = new Parser();
  parser.setLanguage(language);
  const tree = parser.parse(source);
  const query = new Query(language, PATTERN_NAVIGATION_QUERY);
  return { index: patternNavigationIndex(query.captures(tree.rootNode)), tree, source };
}

async function navigate(file) {
  return navigateSource(await fs.readFile(path.join(repo, file), "utf8"));
}

const referenceSites = (index) =>
  [...index.references.values()].reduce((total, sites) => total + sites.length, 0);

// CHARACTERIZATION, not continuous cross-implementation parity. The numbers
// below were taken from the catalogs at the grammar revision pinned in
// vendor/manifest.json. They prove the query read what was there when
// measured, and they break if the query drifts. They do NOT track the
// compiler. Update them whenever the catalogs or the grammar move.
//
// The reference count deliberately EXCEEDS the compiler's `reference_edges`
// by the number of `@name` operands in `policy … end` blocks. The two answer
// different questions: `reference_edges` is a preflight budget over the
// definition graph, and a policy operand names a definition without adding an
// edge to expand — while this index exists so that clicking that operand
// navigates to what it names, which it must. The R1 migration took
// `sf-experimental` from two such operands to five — @ている, @NEG, @される,
// @DA and @という — so the gap is now 5 rather than 2.
// Task 10 named sf-experimental's five repeated lexicons as sets (IU, IERU,
// WAKE, MONO and TE_DE), which is why this catalog carries 5 more
// definitions and 14 more reference sites than the inline-literal form did.
test("the catalog index matches the compiler counts measured at this revision", async () => {
  const { index, tree } = await navigate("patterns/sf-experimental.lx");

  assert.equal(tree.rootNode.hasError, false);
  assert.equal(index.definitions.size, 109);
  assert.equal(referenceSites(index), 106);

  const fragments = [...index.definitions.values()].filter((d) => d.kind === "fragment");
  assert.equal(fragments.length, 18);
  assert.equal(index.references.get("KOTO").length, 19);

  // No name is declared twice in a catalog that compiles.
  assert.equal(index.definitionSites.length, index.definitions.size);
});

// Task 10 named patterns/sfe.lx's one repeated lexicon as a set (VERBAL), which is
// why this catalog carries 1 more definition and 4 more reference sites than
// the inline-literal form did.
test("a second catalog matches too", async () => {
  const { index } = await navigate("patterns/sfe.lx");

  assert.equal(index.definitions.size, 28);
  assert.equal(referenceSites(index), 21);
});

// Task 9 named double-wo's two repeated lexicons as sets
// (DOUBLE_WO_NON_CONTENT and DOUBLE_WO_ADVERBIAL_ANCHORS), which is why this
// catalog carries 2 more definitions than the inline-literal form did.
//
// The reference count then fell from 81 to 62 when `double_wo_fragment_
// boundary_body` collapsed its eight-branch expansion onto one `&`. The 19
// it lost are countable: the branches held 22 reference sites (2 in the
// first, which has no leading gap; 3 in each of the middle six; 2 in the
// last, which has no trailing gap), and the intersection holds 3. The
// definition count is unchanged, because the rewrite added and removed no
// name. `repository_catalog_fingerprints` independently records the same
// -19 as `reference_edges: 71 -> 52`, which is what makes this number a
// cross-check rather than a transcription.
//
// S3 then moved both numbers again, and this time they move *apart*.
// Definitions 27 -> 28: one `test double_wo_boundary`. Reference sites
// 62 -> 64: two `@double_wo_boundary`, one negated in
// `double_wo_non_boundary_gap` and one plain in
// `double_wo_fragment_boundary`. Nothing was removed — the exclusion it
// replaced was the attribute `orthBase!=\u3000`, which is not a reference.
//
// `reference_edges` stays 52, so unlike the intersection collapse the delta
// here is +2 against +0 rather than matching. That is the same exemption the
// policy operands take above: a `test_reference` is resolved into the token
// predicate and never becomes a definition-graph edge to expand, so the
// compiler's preflight budget does not count it while this index must,
// because clicking it has to navigate. The gap between the two numbers is
// therefore exactly the non-edge references, and it stays checkable:
// 62 - 52 = 10 set references (@DOUBLE_WO_NON_CONTENT nine times,
// @DOUBLE_WO_ADVERBIAL_ANCHORS once), and 64 - 52 = 12 once the two test
// references join them.
test("a third catalog matches too", async () => {
  const { index } = await navigate("patterns/double-wo.lx");

  assert.equal(index.definitions.size, 28);
  assert.equal(referenceSites(index), 64);

  // The declaration is navigable and carries its keyword as the kind, and
  // both references resolve — including the negated one, since `!` wraps the
  // reference rather than replacing it.
  assert.equal(index.definitions.get("double_wo_boundary").kind, "test");
  assert.equal(index.references.get("double_wo_boundary").length, 2);
});

// The editor must navigate a document the compiler rejects — that is the state
// a catalog is in for most of an editing session.
//
// The source is inline, not a catalog file. This test previously read
// patterns/sfe.lx because that catalog happened not to compile, which made a
// checked-in file's brokenness load-bearing: fixing the catalog would have
// falsified the test's premise while leaving it green, and the test stood in
// the way of the fix. What is under test is the query's behavior on a rejected
// document, so the rejected document belongs here.
//
// `[orth~/(/]` is an unterminated regex group: the grammar parses the
// definition, the compiler rejects it with InvalidRegex. That is the
// interesting case — a tree good enough to navigate, a program that will not
// build.
const DOES_NOT_COMPILE = `
FIRST := [orth~/(/].
SECOND := @FIRST @FIRST.
THIRD := @SECOND | @FIRST.
`;

test("navigation survives a document that does not compile", async () => {
  const { index } = await navigateSource(DOES_NOT_COMPILE);

  assert.equal(index.definitions.size, 3);
  assert.equal(referenceSites(index), 4);
  assert.equal(index.references.get("FIRST").length, 3);
});

// The `@` is part of the hit area, which the pure tests assert on synthetic
// captures. This proves the query actually produces the outer capture, in the
// order the index assumes.
test("a real reference carries a hit span that starts at the @", async () => {
  const { index, source } = await navigate("patterns/sf-experimental.lx");
  const [first] = index.references.get("KOTO");

  assert.equal(source.slice(first.hitFrom, first.to), "@KOTO");
  assert.equal(source.slice(first.from, first.to), "KOTO");
});

// A set is a declaration the editor navigates to, and its references resolve
// to it — the same affordance a fragment gets, because they share a sigil and
// a reader should not have to know which kind they clicked before clicking.
test("a set declaration is indexed and its references resolve", async () => {
  const { index } = await navigateSource(
    "set CONTENT := {動詞, 助動詞}.\npattern p := [pos1 in @CONTENT].\n",
  );
  const declaration = index.definitions.get("CONTENT");
  assert.ok(declaration, "the set is indexed as a declaration");
  assert.equal(declaration.kind, "set", "and is told apart from a fragment");
  // `references` is a Map from name to sites, not an array.
  assert.equal(index.references.get("CONTENT").length, 1);
  assert.equal(referenceSites(index), 1);
});

test("a lexicon declaration is indexed without reserving the name lexicon", async () => {
  const { index } = await navigateSource(
    'lexicon words by [orth, norm] := {"大学院"}.\npattern lexicon := lexicon.\n',
  );
  assert.equal(index.definitions.get("words").kind, "lexicon");
  assert.equal(index.definitions.get("lexicon").kind, "pattern");
});
