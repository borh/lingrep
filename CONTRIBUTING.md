# Contributing to lingrep

lingrep uses a Nix development environment and a workspace-wide validation
script. Start from a repository checkout:

```bash
nix develop
cargo test --workspace
./ci.sh --fast
```

The workspace minimum supported Rust version is declared in `Cargo.toml`.
The Nix shells provide the matching toolchains and native dependencies; a
non-Nix setup is possible but is maintained by the person using it.

## Before changing code

Read [AGENTS.md](AGENTS.md) for the architecture, prohibited APIs, and
repository workflow. It applies to human contributors and agents.

Then read the guide owned by the affected boundary:

- [Matcher development](docs/fsm-development.md)
- [lingrep patterns grammar/runtime co-development](docs/pattern-language-co-development.md)
- [Performance requirements](docs/PERFORMANCE_REQUIREMENTS.md)
- [Testing](docs/testing-guide.md)
- [Development environment](docs/development-environment.md)
- [Dictionary packages](DICTIONARIES.md)

## Make a change

- Keep the change scoped to one observable outcome.
- Add a regression or characterization test for behavior changes.
- Compare complete `Match` values for matcher changes, including contexts and
  captures.
- Preserve bounded corpus processing; do not collect an entire corpus.
- Update user documentation when a public command, format, or workflow changes.
- Update `AGENTS.md` and `ling check --help` when lingrep patterns or an agent-facing
  invariant changes.

Use standard Rust formatting and Clippy:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

When editing Rust documentation, also run the focused rustdoc lane:

```bash
RUSTDOCFLAGS="-D warnings" cargo doc -p <crate> --no-deps
```

## Test at the right scope

During iteration, run the smallest test that observes the changed boundary:

```bash
cargo test -p lingrep-core --test suite -- <module-or-test-name>
cargo test -p lingrep --test suite -- <module-or-test-name>
cargo test -p lingrep-web --lib <module-or-test-name>
```

Before pushing, run:

```bash
./ci.sh --fast
```

Before finishing a branch, run the complete correctness lane:

```bash
./ci.sh --full
```

Select `./ci.sh --fuzz` only for parser/matcher fuzz evidence and
`./ci.sh --performance` only when the performance requirements call for that
evidence. Inside the Nix shell, a missing tool fails the lane rather than
skipping the check.

The optional repository pre-push hook runs the fast lane:

```bash
git config core.hooksPath .githooks
```

## Performance changes

[docs/PERFORMANCE_REQUIREMENTS.md](docs/PERFORMANCE_REQUIREMENTS.md) owns every
performance target, evidence tier, and acceptance rule. Record a representative
baseline and profile before making an optimization claim. Do not copy a numeric
threshold into another guide.

## lingrep patterns language changes

lingrep patterns syntax is split between the pinned `tree-sitter-lingrep`
grammar and this repository's compiler. Follow the complete checklist in
[pattern-language-co-development.md](docs/pattern-language-co-development.md),
including the runtime lowerer, compiler-contract tests, installed CLI reference,
browser assets, and grammar pin.

## Prepare a release

Set the release version in `Cargo.toml` and all three `python/*/pyproject.toml`
files. Update both adapters' `lingrep-adapter-protocol==` dependencies to the
same version. Refresh the lockfiles and run the checks:

```bash
cargo update --workspace
uv lock --project python/spacy-adapter
uv lock --project python/stanza-adapter
./ci.sh --fast
```

Write `docs/release-notes/v<VERSION>.md` before creating the tag. Pushing a
`v<VERSION>` tag publishes the release automatically.

## Commits

Use a short imperative subject that states the outcome, for example:

```text
fix(corpus): reject mismatched analyzer identity
docs(cli): describe each command in the default help
```

Keep unrelated local changes out of the commit.
