# lingrep patterns language reference

The exact spelling and meaning of every construct the compiler accepts. Read this
before writing or editing a `.lx` catalog under `patterns/`, or when a pattern
compiles but does not match intended tokens.

For changing the syntax itself — the grammar lives in a second repository and a
change requires both — see
[pattern-language-co-development.md](pattern-language-co-development.md). For the
tracked catalogs, which are the worked examples of nearly everything below, see
`patterns/`.

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

`pattern`, `fragment`, `lexicon`, `set`, `test`, `group`, `defaults`, and `schema`
cannot name a bare definition: `schema := …` parses as the block keyword and is a
syntax error. An explicit declaration avoids the collision — `pattern schema := …`
is legal. `set`, `test`, and `group` also cannot be metadata keys. `metadata` and
`begin` are not keywords and are legal names.

## Sets

A `set` names literals so `in` and `not in` can take the name: `set CONTENT :=
{動詞, 助動詞}.` then `[unidic.pos1 not in @CONTENT]`. A set is a value, not a
definition: it carries no metadata and emits nothing. It is scoped to the
accumulated catalog and resolves where a fragment reference does, so `@X` has
exactly one referent whichever kind `X` is.

## Token tests

A `test` names a token predicate so `[…]` can reference it: `test boundary :=
text=\u3000.` then `[!@boundary]`. Inside `[…]`, `&`, `|`, `!` and parentheses
form a complete Boolean algebra, binding `!` then `&` then `|`. Like a set, a
test is a value, not a definition: it carries no metadata and emits nothing. It
shares the one `@` namespace.

`!` is a true complement, which differs from `!=`. `!(unidic.pos1=動詞)` holds
for a token with no `unidic.pos1`; `unidic.pos1!=動詞` does not, because a token
lacking an attribute satisfies neither polarity of any operator. `!` takes only
`(…)` or `@name` — `!unidic.pos1=動詞` is a syntax error — so a complement is
always marked by a bracket or a reference.

A `[…]` is normalized to disjunctive normal form. Two budgets apply, each with
its own diagnostic: `TokenTestTooComplex` names the branch count, and
`TokenTestExpansionTooLarge` names the node count reached while substituting
`test` references, charged both per token test and across the whole catalog.

### Attribute modes

`=` and `!=` take literals only, `in` and `not in` take sets, and `~` and `!~`
take a literal (substring containment) or a regex (substring search).

String-set attributes use `has` and `not has` with one literal, or `has any` and
`not has any` with an inline or named set. The scalar modes are refused for them
at schema binding, except `~` and `!~`, which read either kind. Over a set they
are existential, as `has` is: `~` holds when some entry satisfies it, `!~` when
none does. This finds a MISC entry by prefix, such as GiNZA's `Inf=`.

Regex values use Rust `regex` syntax, not PCRE, and match one attribute value; an
invalid regex is `InvalidRegex`. A regex operand with `=` or `!=` is a compile
error — use `~`, or anchor it as `~ /\A(?:…)\z/` for a whole-value match. A token
lacking the attribute satisfies neither polarity of any of these modes. `[]` is
the any-token predicate: it constrains nothing and so matches every token,
including one with no attributes set.

Combine attributes with `&`, `|`, `!` and parentheses. `;`, `:` and implicit
adjacency are syntax errors.

### Unquoted values and escapes

There is no quoted-value form. An unquoted literal — in `=`, `!=`, `~`, `!~`,
`in`, `not in`, and a `set` member — does not accept a bare `.`, `(` or `)`, so
`[text=a.b]` and `set S := {a.b}.` are both `error[Syntax]`. Two escapes are
decoded in all of those positions: `\uXXXX` (exactly four hexadecimal digits) and
`\\` (one backslash). Every other `\x` is a compile error. `[text=a\u002eb]` and
`set S := {a\u002eb}.` both decode to the literal `a.b`. `patterns/double-wo.lx`
uses the same escape for U+3000, which is invisible in a diff. Regex operands are
not decoded: their backslashes belong to the regex engine.

Two spellings are refused with their own diagnostics:

- An unquoted value is terminated by `&` and not by `|`, so `[text=a|text=b]` is
  one atom whose value is the literal `a|text=b`. The compiler refuses it
  (`PipeInBareValue`) and names both recoveries: `\u007C` for a literal pipe, or
  spacing (`[text=a | text=b]`) for a disjunction.
- Metadata quotes every value and a token test quotes none, so `[text="の"]`
  would test for the three-character literal `"の"`. The compiler refuses it
  (`QuotedAttributeValue`), naming the unquoted spelling and `\u0022` for a value
  whose quotes are real.

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

Three things are refused:

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
complement those existential facts. All four are sentence-local, zero-width, and
accept regular lingrep patterns expressions, including nullable ones and nested
lookaround; captures and `same_as` are forbidden inside.

The four construct names are reserved as bare elements, but remain valid
bracketed values such as `[text=followed_by]`.

Nullable fragments are legal, but emitting patterns must consume a token
(`NullablePattern`) and no quantifier may repeat a nullable expression.

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
incomplete parse never counts as evidence of absence. Captured edges are
positive only. `_` binds no capture.

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

### The captured form

`$head -[labels]-> $dependent` holds when the dependent token's `dep.rel` is
matched by the bracket and its `dep.head` designates the head capture's token.
Both operands must be captures of statically exactly one token. A fragment may
not declare a clause, an operand may not name itself, and each refusal has its
own diagnostic.

The bracket is either `*`, meaning any present basic `dep.rel`, or one or more
terms joined by `|`. The any-label form is written `-[*]->`, does not match a
missing `dep.rel`, and cannot be combined with other terms.

A term is a literal label — exact and subtype-included, `obl:tmod` — or a `@name`
reference to a `set` declaration, resolved exactly where `[x in @S]` resolves, so
`-[@SUBJ|expl]->` is legal. A literal may carry `*` in the subtype position:
`-[obl:*]->` matches any subtype of `obl`, and not bare `obl`. Write
`-[obl|obl:*]->` for either.

`key: [$d.dep.rel]` publishes the label that matched, because a token carries
exactly one `dep.rel`. This holds for a dependent operand only — `$v.dep.rel` on
a head-only capture is that token's own relation to its head.

A multi-label bracket is one disjunctive constraint, not an independent count for
every member. Candidates admitted by its labels compete in the definition's
per-start longest-candidate selection, so a key's published count can change when
sibling labels are added to or removed from the bracket. For a keyness table
whose rows must be each label's own pattern count, generate one definition per
label. `matches --raw-matches` bypasses catalog policy; it does not expose
candidates lost at matcher selection, and `summarize` has no raw mode.

### Static refusals

The clause's own diagnostics:

- `DuplicateRelationLabel` names a term repeated in one bracket, after set
  expansion.
- `InvalidRelationLabel` names a `set` member that is not a well-formed label.
  A set names arbitrary literals, so `set S := {動詞}.` is legal and `-[@S]->` is
  not.
- A `@name` that resolves to a definition or a `test` is `UndefinedReference`,
  naming the kind found, as in the `in` case.
- `WhereContradictoryLabels` refuses two constraints sharing a dependent capture
  with disjoint label sets: a token carries one `dep.rel`, so both can never hold.
  Write `-[a|b]->` when either label is acceptable.
- `WhereContradictoryExistence` refuses a positive and negative anonymous
  constraint on the same head only when the negative label set covers the
  positive set. A positive captured edge counts as positive too. Mere overlap is
  satisfiable through a positive label outside the exclusion. Subtype coverage is
  semantic — `obl:tmod` is covered by `obl:*`, while bare `obl` is not. Two
  negative constraints remain legal.
- `WhereExclusiveCaptures` refuses a clause whose operands can never both bind: a
  capture in an alternation branch binds only when that branch is taken, so
  `((?<a> …) | (?<b> …)) where $a -[obj]-> $b` can never bind both. It applies to
  the whole clause, not per constraint, because `$c -[x]-> $a, $c -[y]-> $b`
  fails for the same reason. An alternation under a quantifier that can repeat
  it — `+`, `*`, `{2,4}` — stays legal, because two iterations can take two
  branches; under `?` it is refused.

### Ordering against publication

The clause filters candidate matches before keep-longest publication filtering —
a shorter passing candidate survives a longer failing one — and before policy.
An assignment that fails the clause does not hide a passing assignment reaching
the same position.

A catalog with a `where` clause requires `dep.head` and `dep.rel` at binding, so a
morphology-only corpus is refused by name before matching. Only basic edges are
consulted; `dep.enhanced` is not.

Optional captured edges, negated labels, ordering predicates and dependency paths
are unsupported. `patterns/ud-dependency.lx` is the tracked exemplar; its
`verbal_argument` carries all three term forms in one bracket —
`-[@CORE_ARGUMENT|obl|obl:*]->`. `patterns/ud-object-realization.lx` exercises
anonymous existence. `patterns/double-wo-dep.lx` expresses `double-wo.lx`'s
candidate set over dependencies: one pattern over a shared skeleton fragment, two
label sets, and `key: [$n1.dep.rel, $n2.dep.rel]` to publish the pair. A fragment
cannot declare a `where` clause, but its captures are legal operands in the
including pattern's clause. Because `double-wo-dep.lx` requires `dep.head` and
`dep.rel`, `double-wo.lx` remains the catalog for morphology-only corpora.

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

`as $edge` optionally names the exact enhanced edge. Unnamed parallel edges are
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

Node keys can project `node.kind`, `node.id`, `text`, `lemma`, `upos`, or `xpos`;
a named edge projects `$edge.rel`. Complete enhanced observation is required, so
unavailable or incomplete DEPS satisfies neither polarity.

Graph-only patterns, generalized anonymous enhanced absence, paths, graph
rewriting, and graph-node span/render projections are unsupported.

`patterns/ud-enhanced-argument.lx` is the introductory tracked exemplar;
`patterns/ud-enhanced-propagation.lx` exercises multi-edge propagation,
reconstruction, and exact anti-edges;
`patterns/ud-enhanced-consistency.lx` compares each non-root basic endpoint with
the complete enhanced graph and publishes compatible relative-reference and
empty-node motifs without declaring every absence an annotation error.

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
and `none`. There is no `null`; omit the key instead. `key: none` in an
annotation run unsets the file default for that definition; it is the only
spelling of `none`, and using it without a default, or anywhere but an
annotation's top level, is a compile error.

Without a `schema` block, any key is accepted. A `schema` block closes the
catalog's descriptive key set, so an undeclared key is refused; an empty `schema`
block closes it to compiler-owned keys alone. Compiler-owned keys —
`output_name`, `groups`, `priority_order`, `key`, `publish`, `transform`, `tags`,
`render`, `dictionary`, `anchor` and `max_nesting` — are admitted without
declaration and are an error to declare. `examples` and `counterexamples` are
authored content, not compiler-owned, and stay declarable. A `pattern` or
`fragment` prefix scopes `required`, so a key can be obligatory on patterns and
absent from every fragment. Declared keys do not affect matching.

Diagnostics about a metadata value point at the key that carried it. A defect in
the definition — a fragment declaring publication policy at all — points at the
definition.

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

### `publish:`

`publish:` lists token attributes every record must carry, without testing them.
A run publishes only the catalog's tested attributes, `text`, and what its policy
projects; name any other attribute a consumer requires here instead of writing a
predicate for it. An attribute the corpus does not supply draws the ordinary
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

Unscoped, a span rewrite narrows the record, and narrowing discards every capture
that falls outside it — usually including the capture the transform was written
for. A scoped step rewrites only its own capture: the record span, the sibling
captures, and any capture nested inside are unchanged. A scope naming a capture
the match did not bind has no effect. `substitute`'s `component:` names an edge —
`first` or `last` — of whichever span the scope selected; `span` and a `$capture`
component are refused. `patterns/npv-collocation.lx` is the tracked exemplar.

A `substitute` with no later `join` is refused unless a `key:` projects the
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
`matches --tsv` refuses them because its fixed headerless row cannot carry the
variable-length `matched_by` list; `summarize --tsv` carries terms and typed
segment lists. `sentence-final` refuses lexicons. `resolve` and `detach` may not
select one, and lexicons may not declare `transform:` or `render:`. The policy
subject `patterns` selects ordinary patterns and excludes lexicons.

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
declaration — `group upper_layer := layer in {"S1", "S2", "S3"}.` — derives
membership from a metadata predicate over the emitting definitions: `=`/`!=`
take one quoted string, `in`/`not in` a braced set, and every form is existential
and two-valued over presence, so a definition lacking the key or carrying a
non-string value is a member of no form. `partition:`, by contrast, refuses a
non-string value.

Naming a derived group in a literal `groups:` list is a compile error
(`LiteralEntryForDerivedGroup`); a second declaration of one name is
`DuplicateGroupDefinition`. Derivation considers emitting definitions only, so a
derived group never contains a fragment; it can contain a lexicon whose metadata
satisfies the predicate. Group names are their own `%` namespace, shared with
nothing `@` resolves.

A group has no effect until a statement names it. A declared group whose
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

### Tracked catalogs with a policy block

`patterns/alternation.lx` declares three statements for distinctive collexeme
analysis. Its `resolve all partition: alternation` keeps a negation record from
deleting the conditional record it overlaps.

`patterns/sf-experimental.lx` declares eight:

```pattern
policy
  resolve all partition: layer, rank: [longest, start, end, pattern].
  exclude @ている with: @NEG, relation: after, evidence: raw.
  exclude @される with: %aspect, relation: after, evidence: current.
  resolve patterns rank: [longest, priority_order, start, end, pattern].
  detach %trailing_copula with: @DA, relation: at_end, evidence: raw.
  admit all terminal: {within: 5, anchor: propositional_end}.
  admit @という terminal: {exact: true, anchor: propositional_end}.
  require %s0_conditional with: %upper_layer, relation: in_sentence, evidence: current.
end
```

The sentence-final distance and the `という` rule are two `admit`s over one
anchor. `%upper_layer` is derived — `group upper_layer := layer in {"S1", "S2",
"S3"}.` — while `aspect`, `trailing_copula` and `s0_conditional` are literal
`groups:` lists: they are curated memberships, not functions of any metadata key.

### Anchors

A declared `anchor:` is measured against by an `admit`. The name `sentence_end` is
reserved: it resolves to the sentence's raw token count without a declaration.
Declaring it is an error.
