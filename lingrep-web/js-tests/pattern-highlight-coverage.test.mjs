import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { highlightRangesFromCaptures } from "../assets/editor/pattern-editor.js";

test("every highlight class emitted by pattern-editor.js has a rule in pattern-editor.css", () => {
  const css = readFileSync("lingrep-web/assets/editor/pattern-editor.css", "utf8");
  for (const cls of [
    "pattern-keyword",
    "pattern-operator",
    "pattern-comment",
    "pattern-string",
    "pattern-regex",
    "pattern-property",
    "pattern-definition",
    "pattern-variable",
    "pattern-punctuation",
    "pattern-diagnostic-excerpt",
    "pattern-diagnostic-caret",
  ]) {
    assert.ok(
      css.includes(`.${cls}`),
      `pattern-editor.css is missing a selector for .${cls}`,
    );
  }
});
// code-unit offsets (CodeMirror positions), so highlightRangesFromCaptures must
// use them verbatim. A capture far into a multi-byte document is exactly where
// the old byte-offset misread collapsed positions and dropped the tail.
test("highlight spans use tree-sitter (UTF-16) indices verbatim", () => {
  const ranges = highlightRangesFromCaptures([
    { name: "entity.name.function", start: 0, end: 2 }, // 猫犬
    { name: "keyword", start: 10, end: 15 },
    { name: "not-a-highlight-name", start: 16, end: 20 }, // filtered out
    { name: "entity.name.function", start: 24_000, end: 24_002 },
  ]);
  assert.deepEqual(ranges, [
    { cls: "pattern-definition", start: 0, end: 2 },
    { cls: "pattern-keyword", start: 10, end: 15 },
    { cls: "pattern-definition", start: 24_000, end: 24_002 },
  ]);
});

test("overlapping captures collapse to a single longest-wins cover", () => {
  const ranges = highlightRangesFromCaptures([
    { name: "comment", start: 5, end: 40 }, // outer, wins its span
    { name: "keyword", start: 10, end: 15 }, // inside the comment, dropped
    { name: "operator", start: 40, end: 45 }, // abuts, kept
    { name: "property", start: 42, end: 43 }, // inside operator, dropped
  ]);
  assert.deepEqual(ranges, [
    { cls: "pattern-comment", start: 5, end: 40 },
    { cls: "pattern-operator", start: 40, end: 45 },
  ]);
});

// End-to-end guard: drive the exact vendored grammar + highlights.scm the
// browser loads against the seeded default grammar file, and assert highlight
// coverage reaches the end of the document. This fails if either the offset
// mapping regresses OR a future grammar bump silently truncates a large file
// (the failure mode originally — and wrongly — blamed on the grammar).
test("the seeded grammar file highlights all the way to EOF", async () => {
  const vendor = `${process.cwd()}/lingrep-web/assets/editor/vendor`;
  const { Parser, Language, Query } = await import(`${vendor}/web-tree-sitter.js`);
  await Parser.init({ locateFile: (name) => `${vendor}/${name}` });
  const parser = new Parser();
  const language = await Language.load(`${vendor}/tree-sitter-lingrep.wasm`);
  parser.setLanguage(language);
  const query = new Query(language, readFileSync(`${vendor}/highlights.scm`, "utf8"));

  const source = readFileSync("patterns/sf-experimental.lx", "utf8");
  const tree = parser.parse(source);
  const spans = highlightRangesFromCaptures(
    query.captures(tree.rootNode).map(({ name, node }) => ({
      name,
      start: node.startIndex,
      end: node.endIndex,
    })),
  );

  const lastEnd = spans.at(-1).end;
  const trailing = source.slice(lastEnd).trim();
  assert.equal(
    trailing,
    "",
    `highlighting stops at position ${lastEnd} of ${source.length}; ` +
      `${trailing.length} chars of the document tail are left unstyled`,
  );

  // highlights.scm ends with a catch-all `(identifier) @variable`, and a
  // definition name is also an `identifier`. If the specific
  // `@entity.name.function` rule stops winning, every declaration name fades
  // to an ordinary variable and the document still looks highlighted — so
  // assert a real definition lights up as a definition token.
  //
  // This guarded a different hazard before the metadata cutover: definitions
  // sat inside a `metadata … begin … end` block that highlights.scm tagged
  // whole as a documentation comment, and "longest wins" painted the
  // definition as one faded comment. That block no longer parses; the catch-all
  // is what the test protects against now.
  const identifier = "させる";
  const at = source.indexOf(`${identifier} := `);
  assert.ok(at > 0, "fixture must contain the させる definition");
  const nameSpan = spans.find((span) => span.start === at && span.end === at + identifier.length);
  assert.ok(
    nameSpan && nameSpan.cls === "pattern-definition",
    "a definition inside a metadata block must render as pattern-definition, " +
      "not be swallowed by the block-level documentation comment",
  );
});

// A token test's own syntax must light up like the rest of the language.
//
// `sf-experimental.lx`, which the coverage test above drives, declares no
// `test` and writes no `!` — so every rule S3 needed could be missing and that
// test would still pass. `double-wo.lx` is the tracked catalog that uses
// both. The reference is asserted whole, sigil included, because a rule that
// captures only the identifier leaves the `@` unstyled beside it.
test("token-test syntax is highlighted in the catalog that uses it", async () => {
  const vendor = `${process.cwd()}/lingrep-web/assets/editor/vendor`;
  const { Parser, Language, Query } = await import(`${vendor}/web-tree-sitter.js`);
  await Parser.init({ locateFile: (name) => `${vendor}/${name}` });
  const parser = new Parser();
  const language = await Language.load(`${vendor}/tree-sitter-lingrep.wasm`);
  parser.setLanguage(language);
  const query = new Query(language, readFileSync(`${vendor}/highlights.scm`, "utf8"));

  const source = readFileSync("patterns/double-wo.lx", "utf8");
  const tree = parser.parse(source);
  const spans = highlightRangesFromCaptures(
    query.captures(tree.rootNode).map(({ name, node }) => ({
      name,
      start: node.startIndex,
      end: node.endIndex,
    })),
  );
  // Union coverage, not a single containing span: a reference is captured as
  // two abutting spans, the `@` and the identifier, and demanding one span
  // cover both would fail on a query that is correct.
  const covers = (at, length) =>
    [...Array(length).keys()].every((offset) =>
      spans.some((span) => span.start <= at + offset && span.end > at + offset),
    );

  const negated = source.indexOf("!@double_wo_boundary\n");
  assert.ok(negated > 0, "fixture must negate a test reference");
  assert.ok(covers(negated, 1), "the complement operator `!` is unhighlighted");
  assert.ok(
    covers(negated + 1, "@double_wo_boundary".length),
    "a negated test reference is unhighlighted",
  );

  const bare = source.indexOf("[@double_wo_boundary]");
  assert.ok(bare > 0, "fixture must reference a test bare");
  assert.ok(
    covers(bare + 1, "@double_wo_boundary".length),
    "a bare test reference is unhighlighted",
  );
});

// The fourth construct S3 added, and the only one no tracked catalog writes —
// so it takes an inline document rather than going unasserted.
test("grouping parentheses inside a token test are highlighted", async () => {
  const vendor = `${process.cwd()}/lingrep-web/assets/editor/vendor`;
  const { Parser, Language, Query } = await import(`${vendor}/web-tree-sitter.js`);
  await Parser.init({ locateFile: (name) => `${vendor}/${name}` });
  const parser = new Parser();
  const language = await Language.load(`${vendor}/tree-sitter-lingrep.wasm`);
  parser.setLanguage(language);
  const query = new Query(language, readFileSync(`${vendor}/highlights.scm`, "utf8"));

  const source = "pattern grouped := [(upos=NOUN | upos=VERB) & form=猫].\n";
  const tree = parser.parse(source);
  const spans = highlightRangesFromCaptures(
    query.captures(tree.rootNode).map(({ name, node }) => ({
      name,
      start: node.startIndex,
      end: node.endIndex,
    })),
  );

  for (const bracket of ["(", ")"]) {
    const at = source.indexOf(bracket);
    assert.ok(
      spans.some((span) => span.start <= at && span.end > at),
      `the \`${bracket}\` grouping a token test is unhighlighted`,
    );
  }
});
