const MAX_HIGHLIGHT_BYTES = 65_536;
const encoder = new TextEncoder();
const states = new WeakMap();
const releasing = new WeakSet();
const assetUrl = (path) => new URL(path, import.meta.url).href;

// Pattern drafts survive reloads so an editing session is never lost. The
// draft is only restored into an empty textarea, so server-rendered content
// always wins, and drafts are kept after a successful search on purpose:
// pattern work is iterative.
function draftKey(textarea) {
  return `pattern-draft:${textarea.id || "pattern"}`;
}

function saveDraft(textarea) {
  try {
    localStorage.setItem(draftKey(textarea), textarea.value);
  } catch (_error) {
    // Storage unavailable or full; persistence is best-effort.
  }
}

function loadDraft(textarea) {
  try {
    return localStorage.getItem(draftKey(textarea));
  } catch (_error) {
    // Storage unavailable or blocked; persistence is best-effort.
    return null;
  }
}

// Fallback persistence for when the CodeMirror bundle or grammar fails to
// load and the plain textarea stays interactive. The enhanced editor saves
// drafts from its own update listener instead.
function attachDraftPersistence(textarea) {
  if (textarea.dataset.lxDraft === "attached") return;
  textarea.dataset.lxDraft = "attached";
  if (!textarea.value.trim()) {
    const draft = loadDraft(textarea);
    if (draft) textarea.value = draft;
  }
  textarea.addEventListener("input", () => saveDraft(textarea));
}

export function shouldPublish(requestSource, currentSource) {
  return requestSource === currentSource;
}

export function focusEditor(textarea) {
  const state = states.get(textarea);
  if (state?.view) {
    state.view.focus();
  } else {
    textarea.focus();
  }
}

export function jumpToDefinitionByName(textarea, name) {
  const state = states.get(textarea);
  if (!state?.view) {
    textarea?.focus();
    return false;
  }
  const index = navigationIndexFor(state);
  const definition = index?.definitions.get(name);
  if (!definition) {
    status(state, `\`${name}\` is not defined in this document.`, true);
    state.view.focus();
    return false;
  }
  const sites = index.references.get(name) ?? [];
  const count = sites.length === 1 ? "1 reference" : `${sites.length} references`;
  status(state, `${definition.kind} ${name}, ${count}.`, true);
  state.view.dispatch({
    selection: { anchor: definition.from, head: definition.to },
    scrollIntoView: true,
  });
  state.view.focus();
  return true;
}

export function highlightingEnabled(source) {
  return encoder.encode(source).byteLength <= MAX_HIGHLIGHT_BYTES;
}

export function mapSelection(oldSource, newSource, offset) {
  let prefix = 0;
  const prefixLimit = Math.min(oldSource.length, newSource.length);
  while (prefix < prefixLimit && oldSource[prefix] === newSource[prefix]) prefix++;
  let suffix = 0;
  while (
    suffix < oldSource.length - prefix &&
    suffix < newSource.length - prefix &&
    oldSource[oldSource.length - suffix - 1] === newSource[newSource.length - suffix - 1]
  ) suffix++;
  if (offset <= prefix) return offset;
  if (offset >= oldSource.length - suffix) {
    return newSource.length - (oldSource.length - offset);
  }
  return prefix;
}

const captureClasses = new Map([
  ["comment", "pattern-comment"],
  ["comment.block.documentation", "pattern-comment"],
  ["comment.documentation", "pattern-comment"],
  ["keyword", "pattern-keyword"],
  ["keyword.operator.definition", "pattern-operator"],
  ["operator", "pattern-operator"],
  ["property", "pattern-property"],
  ["string", "pattern-string"],
  ["string.regex", "pattern-regex"],
  ["entity.name.function", "pattern-definition"],
  ["variable", "pattern-variable"],
  ["variable.member", "pattern-variable"],
  ["variable.parameter", "pattern-variable"],
  ["punctuation.bracket", "pattern-punctuation"],
  ["punctuation.delimiter", "pattern-punctuation"],
  ["punctuation.special", "pattern-punctuation"],
]);

// Definition and reference sites, read from the CST rather than matched out of
// the text. This is not a style preference: a text scan of the checked-in
// catalog finds 182 `@name` occurrences against the compiler's 84, because
// `dm-annotations@c3db462c…` appears in the header comment and in the
// `source:` annotation of nearly every definition. The CST places those
// inside `comment` and `metadata_string`, so the query never sees them.
//
// `@reference.site` wraps the whole `pattern_reference` so the `@` is part of
// the hit area; the inner `@reference` is the identifier we jump to.
//
// First-party, so it lives here rather than in vendor/ — the manifest pins
// vendored bytes only.
// `set` is captured as @kind rather than added as a new capture name: the
// builder's `pendingKind` mechanism already carries a declaration keyword
// to the name that follows it, and `set` is a third keyword in that same
// position — `kind` is just the captured node's text, so no builder change
// is needed to tell a set apart from a pattern or a fragment.
//
// `test` is the fourth such keyword and is captured the same way. A
// `test_reference` needs no special case for `test_negation`: the negation
// wraps the reference rather than replacing it, so `!@name` and `@name`
// present the identical inner node and both stay navigable.
export const PATTERN_NAVIGATION_QUERY = `
(pattern_definition (definition_kind)? @kind (identifier) @definition)
(lexicon_definition "lexicon" @kind (identifier) @definition)
(set_definition "set" @kind (identifier) @definition)
(test_definition "test" @kind (identifier) @definition)
(pattern_reference (identifier) @reference) @reference.site
(set_reference (identifier) @reference) @reference.site
(test_reference (identifier) @reference) @reference.site
`;

// Build the index from query captures. Takes captures rather than a tree so it
// stays pure and testable without the grammar WASM; the WASM-backed
// characterization lives in js-tests/pattern-navigation.test.mjs.
export function patternNavigationIndex(captures) {
  const definitions = new Map();
  const definitionSites = [];
  const references = new Map();
  let pendingKind = null;
  let pendingSite = null;
  for (const { name, node } of captures) {
    const span = { from: node.startIndex, to: node.endIndex };
    if (name === "kind") {
      pendingKind = node.text;
    } else if (name === "definition") {
      // A `pattern_definition` yields @kind before @definition when the
      // optional keyword is present; a bare `NAME :=` yields no @kind at all.
      const kind = pendingKind ?? "pattern";
      pendingKind = null;
      definitionSites.push({ ...span, name: node.text, kind });
      // First declaration wins as the jump target. Two declarations of one
      // name is a compile error, but the editor sees it while a declaration is
      // being copied or renamed, and both must stay addressable — which is
      // what definitionSites is for.
      if (!definitions.has(node.text)) definitions.set(node.text, { ...span, kind });
    } else if (name === "reference.site") {
      pendingSite = span;
    } else if (name === "reference") {
      // Jump to the identifier, hit-test from the `@`.
      const site = { ...span, hitFrom: pendingSite?.from ?? span.from };
      pendingSite = null;
      const sites = references.get(node.text);
      if (sites) sites.push(site);
      else references.set(node.text, [site]);
    }
  }
  return { definitions, definitionSites, references };
}

// The definition or reference name the cursor sits in or against. Both edges
// count, so the cursor just after a name still resolves it.
export function symbolAtPosition(index, pos) {
  for (const site of index.definitionSites) {
    if (pos >= site.from && pos <= site.to) return { name: site.name, role: "definition" };
  }
  for (const [name, sites] of index.references) {
    for (const site of sites) {
      if (pos >= site.hitFrom && pos <= site.to) return { name, role: "reference" };
    }
  }
  return null;
}

// Resolve one navigation request into a destination, or into the reason there
// isn't one. Pure: the caller turns `{from, to}` into a transaction and
// `message` into status text. `action` is "definition", "next", or "previous".
export function navigationTarget(index, pos, action) {
  const symbol = symbolAtPosition(index, pos);
  if (!symbol) return { message: "No definition or reference here." };
  const { name } = symbol;
  const sites = index.references.get(name) ?? [];

  if (action === "definition") {
    const definition = index.definitions.get(name);
    if (!definition) return { message: `\`${name}\` is not defined in this document.` };
    const count = sites.length === 1 ? "1 reference" : `${sites.length} references`;
    // Say so rather than silently picking one: two declarations is a compile
    // error the author needs to see, and go-to-definition is where they meet
    // it.
    const declarations = index.definitionSites.filter((site) => site.name === name).length;
    const duplicated =
      declarations > 1 ? ` — ${declarations} declarations, showing the first` : "";
    return {
      from: definition.from,
      to: definition.to,
      message: `${definition.kind} ${name}, ${count}${duplicated}.`,
    };
  }

  if (!sites.length) return { message: `\`${name}\` has no references.` };
  // Where in the cycle are we? On the definition (or anywhere that is not a
  // reference site) the first step lands on reference 1.
  const current = sites.findIndex((site) => pos >= site.hitFrom && pos <= site.to);
  const step = action === "previous" ? -1 : 1;
  const next =
    current === -1
      ? step === 1
        ? 0
        : sites.length - 1
      : (current + step + sites.length) % sites.length;
  const site = sites[next];
  return { from: site.from, to: site.to, message: `Reference ${next + 1} of ${sites.length}.` };
}

// Turn tree-sitter highlight captures into ordered, non-overlapping decoration
// spans. `captures` are `{ name, start, end }` where start/end are tree-sitter
// node indices — which are UTF-16 code-unit offsets into the very string
// CodeMirror holds, so they ARE CodeMirror positions and must be used as-is.
// (The byte<->UTF-16 position index is only for the server's UTF-8 byte ranges;
// running tree-sitter indices through it mis-scales every position the moment
// the document contains multi-byte text, so decorations drift earlier and
// earlier until captures near the end map before the visible tail and it goes
// completely unstyled.) Sort start-ascending, longest-first on ties, then keep a
// single monotonic cover so the longest capture on any span wins.
export function highlightRangesFromCaptures(captures) {
  const spans = captures
    .filter((capture) => captureClasses.has(capture.name))
    .map((capture) => ({
      cls: captureClasses.get(capture.name),
      start: capture.start,
      end: capture.end,
    }))
    .sort((left, right) => left.start - right.start || right.end - left.end);
  const ranges = [];
  let cursor = 0;
  for (const span of spans) {
    if (span.start < cursor || span.end <= span.start) continue;
    ranges.push(span);
    cursor = span.end;
  }
  return ranges;
}

let grammarPromise;
async function loadGrammar() {
  if (!grammarPromise) {
    grammarPromise = (async () => {
      const { Parser, Language, Query } = await import(assetUrl("./vendor/web-tree-sitter.js"));
      await Parser.init({
        locateFile: (name) => assetUrl(`./vendor/${name}`),
      });
      const [language, querySource] = await Promise.all([
        Language.load(assetUrl("./vendor/tree-sitter-lingrep.wasm")),
        fetch(assetUrl("./vendor/highlights.scm")).then((response) => response.text()),
      ]);
      const query = new Query(language, querySource);
      // Compiled once per page, not per keystroke.
      const navigationQuery = new Query(language, PATTERN_NAVIGATION_QUERY);
      return { Parser, language, query, navigationQuery };
    })();
  }
  return grammarPromise;
}

let codeMirrorPromise;
async function loadCodeMirror() {
  if (!codeMirrorPromise) {
    codeMirrorPromise = import(assetUrl("./vendor/codemirror.js"));
  }
  return codeMirrorPromise;
}

// UTF-8 byte length of one code point without encoding it. Lone surrogates
// take the U+FFFD replacement path, exactly like TextEncoder: 3 bytes.
function utf8ByteLength(character) {
  const codePoint = character.codePointAt(0);
  if (codePoint <= 0x7f) return 1;
  if (codePoint <= 0x7ff) return 2;
  if (codePoint <= 0xffff) return 3;
  return 4;
}

// One O(n) pass over `source` building cumulative UTF-8 byte / UTF-16 unit
// boundaries at every code point start. All byte-offset lookups for a render
// pass or diagnostic then share this index instead of re-walking and
// re-encoding the source per capture. CodeMirror document positions are
// UTF-16 code unit offsets, so the UTF-16 side maps 1:1.
export function createPositionIndex(source) {
  // Code point count never exceeds the UTF-16 length, so this capacity is exact.
  const byteBoundaries = new Uint32Array(source.length + 1);
  const utf16Boundaries = new Uint32Array(source.length + 1);
  let count = 0;
  let bytes = 0;
  let utf16 = 0;
  for (const character of source) {
    bytes += utf8ByteLength(character);
    utf16 += character.length;
    count += 1;
    byteBoundaries[count] = bytes;
    utf16Boundaries[count] = utf16;
  }
  return { byteBoundaries, utf16Boundaries, count, totalBytes: bytes };
}

// UTF-16 length of the longest prefix whose UTF-8 byte length is at most
// `byteOffset` (a mid-character offset resolves to the character's start).
function byteToUtf16(index, byteOffset) {
  if (byteOffset <= 0) return 0;
  let lo = 0;
  let hi = index.count;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (index.byteBoundaries[mid] <= byteOffset) lo = mid;
    else hi = mid - 1;
  }
  return index.utf16Boundaries[lo];
}

function rangeOnIndex(index, range) {
  if (!range || range.start < 0 || range.end < range.start || range.end > index.totalBytes) return null;
  return { start: byteToUtf16(index, range.start), end: byteToUtf16(index, range.end) };
}

export function utf8RangeToUtf16(source, range) {
  if (!range || range.start < 0 || range.end < range.start) return null;
  const index = createPositionIndex(source);
  return rangeOnIndex(index, range);
}

export function diagnosticExcerpt(source, location) {
  const range = utf8RangeToUtf16(source, location?.byte_range);
  let start = range?.start;
  let end = range?.end;
  if (start == null) {
    const lines = source.split("\n");
    if (!location?.line || location.line > lines.length) return null;
    start = lines.slice(0, location.line - 1).reduce((sum, line) => sum + line.length + 1, 0) + Math.max(0, location.column - 1);
    end = Math.min(source.length, start + 1);
  }
  const lineStart = source.lastIndexOf("\n", start - 1) + 1;
  const newline = source.indexOf("\n", end);
  const lineEnd = newline < 0 ? source.length : newline;
  return { text: source.slice(lineStart, lineEnd), caretStart: start - lineStart, caretWidth: Math.max(1, end - start), range: { start, end } };
}

export function shouldSelectDiagnostic(reason) { return reason === "blur" || reason === "submit"; }

// ---------------------------------------------------------------------------
// lingrep patterns vocabulary and completion logic (pure; exercised by the js-tests)
// ---------------------------------------------------------------------------

export const PATTERN_ATTRIBUTES = [
  ["text", "Portable surface form as written"],
  ["lemma", "Dictionary lemma"],
  ["norm", "Normalized form"],
  ["upos", "Portable coarse POS tag (VERB, NOUN, …)"],
  ["xpos", "Provider-native POS tag"],
  ["unidic.pos1", "UniDic POS level 1 (major category)"],
  ["unidic.pos2", "UniDic POS level 2"],
  ["unidic.pos3", "UniDic POS level 3"],
  ["unidic.pos4", "UniDic POS level 4"],
  ["unidic.ctype", "UniDic conjugation type (カ変, 五段, …)"],
  ["unidic.cform", "UniDic conjugation form (終止形, 連用形-一般, …)"],
  ["unidic.lform", "UniDic lemma form"],
  ["unidic.pron", "UniDic pronunciation"],
  ["unidic.orthbase", "UniDic base surface form"],
  ["unidic.pronbase", "UniDic base pronunciation"],
  ["unidic.goshu", "UniDic word origin class (和, 漢, 洋, …)"],
  ["unidic.itype", "UniDic initial inflection type"],
  ["unidic.iform", "UniDic initial inflection form"],
  ["unidic.ftype", "UniDic final inflection type"],
  ["unidic.fform", "UniDic final inflection form"],
  ["unidic.icontype", "UniDic initial connection type"],
  ["unidic.fcontype", "UniDic final connection type"],
  ["unidic.type", "UniDic token type"],
  ["unidic.kana", "UniDic reading in kana"],
  ["unidic.kanabase", "UniDic base reading in kana"],
  ["unidic.form", "UniDic form field"],
  ["unidic.formbase", "UniDic base form"],
  ["unidic.atype", "UniDic accent type"],
  ["unidic.acontype", "UniDic accent connection type"],
  ["unidic.amodtype", "UniDic accent modification type"],
  ["unidic.lextype", "UniDic lexical type"],
];

export const PATTERN_POS_TAGS = [
  ["ADJ", "Adjective (形容詞, 形状詞)"],
  ["ADP", "Adposition (格・係・副助詞)"],
  ["ADV", "Adverb (副詞)"],
  ["AUX", "Auxiliary (助動詞, 非自立)"],
  ["CCONJ", "Coordinating conjunction (接続詞)"],
  ["DET", "Determiner (連体詞)"],
  ["INTJ", "Interjection (感動詞)"],
  ["NOUN", "Noun (名詞, 接尾辞など)"],
  ["NUM", "Numeral (数詞)"],
  ["PART", "Particle (終助詞など)"],
  ["PRON", "Pronoun (代名詞)"],
  ["PROPN", "Proper noun (固有名詞, 外国語)"],
  ["PUNCT", "Punctuation (句点, 読点, 括弧)"],
  ["SCONJ", "Subordinating conjunction (接続・準体助詞)"],
  ["SPACE", "Whitespace (空白)"],
  ["SYM", "Symbol (補助記号, 絵文字)"],
  ["VERB", "Verb (動詞)"],
  ["X", "Other"],
];

export const PATTERN_BY_VALUES = PATTERN_ATTRIBUTES.map(([name]) => name);
export const PATTERN_KEY_COMPONENTS = [
  ["first", "Read the first token of a capture span"],
  ["last", "Read the last token of a capture span"],
  ["span", "Concatenate the attribute across the complete capture span"],
];

// Descriptive keys a catalog declares, and the compiler-owned keys that are
// admitted without declaration. Counts are occurrences across the tracked
// catalogs, which is why `role` and `source` lead: the list was previously
// missing the four most-used keys entirely.
//
// A `schema` block CLOSES a catalog's descriptive key set, so this list is a
// starting point rather than a guarantee — an undeclared key is refused, and
// a compiler-owned key is an error to declare.
export const PATTERN_METADATA_KEYS = [
  ["role", "Declared role (e.g. helper, research)"],
  ["category", "Research category for the declaration"],
  ["source", "Provenance of the declaration"],
  ["examples", "Example sentences, as a JSON-style list"],
  ["counterexamples", "Sentences the declaration must NOT match"],
  ["layer", "Analysis layer (e.g. S0, S1, S2, S3)"],
  ["type", "Pattern type (e.g. 文末尾から)"],
  ["meidai", "命題: the propositional form covered"],
  ["status", "Authoring status (TODO, FIXME, …)"],
  ["mapping_note", "How this maps to its upstream source"],
  // Compiler-owned: admitted without declaration, and an error to declare.
  ["output_name", "Compiler-owned: published name for this definition"],
  ["groups", "Compiler-owned: %group membership, as a list"],
  ["priority_order", "Compiler-owned: rank used by `resolve … priority_order`"],
  ["key", "Compiler-owned: publication key"],
  ["transform", "Compiler-owned: published-value transform"],
  ["tags", "Compiler-owned: publication tags"],
  ["render", "Compiler-owned: rendering selector"],
  ["dictionary", "Compiler-owned: dictionary the catalog expects"],
  ["anchor", "Compiler-owned: anchor an `admit` measures against"],
  ["max_nesting", "Compiler-owned: nesting ceiling for this catalog"],
];

export const PATTERN_TOP_LEVEL = [
  { label: "pattern", template: "pattern ${name} := ${}.", type: "keyword", info: "Emitting pattern declaration" },
  { label: "fragment", template: "fragment ${name} := ${}.", type: "keyword", info: "Reusable definition; emits no matches" },
  { label: "lexicon", template: "lexicon ${name} by [text, norm] := {\n  \"${term}\"\n}.", type: "keyword", info: "Inline authored-term inventory" },
  { label: "set", template: "set ${NAME} := {${}}.", type: "keyword", info: "Named value list for `in` / `not in`" },
  { label: "defaults", template: "defaults\n  ${}\nend", type: "keyword", info: "File-level metadata values; precedes every declaration" },
  { label: "schema", template: "schema\n  ${}\nend", type: "keyword", info: "Closes the catalog's descriptive key set" },
  { label: "policy", template: "policy\n  ${}\nend", type: "keyword", info: "Ordered publication program; one per catalog" },
  { label: "preceded_by", template: "preceded_by(${})", type: "function", info: "Zero-width: expression ends here" },
  { label: "followed_by", template: "followed_by(${})", type: "function", info: "Zero-width: expression starts here" },
  { label: "not_preceded_by", template: "not_preceded_by(${})", type: "function", info: "Zero-width negated lookbehind" },
  { label: "not_followed_by", template: "not_followed_by(${})", type: "function", info: "Zero-width negated lookahead" },
  { label: "same_as", template: "same_as($${name}, by=${form})", type: "function", info: "Repeat a captured span, compared by a canonical schema key" },
  { label: "capture", detail: "(?<name> …)", template: "(?<${name}> ${})", type: "function", info: "Named capture group" },
];

// Documentation shown on hover. Keyword entries double as operator docs.
export const PATTERN_KEYWORD_DOCS = [
  ["pattern", "Emitting pattern declaration: pattern NAME := expression."],
  ["fragment", "Reusable definition; reference it with @NAME. Emits no matches of its own."],
  ["lexicon", "Authored-term inventory; use matches or summarize to inspect occurrences."],
  ["set", "Names a value list: set NAME := {a, b}. Reference it as [attr in @NAME]."],
  ["defaults", "File-level metadata values. Precedes the first annotation or declaration."],
  ["schema", "Declares — and closes — the catalog's descriptive key set."],
  ["policy", "Ordered publication program. Order is semantic."],
  ["end", "Closes a `defaults`, `schema`, or `policy` block."],
  ["in", "Membership test: [text in {ある, 有る}]. Negate with `not in`."],
  ["same_as", "Repeats a captured span: same_as($name, by=text). The projection is any canonical schema key."],
  ["preceded_by", "Zero-width lookbehind: the expression must end at this boundary without consuming input."],
  ["followed_by", "Zero-width lookahead: the expression must start at this boundary without consuming input."],
  ["not_preceded_by", "Negated zero-width lookbehind: the expression must NOT end here."],
  ["not_followed_by", "Negated zero-width lookahead: the expression must NOT start here."],
];

export const PATTERN_ATTRIBUTE_EXAMPLES = {
  text: "[text=走った]",
  lemma: "[lemma=走る]",
  upos: "[upos=VERB]",
  "unidic.pos1": "[unidic.pos1=動詞]",
  "unidic.ctype": "[unidic.ctype=五段-カ行]",
  "unidic.cform": "[unidic.cform=終止形]",
  "unidic.goshu": "[unidic.goshu=和]",
  "unidic.kana": "[unidic.kana=ハシ]",
  "unidic.pron": "[unidic.pron=はし]",
  "unidic.atype": "[unidic.atype=3]",
};

export const PATTERN_BY_VALUE_DOCS = {
  text: "Compare portable surface forms, exactly as written.",
  lemma: "Compare dictionary lemmas (inflection-insensitive).",
  norm: "Compare normalized forms.",
  upos: "Compare portable coarse POS tags.",
  xpos: "Compare provider-native POS tags.",
  "unidic.kana": "Compare UniDic kana readings.",
  "unidic.pron": "Compare UniDic pronunciations.",
};

// Signature help for calls that take arguments. `params` render in order with
// the active one emphasized; `doc` explains the call itself.
export const PATTERN_SIGNATURES = {
  same_as: {
    name: "same_as",
    params: [
      { label: "$capture", doc: "Named capture whose span is repeated." },
      { label: "by=text", doc: "Comparison projection: any canonical schema key." },
    ],
    doc: "Consumes the next span when it compares equal to the captured span.",
  },
};

const WORD_CHAR = /[\p{L}\p{N}_]/u;

// Definition references: `pattern NAME :=`, `fragment NAME :=`, `set NAME :=`,
// or bare `NAME :=` at line start. Names may be non-ASCII (e.g. Japanese).
export function patternDefinitions(source, { includeDuplicates = false } = {}) {
  const definitions = [];
  const seen = new Set();
  const re = /(?:^|\n)\s*(?:(pattern|fragment|set|lexicon)\s+)?([^\s:=#()[\]{},.|&@$]+)(?:\s+by\s+\[[^\]]+\])?\s*:=/g;
  let match;
  while ((match = re.exec(source)) !== null) {
    const name = match[2];
    if (!includeDuplicates) {
      if (seen.has(name)) continue;
      seen.add(name);
    }
    definitions.push({ name, kind: match[1] ?? "pattern" });
  }
  return definitions;
}

/// One-line preview of an lingrep patterns source for folded UI chrome: the first actual
/// pattern name. Grammar-level statements everyone ships (`defaults`,
/// `set NAME := {…}`, `fragment NAME := …`) explain nothing to a researcher,
/// so the preview initially prefers kind === "pattern" (bare or explicit)
/// definitions over fragments and sets, falling back to the first definition
/// of any kind, then to the bare count, then to nothing.
///
/// Non-ASCII names are kept intact. Returns `null` when no definition exists,
/// letting the caller decide its static label.
export function patternPreview(source, label = "Pattern") {
  // Raw occurrences feed the pick: a name defined both as a fragment and as a
  // pattern picks its pattern site even though the navigation index dedupes
  // it away. The (+N more) count stays the distinct definition count.
  const occurrences = patternDefinitions(source, { includeDuplicates: true });
  if (occurrences.length === 0) return label;
  const picked =
    occurrences.find((definition) => definition.kind === "pattern") ??
    occurrences[0];
  const more = patternDefinitions(source).length - 1;
  return `${label}: ${picked.name}${more > 0 ? ` (+${more} more)` : ""}`;
}

export function patternCaptures(source) {
  const names = [];
  const seen = new Set();
  const re = /\(\?<([^\s>]+)>/g;
  let match;
  while ((match = re.exec(source)) !== null) {
    if (seen.has(match[1])) continue;
    seen.add(match[1]);
    names.push(match[1]);
  }
  return names;
}

// Where is the cursor? Pure textual analysis (robust on the incomplete input
// typical while typing, where the parse tree is mostly ERROR nodes). The
// scanner skips `#` line comments and `/regex/` literals so brackets inside
// them do not disturb the nesting state.
export function patternCompletionContext(source, pos) {
  let from = pos;
  while (from > 0 && WORD_CHAR.test(source[from - 1])) from--;
  const word = source.slice(from, pos);
  const before = source[from - 1];

  if (before === "@") return { kind: "reference", from, word };
  if (before === "$") return { kind: "capture", from, word };

  const lineStart = source.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
  const keyPrefix = source.slice(lineStart, from);
  const keyList = keyPrefix.match(/^\s*key\s*:\s*\[([^\]]*)$/u);
  if (keyList) {
    const current = keyList[1].split(",").at(-1).trimStart();
    if (/^\$[\p{L}\p{N}_]+\.[\p{L}\p{N}_]*$/u.test(current)) {
      return { kind: "key-capture-segment", from, word };
    }
    if (/^\$[\p{L}\p{N}_]+\.[\p{L}\p{N}_]+\.[\p{L}\p{N}_]*$/u.test(current)) {
      return { kind: "key-capture-attribute", from, word };
    }
    if (current === "" || /^[\p{L}\p{N}_]*$/u.test(current)) {
      return { kind: "key-match-attribute", from, word };
    }
    return { kind: "none", from, word };
  }

  let bracketDepth = 0;
  let parenDepth = 0;
  let lastBoundary = -1; // last of [ ( & | , { outside comments/regex
  let lastOpenParenOwner = null; // identifier directly before the innermost unmatched (
  let i = 0;
  while (i < from) {
    const ch = source[i];
    if (ch === "#") {
      const newline = source.indexOf("\n", i);
      i = newline < 0 ? from : newline + 1;
      continue;
    }
    if (ch === "/") {
      // Treat as a regex literal and skip to its unescaped closing slash.
      let j = i + 1;
      while (j < from && (source[j] !== "/" || source[j - 1] === "\\")) j++;
      if (j >= from) return { kind: "none", from, word }; // inside an open regex
      i = j + 1;
      continue;
    }
    if (ch === "[") { bracketDepth++; lastBoundary = i; }
    else if (ch === "]") { bracketDepth = Math.max(0, bracketDepth - 1); }
    else if (ch === "(") {
      parenDepth++;
      lastBoundary = i;
      let k = i - 1;
      while (k >= 0 && WORD_CHAR.test(source[k])) k--;
      lastOpenParenOwner = source.slice(k + 1, i) || lastOpenParenOwner;
    } else if (ch === ")") {
      parenDepth = Math.max(0, parenDepth - 1);
      if (parenDepth === 0) lastOpenParenOwner = null;
    } else if (ch === "&" || ch === "|" || ch === "," || ch === "{") {
      lastBoundary = i;
    }
    i++;
  }

  const segment = source.slice(lastBoundary + 1, from);
  const assignment = segment.match(/^\s*([\p{L}\p{N}_]+)\s*=\s*$/u);
  if (assignment && (bracketDepth > 0 || parenDepth > 0)) {
    const attribute = assignment[1].toLowerCase();
    if (attribute === "upos") return { kind: "pos-value", from, word };
    if (attribute === "by") return { kind: "by-value", from, word };
    return { kind: "none", from, word };
  }
  if (parenDepth > 0 && lastOpenParenOwner === "same_as" && segment.trim() === "") {
    return { kind: "same-as-arg", from, word };
  }
  if (bracketDepth > 0) {
    return segment.trim() === "" ? { kind: "attribute", from, word } : { kind: "none", from, word };
  }
  if (parenDepth > 0) return { kind: "none", from, word };

  // Metadata keys live inside an unclosed `defaults` or `schema` block. This
  // used to scan for a line-initial `metadata` and its `begin` — the retired
  // block, which no longer parses, so the context could only ever fire on text
  // the compiler rejects. `policy` is deliberately not here: it holds
  // statements, not `key: value` annotations.
  const lineStartBlock = /(?:^|\n)[ \t]*(defaults|schema)\b/g;
  const lineStartEnd = /(?:^|\n)[ \t]*end\b/g;
  const preceding = source.slice(0, from);
  let lastBlock = -1;
  let lastEnd = -1;
  let m;
  while ((m = lineStartBlock.exec(preceding)) !== null) lastBlock = m.index;
  while ((m = lineStartEnd.exec(preceding)) !== null) lastEnd = m.index;
  if (lastBlock > lastEnd) return { kind: "metadata-key", from, word };

  return { kind: "top-level", from, word };
}

// `@` reaches three kinds of declaration, not two. A binary fragment-or-pattern
// test called a set a pattern that emits matches, which is the opposite of what
// a set does — so name the kind rather than inferring it from not being a
// fragment. `verbose` picks the hover phrasing over the completion one.
function describeKind(kind, verbose) {
  switch (kind) {
    case "fragment":
      return verbose ? "fragment (reusable; emits no matches)" : "fragment (emits nothing)";
    case "lexicon":
      return verbose ? "lexicon (authored-term inventory; emits occurrences)" : "lexicon";
    case "set":
      return verbose ? "set (named values; emits no matches)" : "set (named values)";
    default:
      return verbose ? "pattern (emits matches)" : "pattern";
  }
}

export function patternCompletionOptions(context, env = { definitions: [], captures: [] }) {
  switch (context.kind) {
    case "key-match-attribute":
    case "key-capture-attribute":
      return PATTERN_ATTRIBUTES.map(([name, info]) => ({
        label: name,
        type: "property",
        info: `key projection: ${info}`,
      }));
    case "key-capture-segment":
      return [
        ...PATTERN_ATTRIBUTES.map(([name, info]) => ({
          label: name,
          type: "property",
          info: `capture key projection: ${info}`,
        })),
        ...PATTERN_KEY_COMPONENTS.map(([name, info]) => ({
          label: name,
          type: "enum",
          info,
          apply: `${name}.`,
        })),
      ];
    case "attribute":
      return PATTERN_ATTRIBUTES.map(([name, info]) => ({
        label: name,
        type: "property",
        info,
        apply: `${name}=`,
      }));
    case "pos-value":
      return PATTERN_POS_TAGS.map(([tag, info]) => ({ label: tag, type: "enum", info }));
    case "by-value":
      return PATTERN_BY_VALUES.map((value) => ({
        label: value,
        type: "enum",
        info: "same_as comparison attribute",
      }));
    case "reference":
      return env.definitions.map((definition) => ({
        label: definition.name,
        type: "variable",
        info: describeKind(definition.kind, false),
      }));
    case "capture":
      return env.captures.map((name) => ({
        label: name,
        type: "variable",
        info: "named capture",
      }));
    case "same-as-arg":
      return env.captures.map((name) => ({
        label: `$${name}`,
        type: "variable",
        info: "captured span to repeat",
      }));
    case "metadata-key":
      return PATTERN_METADATA_KEYS.map(([name, info]) => ({
        label: name,
        type: "property",
        info,
        apply: `${name}: `,
      }));
    case "top-level":
      return PATTERN_TOP_LEVEL.map((entry) => ({ ...entry }));
    default:
      return [];
  }
}

// Documentation for the token under `pos`, for hover tooltips. Pure textual
// analysis like patternCompletionContext: tolerant of incomplete input, and
// returns null inside `#` comments where no documentation applies.
export function patternHoverInfo(source, pos) {
  if (pos < 0 || pos > source.length) return null;
  const lineStart = source.lastIndexOf("\n", Math.max(0, pos - 1)) + 1;
  if (source.slice(lineStart, pos).includes("#")) return null;
  let from = pos;
  while (from > 0 && WORD_CHAR.test(source[from - 1])) from--;
  let to = pos;
  while (to < source.length && WORD_CHAR.test(source[to])) to++;
  const word = source.slice(from, to);
  if (!word) return null;

  const hoverLineStart = source.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
  const inKeyProjection = /^\s*key\s*:\s*\[[^\]]*$/u.test(
    source.slice(hoverLineStart, from),
  );

  const marker = source[from - 1];
  if (marker === "@") {
    const definition = patternDefinitions(source).find((entry) => entry.name === word);
    return {
      from: from - 1,
      to,
      title: `@${word}`,
      detail: definition
        ? describeKind(definition.kind, true)
        : "no such definition in this document",
    };
  }
  if (marker === "$") {
    const defined = patternCaptures(source).includes(word);
    return {
      from: from - 1,
      to,
      title: `$${word}`,
      detail: defined
        ? "named capture, defined by (?<name> …)"
        : "no capture with this name in this document",
    };
  }

  if (inKeyProjection) {
    const component = PATTERN_KEY_COMPONENTS.find(([name]) => name === word);
    if (component) return { from, to, title: word, detail: component[1] };
    const attribute = PATTERN_ATTRIBUTES.find(([name]) => name === word);
    if (attribute) {
      return { from, to, title: word, detail: `Key projection: ${attribute[1]}` };
    }
  }

  // Attribute value: the word directly follows `attr=`.
  const before = source.slice(Math.max(0, from - 40), from);
  const valueOf = before.match(/([\p{L}\p{N}_]+)\s*=$/u);
  if (valueOf) {
    const attribute = valueOf[1].toLowerCase();
    if (attribute === "upos") {
      const tag = PATTERN_POS_TAGS.find(([name]) => name === word);
      return tag ? { from, to, title: `upos=${word}`, detail: tag[1] } : null;
    }
    if (attribute === "by") {
      const detail = PATTERN_BY_VALUE_DOCS[word];
      return detail ? { from, to, title: `by=${word}`, detail } : null;
    }
    return null;
  }

  // Attribute name: the word is directly followed by `=`.
  if (/^\s*=/.test(source.slice(to, to + 4))) {
    if (word === "by") {
      return { from, to, title: "by=", detail: "same_as comparison projection: any canonical schema key." };
    }
    const attribute = PATTERN_ATTRIBUTES.find(([name]) => name === word);
    if (!attribute) return null;
    const example = PATTERN_ATTRIBUTE_EXAMPLES[word];
    return {
      from,
      to,
      title: word,
      detail: example ? `${attribute[1]} — e.g. ${example}` : attribute[1],
    };
  }

  // Keywords and call names, but not definition names (`name :=`).
  if (!/^\s*:=/.test(source.slice(to, to + 4))) {
    const keyword = PATTERN_KEYWORD_DOCS.find(([name]) => name === word);
    if (keyword) return { from, to, title: word, detail: keyword[1] };
  }
  return null;
}

// Is the cursor inside a call's argument list? Returns the call and which
// argument is active (top-level commas before the cursor), skipping comments
// and /regex/ literals and ignoring commas nested in [], {}, or deeper ().
export function patternSignatureContext(source, pos) {
  let bracketDepth = 0;
  let braceDepth = 0;
  const parens = []; // stack of { owner, at, commas }
  let i = 0;
  while (i < pos) {
    const ch = source[i];
    if (ch === "#") {
      const newline = source.indexOf("\n", i);
      i = newline < 0 ? pos : newline + 1;
      continue;
    }
    if (ch === "/") {
      let j = i + 1;
      while (j < pos && (source[j] !== "/" || source[j - 1] === "\\")) j++;
      if (j >= pos) return null; // inside an open regex literal
      i = j + 1;
      continue;
    }
    if (ch === "[") bracketDepth++;
    else if (ch === "]") bracketDepth = Math.max(0, bracketDepth - 1);
    else if (ch === "{") braceDepth++;
    else if (ch === "}") braceDepth = Math.max(0, braceDepth - 1);
    else if (ch === "(") {
      let k = i - 1;
      while (k >= 0 && WORD_CHAR.test(source[k])) k--;
      parens.push({ owner: source.slice(k + 1, i), at: i, commas: 0 });
    } else if (ch === ")") {
      parens.pop();
    } else if (ch === "," && parens.length > 0 && bracketDepth === 0 && braceDepth === 0) {
      parens[parens.length - 1].commas++;
    }
    i++;
  }
  const open = parens[parens.length - 1];
  if (!open || !PATTERN_SIGNATURES[open.owner]) return null;
  const signature = PATTERN_SIGNATURES[open.owner];
  return {
    name: open.owner,
    parenPos: open.at,
    argIndex: Math.min(open.commas, signature.params.length - 1),
    signature,
  };
}

// ---------------------------------------------------------------------------
// Browser integration
// ---------------------------------------------------------------------------

// Compile facts for the status line. Returns null when the server sent none —
// a stubbed `{"status":"valid"}` (browser tests) must not break the editor.
export function statsSummary(stats) {
  if (!stats) return null;
  const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;
  return [
    plural(stats.definitions, "definition"),
    plural(stats.references, "reference"),
    `depth ${stats.depth}`,
    `${stats.nfa_states} NFA states`,
  ].join(", ");
}

// What the status line says about a completed check. Null for anything that is
// not a successful compile — the invalid path has its own message, built from
// the blocking diagnostic.
export function checkStatusLine(result) {
  if (result?.status !== "valid") return null;
  const warnings = result.diagnostics?.length ?? 0;
  const headline = warnings
    ? `Pattern is valid, ${warnings} warning${warnings === 1 ? "" : "s"}.`
    : "Pattern is valid.";
  const summary = statsSummary(result.stats);
  return summary ? `${headline} ${summary}.` : headline;
}

// A warning is not an invalidity. Kept separate from the status text so the
// distinction is asserted rather than implied by where a DOM call sits.
export function ariaInvalidFor(result) {
  return result?.status === "valid" ? "false" : "true";
}

function status(state, message, terminal = false) {
  state.status.textContent = message;
  state.status.setAttribute("aria-live", terminal ? "polite" : "off");
}

// The navigation index for the current parse. Rebuilt per request rather than
// cached: `state.tree` is replaced on every edit, and a stale index would
// navigate to positions the document no longer has. Returns null when there is
// no tree at all.
function navigationIndexFor(state) {
  if (!state.tree || !state.navigationQuery) return null;
  return patternNavigationIndex(state.navigationQuery.captures(state.tree.rootNode));
}

// tree-sitter returns a tree for malformed text — with ERROR nodes in it — so
// "the pattern does not parse" is never why navigation is unavailable. The two
// real causes are the highlight cap (which nulls the tree and marks the
// wrapper) and a request that arrives before the first parse.
function navigationUnavailable(state) {
  return state.wrapper.classList.contains("pattern-editor-plain")
    ? "Navigation is unavailable above the 64 KiB highlighting cap."
    : "Navigation is unavailable until the first parse completes.";
}

function runNavigation(state, action) {
  const index = navigationIndexFor(state);
  if (!index) {
    status(state, navigationUnavailable(state), true);
    return true;
  }
  const target = navigationTarget(index, state.view.state.selection.main.head, action);
  status(state, target.message, true);
  if (target.from === undefined) return true;
  state.view.dispatch({
    selection: { anchor: target.from, head: target.to },
    scrollIntoView: true,
  });
  return true;
}

// Tree-sitter powered syntax highlighting, kept outside @codemirror/language
// so the pinned lingrep patterns WASM grammar and highlights.scm stay the single source of
// truth. Reparses fully on each change; documents over the highlight cap fall
// back to plain text.
function patternTreeSitter(state, cm) {
  const { ViewPlugin, Decoration } = cm;
  const compute = (view) => {
    const source = view.state.doc.toString();
    const positions = createPositionIndex(source);
    state.positions = positions;
    if (positions.totalBytes > MAX_HIGHLIGHT_BYTES) {
      state.tree?.delete();
      state.tree = null;
      state.wrapper.classList.add("pattern-editor-plain");
      status(state, "Syntax highlighting paused above 64 KiB; server checking remains available.");
      return Decoration.none;
    }
    state.wrapper.classList.remove("pattern-editor-plain");
    const tree = state.parser.parse(source);
    state.tree?.delete();
    state.tree = tree;
    const spans = highlightRangesFromCaptures(
      state.query.captures(tree.rootNode).map(({ name, node }) => ({
        name,
        start: node.startIndex,
        end: node.endIndex,
      })),
    );
    const ranges = spans.map((span) =>
      Decoration.mark({ class: span.cls }).range(span.start, span.end),
    );
    return Decoration.set(ranges);
  };
  return ViewPlugin.fromClass(
    class {
      constructor(view) {
        this.decorations = compute(view);
      }
      update(update) {
        if (update.docChanged) this.decorations = compute(update.view);
      }
    },
    { decorations: (plugin) => plugin.decorations },
  );
}

// Instant local diagnostics from the latest tree-sitter parse: ERROR and
// MISSING nodes, mapped onto UTF-16 document positions.
function localSyntaxDiagnostics(state) {
  const tree = state.tree;
  if (!tree || !tree.rootNode.hasError) return [];
  const docLength = state.view.state.doc.length;
  const diagnostics = [];
  const cursor = tree.walk();
  const visit = () => {
    do {
      if (cursor.nodeType === "ERROR" || cursor.nodeIsMissing) {
        const node = cursor.currentNode;
        // Tree-sitter node indices are already UTF-16 (CodeMirror) positions.
        const start = node.startIndex;
        const end = Math.max(start + 1, node.endIndex);
        diagnostics.push({
          from: Math.min(start, docLength),
          to: Math.min(end, docLength),
          severity: "error",
          message: cursor.nodeIsMissing ? `Missing ${node.type}` : "Syntax error",
          source: "pattern (local)",
        });
        continue; // do not descend into error subtrees
      }
      if (cursor.currentNode.hasError && cursor.gotoFirstChild()) {
        visit();
        cursor.gotoParent();
      }
    } while (cursor.gotoNextSibling());
  };
  if (cursor.gotoFirstChild()) visit();
  return diagnostics;
}

// Map server diagnostics onto CodeMirror lint diagnostics. Pure: `positions`
// is derived from `source` when the caller has no cached index.
export function mapServerDiagnostics(source, diagnostics, positions = createPositionIndex(source)) {
  const docLength = source.length;
  return diagnostics.map((diagnostic) => {
    const range = rangeOnIndex(positions, diagnostic.location?.byte_range);
    const start = Math.min(range?.start ?? 0, docLength);
    const end = Math.min(Math.max(range?.end ?? start + 1, start + 1), docLength);
    return {
      from: start,
      to: end,
      // Anything unrecognised stays an error: a severity we do not understand
      // must not be quietly demoted to a warning.
      severity: diagnostic.severity === "warning" ? "warning" : "error",
      message: diagnostic.message || "Pattern is invalid.",
      source: `pattern:${diagnostic.code}`,
    };
  });
}

function serverDiagnosticMapping(state, source, diagnostics) {
  return mapServerDiagnostics(source, diagnostics, state.positions ?? createPositionIndex(source));
}

async function checkSource(state, source, reason = "input") {
  state.checkController?.abort();
  const controller = new AbortController();
  state.checkController = controller;
  status(state, "Checking…");
  try {
    const response = await fetch("api/pattern/check", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ source }),
      signal: controller.signal,
    });
    const result = await response.json();
    if (!shouldPublish(source, state.source())) return [];
    if (result.status === "valid") {
      state.textarea.setAttribute("aria-invalid", ariaInvalidFor(result));
      // Warnings arrive here, on the success path, and go through the same
      // mapping as errors so they reach the lint gutter. The caret excerpt
      // stays cleared: it is for the one diagnostic that blocked a compile,
      // and a warning blocked nothing.
      const diagnostics = serverDiagnosticMapping(state, source, result.diagnostics ?? []);
      status(state, checkStatusLine(result), true);
      state.diagnosticNode.replaceChildren();
      state.lastCheck = { source, diagnostics };
      state.textarea.dispatchEvent(
        new CustomEvent("pattern:valid", {
          bubbles: true,
          detail: { source, stats: result.stats ?? null },
        }),
      );
      return diagnostics;
    }
    if (result.status === "invalid") {
      state.textarea.setAttribute("aria-invalid", "true");
      const diagnostic = result.diagnostics?.[0];
      const where = diagnostic?.location
        ? ` at ${diagnostic.location.line}:${diagnostic.location.column}`
        : "";
      status(state, `${diagnostic?.message || "Pattern is invalid."}${where}`, true);
      const excerpt = diagnosticExcerpt(source, diagnostic?.location);
      state.diagnosticNode.replaceChildren();
      if (excerpt) {
        const code = document.createElement("code"); code.textContent = excerpt.text;
        const caret = document.createElement("span"); caret.className = "pattern-diagnostic-caret";
        caret.textContent = `${" ".repeat(excerpt.caretStart)}${"^".repeat(excerpt.caretWidth)}`;
        state.diagnosticNode.append(code, caret);
        state.diagnosticRange = excerpt.range;
        if (shouldSelectDiagnostic(reason) && shouldPublish(source, state.source())) {
          state.view.dispatch({
            selection: { anchor: excerpt.range.start, head: excerpt.range.end },
            scrollIntoView: true,
          });
          state.view.focus();
        }
      }
      const diagnostics = serverDiagnosticMapping(state, source, result.diagnostics ?? []);
      state.lastCheck = { source, diagnostics };
      return diagnostics;
    }
    status(state, "Pattern checking is temporarily unavailable.", true);
    return [];
  } catch (error) {
    if (error.name !== "AbortError" && shouldPublish(source, state.source())) {
      status(state, "Pattern checking is temporarily unavailable.", true);
    }
    return [];
  }
}

// Combined lint source: instant local tree-sitter errors win; only locally
// valid sources make the server round trip.
function patternLintSource(state) {
  return async (view) => {
    const source = view.state.doc.toString();
    const local = localSyntaxDiagnostics(state);
    if (local.length > 0) {
      state.textarea.setAttribute("aria-invalid", "true");
      status(state, "Pattern has a local syntax error.", true);
      return local;
    }
    if (state.lastCheck?.source === source) return state.lastCheck.diagnostics;
    return checkSource(state, source, "input");
  };
}

function patternCompletionSource(state) {
  return (context) => {
    const source = context.state.doc.toString();
    const completionContext = patternCompletionContext(source, context.pos);
    if (completionContext.kind === "none") return null;
    if (!context.explicit && completionContext.word.length === 0 && completionContext.kind !== "reference" && completionContext.kind !== "capture") {
      return null;
    }
    const options = patternCompletionOptions(completionContext, {
      definitions: patternDefinitions(source),
      captures: patternCaptures(source),
    }).map((option) =>
      option.template ? state.cm.snippetCompletion(option.template, option) : option,
    );
    if (options.length === 0) return null;
    return { from: completionContext.from, options };
  };
}

// Hover tooltips with field documentation, fed by patternHoverInfo.
function patternHoverTooltip(cm) {
  return cm.hoverTooltip((view, pos) => {
    const info = patternHoverInfo(view.state.doc.toString(), pos);
    if (!info) return null;
    return {
      pos: info.from,
      end: info.to,
      above: true,
      create: () => {
        const dom = document.createElement("div");
        dom.className = "cm-pattern-hover";
        const title = document.createElement("strong");
        title.textContent = info.title;
        const detail = document.createElement("div");
        detail.textContent = info.detail;
        dom.append(title, detail);
        return { dom };
      },
    };
  });
}

// Signature help as a tooltip pinned to the call's open paren while the
// cursor is inside its argument list. The StateField keeps the same tooltip
// object across cursor moves that change nothing, so the DOM does not flicker;
// a new active argument swaps the object and re-renders.
function signatureTooltipFor(editorState) {
  const context = patternSignatureContext(
    editorState.doc.toString(),
    editorState.selection.main.head,
  );
  if (!context) return null;
  const { signature, argIndex } = context;
  const tooltip = {
    pos: context.parenPos,
    above: true,
    create: () => {
      const dom = document.createElement("div");
      dom.className = "cm-pattern-signature";
      const label = document.createElement("div");
      label.className = "cm-pattern-signature-label";
      label.append(`${signature.name}(`);
      signature.params.forEach((param, index) => {
        if (index > 0) label.append(", ");
        const part = document.createElement(index === argIndex ? "strong" : "span");
        part.textContent = param.label;
        label.append(part);
      });
      label.append(")");
      const active = signature.params[argIndex];
      const doc = document.createElement("div");
      doc.className = "cm-pattern-signature-doc";
      doc.textContent = `${signature.doc} ${active.doc}`;
      dom.append(label, doc);
      return { dom };
    },
  };
  tooltip.lxKey = `${context.name}:${context.parenPos}:${argIndex}`;
  return tooltip;
}

function patternSignatureHelp(cm) {
  const field = cm.StateField.define({
    create: (editorState) => signatureTooltipFor(editorState),
    update: (value, transaction) => {
      if (!transaction.docChanged && !transaction.selection) return value;
      const next = signatureTooltipFor(transaction.state);
      if (value && next && value.lxKey === next.lxKey) return value;
      return next;
    },
    provide: (field) => cm.showTooltip.from(field),
  });
  return field;
}

function patternTheme(cm) {
  return cm.EditorView.theme({
    "&": {
      backgroundColor: "transparent",
      color: "var(--pattern-mirror-text)",
      border: "1px solid var(--pattern-mirror-border)",
      borderRadius: "3px",
    },
    ".cm-scroller": {
      font: "14px/1.45 var(--pattern-font-mono)",
      overflow: "auto",
      maxHeight: "32rem",
    },
    ".cm-content": {
      caretColor: "var(--pattern-caret)",
      padding: "0.65rem 0",
    },
    "&.cm-focused": {
      outline: "2px solid var(--pattern-focus, #4b6cb7)",
      outlineOffset: "1px",
    },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
      backgroundColor: "var(--pattern-selection)",
    },
    ".cm-selectionMatch": {
      backgroundColor: "var(--pattern-selection)",
    },
    ".cm-matchingBracket": {
      backgroundColor: "var(--pattern-selection)",
      outline: "none",
    },
    ".cm-gutters": {
      backgroundColor: "transparent",
      color: "var(--pattern-status-text)",
      border: "none",
    },
  });
}

export async function formatCurrent(textarea) {
  const state = states.get(textarea);
  if (!state) return { status: "unavailable" };
  const source = state.source();
  if (state.formatKey === source && state.formatPromise) return state.formatPromise;
  state.formatKey = source;
  state.formatPromise = fetch("api/pattern/format", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ source }),
  })
    .then((response) => response.json())
    .then((result) => {
      if (result.status !== "formatted" || !shouldPublish(source, state.source())) return result;
      if (result.source !== source) {
        const selection = state.view.state.selection.main;
        const anchor = mapSelection(source, result.source, selection.anchor);
        const head = mapSelection(source, result.source, selection.head);
        state.view.dispatch({
          changes: { from: 0, to: state.view.state.doc.length, insert: result.source },
          selection: { anchor, head },
        });
        saveDraft(textarea);
        textarea.dispatchEvent(new CustomEvent("pattern:formatted", { bubbles: true }));
        void checkSource(state, result.source);
      }
      return result;
    })
    .catch(() => ({ status: "server_error" }))
    .finally(() => {
      if (state.formatKey === source) {
        state.formatKey = undefined;
        state.formatPromise = undefined;
      }
    });
  return state.formatPromise;
}

async function enhance(textarea, grammar, cm) {
  if (states.has(textarea)) return;
  const wrapper = document.createElement("div");
  wrapper.className = "pattern-editor";
  textarea.before(wrapper);
  wrapper.append(textarea);
  // The textarea stays in the form (htmx serializes it) but the CodeMirror
  // view is the editing surface; keep the two in sync below. `required` must
  // go: a hidden required control blocks form submission without a way to
  // focus it, and the submit gate validates emptiness instead.
  textarea.classList.add("pattern-editor-source");
  textarea.removeAttribute("required");
  textarea.setAttribute("tabindex", "-1");
  textarea.setAttribute("aria-hidden", "true");
  const statusAnchor = textarea.closest(".corpus-pattern-editor") ?? wrapper;
  const statusNode = document.createElement("p");
  statusNode.className = "pattern-editor-status";
  statusNode.id = `${textarea.id || "pattern"}-status`;
  statusAnchor.after(statusNode);
  const diagnosticNode = document.createElement("pre");
  diagnosticNode.className = "pattern-diagnostic-excerpt";
  diagnosticNode.id = `${textarea.id || "pattern"}-diagnostic`;
  statusNode.after(diagnosticNode);
  const parser = new grammar.Parser();
  parser.setLanguage(grammar.language);
  const state = {
    textarea,
    wrapper,
    status: statusNode,
    diagnosticNode,
    parser,
    query: grammar.query,
    navigationQuery: grammar.navigationQuery,
    cm,
    tree: null,
    positions: null,
    lastCheck: null,
    source: () => state.view.state.doc.toString(),
  };
  states.set(textarea, state);

  const view = new cm.EditorView({
    parent: wrapper,
    doc: textarea.value,
    extensions: [
      cm.history(),
      cm.closeBrackets(),
      cm.bracketMatching(),
      cm.highlightSelectionMatches(),
      cm.autocompletion({ override: [patternCompletionSource(state)] }),
      cm.linter(patternLintSource(state), { delay: 400 }),
      cm.lintGutter(),
      patternHoverTooltip(cm),
      patternSignatureHelp(cm),
      patternTreeSitter(state, cm),
      patternTheme(cm),
      cm.EditorView.lineWrapping,
      cm.placeholder(textarea.getAttribute("placeholder") ?? ""),
      cm.keymap.of([
        { key: "Shift-F8", run: cm.previousDiagnostic, preventDefault: true },
        // `run` returns true on every path, including the failures: a
        // "nowhere to go" message is a handled key, not a fall-through.
        { key: "F12", run: () => runNavigation(state, "definition"), preventDefault: true },
        { key: "F4", run: () => runNavigation(state, "next"), preventDefault: true },
        { key: "Shift-F4", run: () => runNavigation(state, "previous"), preventDefault: true },
        ...cm.closeBracketsKeymap,
        ...cm.defaultKeymap,
        ...cm.historyKeymap,
        ...cm.completionKeymap,
        ...cm.lintKeymap,
        ...cm.searchKeymap,
        cm.indentWithTab,
      ]),
      cm.EditorView.updateListener.of((update) => {
        if (!update.docChanged) return;
        textarea.value = update.state.doc.toString();
        saveDraft(textarea);
        state.diagnosticRange = null;
        diagnosticNode.replaceChildren();
      }),
    ],
  });
  state.view = view;
  textarea.setAttribute("aria-describedby", `${statusNode.id} ${diagnosticNode.id}`);
  // The <label for> targets the hidden source textarea; route its activation
  // and accessible name to the CodeMirror input instead.
  const label = textarea.id ? document.querySelector(`label[for="${textarea.id}"]`) : null;
  if (label) {
    view.contentDOM.setAttribute("aria-label", label.textContent.trim());
    label.addEventListener("click", () => view.focus());
  }

  // External writes (draft restore, tests, tooling) flow into the view.
  textarea.addEventListener("input", () => {
    const value = textarea.value;
    if (value !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
    }
  });
  view.contentDOM.addEventListener("blur", () => {
    void formatCurrent(textarea);
    void checkSource(state, state.source(), "blur").then(() => cm.forceLinting(view));
  });
}

async function initializeEditors() {
  const editors = [...document.querySelectorAll("textarea[data-pattern-editor]")];
  if (!editors.length) return;
  // Draft restore must run before the editor reads its initial document, and
  // persistence must survive even if enhancement fails entirely.
  for (const editor of editors) attachDraftPersistence(editor);
  try {
    const [grammar, cm] = await Promise.all([loadGrammar(), loadCodeMirror()]);
    await Promise.all(editors.map((editor) => enhance(editor, grammar, cm)));
  } catch (error) {
    for (const editor of editors) {
      editor.dataset.lxEnhancement = "unavailable";
      editor.dataset.lxEnhancementError = String(error);
    }
  }
}

if (typeof document !== "undefined") {
  document.addEventListener("DOMContentLoaded", initializeEditors);
  document.addEventListener("htmx:load", initializeEditors);
  document.addEventListener("htmx:confirm", async (event) => {
    const action = event.target.closest?.("[data-pattern-submit]");
    if (!action || releasing.has(action)) return;
    const textarea = document.getElementById(action.dataset.lxSubmit);
    if (!textarea) return;
    event.preventDefault();
    const state = states.get(textarea);
    if (state && !state.source().trim()) {
      status(state, "Pattern is empty.", true);
      return;
    }
    const captured = textarea.value;
    const result = await formatCurrent(textarea);
    if (!shouldPublish(captured, textarea.value) && result.source !== textarea.value) {
      if (state) status(state, "Pattern changed; submit again.", true);
      return;
    }
    if (state) await checkSource(state, textarea.value, "submit");
    releasing.add(action);
    try {
      // Query history observes htmx:beforeRequest as proof that this async
      // gate released a real request. Preserve that boundary when changing it.
      event.detail.issueRequest(true);
    } finally {
      queueMicrotask(() => releasing.delete(action));
    }
  });
}
