# lingrep patterns compiler limitations

What the pattern language cannot express, and the budgets compilation enforces.
[pattern-language-reference.md](pattern-language-reference.md) owns the syntax.

## Regular context only

lingrep patterns supports exact sentence anchors (`^`, `$`) and four non-consuming lookaround
assertions: `preceded_by(E)`, `not_preceded_by(E)`, `followed_by(E)`, and
`not_followed_by(E)`. Their bodies are regular lingrep patterns expressions, not single
predicates — `preceded_by([upos=NOUN] [upos=VERB])` and `preceded_by([]{2,3})`
both compile — and may nest lookaround to any depth. Captures and `same_as` are
rejected inside a body. Because the bodies are regular, no unbounded
backreference or recursive context is expressible.

## Strict source acceptance

The compiler accepts only the CST shape produced by the pinned
`tree-sitter-lingrep` revision. Tree-sitter recovery nodes (`ERROR` or `MISSING`)
are compile errors; the compiler does not guess at recovered meaning. Every
definition requires its terminal period.

There is no quoted-value form; a literal that contains `.`, `(`, `)` or `|` is
written with an escape, as
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

## Resource limits

Compilation enforces deterministic budgets for source bytes, definitions,
expression nodes, depth, metadata, regexes, reference edges, expansion work,
generated owned bytes, states, and transitions. Exceeding any budget returns
`LimitExceeded` without a panic or partial matcher. CLI and web callers use the
defaults and cannot raise them.

Two further budgets are specific to Boolean token tests and return their own
codes rather than `LimitExceeded`: `TokenTestTooComplex` for the
disjunctive-normal-form branch count, and `TokenTestExpansionTooLarge` for the
node count reached while substituting `test` references — charged both per
`[…]` and across the whole catalog. See
[matcher architecture](fsm-development.md).
