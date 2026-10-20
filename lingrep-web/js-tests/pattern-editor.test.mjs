import test from "node:test";
import assert from "node:assert/strict";

import {
  ariaInvalidFor,
  checkStatusLine,
  createPositionIndex,
  highlightingEnabled,
  mapSelection,
  mapServerDiagnostics,
  shouldPublish,
  patternCaptures,
  patternCompletionContext,
  patternCompletionOptions,
  patternDefinitions,
  patternHoverInfo,
  patternNavigationIndex,
  patternSignatureContext,
  statsSummary,
  navigationTarget,
  symbolAtPosition,
  utf8RangeToUtf16,
  diagnosticExcerpt,
  focusEditor,
  shouldSelectDiagnostic,
} from "../assets/editor/pattern-editor.js";

test("focusEditor falls back to an unenhanced textarea", () => {
  let focused = false;
  focusEditor({
    focus() {
      focused = true;
    },
  });
  assert.equal(focused, true);
});

test("publication follows source identity, including a to b to a", () => {
  assert.equal(shouldPublish("a", "a"), true);
  assert.equal(shouldPublish("a", "b"), false);
  assert.equal(shouldPublish("a", "a"), true);
});

test("UTF-8 diagnostic ranges map to Unicode-safe browser positions", () => {
  const source = "猫犬 := @missing.";
  const start = new TextEncoder().encode("猫犬 := @").length;
  const end = start + new TextEncoder().encode("missing").length;
  assert.deepEqual(utf8RangeToUtf16(source, { start, end }), { start: 7, end: 14 });
  const excerpt = diagnosticExcerpt(source, { byte_range: { start, end } });
  assert.equal(excerpt.text, source);
  assert.equal(excerpt.caretWidth, 7);
  assert.equal(shouldSelectDiagnostic("input"), false);
  assert.equal(shouldSelectDiagnostic("submit"), true);
});

test("highlight threshold counts UTF-8 bytes", () => {
  assert.equal(highlightingEnabled("a".repeat(65_536)), true);
  assert.equal(highlightingEnabled("a".repeat(65_537)), false);
  assert.equal(highlightingEnabled("猫".repeat(21_846)), false);
});

test("selection mapping preserves UTF-16 positions around Japanese text", () => {
  const oldSource = "猫:=[pos=VERB].";
  const newSource = "猫 := [pos=VERB].\n";
  assert.equal(mapSelection(oldSource, newSource, 1), 1);
  assert.equal(
    mapSelection(oldSource, newSource, oldSource.length),
    newSource.length,
  );
});

// Reference for the position index: the exact walk the index replaces,
// computed with real UTF-8 encoding of every code point.
function referenceByteToUtf16(source, byteOffset) {
  let bytes = 0;
  let utf16 = 0;
  for (const character of source) {
    const size = Buffer.byteLength(character, "utf8");
    if (bytes + size > byteOffset) break;
    bytes += size;
    utf16 += character.length;
  }
  return utf16;
}

test("position index matches the reference walk at every byte offset", () => {
  // BMP CJK, a surrogate-pair emoji, ASCII, halfwidth katakana, a combining
  // sequence, and a lone surrogate (encoder replacement path).
  const source = "a猫🐱犬 := @x.\nｶﾀｶﾅ e\u0301 \uD83D end";
  const total = Buffer.byteLength(source, "utf8");
  const index = createPositionIndex(source);
  assert.equal(index.totalBytes, total);
  for (let offset = 0; offset <= total; offset++) {
    const mapped = utf8RangeToUtf16(source, { start: offset, end: total });
    assert.equal(
      mapped.start,
      referenceByteToUtf16(source, offset),
      `byte offset ${offset}`,
    );
    assert.equal(mapped.end, source.length, `end at byte offset ${offset}`);
  }
  assert.equal(utf8RangeToUtf16(source, { start: 0, end: total + 1 }), null);
});

// Completion contexts. `cursor` marks the cursor position with `|`.
function contextAt(source) {
  const pos = source.indexOf("|");
  return patternCompletionContext(source.slice(0, pos) + source.slice(pos + 1), pos);
}

test("attribute names complete at a token-clause start", () => {
  assert.equal(contextAt("verb := [p|").kind, "attribute");
  assert.equal(contextAt("verb := [upos=VERB & u|").kind, "attribute");
  const options = patternCompletionOptions(contextAt("verb := [|"));
  assert.ok(options.some((option) => option.label === "unidic.cform"));
  assert.ok(options.every((option) => option.apply?.endsWith("=")));
});

test("POS values complete after upos=", () => {
  assert.equal(contextAt("verb := [upos=V|").kind, "pos-value");
  assert.equal(contextAt("verb := [lemma=猫 & upos=|").kind, "pos-value");
  const options = patternCompletionOptions(contextAt("verb := [upos=|"));
  assert.ok(options.some((option) => option.label === "VERB"));
  // Other attributes are free-form: no completion noise.
  assert.equal(contextAt("verb := [lemma=食|").kind, "none");
});

test("brackets inside regex literals do not corrupt nesting", () => {
  assert.equal(contextAt("verb := [upos=/[AB]/ & u|").kind, "attribute");
  assert.equal(contextAt("verb := [upos=/VER|").kind, "none");
  assert.equal(contextAt("verb := [text=/a\\/b/ & l|").kind, "attribute");
});

test("definition references complete after @", () => {
  const source = "fragment ARU := [lemma=ある].\npattern p := @A|";
  const context = contextAt(source);
  assert.equal(context.kind, "reference");
  const options = patternCompletionOptions(context, { definitions: patternDefinitions(source.slice(0, source.indexOf("|")) + source.slice(source.indexOf("|") + 1)), captures: [] });
  assert.deepEqual(options.map((option) => option.label), ["ARU", "p"]);
  assert.equal(options[0].info, "fragment (emits nothing)");
});

// `@` reaches a set as well as a fragment and a pattern, so the two places
// that describe a reference must tell all three apart. A binary test called a
// set "pattern (emits matches)", which is exactly backwards.
test("a set is described as itself, not as a pattern", () => {
  const source = "set CONTENT := {動詞, 助動詞}.\npattern p := [unidic.pos1 in @C|";
  const context = contextAt(source);
  const clean = source.replace("|", "");
  const options = patternCompletionOptions(context, { definitions: patternDefinitions(clean), captures: [] });
  const set = options.find((option) => option.label === "CONTENT");
  assert.ok(set, "the set is offered as a completion");
  assert.equal(set.info, "set (named values)");
  assert.equal(hoverAt("set CONTENT := {動詞}.\np := [unidic.pos1 in @CONT|ENT].").detail, "set (named values; emits no matches)");
});

test("named captures complete after $ and inside same_as", () => {
  const source = "p := (?<unit> [upos=ADV]) same_as($|";
  const clean = source.replace("|", "");
  const context = contextAt(source);
  assert.equal(context.kind, "capture");
  assert.deepEqual(
    patternCompletionOptions(context, { definitions: [], captures: patternCaptures(clean) }).map((option) => option.label),
    ["unit"],
  );
  const argContext = contextAt("p := (?<unit> [upos=ADV]) same_as($unit, |");
  assert.equal(argContext.kind, "same-as-arg");
  assert.equal(contextAt("p := (?<u> [upos=ADV]) same_as($u, by=f|").kind, "by-value");
});

test("ordered key projections have dedicated completion contexts", () => {
  const root = contextAt("key: [up|");
  assert.equal(root.kind, "key-match-attribute");
  assert.ok(patternCompletionOptions(root).some((option) => option.label === "upos"));

  const segment = contextAt("key: [$head.no|");
  assert.equal(segment.kind, "key-capture-segment");
  const segmentOptions = patternCompletionOptions(segment);
  assert.ok(segmentOptions.some((option) => option.label === "norm"));
  assert.ok(segmentOptions.some((option) => option.label === "span" && option.apply === "span."));

  const attribute = contextAt("key: [$head.span.no|");
  assert.equal(attribute.kind, "key-capture-attribute");
  assert.ok(patternCompletionOptions(attribute).some((option) => option.label === "norm"));
  assert.equal(contextAt("key: [$head.span.norm.ex|").kind, "none");
});

test("ordered key hover documents captures, components, and attributes", () => {
  const source = "key: [$he|ad.span.norm]\npattern p := (?<head> []).";
  assert.match(hoverAt(source).detail, /defined by/);
  assert.match(hoverAt("key: [$head.sp|an.norm]\npattern p := (?<head> []).").detail, /complete capture span/);
  assert.match(hoverAt("key: [$head.span.no|rm]\npattern p := (?<head> []).").detail, /Key projection/);
});

test("top-level and metadata contexts are distinguished", () => {
  assert.equal(contextAt("|").kind, "top-level");
  assert.equal(contextAt("verb := [pos=VERB].\np|").kind, "top-level");
  assert.equal(contextAt("# a comment with [brackets\np|").kind, "top-level");
  const top = patternCompletionOptions(contextAt("|"));
  assert.ok(top.some((option) => option.label === "pattern"));
  assert.ok(top.some((option) => option.label === "preceded_by"));
  // Metadata keys live inside `defaults` and `schema`, and the block ends at
  // `end`. The retired `metadata … begin … end` block is gone, so a document
  // spelling it gets no key completion — it does not parse either.
  assert.equal(contextAt("defaults\n  stat|").kind, "metadata-key");
  assert.equal(contextAt("schema\n  pattern lay|").kind, "metadata-key");
  assert.equal(contextAt("defaults\n  status: \"x\"\nend\nfrag|").kind, "top-level");
  assert.equal(contextAt("metadata\n  cat|").kind, "top-level");
});

// The editor's own keyword list is a syntax claim, and an author reaches for
// completion precisely when they are unsure. Offering a construct the compiler
// refuses — or omitting one it requires — is how the editor teaches a syntax
// the compiler does not have.
test("top-level completion offers every current declaration and no retired one", () => {
  const labels = patternCompletionOptions(contextAt("|")).map((option) => option.label);
  for (const declaration of ["pattern", "fragment", "set", "defaults", "schema", "policy"]) {
    assert.ok(labels.includes(declaration), `${declaration} is offered`);
  }
  for (const retired of ["metadata", "begin"]) {
    assert.ok(!labels.includes(retired), `${retired} is not offered`);
  }
  // Nothing may insert text the current grammar rejects.
  for (const option of patternCompletionOptions(contextAt("|"))) {
    assert.ok(
      !/\bbegin\b/.test(option.template ?? ""),
      `${option.label} inserts the retired block`,
    );
  }
});

test("definition extraction handles Japanese names and dedupes", () => {
  const source = [
    "fragment 動詞連用形 := [pos=VERB].",
    "pattern result := @動詞連用形.",
    "verb := [pos=VERB].",
    "verb := [pos=NOUN].",
  ].join("\n");
  assert.deepEqual(
    patternDefinitions(source).map((definition) => `${definition.kind}:${definition.name}`),
    ["fragment:動詞連用形", "pattern:result", "pattern:verb"],
  );
});

// Hover documentation. `|` marks the hover position.
function hoverAt(source) {
  const pos = source.indexOf("|");
  return patternHoverInfo(source.slice(0, pos) + source.slice(pos + 1), pos);
}

test("hover documents attribute names, with examples where known", () => {
  const posHover = hoverAt("verb := [up|os=VERB].");
  assert.equal(posHover.title, "upos");
  assert.match(posHover.detail, /coarse POS tag/i);
  assert.equal(posHover.from, 9);
  assert.equal(posHover.to, 13);
  const formHover = hoverAt("verb := [te|xt=走った].");
  assert.match(formHover.detail, /\[text=走った\]/);
  assert.equal(hoverAt("verb := [lemm|a=食べる].").title, "lemma");
});

test("hover documents upos= and by= values", () => {
  const verbHover = hoverAt("verb := [upos=VE|RB].");
  assert.equal(verbHover.title, "upos=VERB");
  assert.match(verbHover.detail, /動詞/);
  const formHover = hoverAt("p := (?<u> [upos=ADV]) same_as($u, by=te|xt).");
  assert.equal(formHover.title, "by=text");
  assert.match(formHover.detail, /surface forms/);
  const byHover = hoverAt("p := (?<u> [upos=ADV]) same_as($u, b|y=text).");
  assert.equal(byHover.title, "by=");
  // Free-form values and unknown tags stay quiet.
  assert.equal(hoverAt("verb := [lemma=食|べる]."), null);
  assert.equal(hoverAt("verb := [upos=NO|TAG]."), null);
});

test("hover resolves @references and $captures against the document", () => {
  const source = "fragment 動詞 := [upos=VERB].\npattern p := @動|詞 same_as($u, by=text).";
  const reference = hoverAt(source);
  assert.equal(reference.title, "@動詞");
  assert.match(reference.detail, /^fragment/);
  const missing = hoverAt("pattern p := @mis|sing.");
  assert.match(missing.detail, /no such definition/);
  const capture = hoverAt("p := (?<unit> [upos=ADV]) same_as($un|it, by=text).");
  assert.equal(capture.title, "$unit");
  assert.match(capture.detail, /defined by/);
  const unknownCapture = hoverAt("p := same_as($no|pe, by=text).");
  assert.match(unknownCapture.detail, /no capture/);
});

test("hover documents keywords and calls, but not definition names or comments", () => {
  const sameAs = hoverAt("p := (?<u> [upos=ADV]) same_|as($u, by=text).");
  assert.equal(sameAs.title, "same_as");
  assert.match(sameAs.detail, /by=text/);
  assert.equal(hoverAt("p := fol|lowed_by([upos=NOUN]).").title, "followed_by");
  assert.equal(hoverAt("verb| := [upos=VERB]."), null);
  assert.equal(hoverAt("# [up|os=VERB]\np := [upos=NOUN]."), null);
  assert.equal(hoverAt("p := [upos=VERB] | ."), null);
});

// Signature help. `|` marks the cursor position.
function signatureAt(source) {
  const pos = source.indexOf("|");
  return patternSignatureContext(source.slice(0, pos) + source.slice(pos + 1), pos);
}

test("signature help tracks the active same_as argument", () => {
  const first = signatureAt("p := (?<unit> [upos=ADV]) same_as(|");
  assert.equal(first.name, "same_as");
  assert.equal(first.argIndex, 0);
  assert.equal(first.signature.params[0].label, "$capture");
  const second = signatureAt("p := (?<unit> [upos=ADV]) same_as($unit, |");
  assert.equal(second.argIndex, 1);
  assert.equal(second.signature.params[1].label, "by=text");
  // Extra commas clamp to the last parameter instead of overflowing.
  assert.equal(signatureAt("p := same_as($a, by=text, stray|").argIndex, 1);
});

test("signature help ignores completed, nested, and non-signature calls", () => {
  assert.equal(signatureAt("p := same_as($u, by=orth)|."), null);
  assert.equal(signatureAt("p := followed_by([pos=NOUN]|"), null);
  assert.equal(signatureAt("p := [pos=VERB]|"), null);
  // A nested same_as still wins while its own argument list is open.
  const nested = signatureAt("p := followed_by(same_as($u, |");
  assert.equal(nested.name, "same_as");
  assert.equal(nested.argIndex, 1);
});

test("signature help skips comments and regex literals", () => {
  assert.equal(signatureAt("# same_as($x, \np := a|"), null);
  assert.equal(signatureAt("x := [orth=/a(b,/]|"), null);
  // A comment after the open paren does not close the call.
  const afterComment = signatureAt("p := same_as($u, # note\n |");
  assert.equal(afterComment.argIndex, 1);
});

test("stats summary reads as prose, and is absent without stats", () => {
  assert.equal(
    statsSummary({ definitions: 97, references: 84, depth: 5, expressions: 410, nfa_states: 547, transitions: 581 }),
    "97 definitions, 84 references, depth 5, 547 NFA states",
  );
  assert.equal(
    statsSummary({ definitions: 1, references: 0, depth: 1, expressions: 2, nfa_states: 3, transitions: 4 }),
    "1 definition, 0 references, depth 1, 3 NFA states",
  );
  // A stubbed or older server sends `valid` with no stats; that must not throw.
  assert.equal(statsSummary(undefined), null);
  assert.equal(statsSummary(null), null);
});

// The bug being fixed is that diagnostics on the VALID path were discarded.
// These assertions fail against any implementation that keeps discarding them.
const validWithWarning = () => ({
  status: "valid",
  stats: { definitions: 2, expressions: 2, references: 0, depth: 1, nfa_states: 3, transitions: 4 },
  diagnostics: [
    {
      code: "UnusedFragment",
      category: "lint",
      severity: "warning",
      message: "A fragment is never referenced.",
      location: {
        source_name: "lingrep-web.lx",
        line: 1,
        column: 1,
        byte_range: { start: 0, end: 30 },
      },
    },
  ],
});

test("a warning on a valid pattern becomes a warning lint diagnostic", () => {
  const source = "fragment orphan := [pos=NOUN].\np := [pos=VERB].";
  const mapped = mapServerDiagnostics(source, validWithWarning().diagnostics);

  assert.equal(mapped.length, 1);
  assert.equal(mapped[0].severity, "warning");
  assert.equal(mapped[0].from, 0);
  assert.equal(mapped[0].to, 30);
  assert.equal(mapped[0].message, "A fragment is never referenced.");
  assert.equal(mapped[0].source, "pattern:UnusedFragment");
});

test("an error keeps error severity, and an unknown severity is treated as one", () => {
  const source = "x := @missing.";
  const [error] = mapServerDiagnostics(source, [
    { code: "UndefinedReference", severity: "error", message: "Pattern references are invalid.",
      location: { byte_range: { start: 5, end: 13 } } },
  ]);
  assert.equal(error.severity, "error");

  const [unknown] = mapServerDiagnostics(source, [
    { code: "Mystery", severity: "catastrophe", message: "?",
      location: { byte_range: { start: 0, end: 1 } } },
  ]);
  assert.equal(unknown.severity, "error", "an unrecognised severity must stay loud");
});

test("the status line reports warnings alongside the stats", () => {
  assert.equal(
    checkStatusLine(validWithWarning()),
    "Pattern is valid, 1 warning. 2 definitions, 0 references, depth 1, 3 NFA states.",
  );
  const clean = { ...validWithWarning(), diagnostics: [] };
  assert.equal(
    checkStatusLine(clean),
    "Pattern is valid. 2 definitions, 0 references, depth 1, 3 NFA states.",
  );
  // The browser fixture stubs a bare `{"status":"valid"}`; it must degrade.
  assert.equal(checkStatusLine({ status: "valid" }), "Pattern is valid.");
  assert.equal(checkStatusLine({ status: "invalid" }), null);
});

test("a warning does not make the pattern invalid", () => {
  assert.equal(ariaInvalidFor(validWithWarning()), "false");
  assert.equal(ariaInvalidFor({ status: "valid" }), "false");
  assert.equal(ariaInvalidFor({ status: "invalid" }), "true");
});

// Capture fixtures mirror what `query.captures()` yields: outer captures
// before inner ones, in node order.
const capture = (name, text, startIndex) => ({
  name,
  node: { startIndex, endIndex: startIndex + text.length, text },
});
const referenceAt = (text, atIndex) => [
  capture("reference.site", `@${text}`, atIndex),
  capture("reference", text, atIndex + 1),
];

test("the navigation index pairs one definition with every reference site", () => {
  // fragment F := [pos=NOUN].
  // p := @F @F.
  const index = patternNavigationIndex([
    capture("kind", "fragment", 0),
    capture("definition", "F", 9),
    capture("definition", "p", 26),
    ...referenceAt("F", 31),
    ...referenceAt("F", 34),
  ]);

  assert.deepEqual(index.definitions.get("F"), { from: 9, to: 10, kind: "fragment" });
  assert.deepEqual(index.definitions.get("p"), { from: 26, to: 27, kind: "pattern" });
  assert.deepEqual(index.references.get("F"), [
    { from: 32, to: 33, hitFrom: 31 },
    { from: 35, to: 36, hitFrom: 34 },
  ]);
  assert.equal(index.references.has("p"), false);
});

test("an undeclared reference is indexed without inventing a definition", () => {
  const index = patternNavigationIndex(referenceAt("missing", 4));

  assert.equal(index.definitions.has("missing"), false);
  assert.deepEqual(index.references.get("missing"), [{ from: 5, to: 12, hitFrom: 4 }]);
});

test("the symbol under the cursor is found at either end, and on the @", () => {
  const index = patternNavigationIndex([
    capture("definition", "F", 9),
    ...referenceAt("F", 31),
  ]);

  assert.deepEqual(symbolAtPosition(index, 9), { name: "F", role: "definition" });
  assert.deepEqual(symbolAtPosition(index, 10), { name: "F", role: "definition" });
  assert.deepEqual(symbolAtPosition(index, 31), { name: "F", role: "reference" }, "on the @");
  assert.deepEqual(symbolAtPosition(index, 32), { name: "F", role: "reference" });
  assert.equal(symbolAtPosition(index, 20), null);
});

// A duplicate definition is a compile error, but the editor is in that state
// every time a declaration is being copied or renamed. Both declarations must
// stay addressable.
test("every declaration of a duplicated name is recognised by the cursor", () => {
  const index = patternNavigationIndex([
    capture("kind", "fragment", 0),
    capture("definition", "F", 9),
    capture("definition", "F", 26),
    ...referenceAt("F", 40),
  ]);

  // The first declaration is the canonical jump target.
  assert.deepEqual(index.definitions.get("F"), { from: 9, to: 10, kind: "fragment" });
  // Both are addressable from the cursor.
  assert.deepEqual(symbolAtPosition(index, 9), { name: "F", role: "definition" });
  assert.deepEqual(symbolAtPosition(index, 26), { name: "F", role: "definition" });
  assert.deepEqual(
    index.definitionSites.map((site) => [site.name, site.from, site.kind]),
    [
      ["F", 9, "fragment"],
      ["F", 26, "pattern"],
    ],
    "the second declaration has no `fragment` keyword of its own",
  );
});

const navIndex = () =>
  patternNavigationIndex([
    capture("kind", "fragment", 0),
    capture("definition", "F", 9),
    ...referenceAt("F", 31),
    ...referenceAt("F", 39),
    ...referenceAt("ghost", 49),
  ]);

test("go-to-definition jumps from a reference and reports the target", () => {
  assert.deepEqual(navigationTarget(navIndex(), 32, "definition"), {
    from: 9,
    to: 10,
    message: "fragment F, 2 references.",
  });
});

test("go-to-definition on the definition itself reports without moving", () => {
  assert.deepEqual(navigationTarget(navIndex(), 9, "definition"), {
    from: 9,
    to: 10,
    message: "fragment F, 2 references.",
  });
});

test("go-to-definition works with the caret on the @", () => {
  assert.deepEqual(navigationTarget(navIndex(), 31, "definition"), {
    from: 9,
    to: 10,
    message: "fragment F, 2 references.",
  });
});

test("reference cycling wraps and starts from the definition", () => {
  const index = navIndex();
  assert.deepEqual(navigationTarget(index, 9, "next"), { from: 32, to: 33, message: "Reference 1 of 2." });
  assert.deepEqual(navigationTarget(index, 32, "next"), { from: 40, to: 41, message: "Reference 2 of 2." });
  assert.deepEqual(navigationTarget(index, 40, "next"), { from: 32, to: 33, message: "Reference 1 of 2." });
  assert.deepEqual(navigationTarget(index, 32, "previous"), { from: 40, to: 41, message: "Reference 2 of 2." });
});

test("navigation says so rather than moving when there is nowhere to go", () => {
  const index = navIndex();
  // Cursor on nothing.
  assert.deepEqual(navigationTarget(index, 20, "definition"), { message: "No definition or reference here." });
  // A reference whose definition is absent — the compiler would reject this,
  // but the editor sees it constantly while typing.
  assert.deepEqual(navigationTarget(index, 50, "definition"), { message: "`ghost` is not defined in this document." });
  // A definition nothing references.
  const lonely = patternNavigationIndex([capture("definition", "only", 0)]);
  assert.deepEqual(navigationTarget(lonely, 0, "next"), { message: "`only` has no references." });
});

test("a duplicated definition is reported rather than silently picking one", () => {
  const index = patternNavigationIndex([
    capture("definition", "F", 9),
    capture("definition", "F", 26),
    ...referenceAt("F", 40),
  ]);

  assert.deepEqual(navigationTarget(index, 41, "definition"), {
    from: 9,
    to: 10,
    message: "pattern F, 1 reference — 2 declarations, showing the first.",
  });
});
