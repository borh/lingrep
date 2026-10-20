# Matcher development guide

Where the compiler and matcher live, the invariants a change must keep, and the
procedure for changing matcher behavior. The pattern syntax itself is specified
in [pattern-language-reference.md](pattern-language-reference.md).

## Supported architecture

The public matcher has one atomic compiler/runtime boundary:

```text
lingrep patterns source → compile_catalog → CompileOutput → CompiledCatalog → capability dispatch
```

`compile_catalog` strictly lowers one complete document, binds its definition graph,
checks compile budgets, and constructs the NFA. Success returns the complete
matcher, metadata catalog, warnings, and compiler statistics. Failure returns
structured diagnostics and no partial matcher. `FsmBuilder` is the incremental
form for registering exactly one definition per call; it delays binding until
the consuming `build()`, so references can point to definitions registered
later. `CompiledCatalog` is immutable and has no runtime engine or scheduling
flags.

Results are sorted in the canonical order `(start, end, pattern)`. `Match`
equality covers every field: `pattern`, `start`, `end`, captures, graph
bindings, and lexical identity.

```rust
use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

let output = compile_catalog(
    PatternSource::named("verbs.lx", "verb := [upos=VERB]."),
    CompileLimits::default(),
)?;
let matcher = output.matcher;

let matches = matcher.find_matches(&tokens);
# Ok::<(), Box<dyn std::error::Error>>(())
```

For a body supplied through an inline CLI option, call `normalize_inline_pattern`
and compile the resulting document with `compile_catalog`. Supply the caller's real
source label to `PatternSource::named` so diagnostics identify the input boundary.

`CompiledCatalog` also contains a lexical recognizer for `lexicon` declarations.
It retains prefixes and overlaps, deduplicates by `(definition, start, end,
term)`, and merges its sorted records with the ordinary stream. Its per-worker
scratch is bounded by the largest sentence that worker has seen.

## lingrep patterns compiler contract

- Diagnostic codes, severity, source label, byte span, line/column, and related
  locations are the machine-facing protocol. Message text is presentation, and
  diagnostics do not retain submitted source or metadata.
- Definition syntax never compiles to an empty matcher. Empty or comment-only
  input is the only valid empty document.
- A `fragment` creates neither a final state nor a runtime-catalog entry; `set`
  and `test` declarations are values, not definitions.
- Lookaround bodies compile to direction-specialized Boolean fact programs
  evaluated at most once per sentence; they add no capture or history state to
  the NFA. Attributes used only by those programs are part of
  `required_attributes()`.
- Graph bindings participate in equality, ordering, deduplication, key
  projection, query/v2, and browser inspection.
- Ordered `key:` fields are carried as typed `(name, value)` values through
  policy and aggregation. Query/v1 represents summary groups as `surface`,
  `unit`, authored lexical `term`, or `key`; summarize-TSV/v1 renders a key group
  as compact JSON.

### Compile budgets

The default and hard-ceiling values are `SUPPORTED_DEFAULTS` and `HARD_CEILING`
in `lingrep-core/src/fsm/compiler/limits.rs`. Callers may tighten defaults with
`CompileLimits::tightened`; they cannot raise them. CLI and web surfaces always
use the defaults and expose no override flag. Generated owned bytes charge
compatibility predicates, consuming-transition payload fan-out, catalog names,
and deep-cloned catalog metadata before each allocation; this counter is not
RSS.

Boolean token tests have two independent budgets with separate diagnostics.
`TokenTestTooComplex` bounds the disjunctive-normal-form branch count a `[…]`
distributes to. `TokenTestExpansionTooLarge` bounds the node count reached while
substituting `test` references before distribution — `test a := @b & @b.`
doubles the resolved tree at every level of a chain while still distributing to
one branch, so the branch budget cannot detect it. Substitution nodes are charged
twice: per `[…]`, which bounds one test's shape and points the diagnostic at the
offending chain, and against a catalog-wide counter seeded from the
expansion-work budget, which bounds a chain referenced by many token tests.

## Repetition semantics

Quantifiers repeat the complete bound operand, whether it is a token, group, or
definition reference. Returned ends are exclusive and stop after the last
satisfying token. Top-level zero-width paths are not public matches. A test-only
bound-expression interpreter, `lingrep-core/src/fsm/compiler/oracle.rs`, is the
independent semantic oracle.

## Which executor recognizes a definition

`ExecutionCapability` selects the recognizer, and
`ExecutionCapability::evaluates_whole_definition` is the single answer to whether
the tree evaluator recognizes the whole definition. `compiler::nfa` routes such a
definition around the recognition NFA, and `matcher::compiled` builds the plan
that recognizes it instead; a capability handled on one side and not the other
yields a pattern that is in the catalog and matches nothing.

| Capability | Recognized by |
|---|---|
| `Plain` | dense NFA |
| `Captures` | dense NFA; a program replays the accepted span to recover bindings |
| `SpanEquality`, `Intersection`, `AnyOrder`, `Relations` | `Program` (tree evaluator) |

A Boolean token test (`&`, `|`, `!`, parentheses, and a named `test`
reference inside `[…]`) adds no capability. It is normalized to disjunctive
normal form at bind time, before capability selection: a multi-branch DNF
becomes a `BoundExpr::Alternation` of `BoundExpr::Token`s, and a single-branch
one, including `!` over a single atom, stays one `BoundExpr::Token`. An
intersection-free, capture-free definition using `|` and `!` is therefore still
recognized by the dense NFA.

`E1 & E2` is evaluated inside `evaluate` by intersecting its operands' candidate
end positions from a shared start position. `oracle.rs` states the same operator
by folding operand end-position sets, and a property test compares the two.

## Concurrency boundary

Use `SentenceMatcher` only to schedule independent sentences on a caller-owned
Rayon pool. The input is a current batch, not a whole corpus, and output remains
in input-sentence order.

```rust
use rayon::ThreadPool;
use lingrep_core::fsm::SentenceMatcher;

let scheduled = SentenceMatcher::new(&matcher).match_all(&pool, &sentences);
```

Callers must keep streaming and chunking bounded. Parallel scheduling changes
neither the pattern language nor match semantics.

## Changing matcher behavior

1. Read [Performance Requirements](PERFORMANCE_REQUIREMENTS.md) before making
   a proposal. Keep memory bounded by the current batch.
2. Add a characterization or regression test before changing semantics.
3. Test complete `Match` values, not only match counts or spans.
4. For an alternative executor, compare it with the NFA oracle across bounded
   patterns and token streams, including sequence, alternation, references,
   optional, star, plus, and overlap.
5. Run focused tests, `cargo test --workspace`, formatting, clippy, and the
   repository CI command before claiming completion.
6. Benchmark supported paths before and after performance work.

## Relevant source files

- `lingrep-core/src/fsm/compiler/`: strict lowering, binding, diagnostics, budgets,
  and NFA construction.
- `lingrep-core/src/fsm/matcher/builder.rs`: mutable definition collection.
- `lingrep-core/src/fsm/matcher/compiled.rs`: immutable public executor.
- `lingrep-core/src/fsm/matcher/nfa_matcher.rs`: reference matching behavior.
- `lingrep-core/src/fsm/sentence_matcher.rs`: batch-scoped sentence scheduling.
- `lingrep-core/src/fsm/inline.rs`: inline-pattern construction helper.
- `lingrep-core/src/fsm/types.rs`: `Match`, patterns, and token specifications.

Parser changes are a coordinated change with the pinned `tree-sitter-lingrep`
dependency; see [pattern-language co-development](pattern-language-co-development.md).
