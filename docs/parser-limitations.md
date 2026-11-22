# lingrep patterns compiler limitations

This document lists what the pattern language cannot express and the budgets
that compilation enforces.
[pattern-language-reference.md](pattern-language-reference.md) defines the syntax.

## Regular context only

lingrep patterns supports exact sentence anchors (`^`, `$`) and four non-consuming lookaround
assertions: `preceded_by(E)`, `not_preceded_by(E)`, `followed_by(E)`, and
`not_followed_by(E)`. Their bodies accept regular lingrep expressions,
including nested lookaround within the compiler's depth budget. Both
`preceded_by([upos=NOUN] [upos=VERB])` and `preceded_by([]{2,3})` compile.
Captures and `same_as` are rejected inside a body. Because the bodies are
regular, no unbounded backreference or recursive context is expressible.

## Strict source acceptance

The compiler accepts only the CST shape produced by the pinned
`tree-sitter-lingrep` revision. Tree-sitter recovery nodes (`ERROR` or `MISSING`)
are compile errors. End every definition with a period.

Use unquoted values in token tests. Escape `.`, `(`, `)` or `|` in a literal, as
[Unquoted values and escapes](pattern-language-reference.md#unquoted-values-and-escapes)
describes.

## Unsupported constructs

- `same_as` compares exact attribute projections such as `text`, `lemma`, and
  `unidic.kana` over one definitely assigned 1–8-token capture per definition.
  Transformed equality and unbounded backreferences are unsupported.
- Regex values use Rust `regex` syntax, not PCRE. Lookaround and backreferences
  are unsupported by that engine and produce `InvalidRegex`. `~ /re/` is an
  unanchored search; for a whole-value match write `~ /\A(?:re)\z/`.
- Zero-length top-level matches are unsupported.
- [Scoped lookaround](pattern-language-reference.md#lookaround) can test
  neighboring sentences. Use an
  [external join](cli-usage-guide.md#joining-annotated-relations-across-sentences)
  to return both endpoints of an annotated relation across sentences.
- Span selectors test sentence-local intervals, labels, heads, and fixed paths
  of recorded governor links. Immediate dominance, sisterhood, phrase
  precedence, c-command, unary chains, and unbounded governor paths require
  external analysis.

## Resource limits

Compilation enforces deterministic budgets for source bytes, definitions,
expression nodes, depth, metadata, regexes, reference edges, expansion work,
generated owned bytes, states, and transitions. Exceeding any budget returns
`LimitExceeded` without a panic or partial matcher. CLI and web callers use the
defaults and cannot raise them.

Two further budgets are specific to Boolean token tests and return their own
codes rather than `LimitExceeded`: `TokenTestTooComplex` for the
disjunctive-normal-form branch count, and `TokenTestExpansionTooLarge` for the
node count reached while substituting `test` references (charged both per `[…]`
and across the whole catalog). See
[matcher architecture](fsm-development.md).
