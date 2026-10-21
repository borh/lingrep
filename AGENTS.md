# AI agent guide for lingrep

lingrep is a Rust pipeline for Japanese tokenization, pattern matching in the
lingrep patterns language, and corpus analysis. This is the single agent-facing
guide; `CLAUDE.md` is a symlink to it.

## Read before acting

| Before you | Read |
|---|---|
| write or edit a `.lx` catalog | [docs/pattern-language-reference.md](docs/pattern-language-reference.md) |
| run an unfamiliar `ling` subcommand or flag | [docs/cli-usage-guide.md](docs/cli-usage-guide.md), `ling describe <subcommand>`, `ling contracts` |
| change the pattern grammar | [docs/pattern-language-co-development.md](docs/pattern-language-co-development.md) |
| work on the matcher | [docs/fsm-development.md](docs/fsm-development.md) |
| work on prepared corpora or `lingrep-web` | [docs/cli-usage-guide.md](docs/cli-usage-guide.md#prepared-v1-corpora), [docs/deployment-guide.md](docs/deployment-guide.md) |
| touch benchmarks or campaign tooling | [docs/PERFORMANCE_GUIDE.md](docs/PERFORMANCE_GUIDE.md) |
| propose or change code at all | [docs/PERFORMANCE_REQUIREMENTS.md](docs/PERFORMANCE_REQUIREMENTS.md) |

Record a baseline and profile before making an optimization claim;
`PERFORMANCE_REQUIREMENTS.md` owns every performance bar, tier and acceptance
criterion. Do not add performance numbers to this guide.

## Workspace map

- `patterns/`: the tracked catalogs. Every `.lx` file in the tree is
  discovered rather than listed, so adding one requires its entries in the
  claim-count, definition-count, execution-attribute, spawn-table,
  dispatch-shape and browser-projection pins, plus three baseline snapshots.
  `collexeme.lx` and `alternation.lx` are a worked example of that checklist.
  The pins key each catalog by its repository-relative path, so moving a
  catalog requires the same checklist as adding one.
- `lingrep-core/src/fsm/`: parser, NFA compiler/executor, and sentence scheduler.
- `lingrep/`: the packaged CLI, including the interactive explorer and
  sentence-final workflow.
- `lingrep-web/`: web consumer of `CompiledCatalog` and immutable V1 artifacts.
  Packaged as `.#lingrep-web` and exposed as `nixosModules.lingrep-web`. The
  unit binds loopback by default and expects a reverse proxy (Caddy/Nginx) to
  terminate TLS.
- `lingrep-corpus/`: immutable V1 prepared-corpus artifacts, identity, provenance,
  and bounded sentence sources. N-grams are expressed by `patterns/ngrams.lx`;
  gravity is an external analysis example over `ling` JSONL.
- `lingrep-cli-common/`: shared clap argument groups reused across the CLI binaries.
- `lingrep-perf/`: campaign tooling for performance work, not a CI feature. The
  counted-work screen and the orchestration half require `--features screen`; do
  not make `lingrep-core`'s `bench-instrumentation` feature unconditional,
  because Cargo would unify it into the shipping binaries.
- `docs/`: current guides, indexed by [docs/README.md](docs/README.md).

Sample corpora are in `data-archive/`.

## Matcher API

The production API is:

```text
lingrep patterns definitions → FsmBuilder → immutable CompiledCatalog → NFA matching
```

`FsmBuilder` owns mutable definition registration. Its atomic `build` rejects
duplicate names and binds references against the complete accumulated catalog.
`CompiledCatalog` is immutable and produces complete `Match` values in canonical
`(start, end, pattern)` order.

```rust
use lingrep_core::fsm::{FsmBuilder, PatternSource};

let mut builder = FsmBuilder::new();
builder.add_definition(PatternSource::named("verb.lx", "verb := [upos=VERB]."))?;
let matcher = builder.build()?.matcher;
let matches = matcher.find_matches(&tokens);
# Ok::<(), String>(())
```

For independent sentences, use `SentenceMatcher` with a caller-owned Rayon
pool and only the current streaming/chunked batch. It preserves sentence order;
it does not authorize collecting a corpus. `CompiledCatalog` is the only
compiled matcher, and there is no pattern-level parallelism.

Reusable corpus storage belongs to `lingrep-corpus` V1 artifacts alone.

## Token attributes

The portable, unprefixed keys are `text`, `lemma`, `norm`, `upos`, `xpos` —
CoNLL-U's `FORM/LEMMA/UPOS/XPOS` plus spaCy's `norm_`, with `text` for the
surface. Everything else is provider-namespaced: `unidic.*`, `sudachi.*`,
`spacy.*`, or `feat.*`.

The set is closed in two places — `validate_canonical_key` in
`lingrep-core/src/token_match/attribute_key.rs` and `portable_attribute_key` in
`tree-sitter-lingrep`'s `grammar.js`. Changing it requires both, in the order
[pattern-language-co-development.md](docs/pattern-language-co-development.md)
gives. An unprefixed key outside the set is a compile error, so `[orth=…]` and
`[form=…]` are both refused: write `[text=…]`, or `[unidic.orthbase=…]` for
書字形基本形. `unidic.form` is 語形出現形, not the surface.

Attribute keys are hashed into the token-schema digest and from there into
analysis identity, so renaming a key changes every prepared artifact's identity
and makes artifacts written before the rename unreadable.

Analyzer identities are built from explicit revision constants, never from
source bytes, so that editing a comment does not invalidate prepared corpora.
Replace a revision when output changes; the pinned identity literals in
`artifact_fingerprint_golden.rs` and `sudachi_contract.rs` fail until then.

Pretokenized CoNLL-U adds analyzer-neutral dependency keys `dep.head`,
`dep.rel`, and `dep.enhanced`. Integer word `ID` is not a separate attribute:
matcher index plus one is the same sentence-local identity. Known UD FEATS use
`feat.*`; complete raw FEATS and MISC entries are available as
`conllu.feats` and `conllu.misc`. Range IDs and empty-node IDs are structural
rows and do not become matcher tokens.

Referencing an attribute the bound corpus's token schema does not carry —
`[dep.rel=nsubj]` over Vibrato/UniDic, `[unidic.cform~連用形]` over CoNLL-U — is
a warning, not an error: the attribute binds as absent on every token, so
positive tests and `!=`/`!~`/`not in` match nothing while the complement `!(…)`
matches every token. `matches` and `summarize` report this by default as
`warning[attribute_unavailable]`, naming the attribute, the referencing
definitions (`CatalogAnalysis::attribute_references`), and the corpus's token
schema. Two cases are refused with exit code `2`: a dependency `where` clause
without `dep.head`/`dep.rel`, and a value-kind conflict.
`check --schema {unidic-morphology, sudachi-morphology, tokenize, conllu}`
reports the same finding without a corpus, and `doctor` adds a binding probe
when both a catalog and a corpus are supplied.

## Analyzers and dictionaries are separate axes

The `--schema` values name token schemas, not analyzers. Vibrato has no feature
layout of its own: `featurize` reads `token.feature()` positionally in UniDic's
CSV column order, so `TokenSchema::unidic_morphology()` describes the
dictionary. `declared_token_schema(Vibrato, Morphology)` returns it because
every packaged dictionary is UniDic 202512; admitting a non-UniDic `.dic`
requires a change in that function, not only in the loader. Sudachi's
morphology layout is fixed by the Sudachi library, and `*/Tokenize` is `text`
alone.

`AnalyzerFamily::Vibrato`, `--analyzer vibrato`, `--dict-path`, `VibratoPlan`
and the `lingrep.vibrato-*/v1` semantic tags name the engine; do not rename them
after the dictionary. A catalog states both axes with
`analyzer: { family: "vibrato", resource: "unidic-cwj" }`;
`lingrep/src/dictionary_binding.rs` compares the resource against the loaded
dictionary and reports a mismatch as a notice, never an error.

## Dictionaries

`DICTIONARIES.md` catalogs the seven UniDic 202512 dictionaries. Each is a
`.dic.zst` on the `unidic-202512` GitHub release, which both
`ling dictionary install` (`lingrep/src/packaged_dictionaries.rs`) and the
`.#vibrato-dict-*` packages download; `.#vibrato-dictionaries-from-source`
converts the NINJAL archives. Every binary resolves `--dict-path`, then
`LINGREP_DICTIONARY_PATH` (which the dev shell exports), then the installed
`cwj`.

The BLAKE3 of each decompressed `.dic` is pinned twice: in `DICTIONARIES` in
`packaged_dictionaries.rs`, which `ling dictionary install` checks, and in
`vibrato-dictionaries.tests.digests` in `flake.nix`, which checks the release
and the conversion from source. A unit test asserts that both pins match. A
dictionary whose bytes change is a new release tag with new digests, never a
replaced asset, because the digest is part of every analysis identity.

The installed and packaged dictionaries are uncompressed, rkyv-serialized,
memory-mapped `.dic` files. The loader expands a user-supplied `.zst` archive
into `~/.cache/vibrato-rkyv/` and maps that copy; the identity digest is over
the decompressed bytes, so compression does not change `dictionary_identity`.

Adding a dictionary requires its source in `unidicSources` and its asset hash in
`unidicReleaseHashes` in `flake.nix`, its BLAKE3 in both pins, an entry in
`DICTIONARIES.md`, and a tokenization check against its `share/vibrato/*.dic`
output.

## Development workflow

```bash
nix develop
cargo test --workspace
./ci.sh
```

`ci.sh --fast` is the pre-push lane: repository guards, formatting, clippy,
unit tests, the feature-gated screen lane, and the doc lane. `./ci.sh` adds the
full test inventory, the adapters, and the browser checks. See
[docs/testing-guide.md](docs/testing-guide.md) for what each lane runs and
[docs/development-environment.md](docs/development-environment.md) for setup.

Use focused tests during iteration, then validate the workspace. For behavior
changes, write a characterization or regression test first. Compare complete
`Match` values, including captures, graph bindings and lexical identity, rather
than only counts or spans.

When editing doc comments, also run the doc lane for that crate:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps
```

`cargo test` and `cargo clippy` do not report broken intra-doc links. For
`lingrep-perf`, add `--features screen`: `cargo doc --no-deps` selects targets,
not features.

Before performance work, profile, then screen counted matcher work, then run a
paired campaign:

```bash
cargo bench -p <crate> --bench <relevant-benchmark>
perf record -g cargo bench -p <crate> --bench <relevant-benchmark>
perf script | flamegraph-rs > flamegraph.svg

just perf-work --json > before.json     # change something, then:
just perf-work --baseline before.json   # an unchanged counter is a valid result
```

The campaign commands, the sealed manifest, the `--template` (`matches` versus
`summarize`) and `--stream-output` choices, and the risk-adjusted evidence tier
are specified in
[docs/PERFORMANCE_REQUIREMENTS.md](docs/PERFORMANCE_REQUIREMENTS.md). Benchmark
only the supported path when making a performance claim.

## Co-development with tree-sitter-lingrep

The lingrep patterns grammar is a pinned `tree-sitter-lingrep` Cargo Git
dependency, and a syntax change requires coordinated work in both repositories.
A local grammar clone is optional; this workspace does not vendor or build one.

- A new syntax requires the pinned revision bumped in `Cargo.toml` with
  `Cargo.lock` refreshed, the lowerer
  (`lingrep-core/src/fsm/compiler/lower.rs`), `lingrep/src/pattern_reference.rs`
  so `ling check --help` and `ling-check(1)` describe it, and every exhaustive
  match the lowerer feeds — `fsm/types.rs` impls, `matcher/predicate.rs`,
  `compiler/oracle.rs`, and the owned-byte and regex-byte accounting sites.
- Tests go in `lingrep-core/tests/suite/pattern_compiler_contract.rs` or another
  `lingrep-core/tests/suite/*.rs`, plus a matching `mod` line in
  `lingrep-core/tests/suite/main.rs`. Do not add a top-level
  `lingrep-core/tests/*.rs`, which builds a separate test binary;
  `scripts/check-test-consolidation.sh` enforces both rules.
- A user-visible syntax change updates
  [docs/pattern-language-reference.md](docs/pattern-language-reference.md).

The full procedure, including the grammar clone and the verification commands, is
[docs/pattern-language-co-development.md](docs/pattern-language-co-development.md).

## Git workflow

Commits are `type(scope): summary` with a body explaining what and why. Types
in use: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`, `ci`,
`merge`. Work happens on a branch off `main`, which always passes CI.

Create worktrees with worktrunk: `wt switch --create <branch>` puts one beside
the primary checkout as `../lingrep.<branch>`, and the `post-start` hook in
`.config/wt.toml` reflink-copies `target/` into it, bounded by
`.worktreeinclude`. Remove a landed one with `wt remove`. Do not create
worktrees under `.worktrees/`. `scripts/worktree-disk.sh` enumerates from
`git worktree list --porcelain`, so it finds every tree wherever it lives.

**Never use `git stash`.** The stash is shared by every worktree of the
repository, so a stash taken in one tree can be popped in another. Use a
temporary commit and `git reset HEAD~` instead.

`.githooks/pre-push` runs `ci.sh --fast`. It gates a push only in a clone
that has opted in with `git config core.hooksPath .githooks`
([CONTRIBUTING.md](CONTRIBUTING.md)); without that, run `./ci.sh --fast`
before pushing.

## Worktree lifecycle

After a worktree's branch is merged, verify that the worktree is clean, remove
it, and prune stale worktree registrations. If a worktree is dirty, keep it and
report the remaining changes instead of force-removing it. Keep the primary
checkout and any worktree that holds unmerged work.

## Debugging a pattern that does not match

1. Parse it from a `tree-sitter-lingrep` clone: `tree-sitter parse patterns/x.lx`.
2. `ling check --patterns patterns/x.lx` for compiled-catalog warnings
   and drift evidence; add `--schema <schema>` to list the referenced
   attributes that schema cannot supply.
3. Read the run's stderr: a `warning[attribute_unavailable]` means the corpus
   cannot supply a referenced attribute at all, and names the definitions.
4. `cargo test -p lingrep-core --lib fsm::compiler` and `... fsm::matcher`.
5. `ling coverage --patterns patterns/x.lx` — the usual cause is an
   absence-sensitive exclusion over a sparse attribute, and this command
   names it.
6. `ling explore` for interactive iteration.
7. `ling matches --trace-policy N` when the match exists but is not
   published; the cause is then the policy, not matching.

## Documentation index

[docs/README.md](docs/README.md) indexes the current guides. See also
`CONTRIBUTING.md` and `DICTIONARIES.md`.
