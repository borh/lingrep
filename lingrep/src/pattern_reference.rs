//! User-facing lingrep patterns language reference shared by CLI help and manual pages.

pub const PATTERN_REFERENCE: &str = r#"lingrep patterns LANGUAGE REFERENCE

A lingrep patterns file contains declarations. Definitions and policy statements end with
`.`. Metadata and `analyzer:` lines do not. Blocks close with `end`, and `#`
starts a line comment.

DECLARATIONS

  name := EXPR.                    emitting pattern
  pattern name := EXPR.            explicitly emitting pattern
  fragment name := EXPR.           reusable, non-emitting expression
  set name := {value, value}.      reusable literal set
  test name := TOKEN_TEST.         reusable token predicate
  lexicon name by [ATTR, ...] := {"term", "term"}.
  group name := META_KEY = "value".
  group name := META_KEY != "value".
  group name := META_KEY in {"value", "value"}.
  group name := META_KEY not in {"value", "value"}.

PATTERN EXPRESSIONS

  日本語                             one token whose `text` is the literal
  [TOKEN_TEST]                     one token satisfying the predicate
  []                               any one token
  @name                            a pattern or fragment reference
  EXPR EXPR                        sequence
  EXPR | EXPR                     alternation
  EXPR & EXPR                     intersection on the same span
  any_order((?<a> EXPR), ...)     distinct named one-token nodes in any order
  (EXPR)                           grouping
  EXPR?  EXPR*  EXPR+             optional and unbounded repetition
  EXPR{N}  EXPR{N,}  EXPR{N,M}    bounded repetition
  ^  $                             sentence start and sentence end
  (?<name> EXPR)                  named capture
  same_as($name)                  repeat the captured token span by `text`
  same_as($name, by=ATTR)         repeat it by another attribute
  preceded_by(EXPR)               require a match ending here
  not_preceded_by(EXPR)           require no match ending here
  followed_by(EXPR)               require a match starting here
  not_followed_by(EXPR)           require no match starting here

Quantifiers bind before sequence, sequence before intersection, and
intersection before alternation. Lookarounds are zero-width and sentence-local.
`any_order` admits implicit `[]*` gaps, publishes one longest result per
surface permutation, and requires two or more exact-width-one captures.

DEPENDENCY CONSTRAINTS

An emitting pattern may close with a `where` clause of comma-separated edge
constraints over its named captures:

  pattern sov := (?<s> [dep.rel=nsubj]) []* (?<o> [dep.rel=obj]) []*
                 (?<v> [upos=VERB])
    where $v -[nsubj]-> $s, $v -[obj]-> $o.

`$head -[label]-> $dependent` holds when the dependent token's `dep.rel`
matches the bracket and its `dep.head` designates the head token. Both
operands must be captures of statically exactly one token; a fragment may not
declare a clause, and an operand may not relate a capture to itself. The
clause filters candidate matches before publication filtering, so a shorter
passing match survives a longer failing one. A relation-bearing catalog
requires a corpus that carries `dep.head` and `dep.rel` (CoNLL-U or a
dependency-parsing analyzer) and is refused at binding otherwise. This form
matches basic edges only; `dep.enhanced` is not consulted.

Use `_` when only existence matters, not the dependent's identity:

  where $v -[obj|obj:*]-> _       some direct object exists
  where not $v -[obj|obj:*]-> _   no direct object exists
  where $v -[obj]-> [upos=PRON]   some direct object satisfies the token test
  where not $v -[obj]-> [upos=PRON]
                                      no direct object satisfies the complete test

The anonymous form emits once per body match even when several dependents
match. Captured edges remain positive. Every
anonymous constraint fails unless every token in the supplied sentence has a
usable scalar `dep.head` and `dep.rel` and every head is in range. Missing
dependency evidence therefore never proves absence. Neither `_` nor a target
predicate adds a capture or result field.

A bracketed dependent is likewise anonymous and constrained by the ordinary
one-token Boolean test. Named tests and sets are legal there. A missing value
does not witness the positive form. For `not`, every edge-compatible dependent
must supply every referenced predicate attribute; a missing required value
fails the constraint rather than proving absence.

The bracket holds one or more terms joined by `|`:

  -[*]->                          any present basic relation
  -[nsubj]->                      one exact label, subtype included
  -[obl:tmod]->                   an exact subtyped label
  -[obl:*]->                      any subtype of `obl`, but not bare `obl`
  -[nsubj|csubj]->                either label
  -[@SUBJ|expl]->                 a named `set`'s members, plus a literal

`-[*]->` matches any scalar `dep.rel` value present on the dependent token;
it does not match a missing attribute and cannot be combined with other terms.
`obl` and `obl:tmod` are different labels, so `-[obl:*]->` excludes the
unsubtyped form; write `-[obl|obl:*]->` for either. A `@name`
term names a `set` declaration and resolves where `[x in @S]` resolves; a
member that is not a well-formed relation label is a compile error. A term
repeated in one bracket, and two constraints sharing a dependent capture with
disjoint label sets, are both refused: a token carries exactly one `dep.rel`,
so the first adds nothing and the second could never match.

Two operands that no single match can bind together are refused as well: a
capture in an alternation branch binds only when that branch is taken, so
`((?<a> …) | (?<b> …)) where $a -[obj]-> $b` could never match. A repetition
is the exception, and it is legal — `((?<a> …) | (?<b> …))+` enters the
alternation once per iteration, so two iterations bind both.

Reporting which member matched requires no new syntax: the dependent
capture carries its own `dep.rel`, published via `key: [$d.dep.rel]`.

A multi-label bracket is one disjunction, not an independent count per label.
Its admitted candidates compete in the definition's per-start longest-match
selection, so keyed counts can depend on the bracket's sibling labels. Use one
definition per label when every row must be that label's own pattern count.
`matches --raw-matches` bypasses policy, but not this matcher selection;
`summarize` has no raw mode.

Enhanced dependency graph constraints put `enhanced` first inside the label
bracket. Either endpoint may reuse an exact-width-one surface capture or
introduce one named graph node with an ordinary one-token predicate:

  pattern elided_argument := (?<argument> [upos in {NOUN, PROPN, PRON}])
    where (?<predicate> [node.kind=empty])
            -[enhanced nsubj|obj|obl:* as $role]-> $argument.

The body must consume at least one surface token, and every connected enhanced
component must reach one of its captures. A graph node is identified by its
sentence-local CoNLL-U ID and is not assigned a surface span; `node.kind` is
`word` or `empty`, and `node.id` preserves the integer or decimal spelling.
Introduced nodes may use `node.kind`, `node.id`, `text`, `lemma`, `upos`, and
`xpos` in their predicates. Another constraint may reuse the introduced name.

`basic_head($dependent)` instead derives the non-root word selected by an
exact-width-one surface capture's scalar `dep.head`:

  pattern absent := (?<dependent> [dep.head!=0])
    where not basic_head($dependent) -[enhanced *]-> $dependent.

It is not a capture or graph binding and publishes no node identity. A missing,
malformed, root, or out-of-range head produces no endpoint. It may appear on
either side of a positive or negative enhanced relation.

An exact negative enhanced edge uses bound or derived endpoints:

  not $head -[enhanced obj|obl:*]-> $dependent

It holds when no matching edge connects those node identities. It introduces
no graph binding; `as $edge`, endpoint binders, `_`, and predicate targets are
positive-only. Positive graph assignments are completed before negative edges
are checked, so constraint order is not semantic. A matching parallel edge
falsifies the constraint; other parallel labels are irrelevant.

`as $edge` optionally names the exact enhanced edge. This matters when the
same endpoints carry parallel labels: an unnamed edge is existential, while a
named edge publishes one assignment per matching edge. A `key:` may project
`$node.node.kind`, `$node.node.id`, the graph-node lexical attributes above,
and `$edge.rel`. Published graph bindings use canonical node-name then
edge-name order, independent of constraint order. Complete enhanced observation
is mandatory; unavailable or
incomplete DEPS satisfies neither positive nor negative enhanced constraints.
Optional graph bindings, generalized anonymous enhanced absence, and dependency
paths are not supported.

TOKEN TESTS

  ATTR = value                    exact literal equality
  ATTR != value                   exact literal inequality
  ATTR in {value, ...}            membership in an inline set
  ATTR in @set                    membership in a named set
  ATTR not in SET                 non-membership
  ATTR ~ value                    literal substring search
  ATTR ~ /regex/                  regular-expression substring search
  ATTR !~ value                   negated substring search
  ATTR !~ /regex/                 negated regular-expression search
  ATTR has value                 set-valued attribute contains a value
  ATTR has any SET               contains any member of a set
  ATTR not has value             does not contain a value
  ATTR not has any SET           contains no member of a set
  @test                           a named token test
  !(TOKEN_TEST)  !@test           true Boolean complement
  TEST & TEST                    conjunction
  TEST | TEST                    disjunction

`has`, `has any`, `not has`, and `not has any` require a set-valued attribute;
`=`, `!=`, `in`, and `not in` require a scalar one. `~` and `!~` read either:
over a set they are existential over its entries, so `[conllu.misc ~ Inf=]`
holds when some entry starts that way.

Inside `[...]`, `!` binds before `&`, and `&` before `|`. A missing attribute
satisfies neither `=` nor `!=`, and neither `~` nor `!~`. Boolean `!` is a true
complement, so it does match a token that lacks the complemented attribute.
Here, SET means either `{value, ...}` or a named set such as `@name`.

ATTRIBUTES AND VALUES

Portable attributes are `text`, `lemma`, `norm`, `upos`, and `xpos`. UD
features use `feat.Name` or `feat.Name[layer]`; analyzer-specific attributes
use a lowercase dotted name such as `unidic.pos1`. Run `ling coverage`
to see which attributes are present in a corpus.

Token-test values are unquoted. Use `/.../` for a regular expression and
`\uXXXX` for punctuation or whitespace that cannot appear bare. `=`, `!=`,
`in`, and `not in` accept literals only; regex operands require `~` or `!~`.
A bare value ends at whitespace or `&`, but not at `|`: `[text=a|text=b]` is
refused as `PipeInBareValue`. Write `[text=a | text=b]` for disjunction, or
`[text=a\u007Cb]` when the literal value contains a pipe.
Metadata quotes its values and a token test never does, so `[text=\u0022x\u0022]`
would specify the literal `\u0022x\u0022`; it is refused as
`QuotedAttributeValue`.

METADATA

Metadata lines immediately before a definition annotate that definition:

  layer: "S2"                     string (strings are always quoted)
  examples: ["one", "two"]       list
  enabled: true                   boolean
  weight: 1.5                     number
  options: {mode: "strict"}       map

`none` is valid only on a top-level annotation to remove an inherited
default. There is no `null` value.

  defaults
    status: "experimental"
    key: [lemma]
  end

  schema
    pattern layer: string required in {"S0", "S1", "S2"}
    fragment role: string
    examples: list
  end

One `defaults` block and one `schema` block may appear before the first
definition. Schema types are `string`, `number`, `boolean`, and `list`.

ANALYZER REQUIREMENTS AND RESULT KEYS

  analyzer: {family: "vibrato", segmentation: ["input"]}
  analyzer: {family: "sudachi", resource: "core", split: "C",
             segmentation: ["input", "analyzer"]}

Repeat `analyzer:` to declare alternatives. A result key is an ordered list:

  key: [lemma, $head.norm, $span.first.text, $span.last.lemma,
        $span.span.norm]

Lexicons may also project `term` and `segments(ATTR)`. `key: none` removes a
default key.

COMPILER-OWNED METADATA

These annotations control publication and require no `schema` declaration:

  output_name: "published name"
  groups: ["group-name", "other-group"]
  priority_order: 2
  publish: ["conllu.misc"]
  tags: {source: "corpus", voice: {passive: "passive"},
         head_lemma: {$head: "lemma"}}
  transform: [{op: "drop_edge", scope: "$vb", edge: "trailing",
               drop: "[upos=AUX]", keep: "[lemma=だ]"},
              {op: "truncate_before", stop: "[upos=SCONJ]"},
              {op: "substitute", component: "last", attr: "lemma",
               table: {為る: "する"}},
              {op: "join"}]
  anchor: {name: "propositional_end", exclude: "trailing_fragment"}
  max_nesting: {depth: 0, witness: ["sentence|。"],
                divergence: ["sentence|（note）。"]}

`publish:` carries an attribute into published records without constraining
on it. A `transform:` entry may carry `scope: "$capture"`, confining that
operation to one capture's span; without it a span rewrite narrows the record
and drops the captures that fall outside. A `key:` projecting the written form
reads what a `substitute` wrote.

`anchor:` may also be a list of anchor maps. `max_nesting:` belongs on the
fragment named by an anchor's `exclude`. `render:` is reserved, but this
release has no registered renderer names; use `transform:` to build a unit.
`dictionary:` is compiler-owned but retired and always refused; declare
compatible analyzers with `analyzer:` instead. Compiler-owned keys must not be
declared in a `schema` block.

POLICY PROGRAMS

  policy
    resolve SUBJECT partition: META_KEY,
      rank: [longest, start, end, pattern].
    exclude SUBJECT with: OPERAND, relation: within, evidence: raw.
    require SUBJECT with: OPERAND, relation: in_sentence, evidence: current.
    detach SUBJECT with: OPERAND, relation: at_end, evidence: raw.
    admit SUBJECT terminal: {within: 5, anchor: sentence_end}.
  end

A SUBJECT is `all`, `patterns`, `@definition`, or `%group`. An OPERAND is
`@definition` or `%group`. Relations are `at_start`, `at_end`, `after`,
`before`, `within`, and `in_sentence`; evidence is `raw` or `current`.
Rank criteria are `longest`, `shortest`, `priority_order`, `start`, `end`, and
`pattern`; every rank list must end with `start, end, pattern`. `partition:` is optional.
`admit` accepts `terminal: {exact: true, anchor: NAME}` or
`terminal: {within: N, anchor: NAME}`. Policy statements run in source order.

Run `ling check --patterns FILE` to validate a complete catalog, or
`ling check --pattern 'EXPR'` to validate one inline expression."#;
