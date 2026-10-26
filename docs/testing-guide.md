# Testing Guide for lingrep

For every coordinated lingrep patterns grammar/runtime change, run the pinned dependency
gate (the exact test name matters; Cargo filters that match zero tests are not
evidence):

```bash
cargo test -p lingrep-core --test suite -- pattern_compiler_contract::pinned_tree_sitter_parses_repository_catalogs --exact
```

When a CST node is replaced, that gate must assert the new node is exercised
and the replaced node is absent. Also run the grammar corpus and queries, compare
the stable catalog/compiler fingerprint snapshot, and compare complete
canonical matches with the independent typed-expression oracle.

For ordered `key:` changes, tests must prove authored field order survives
lowering, matching, spill/merge aggregation, JSONL, TSV, formatting, and editor
tooling. Include compile-fail coverage for empty/duplicate projections, unknown
attributes/components, a map in place of the field list, invalid whole-match
widths, and invalid capture widths. Runtime tests must prove one missing field drops the
entire key and increments `records_unkeyed`.

## Quick Tests During Development

Choose the narrowest command that observes the changed boundary. Run inside
[`nix develop`](development-environment.md#start-a-shell), which supplies the dictionary.

```bash
# Unit tests - fast, direct library access
cargo test --lib

# TUI and interactive worker tests
cargo test -p lingrep tui

# Matcher correctness gate
./scripts/test_matcher_correctness.sh

# CLI contract and characterization tests
cargo test -p lingrep --test suite -- cli_shape::
cargo test -p lingrep --test suite -- explore_characterization::

# Interactive testing (manual validation)
ling explore --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx data-archive/test-corpus.jsonl

# NFA and prepared V1 corpus gates
cargo test -p lingrep-core --test suite -- matcher_contract::
cargo test -p lingrep-corpus --test suite -- prepared_tokens
cargo test -p lingrep --test suite -- prepared_protocol::
```

### Correctness gate

The matcher correctness gate is `scripts/test_matcher_correctness.sh`. It runs
the dictionary-free `lingrep-core` tests that pin observable `CompiledCatalog` behavior
against the independent oracle (`lingrep-core/src/fsm/compiler/oracle.rs`) plus the
lingrep patterns contract suites. It requires no dictionary. The full `ci.sh`
lane and the GitHub CI workflow both run it.

## Pre-Commit Validation

Before pushing changes, run bounded validation:

```bash
# Run routine guards and unit-level validation
./ci.sh --fast
```

`--fast` includes the rustdoc lane, in both its workspace and
`-p lingrep-perf --features screen` forms, because `cargo doc --no-deps` selects
targets rather than features and so cannot see a feature-gated module at all.

Run `./ci.sh --full` before finishing a branch. It adds the complete
integration inventory, the matcher correctness gate, the adapter suites, and
the browser layout checks.
Fuzz and performance are independent `--fuzz` and `--performance` modes.

### Matcher properties and fuzzing after parser or matcher changes

Run matcher property tests after matcher changes. For parser or matcher changes,
also run the bounded fuzzing smoke when `cargo-fuzz` is available. They cover
different claims: generated invariant checks and crash-safety, respectively.

```bash
cargo test -p lingrep-core --test suite -- matcher_property_tests::
./ci.sh --fuzz
```

## Edge Case Testing

The edge case suite covers:

- Empty inputs (empty text, empty patterns)
- Boundary conditions (very long texts, single characters)
- Unicode and emoji handling
- Mixed line endings and whitespace

```bash
cargo test -p lingrep-core --test suite -- edge_cases_consolidated::
```

## OOC Execution Validation

The bounded matcher foundation has separate correctness, regression, real-data,
and resource-evidence layers. Run the focused correctness contracts first:

```bash
cargo test -p lingrep-core --test suite -- execution_limits::
cargo test -p lingrep-core --test suite -- execution_source::
cargo test -p lingrep-core --test suite -- execution_runner::
cargo test -p lingrep-corpus --test suite -- prepared_tokens
cargo test -p lingrep --test suite -- prepared_protocol::
cargo test -p lingrep --test suite -- direct_protocol::
cargo test -p lingrep --test suite -- policy_publication::
```

These tests pin direct/V1 complete-`Match` parity, ordinal and source-error
semantics, physical-versus-logical batch boundaries, sentence-final spill
cadence, cancellation reports, artifact publication, and bounded `ling`
input behavior. A passing filtered command is evidence only when Cargo reports
that the intended nonzero test count ran.

For prepared-corpus performance evidence, save and compare a Criterion baseline
from the prepared benchmark:

```bash
cargo bench -p lingrep-corpus --bench prepared_tokens -- --save-baseline pre-change
cargo bench -p lingrep-corpus --bench prepared_tokens -- --baseline pre-change
```

Confirm that Criterion emitted estimates for `normal_open`,
`full_verification`, `projected_decode`, and `matching_throughput` for the
compressed format. The benchmark is synthetic; use an exact production `ling`
command and GNU Time when real-corpus end-to-end latency or RSS is required.

The dictionary-backed adversarial check is ignored in routine workspace tests because it performs five real 64 KiB tokenizations:

```bash
LINGREP_DICTIONARY_PATH="$LINGREP_DICTIONARY_PATH" nix develop -c cargo test -p lingrep-core \
  --test suite \
  -- execution_adversarial_probe::public_executor_bounds_adversarial_64k_records_and_rejects_64k_plus_one \
  --ignored --exact --nocapture
```

It must report all five fixture estimates within `max_tokenized_bytes` and
prove that each 64 KiB + 1 variant fails before the tokenizer callback.

Estimated residency is not process RSS. When RSS is a required evidence field,
resolve an executable GNU Time rather than assuming the FHS
`/usr/bin/time` path exists:

```bash
LINGREP_OOC_GNU_TIME="$(type -P time)"
test -n "$LINGREP_OOC_GNU_TIME"
"$LINGREP_OOC_GNU_TIME" --version
"$LINGREP_OOC_GNU_TIME" -v <exact production command>
```

Verify the version output identifies GNU Time and record the resolved
executable, complete production command, exit status, input scope, output
redirection, elapsed time, and maximum RSS. If no GNU Time executable is
available, record the RSS row as unmeasured. Do not substitute another
utility or derive RSS from the executor estimator. Do not use an eager
Criterion corpus collector as a full-corpus RSS substitute.

## Writing Tests

### Prefer the library API

Use direct library tests when the behavior does not cross a CLI process
boundary; retain CLI tests for argument, exit-status, and stream contracts.

### Test Utilities Available

```rust
use lingrep_core::testing::{run_matching_test, test_tokenizer};

let stats = run_matching_test(
    &[("noun", "[upos=NOUN]")],
    &["テストです。"]
);
```

`lingrep_core::testing::strategies` provides Japanese sentence and pattern strategies
for property tests. Pair a generated input with an explicit invariant; the
strategy alone proves nothing.

### Snapshot locations

Matcher snapshots are stored beside their owning suites under
`lingrep-core/src/fsm/compiler/snapshots/` and `lingrep-core/tests/suite/snapshots/`.

## Test Categories

1. **Unit tests:** `#[cfg(test)]` modules beside the code, for example
   `lingrep-core/src/fsm/compiler/oracle.rs`.
2. **Integration tests:** one consolidated `tests/suite/` binary per crate; add
   a module there with a matching `mod` line in `tests/suite/main.rs`.
3. **Property tests:** `lingrep-core/tests/suite/matcher_property_tests.rs`,
   generated inputs checked against matcher invariants.
4. **Fuzz targets:** `lingrep-core/fuzz`, parser and matcher crash-safety;
   they require `cargo-fuzz` and run with `./ci.sh --fuzz`.
5. **Benchmarks:** `lingrep-core/benches/`, `lingrep-corpus/benches/`, and
   `lingrep/benches/`.

## Related documentation

- [AGENTS.md](../AGENTS.md) — repository architecture and validation rules
- [Matcher development](fsm-development.md) — compiler and matcher contracts
- [Performance requirements](PERFORMANCE_REQUIREMENTS.md) — benchmark and
  performance-evidence selection
