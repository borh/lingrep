# lingrep patterns language reference

This reference specifies the syntax and semantics of every construct that the
compiler accepts. Read it before writing or editing a `.lx` catalog under
`patterns/`, or when a pattern compiles but does not match intended tokens.

To change the syntax itself, see
[pattern-language-co-development.md](pattern-language-co-development.md); the
grammar lives in a separate repository and syntax changes require updates in
both. For worked examples of nearly everything below, see the tracked catalogs
under `patterns/`.

## Definitions

Bare definitions and explicit `pattern` declarations emit matches. A `fragment` is
referenceable but emits nothing:

```pattern
fragment ARU := [text in {ある, 有る}].
pattern result := ^ not_preceded_by([unidic.cform=連用形-一般]) [text=そう] @ARU $.
pattern contextual := [text=そう] followed_by([lemma=だ] $).
excluded := [text not in {必ず, 決して}].
pattern precedence := (a | b) c+.
pattern reduplicated := (?<unit> [upos=ADV]{1,2}) same_as($unit, by=unidic.kana).
```

lingrep patterns documents contain declarations, not standalone expressions.
References are resolved across the whole document and may point forward;
duplicate definitions, undefined references, and cycles are compile errors.

## Reserved names

`pattern`, `fragment`, `lexicon`, `set`, `test`, `group`, `defaults`, `schema`,
`analyzer`, and `labels` cannot be used as bare definition names: `schema := …`
parses as the block keyword and is a syntax error. An explicit declaration avoids
the collision: `pattern schema := …` is legal. `set`, `test`, `group`, `analyzer`,
and `labels` also cannot be metadata keys. `metadata` and `begin` are not
keywords and are legal names.

## Sets

A `set` binds a set of literals to an identifier so `in` and `not in` can
reference it: `set CONTENT := {動詞, 助動詞}.` then `[unidic.pos1 not in @CONTENT]`. A set is a value, not a
definition: it carries no metadata and emits nothing. It is scoped to the
accumulated catalog and resolves where a fragment reference does, so `@X` has
exactly one referent whichever kind `X` is.

## Similarity tests

`[unidic.orthbase near 研究 >= 0.6]` holds for a token whose `unidic.orthbase`
value has a word vector with a cosine similarity to the vector of `研究` of at
least 0.6. `not near` is its negation over present values, as `not in` is: it
holds for a value that is less similar or has no vector, and not for a token
that lacks the attribute.

The test is resolved when the catalog is compiled. The compiler reads a
word-vector file, collects the file's values that reach the threshold, and
compiles the test as `in` over that set; `not near` compiles as `not in`.
Matching is therefore exact and repeatable for one file, and nothing about
vectors is stored in a prepared corpus. Three consequences follow:

- A catalog with a `near` test compiles only with a word-vector file.
  `ling matches`, `ling summarize` and `ling check` take `--vectors PATH`; every
  command that compiles a catalog reads `LINGREP_VECTORS_PATH` otherwise. Without a file, or when the
  file has no vector for the word, the test is `WordVectorUnavailable`.
- Spell the file's values exactly as they appear in the tested attribute.
- The same threshold can select different sets from different files.
  Declare the file used to choose the thresholds with a header line, `vectors: {resource: "NAME", blake3: "DIGEST"}`. DIGEST
  is the BLAKE3 digest of the file as 64 lowercase hex digits and is what
  identifies it; NAME is a label for readers. Repeat the line to admit
  alternatives. Reading a file with another digest is reported as
  `warning[vectors_binding_mismatch]`, and a catalog file that has a `near`
  test and no `vectors:` line as `warning[vectors_undeclared]`, which prints
  the line to add. The run continues in both cases.

`ling serve` and `lingrep-web` take the same `--vectors PATH`, read
`LINGREP_VECTORS_PATH` otherwise, pass the file to the `ling` processes they
start, and stop at startup when it cannot be opened. `--vectors` on `ling
matches` or `ling summarize` also applies to the server that `--serve` starts
and to the catalog that `--link` checks. With a file, the Workbench reports the
two `vectors:` warnings in its status line, its Definition tab lists the values
each `near` test selects, and the Similar Words page ranks the values near one
word.

A CLI or Workbench share link for a compiled `near` query records the full
digest of the table it used. Opening the link on a server with different
table bytes, or with no table, restores the source and leaves execution paused.
An identical table at another path satisfies the link.

A file that cannot be read or parsed is `WordVectorTableUnreadable`, reported
at each `near` test with the file's line number.

The threshold is a number from 0 to 1 with at most six decimal places, and
white space must separate the word from `>=`, because `>` and `=` are legal in
an unquoted value. A similarity is rounded to six decimal places before it is
compared, so a word whose first vector is nonzero is near itself at threshold 1.
`ling neighbors --min-similarity T` lists exactly the values that
`near WORD >= T` selects, with their similarities.

The file is word2vec text: a value, one space, and the numbers of its vector
on each line, after an optional `count dimensions` first line. Components must
be finite; components whose squares overflow or underflow, and squared norms
that overflow, are rejected. All-zero rows never match. Query words use their
first row and require a nonzero vector. A candidate value matches when any of
its rows reaches the threshold.

A run's `run` record carries `word_vectors` when the catalog has a `near`
test: the file's path and digest, and for each distinct test its word,
threshold and the number of values it selected. A [word-vector bundle](cli-usage-guide.md#sets-of-similar-words-ling-neighbors) also supplies the vocabulary attribute, sources, selection and encoding in the run's `word_vectors.provenance`.

## Token tests

A `test` binds a token predicate to an identifier so `[…]` can reference it:
`test boundary := text=\u3000.` then `[!@boundary]`. Inside `[…]`, `&`, `|`,
`!` and parentheses form a complete Boolean algebra, binding `!` then `&` then
`|`. Like a set, a test is a value, not a definition: it carries no metadata and
emits nothing. It shares the one `@` namespace.

`!` is a true complement, which differs from `!=`. `!(unidic.pos1=動詞)` holds
for a token with no `unidic.pos1`; `unidic.pos1!=動詞` does not, because a token
lacking an attribute satisfies neither polarity of any operator. `!` takes only
`(…)` or `@name`, so a complement is always marked by parentheses or a
reference (`!unidic.pos1=動詞` is a syntax error).

A `[…]` is normalized to disjunctive normal form. Two budgets apply, each with
its own diagnostic: `TokenTestTooComplex` reports the branch count, and
`TokenTestExpansionTooLarge` reports the node count reached while substituting
`test` references, charged both per token test and across the whole catalog.

### Attribute modes

`=` and `!=` take literals only, `in` and `not in` take sets, and `~` and `!~`
take a literal (substring containment) or a regex (substring search).

String-set attributes use `has` and `not has` with one literal, or `has any` and
`not has any` with an inline or named set. The scalar modes are rejected for them
at schema binding, except `~` and `!~`, which read either kind. Over a set they
are existential, as `has` is: `~` holds when some entry satisfies it, `!~` when
none does. Use `~` to find a MISC entry by prefix, such as GiNZA's `Inf=`.

Regex values use Rust `regex` syntax, not PCRE, and match one attribute value; an
invalid regex is `InvalidRegex`. A regex operand with `=` or `!=` is a compile
error; use `~`, or anchor it as `~ /\A(?:…)\z/` for a whole-value match. A token
lacking the attribute satisfies neither polarity of any of these modes. `[]` is
the any-token predicate: it constrains nothing and so matches every token,
including one with no attributes set.

Combine attributes with `&`, `|`, `!` and parentheses. `;`, `:` and implicit
adjacency are syntax errors.

### `text` and `surface`

`text`, `surface`, `lemma`, `norm`, `upos` and `xpos` are the portable
attributes. `surface` is the exact source text of a token, or the FORM column
of a CoNLL-U word, and `text` is its match key, the surface folded in this
order:

1. variation selectors U+FE00..U+FE0F and U+E0100..U+E01EF removed;
2. half-width katakana and punctuation U+FF61..U+FF9F widened, with ﾞ and ﾟ
   composed onto the preceding kana, or as U+309B and U+309C when they compose
   with nothing;
3. U+FF5E FULLWIDTH TILDE as U+301C WAVE DASH;
4. canonical composition (NFC);
5. full-width ASCII U+FF01..U+FF5D as ASCII U+0021..U+007D. U+3000
   IDEOGRAPHIC SPACE is kept.

The compiler folds every `text` literal the same way: in `=`, `!=`, `in`,
`not in`, `~` and `!~`, in an inline or named set compared with `text`, and
in a lexicon member read `by [text]`. `[text=ｶﾞﾗｽ]`
and `[text=ガラス]` find the same tokens, `[text=ＮＨＫ]` finds `NHK`, and
`[text=長～い]` finds 長〜い. Spellings that fold to one key, as in
`{ＮＨＫ, NHK}`, are one set member. A literal that the fold changes is reported as
`FoldedLiteral`, showing both spellings. A lexicon match reports the member as
written. The fold applies to `text` alone: `lemma`, `norm` and provider
attributes are compared as written.

Analyzers and the CoNLL-U reader publish `surface` and the folded `text`.
A corpus prepared before 0.9 has unfolded `text` and no `surface`. Over such
an artifact, `[text=ＮＨＫ]` tests for the folded key `NHK`, which does not match
the stored value `ＮＨＫ`. `matches` and `summarize` report `warning[text_unfolded]`
and list the definitions that reference `text`. A `surface` test binds as
absent and the command reports `warning[attribute_unavailable]`. Rebuild the
corpus from source with `ling corpus build` to make both attributes available.

`surface` literals, including the members of a named set compared with
`surface`, are compared exactly, without the fold or NFC. Over the source 葛
U+E0100 城, `[text=葛城]` matches and `[surface=葛城]` does not.

A regex on `text` is compiled as written, because folding a
regex can change what it means: `[ｶﾞ]` is a class of two characters, ｶ and ﾞ,
and neither occurs in the folded key ガ. A regex containing a character that the
fold changes is reported as `RegexNotFolded`, with its folded spelling for
comparison; the folded `text` never contains that character. Write the regex
in the folded form, or test `surface` for the exact spelling. The check reads
the regex source, so an escape such as `\x{FF5E}` is not reported. A bare token
literal cannot contain a character that the fold changes.

Vibrato and Sudachi analyze the same folded text, except that they keep
full-width ASCII and keep a CJK compatibility ideograph inside an occurrence
of a dictionary surface that contains it, such as 臭氣 with U+FA5C.

### Unquoted values and escapes

There is no quoted-value form. An unquoted literal (in `=`, `!=`, `~`, `!~`,
`in`, `not in`, and `set` members) does not accept a bare `.`, `(`, or `)`, so
`[text=a.b]` and `set S := {a.b}.` are both `error[Syntax]`. Two escapes are
decoded in all of those positions: `\uXXXX` (exactly four hexadecimal digits) and
`\\` (one backslash). Every other `\x` is a compile error. `[text=a\u002eb]` and
`set S := {a\u002eb}.` both decode to the literal `a.b`. `patterns/double-wo.lx`
uses the same escape for U+3000, which is invisible in a diff. Regex operands are
not decoded: their backslashes belong to the regex engine.

Two spellings are rejected with their own diagnostics:

- An unquoted value is terminated by `&` and not by `|`, so `[text=a|text=b]` is
  one atom whose value is the literal `a|text=b`. The compiler rejects it
  (`PipeInBareValue`) and reports both recoveries: `\u007C` for a literal pipe, or
  spacing (`[text=a | text=b]`) for a disjunction.
- Metadata quotes every value and a token test quotes none, so `[text="の"]`
  would test for the three-character literal `"の"`. The compiler rejects it
  (`QuotedAttributeValue`) and reports the unquoted spelling and `\u0022` for a
  literal quote character. The quotes that a Japanese IME or word processor
  types are rejected the same way: `“…”` and `”…”`, `‘…’`, `「…」`, `『…』`,
  `＂…＂`, and `＇…＇`, each with its own `\uXXXX` escapes.

The compiler normalizes literal values, `set` and lexicon members, and bare
token literals to Unicode NFC, except a literal compared with `surface`. The
`text` key is composed even where the corpus keeps its source unchanged, so a
literal written as `カ` followed by U+3099 matches `ガ`. `NormalizedLiteral`
identifies the definition and both spellings. A named set reports it at the
member, unless the set is referenced and every reference compares it with
`surface`. A set rejects repeated decoded spellings, including a literal
repeated through an escape. Distinct spellings that share an NFC form,
such as `カ\u3099` and `ガ`, can belong to one `surface` set. A reference
comparing that set with `lemma` or `text` reports the collision. Distinct
NFC forms that fold to the same `text` key, such as `ＮＨＫ` and `NHK`,
still count as one member in a `text` comparison.

The compiler preserves regex source and reports `RegexNotNfc` for non-NFC
regexes. A class containing U+30AB and U+3099 as separate characters matches
either character. A class containing only their composition U+30AC matches
`ガ`. `matches` and `summarize` print these diagnostics as
`literal_normalized`, `regex_not_nfc`, `literal_folded` and
`regex_not_folded`. Catalogs whose source is already in NFC retain their
0.8.1 fingerprints. The `text` fold changes no fingerprint; it advances the
matcher semantics identity to `lingrep-core-matcher/v2`, so a count stored
before it is stale.

Syntax is ASCII. When a syntax error's span contains a full-width character
(U+FF01 to U+FF5E, such as `［` or `＝`) or the ideographic space U+3000, the
error carries a note at that character showing its ASCII replacement.

## Operators and precedence

Quantification binds more tightly than sequence, sequence more tightly than
expression intersection, and intersection more tightly than alternation.
Parentheses affect precedence only. A named group `(?<name> …)` is an observable
capture with a half-open span. A quantifier repeats its complete operand,
including a group or a referenced definition.

`same_as($name)` consumes a span equal to the capture's; the capture must be
1–8 tokens and definitely assigned, and one definition may reference one capture.
`by` is any token attribute name (default `text`).

`any_order((?<a> E), (?<b> E), ...)` binds two or more distinct, named,
statically one-token nodes in every surface permutation, with implicit unbounded
`[]*` gaps and no outer gap. Longest passing selection is independent per
permutation. An operand cannot reference a sibling's capture; captures
established before the construct remain available. The permutation count is
bounded by the expansion-work compile budget, not by a fixed token distance.

### Intersection (`&`)

Between expressions, `&` is intersection: `E1 & E2` matches where every operand
matches the same span. Inside `[…]` the same glyph is attribute conjunction. It
is commutative, associative and idempotent, and `A & B`'s matches are a subset
of `A`'s.

Express a length bound over a variable-length shape with `&` rather than
enumerating branches by hand: `( @nbgap* @bnd @gap* ) & []{1,8}` states one bound
where multiplication through the concatenation would give an alternative per
position in `patterns/double-wo.lx`.

The compiler rejects three constructs:

- An intersection whose operand widths are disjoint (`EmptyIntersection`).
  Overlapping widths are accepted even when no span matches both.
- A capture or a `same_as` inside any operand. A capture on one side would make
  `A & B` and `B & A` emit different records.
- `&` inside a lookaround body (`IntersectionInsideLookaround`). A lookaround
  inside `&` is allowed.

## Lookaround

`^` and `$` assert the exact boundaries of the supplied sentence-token slice.
`preceded_by(E)` succeeds when `E` ends at the current boundary; `followed_by(E)`
succeeds when `E` starts there. `not_preceded_by(E)` and `not_followed_by(E)`
complement those existential facts. All four are zero-width and accept regular
lingrep patterns expressions, including nullable ones and nested lookaround;
captures and `same_as` are forbidden inside.

Without a scope, a lookaround reads only the current sentence. With
`, within=document`, as in `followed_by([text=。] [upos=NOUN], within=document)`,
the body may also match tokens of the neighboring sentences of the same
document, and `^` and `$` inside it hold at the boundaries between sentences.
With `, within=KIND`, where `KIND` is a division kind such as `paragraph` or
`section`, the neighbors are the sentences of the same innermost division of
that kind; for a sentence in no such division, the lookaround reads only that
sentence. When the corpus observes no division of that kind, the lookaround
reads only the current sentence. `ling matches` and `ling summarize` then report
`warning[division_unavailable]`, except after a direct-input `ling matches` run
that `--limit` stopped; the browser and `ling explore` do not report it. A
scoped body must have a maximum width and contain no lookaround
(`ScopedLookaroundUnsupported`). Anchor exclusion fragments cannot contain scoped
lookarounds, including through references.

A window ends at the first sentence excluded by a scope clause, `--sample`,
`--sample-blocks` or an oversized-sentence skip. Results next to an excluded
sentence can therefore differ from an unrestricted run.
The browser's sample preview reads its lines as consecutive sentences of one
document with no divisions.

Retained execution windows, including empty sentences, count toward the estimated
tokenized byte limit. Execution fails if a required window exceeds that limit.

The four construct names are reserved as bare elements, but remain valid
bracketed values such as `[text=followed_by]`.

Nullable fragments are legal, but emitting patterns must consume a token
(`NullablePattern`) and no quantifier may repeat a nullable expression.

## Sentence spans

An emitting pattern can require its consumed tokens to lie inside one
sentence-local annotation. The kind is supplied by the corpus. With overlapping
annotations, any single annotation satisfying all fields is a witness.

```pattern
within_span: "bunsetsu"
in_chunk := [upos=NOUN]+.

within_span: {kind: "bunsetsu", label: "NP", position: "whole"}
noun_chunk := []+.

within_span: {kind: "bunsetsu", capture: "v", position: "head"}
chunk_head := (?<v> [upos=VERB]) []?.

span_dependency: {kind: "bunsetsu", dependent: "d", governor: "g"}
chunk_edge := (?<d> [upos=NOUN]) []* (?<g> [upos=VERB]).
```

`position` defaults to `"inside"`. Use `"whole"` for the exact annotated
interval and `"head"` for its single head token. `capture` limits the
condition to that named capture. For a dependency, each capture must be
nonempty and wholly inside its respective span, and the dependent span must
point directly to the governor span. A missing span, label, head, capture or
edge required by the query yields no witness. These annotations filter each
candidate before longest-match selection. Sentence anchors and lookaround
still use the sentence context. Token-level `where` constraints can be
combined with both annotations. A reference to a definition expands its token
expression. Its emission constraints apply when that definition emits.

A `defaults` block may supply `within_span` or `span_dependency`, which then
applies to every pattern and to no fragment or lexicon; `within_span: none`
removes it from one pattern. Either annotation written on a fragment or a
lexicon is a compile error (`InvalidPolicyValue`).

### Paths between annotated spans

Use `where span(layer, path)` to constrain a capture or connect several
captures. The body determines word order. The arrow points from governor to
dependent, as it does for token edges. For example, this requires a bunsetsu
dependency and a token-level subject relation between the two captured heads.

```pattern
subject_phrase := (?<noun> [upos=NOUN]) []* (?<verb> [upos=VERB])
  where span(bunsetsu, $verb.head@VP -> $noun.head@NP),
        $verb -[nsubj]-> $noun.
```

A bare `$capture` or `$capture.inside` must occupy a nonempty interval
inside the span. Use `.whole` for equality with its interval and `.head`
for its single head token. A variable-width capture may select a head if it
can match one token. `@label` tests the exact phrase label. Quote labels that
contain punctuation, as in `$noun.whole@"名詞-普通名詞-一般"`. Layer names can
be identifiers, dotted names such as `sudachi.c`, or quoted strings.

```pattern
modified_argument := (?<modifier> []) []* (?<noun> [upos=NOUN]) []* (?<verb> [upos=VERB])
  where span(bunsetsu, $verb -> $noun -> $modifier),
        span(luw, $noun.whole).
```

Both edges in a path pass through the same middle span. Separate clauses
choose independently: in `span(chunk, $a -> $b), span(chunk, $a -> $c)`,
two different overlapping chunks containing `$a` can witness the two edges.
A span-only clause works with the span layer alone, including on a corpus
without token dependency attributes. A sentence missing the requested layer
yields no matches for that definition and produces no warning of its own. When no sentence of a `ling matches` or `ling summarize` run carries the layer,
the run ends with `warning[span_kind_unavailable]`, which lists the definitions.
Other definitions can still match. If the catalog also contains token dependency
clauses, those require a corpus schema with `dep.head` and `dep.rel` before
matching starts.

The `within_span` shorthand and the `span_dependency` map, quoted path and
list annotations remain available. For example,
`span_dependency: "bunsetsu: $v.head -> $n.head"` has the same matching
behavior as `where span(bunsetsu, $v.head -> $n.head)`. A quoted path ignores
whitespace around the layer, `$`, `.`, `@` and `->`, as the native form does.
Its label is the text after `@` without surrounding whitespace; write a label
that begins or ends with a space as a quoted label in the native form.

## Dependency constraints

An emitting pattern may close with a `where` clause of comma-separated
dependency-edge constraints over its named captures:

```pattern
pattern sov := (?<s> [dep.rel=nsubj]) []* (?<o> [dep.rel=obj]) []*
               (?<v> [upos=VERB])
  where $v -[nsubj]-> $s, $v -[obj]-> $o.
```

When dependent identity is irrelevant, `_` tests existence without introducing a
capture: `$v -[obj|obj:*]-> _` holds when some matching direct dependent exists,
and `not $v -[obj|obj:*]-> _` holds when none exists. The anonymous form
publishes at most once per body match even when several tokens witness the edge.
Both polarities fail unless every token in the supplied sentence slice carries
usable scalar `dep.head` and `dep.rel` values with an in-range head, so an
incomplete parse never counts as evidence of absence. `_` binds no capture.

`not`, `_`, and `where` are contextual: `not := [].`, `_ := [].`, `where := [].`,
`[text=not]`, `[text=_]`, and `[text=where]` remain legal outside this position.

### A token test in dependent position

A bracketed token test in dependent position is a constrained anonymous form:
`$v -[obj|obj:*]-> [upos=PRON]` holds when some matching dependent satisfies the
ordinary one-token Boolean test. `not $v -[obj|obj:*]-> [upos=PRON]` holds when
none does, but only when every edge-compatible dependent supplies every attribute
referenced by the normalized test. A missing required value makes the negative
constraint fail; missing values on other dependents or labels are irrelevant. In
the positive form, a dependent missing a value is not a witness. Named tests and
sets keep the ordinary DNF and substitution budgets. Neither polarity assigns or
publishes the dependent, and both require the complete basic-dependency
observation described above.

### Counting direct dependents

A count after a dependent test selects the head once for the qualifying count.
Use `{2}` for exactly two, `{1,3}` for one through three, or `{2,}` for at
least two. Use `[]` to count every word with a matching relation.

```pattern
pattern two_nominals := (?<v> [upos=VERB])
  where $v -[*]-> [upos in {NOUN, PROPN, PRON}]{2}.
```

Each dependent word contributes at most one, including when it satisfies two
branches of the token test. A root attachment contributes no dependent.
A count is unknown when the sentence has no usable basic parse, or when an
edge-compatible word satisfies no alternative of the test and lacks a scalar
attribute the test reads: with `lemma` missing, `[lemma=本]` is undecided. A
word without a string-set attribute holds no member of it, so
`[feat.Case has Nom]` is false there and the count stays known. An unknown
count satisfies no range, including `{0}` and `{0,}`. Write a range directly
without `not`.

To measure the count for every head, bind it with `as` and use the
bare binding in `key:` for a histogram. Each verb contributes one record.
The JSON graph binding carries a number, including zero, or `null` when
the count is unknown. An unknown value leaves the key unfilled.
A count range before `as` also filters the head.

```pattern
key: [$nominals]
pattern nominal_load := (?<v> [upos=VERB])
  where $v -[*]-> [upos in {NOUN, PROPN, PRON}] as $nominals.
```

### The captured form

`$head -[labels]-> $dependent` holds when the dependent token's `dep.rel` is
matched by the bracket and its `dep.head` designates the head capture's token.
Both operands must be captures of statically exactly one token, except for the
binder form below. A fragment may not declare a clause, an operand may not
reference itself, and each violation has its own diagnostic.

`not $head -[labels]-> $dependent` holds when both captures bind and no matching
edge connects their final token assignments. It requires the complete
basic-dependency observation described above.

### A graph-node binder at one endpoint

Either endpoint of a positive basic edge may be a graph-node binder
`(?<name> [TOKEN_TEST])` instead of a capture reference. The binder binds one
word anywhere in the sentence and has no surface span; the other endpoint must
be a capture of statically exactly one token:

```pattern
key: [$h.lemma, $d.dep.rel, $d.lemma]
pattern edge := (?<d> []) where (?<h> []) -[*]-> $d.
```

Each binding is a separate match, so `edge` matches every basic edge of the
sentence once, regardless of the distance or order between head and dependent.
A token whose `dep.head` is `0`, missing, unparseable, or outside the sentence
contributes no edge, and the sentence root cannot be bound. A key reads a binder's `text`,
`lemma`, `upos`, `xpos`, `node.id` and `node.kind`. Only one endpoint of an edge may be a binder, and the form has no `not`
polarity. A later edge may reference an earlier binder as `$name`, so one clause
can bind every word of a connected subtree that is reached from one surface
capture. The binders of basic edges bind distinct words, and none binds the
token of a surface capture that a basic edge references.

Graph enumeration has a [sentence-scoped resource budget](fsm-development.md#supported-architecture).
A search that exceeds it fails with `graph_assignment_limit`; no truncated set
of assignments is returned as a successful result. Narrow the token or graph
predicates, or add ordering constraints between binders to reduce enumeration.

The bracket contains either `*` (any present basic `dep.rel`) or one or more
terms joined by `|`. The any-label form is written `-[*]->`, does not match a
missing `dep.rel`, and cannot be combined with other terms.

A term is a literal label (exact or subtype-qualified, such as `obl:tmod`) or a
`@name` reference to a `set` declaration, resolved exactly where `[x in @S]`
resolves, so `-[@SUBJ|expl]->` is legal. A literal may carry `*` in the subtype
position: `-[obl:*]->` matches any subtype of `obl`, and not bare `obl`. Write
`-[obl|obl:*]->` for either.

`key: [$d.dep.rel]` publishes the label that matched, because a token carries
exactly one `dep.rel`. The matched label is available this way only on a
dependent operand. On a head-only capture, `$v.dep.rel` is that token's own
relation to its head.

A multi-label bracket is one disjunctive constraint, not an independent count for
every member. Candidates admitted by its labels compete in the definition's
per-start longest-candidate selection, so a key's published count can change when
sibling labels are added to or removed from the bracket. For a keyness table
whose rows must be each label's own pattern count, generate one definition per
label. `matches --raw-matches` bypasses catalog policy; it does not expose
candidates lost at matcher selection, and `summarize` has no raw mode.

### Order constraints between `where` nodes

`$a < $b < $c` in a `where` clause requires each named node to precede the next
in sentence order. An operand is a capture of statically exactly one token or a
graph-node binder declared anywhere in the clause. When two binders have the
same test, relation and head, a pattern matches each pair of words twice, once
for each assignment of the words to the binders; an order constraint between
the binders keeps one of the two matches:

```pattern
key: [$v.lemma, $a.lemma, $b.lemma]
pattern two_objects := (?<v> [upos=VERB])
  where $v -[obj]-> (?<a> [upos=NOUN]), $v -[obj]-> (?<b> [upos=NOUN]), $a < $b.
```

An order constraint compares enhanced empty nodes in CoNLL-U order: `2.1` comes
after word `2` and before word `3`.

### Comparing attributes of bound words

`$a.lemma = $b.lemma` and `$a.surface != $b.surface` compare scalar values. The
operands may use different token attributes. Both must designate one word
and both values must be present for either operator to succeed.

```pattern
pattern spelling_contrast := (?<v> [upos=VERB])
  where $v -[nsubj]-> (?<s> []), $v -[obj]-> (?<o> []),
        $s.lemma = $o.lemma, $s.surface != $o.surface.
```

For additive label frequencies, consume the dependent and bind its governor.
The following query emits one observation per qualifying basic edge, even
when the same governor has several objects. Group its key to obtain separate
frequencies and sum them for the pooled total.

```pattern
key: [$d.dep.rel]
pattern argument_edge := (?<d> []) where (?<v> [upos=VERB]) -[nsubj|obj]-> $d.
```

### Static validation

The clause's own diagnostics:

- `DuplicateRelationLabel` reports a term repeated in one bracket, after set
  expansion.
- `InvalidRelationLabel` reports a `set` member that is not a well-formed label.
  A set holds arbitrary literals, so `set S := {動詞}.` is legal and `-[@S]->` is
  not.
- A `@name` that resolves to a definition or a `test` produces `UndefinedReference`.
  The diagnostic reports the kind found, as in the `in` case.
- `WhereContradictoryLabels`: two positive constraints share a dependent capture
  but have disjoint label sets; a token carries one `dep.rel`, so both cannot hold.
  Write `-[a|b]->` when either label is acceptable.
- `WhereContradictoryExistence`: a positive and negative anonymous constraint
  share a head and the negative label set covers the positive set. A positive
  captured edge counts as positive too. Mere overlap is satisfiable through a
  positive label outside the exclusion. Subtype coverage is semantic: `obl:tmod`
  is covered by `obl:*`, while bare `obl` is not. Two
  negative constraints remain legal. A captured negative constraint contradicts
  a positive captured edge only when both endpoints match and its labels cover
  the positive labels.
- `WhereExclusiveCaptures`: the clause's operands cannot all bind in one match; a
  capture in an alternation branch binds only when that branch is taken, so
  `((?<a> …) | (?<b> …)) where $a -[obj]-> $b` can never bind both. It applies to
  the whole clause, not per constraint, because `$c -[x]-> $a, $c -[y]-> $b`
  fails for the same reason. An alternation under a repeating quantifier
  (`+`, `*`, `{2,4}`) stays legal, because two iterations can take two
  branches; under `?` it is rejected.

### Ordering against publication

The clause filters candidate matches before keep-longest publication filtering
and before policy, so a shorter passing candidate survives a longer failing one.
An assignment that fails the clause does not hide a passing assignment reaching
the same position.

A catalog with a basic token dependency constraint requires `dep.head` and `dep.rel` at binding, so a
morphology-only corpus is rejected by name before matching. Only basic edges are
consulted; `dep.enhanced` is not.

Optional captured edges, negated labels and dependency paths are unsupported. `patterns/ud-dependency.lx` is the tracked exemplar; its
`verbal_argument` carries all three term forms in one bracket:
`-[@CORE_ARGUMENT|obl|obl:*]->`. `patterns/ud-object-realization.lx` exercises
anonymous existence. `patterns/double-wo-dep.lx` expresses `double-wo.lx`'s
candidate set over dependencies: one pattern over a shared skeleton fragment, two
label sets, and `key: [$n1.dep.rel, $n2.dep.rel]` to publish the pair. A fragment
cannot declare a `where` clause, but its captures are legal operands in the
including pattern's clause. Because `double-wo-dep.lx` requires `dep.head` and
`dep.rel`, `double-wo.lx` remains the catalog for morphology-only corpora.

## Occurrence measurements

Place `measure` after the pattern body and any `where` clause. Each named
value belongs to the occurrence the pattern selected. An empty selection
retains the occurrence. A graph pattern that emits several assignments
still emits one record for each assignment. Fragments cannot emit measures.

```pattern
test nominal := upos in {NOUN, PROPN, PRON}.
key: [$n]
verb := (?<v> [upos=VERB])
  measure count($v -[*]-> [@nominal]) as $n,
          mean(($v -[*]-> [@nominal]).length) as $distance,
          tree(word).depth($v) as $depth.
```

| Collection | Unit and scope |
| --- | --- |
| `match.words[TEST]` | Words inside the consumed occurrence, including words crossed by `skip:`. Use `count` with the usual token tests and named tests. |
| `$v -[LABELS]-> [TEST]` | Direct word links from the captured governor anywhere in its sentence. Count the links, or select `(…).length` for their absolute lengths. |
| `tree(LAYER).links.length` | Absolute surface-position differences in the sentence tree, excluding the root attachment. Adjacent units have length one. |
| `tree(LAYER).nodes.depth` | Distances to the sentence root, including the root at zero. `tree(LAYER).depth($v)` reads the depth of the unit containing the single captured word. |
| `tree(LAYER).boundaries.open_links` | Links crossing each unit boundary, including the two sentence edges with load zero. The maximum is zero for a singleton. |

Use `tree(word)` for basic word dependencies. For stored spans, name the layer
with an identifier, dotted name, or quoted string, as in `tree(custom.layer)`.
The spans must partition the sentence in surface order and their governor links
must lead to one root in the same layer. For bunsetsu, the root span must
contain an observed root word. Other span layers require at least two spans
connected by recorded governor links. Quoting `"word"` selects a stored layer
with that name.

Punctuation and skipped tokens retain their positions. `count`, `mean`, and
`max` reduce numeric collections. `count` also accepts the collection itself.

JSON match records publish a binding of kind `measure` with its name, numeric
value, and status. An observed value has status `ok`. The count of an observed
empty selection is zero; its mean and maximum have status `empty`. A missing
layer or capture gives `unavailable`, as does a test that is undecided on a word
under the rule for [dependent counts](#counting-direct-dependents). A partially
annotated or inconsistent tree gives `invalid_tree`. These three non-`ok`
statuses carry a `null` value. Using such a binding in `key:` leaves the record
unkeyed; `ling summarize` reports how many records had no key.

On a valid tree, `measure count($v -[L]-> [T])` counts the same links as
`where $v -[L]-> [T] as $n`. If every word has a usable basic dependency
but the edges fail tree validation, the measure returns `invalid_tree`.
The `where` count can still use the observed edges.

Measures are published in JSON match records only. When a pattern with
`interpret` entries has several readings of one occurrence, readings with the
same measures share a record and readings with different measures appear as
separate records. Any `where` dependency prerequisite still applies when the
pattern has one.

## Enhanced graph constraints

Enhanced graph constraints use `enhanced` inside the relation bracket and may
introduce graph-node binders at either endpoint:

```pattern
pattern propagated_argument := (?<argument> [upos=NOUN])
  where (?<predicate> [node.kind=empty])
          -[enhanced nsubj|obj|obl:* as $role]-> $argument.
```

A graph-node binder is contextual `(?<name> [TOKEN_TEST])`; it is exactly one
graph node wide but has no surface span. At least one exact-width-one surface
capture anchors every connected enhanced component. A bound node is a tagged
sentence-local word or decimal empty-node identity; root is not bindable.

`basic_head($dependent)` is a derived endpoint: it denotes the non-root word
selected by an exact-one-token surface capture's scalar `dep.head`. It is not a
capture or graph binding and publishes no identity. Missing, malformed, root, and
out-of-range heads produce no endpoint. Either polarity may use it; incomplete
enhanced observation proves neither presence nor absence.

`as $edge` optionally binds a name to the exact enhanced edge. Unnamed parallel edges are
existential, while named parallel edges publish distinct complete assignments.

`not ENDPOINT -[enhanced labels]-> ENDPOINT` is the only negative form: each
endpoint must be a named surface/graph binding or a derived `basic_head(...)`,
and it holds only when no matching edge connects those exact node identities. It
introduces no node or edge binding; `as`, endpoint binders, `_`, and predicate
targets are positive-only. Clause order does not affect the result. A matching
parallel edge falsifies the constraint; parallel edges outside its label set are
irrelevant.

Each assignment is one ordinary consuming `Match`, whose ordered `graph_bindings`
participates in equality, deduplication, and canonical order. The vector is
canonical by binding kind then name (nodes before edges), not by authored
constraint order; a `where` clause remains a conjunction.

Node keys can project `node.kind`, `node.id`, `text`, `lemma`, `upos`, or `xpos`.
A binding constrained to words by a positive basic edge or by `node.kind=word`
in every predicate branch can also project any scalar token attribute, such as
`$v.norm`; schema binding checks its value kind. A missing attribute makes the
record unkeyable. A named edge projects `$edge.rel`. Complete enhanced
observation is required for enhanced edges, so unavailable or incomplete DEPS
satisfies neither polarity.

Graph-only patterns, generalized anonymous enhanced absence, paths, graph
rewriting, and graph-node span/render projections are unsupported.

`patterns/ud-enhanced-argument.lx` is the introductory tracked exemplar;
`patterns/ud-enhanced-propagation.lx` exercises multi-edge propagation,
reconstruction, and exact anti-edges;
`patterns/ud-enhanced-consistency.lx` compares each non-root basic endpoint with
the complete enhanced graph and publishes compatible relative-reference and
empty-node motifs without declaring every absence an annotation error.

## Analytical interpretations

Use `interpret $capture as role {labels}` when alternative readings of the
same match matter. Give each capture one public role. Several captures may
supply the same role. Add `classify by role` to project that role's labels
onto the whole reading. Both declarations apply to the following pattern.

```pattern
interpret $possible as reading {voice: "potential"}
interpret $affected as reading {voice: "passive"}
classify by reading
rareru := (?<possible> [text=られる]) | (?<affected> [text=られる]).
```

On one token られる, this example emits one match with two interpretations.
A reading carries its own role occurrences, each with a public name,
a half-open token range in the original sentence, and string, boolean or
signed integer labels. Equal joint assignments appear once. Every occurrence
of a repeated role is retained. An absent optional role contributes no
occurrence. Classification collects the distinct values of each label on the
designated role in each reading. A reading without that role has no
classification. The emitting pattern's declaration supplies the public role
name, including for captures from a referenced fragment.

`ling matches --json` publishes the readings in the match's `interpretations`
array. Ordinary match counts still count that record once. To count each
interpretation by voice in this example, pipe the JSONL through:

```sh
jq -r 'select(.type == "match") | .data.interpretations[]? | .classification.voice[]?' |
  sort | uniq -c
```

Interpretation extraction supports token tests, named captures, sequences,
alternation and repetition. A `where` clause filters each reading using that reading's capture assignments
and the published graph bindings. Role declarations reference surface
captures. If extraction exceeds the per-sentence resource limit, the query
fails before publishing that sentence's results. The Workbench hit inspector shows the same roles and classifications.

## Metadata

A `key: value` annotation binds to the definition that follows it; a run of
annotations binds as a group, and comments and blank lines do not break a run.
One optional `defaults` block supplies file-level values, and one optional
`schema` block declares the catalog's own keys. Both must precede the first
annotation or definition.

```pattern
defaults
  status: "experimental"
end

schema
  pattern layer: string required in {"S0", "S1", "S2", "S3"}
  pattern examples: list required
  fragment role: string required in {"helper"}
  status: string required in {"experimental"}
  source: string
end

layer: "S2"
examples: ["確かなようである"]
source: "corpus-study@2026"
pattern ようである := [text=よう] [lemma=だ].
```

Values are typed by the grammar: `string`, `number`, `boolean`, `list`, and `map`.
Strings are always quoted; the only unquoted tokens are `true`, `false`, numbers,
and `none`. There is no `null`; omit the key instead. In a definition's
annotation run, `KEY: none` removes a file default, and `override KEY: none`
removes a bundle value.
Using `none` without an inherited value or inside a list or map is a compile error.

Without a `schema` block, any key is accepted. A `schema` block closes the
catalog's descriptive key set, so an undeclared key is rejected; an empty `schema`
block closes it to compiler-owned keys alone. A declaration specifies one of the
five value types; `map` admits any map, whatever its keys. Compiler-owned keys
(`output_name`, `groups`, `priority_order`, `key`, `skip`, `publish`,
`transform`, `tags`, `render`, `dictionary`, `anchor`, `within_span`,
`span_dependency`, and `max_nesting`) are admitted without declaration, and
declaring any of them in `schema` is a compile error. `examples` and
`counterexamples` are authored content, not compiler-owned, and stay declarable.
A `pattern` or `fragment` prefix scopes `required`, so a key can be obligatory
on patterns and absent from every fragment. Declared keys do not affect matching.

Diagnostics about a metadata value point at the key that carried it. A defect in
the definition (such as a fragment declaring a publication policy) points at the
definition.

`ling summarize --by-section --pattern-filter KEY=VALUE` selects published
patterns by an authored string metadata value. `--group-patterns-by KEY` sums
section counts by that value. The section-analysis page uses the same selection.
Missing or non-string values are omitted and reported. Aggregated sentence counts
sum member pattern–sentence occurrences rather than distinct sentences: two
patterns matching one sentence contribute two, even at the same span. These
options operate after publication policy and do not infer metadata from referenced
fragments. See [section summaries](cli-usage-guide.md#counting-per-section).

### Reusing annotations

A named bundle supplies descriptive metadata to the next definition that
selects it. Keep the member name and examples beside the individual pattern.

```pattern
bundle light_verb
  alternation: "light_verb"
  slot: "sahen_noun"
end

use @light_verb
member: "Nをする"
pattern lv_wo_suru := (?<noun> [upos=NOUN]) を する.
```

Declare a bundle before its first use in the same file. Bundle names have
their own namespace and must be unique in the file. Start an annotation run
with one `use @name`. File defaults apply first, followed by bundle values
and then local annotations. A local entry can repeat the same value. To change
or remove a bundle value, write `override slot: "other"` or
`override slot: none`. An override must reference a key that the selected bundle
supplies. Removal also suppresses any file default for that key. The schema
checks the effective metadata of each definition, including required keys after
removal.

Compiler-owned keys such as `key`, `skip`, and `output_name` must stay outside
bundles. Applying a bundle to a fragment annotates that fragment alone.

### `key:`

`key:` takes a nonempty, ordered list of projections. A bare attribute projects
the complete match and therefore requires the definition to have statically exact
width one. A capture projection can select its sole token or explicitly select
the first, last, or joined span:

```pattern
key: [unidic.pos1, unidic.pos2]
pattern distribution := [].

key: [$head.span.norm, $collocate.norm]
pattern relation := (?<head> []+) (?<collocate> []).
```

Authored field order is preserved. Projection is all-or-nothing: a missing
capture or attribute makes the record unkeyable rather than publishing a partial
tuple. A `defaults` block may supply `key:`; local `key: none` removes that
inherited key, and is an error when there is no default to remove.

A `key:` projecting the written form reads what a `substitute` wrote, and reads
it only there: `$vb.span.text` renders the substitution, `$vb.span.lemma` reports
what the tokens carry.

### `skip:`

`skip: @T` makes a pattern match as if the tokens satisfying the test `T` were
absent from the sentence:

```pattern
test dropped := text ~ /\A\W\z/.

skip: @dropped
pattern for_example := [text=for] [text=example].
```

`for “ example` matches, and the match span includes the quote. No element of
the pattern, `[]` included, matches a skipped token, and neither a match nor a
capture starts or ends on one; spans and captures report sentence positions.
Lookarounds, `^` and `$` step over skipped tokens, so `^ [text=well]` matches
after a leading quote. A `within=` lookaround reads neighboring sentences up
to the body's maximum width before skip rewriting. Skipped tokens count toward
that limit: under `skip: @dropped`, `preceded_by([text=yes], within=document)`
holds after a previous sentence that ends in `yes` but not after one that ends
in `yes ”`. `any_order` already admits any tokens between its operands. A
pattern without `skip:` matches the skipped tokens like any other token.

A `defaults` block may supply `skip:`, which then applies to every pattern and
to no fragment or lexicon; `skip: none` removes it from one pattern. A `skip:`
written on a fragment or a lexicon is a compile error (`SkipUnsupported`), as
is a pattern with `skip:` that references another definition or uses
`same_as`.

### `publish:`

`publish:` lists token attributes every record must carry, without testing them.
A run publishes only the catalog's tested attributes, `text`, and what its policy
projects; list any other attribute a consumer requires here instead of writing a
predicate for it. An attribute that the corpus does not supply draws the ordinary
`warning[attribute_unavailable]`. Like `key:`, it is declared per definition and
unioned across the catalog.

### `transform:`

A `transform:` entry may carry `scope: "$capture"`, which confines that one
operation to a named capture's span:

```pattern
key: [$n.lemma, $p.lemma, $vb.span.text]
transform: [
  {op: "drop_edge", scope: "$vb", edge: "trailing", drop: "[upos in {AUX, ADP}]"},
  {op: "substitute", scope: "$vb", component: "last", attr: "lemma", table: {為る: "する"}},
  {op: "join", scope: "$vb"}
]
pattern npv := (?<n> [upos=NOUN]) (?<p> [upos=ADP]) (?<vb> []+).
```

Without a scope, a span rewrite narrows the record span and discards every
capture outside that span. The discarded captures usually include the capture
that the transform was written for. A scoped step rewrites only its own
capture: the record span, the sibling captures, and any capture nested inside
are unchanged. A scope referencing a capture that the match did not bind has no
effect. `substitute`'s `component:` selects an edge (`first` or `last`) of the
scoped span; `span` and a `$capture` component are rejected.
`patterns/npv-collocation.lx` is the tracked exemplar.

A `substitute` with no later `join` is rejected unless a `key:` projects the
written form, because no output would read the substitution.

### Lexicons

Inline lexicons count authored terms however the analyzer segments them. `by` is
an authored-order union: each projection is scanned independently, and one
occurrence records every projection that recognized it.

```pattern
lexicon target_words by [text, norm] := {"大学院", "行う"}.

key: [term, segments(text)]
lexicon segmentation_counts by [text, norm] := {"大学院", "行う"}.
```

Without a key, `summarize` groups by the authored `term`, not observed surface.
`segments(text)` and `segments(norm)` preserve the actual token sequence as a
list. Prefixes and overlaps are retained, unlike ordinary-pattern containment
filtering. Missing or empty `by` values break that projection's run and are
reported by `coverage --json`; changing versioned `norm` can therefore change
lexical results and catalog identity.

Lexicons work with `matches`, `summarize`, `explore`, and JSON evidence.
`matches --tsv` rejects them because its fixed headerless row cannot carry the
variable-length `matched_by` list; `summarize --tsv` carries terms and typed
segment lists. `resolve` and `detach` may not
select one, and lexicons may not declare `transform:` or `render:`. The policy
subject `patterns` selects ordinary patterns and excludes lexicons.

## Label sets

A `labels:` entry declares, per attribute, the label set to which the file's
literals belong. It precedes every annotation and definition, and a file has at
most one.

```pattern
labels: {
  upos: "ud-v2",
  dep.rel: ["clearnlp", "ud-v2"],
  xpos: "penn",
  feat.VerbForm: {Fin, Ger, Inf, Part}
}

pattern passive := (?<v> [upos=VERB]) (?<s> []) where $v -[nsubjpass | nsubj:pass]-> $s.
```

A value is a named set, a list of named sets whose union applies, or an
explicit `{…}` set:

| Name | Attribute | Values |
|---|---|---|
| `ud-v2` | `upos` | the 17 UD v2 universal POS tags |
| `spacy` | `upos` | `ud-v2` and `SPACE`, which spaCy assigns to whitespace tokens |
| `ud-v2` | `dep.rel` | the 37 UD v2 universal relations; a subtype such as `nsubj:pass` is checked by its base |
| `clearnlp` | `dep.rel` | the relations of spaCy's English parsers, with the root as `ROOT` or `root` |
| `penn` | `xpos` | the Penn Treebank tags of spaCy's English taggers |

Every literal of a declared attribute in the file's definitions must be in its
set: values after `=`, `!=`, `in`, `not in`, `has` and `has any`, members of a
referenced `set`, and basic and enhanced `where` relation labels, where
`nsubj:*` is checked as `nsubj`. A literal outside the set is a compile error.
Regex and substring tests are not checked. The declaration applies to the file
it appears in, so a catalog built from several files can combine files written
for different schemes.

A `matches` or `summarize` run also counts, over the sentences it reads, the
tokens whose value of a declared attribute is outside its set, and reports them
after the run as `warning[label_outside_declared_set]`, listing up to eight
values with token counts. The warning does not change the exit status. Such
values show that the corpus uses labels outside the catalog's scheme, as when a
ClearNLP catalog runs over a UD parse and its `dobj` tests match nothing.
Over a prepared corpus, a run can read only the sentences that the posting
index selects for the catalog's `lemma` and `unidic.pos1` tests, and the counts
then cover those sentences alone.

## Policy block

One optional `policy … end` block per accumulated catalog holds an ordered
program that runs on candidate matches before publication. Order is semantic: a
`resolve` before an `exclude` contests a different field than one after it.

```pattern
policy
  resolve %heads rank: [longest, start, end, pattern].
  resolve all partition: layer, rank: [longest, start, end, pattern].
  exclude @toiu with: @QUOTE, relation: within, evidence: raw.
  require @sou with: @ARU, relation: after, evidence: raw.
  detach @clause with: @TRAILING, relation: at_end, evidence: raw.
  admit all terminal: {exact: true, anchor: propositional_end}.
end
```

### Subjects

A subject is `all`, `patterns`, `@definition`, or `%group`; `patterns` selects all
ordinary emitting patterns and excludes lexicons.

`%group` membership is the union of `groups:` lists and derived `group`
declarations, so a definition may be selected by several statements. A
declaration such as `group upper_layer := layer in {"S1", "S2", "S3"}.` derives
membership from a metadata predicate over the emitting definitions: `=`/`!=`
take one quoted string, `in`/`not in` a braced set, and every form is existential
and two-valued over presence, so a definition lacking the key or carrying a
non-string value is a member of no form. `partition:`, by contrast, rejects a
non-string value.

Listing a derived group in a literal `groups:` list is a compile error
(`LiteralEntryForDerivedGroup`); a second declaration of one name is
`DuplicateGroupDefinition`. Derivation considers emitting definitions only, so a
derived group never contains a fragment; it can contain a lexicon whose metadata
satisfies the predicate. Group names are their own `%` namespace, shared with
nothing `@` resolves.

A group has no effect until a statement references it. A declared group whose
predicate selects nothing still resolves and draws the empty-subject warning.

### Rank and partition

Enums are bare, never quoted. `rank` must end `start, end, pattern`, which makes
the order total.

`resolve` alone takes `partition: <key>`: the selected set is subdivided by the
string value of that metadata key on each record's definition, and the rank
contest runs independently per class. A selected definition lacking the key is
not selected by the statement; carrying it with a non-string value is a compile
error at that definition's key (`PartitionKeyNotString`), and `partition:` on any
other verb is `PartitionOnNonResolve`.

### Relations and evidence

`evidence` is `raw` or `current`, and a fragment operand forces `raw`, because a
fragment never emits.

`detach` takes `at_start` or `at_end` only, removes a region at that edge, and
cannot empty a record: the start bound is strict, so an operand covering the
whole span is not a trailing region.

Relations are evaluated on raw spans; `admit` alone reads the published end, so
its position among the statements matters.

Declaring a block changes what a catalog publishes and never what it matches. A
statement whose subject selects no publishable definition draws a warning, not an
error, and has no effect.

A run reports each statement by its own source text and line under `--progress
always`, including a statement that did nothing; `matches --trace-policy N`
explains at most `N` individual records, where `not selected` is a distinct
outcome from `kept` and a dropped record's account ends at the statement that
dropped it.

### Overlaps chosen per run

A run can override the catalog's `resolve` statements for overlaps between
different definitions. `matches` and `summarize`
take `--overlaps`, and the Workbench offers the same values as "Overlapping
matches":

| Value | The run executes |
|---|---|
| `catalog` (default) | the block as written |
| `keep` | the block without its `resolve` statements; a later statement with `evidence: current` then receives the records that `resolve` would have dropped |
| `longest` | the block, followed by `resolve patterns rank: [longest, start, end, pattern].` when the block declares no `resolve`; a block with a `resolve` runs as written |

All three retain per-start longest-candidate selection within each definition.
The `run` record
carries `overlaps` when the value is not `catalog`.

### Tracked catalogs with a policy block

`patterns/alternation.lx` declares three statements for distinctive collexeme
analysis. Its `resolve all partition: alternation` keeps a negation record from
deleting the conditional record it overlaps.

`patterns/sf-experimental.lx` declares six and no `resolve`, so overlapping
definitions are all counted and `--overlaps longest` selects one record per
overlapping set:

```pattern
policy
  exclude @ている with: @NEG, relation: after, evidence: raw.
  exclude @される with: %aspect, relation: after, evidence: current.
  detach %trailing_copula with: @DA, relation: at_end, evidence: raw.
  admit all terminal: {within: 5, anchor: propositional_end}.
  admit @という terminal: {exact: true, anchor: propositional_end}.
  require %s0_conditional with: %upper_layer, relation: in_sentence, evidence: current.
end
```

The sentence-final distance and the `という` rule are two `admit`s over one
anchor. `%upper_layer` is derived (`group upper_layer := layer in {"S1", "S2",
"S3"}.`), while `aspect`, `trailing_copula`, and `s0_conditional` are literal
`groups:` lists: they are curated memberships, not functions of any metadata key.

### Anchors

A declared `anchor:` is measured against by an `admit`. The name `sentence_end` is
reserved: it resolves to the sentence's raw token count without a declaration.
Declaring it is an error.
