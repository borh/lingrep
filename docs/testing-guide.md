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

## Table of Contents

- [Quick Tests During Development](#quick-tests-during-development)
- [Pre-Commit Validation](#pre-commit-validation)
- [Edge Case Testing](#edge-case-testing)
- [Performance Benchmarking](#performance-benchmarking)
- [OOC Execution Validation](#ooc-execution-validation)
- [Writing Tests](#writing-tests)
- [Advanced Testing Techniques](#advanced-testing-techniques)
- [Test Categories](#test-categories)

## Quick Tests During Development

Choose the narrowest command that observes the changed boundary.

The `nix develop` shell exports `LINGREP_DICTIONARY_PATH` (the pinned CWJ
artifact), which every tool, test, and benchmark reads. Only set it yourself to
run against a different dictionary:

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
export LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-cwj-202512.dic"
```

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

# Prepared V1 corpus benchmark (optional)
cargo bench -p lingrep-corpus --bench prepared_tokens

# Interactive testing (manual validation)
ling explore --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx data-archive/test-corpus.jsonl

# NFA and prepared V1 corpus gates
cargo test -p lingrep-core --test suite -- matcher_contract::
cargo test -p lingrep-corpus --test suite -- prepared_tokens
cargo test -p lingrep --test suite -- prepared_protocol::
```

### What These Tests Cover

- **Unit tests:** Individual function and module correctness
- **Scroll tests:** TUI boundary conditions and scrolling behavior
- **Matcher correctness gate:** Compares complete matcher behavior with the
  independent oracle
- **Property tests:** Generated matcher invariants
- **Fuzz targets:** Parser and matcher crash-safety
- **Benchmarks:** Performance regression detection

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

The `ci.sh` script validates everything that CI checks:
- ✅ All targets compile (lib, bins, tests, benchmarks)
- ✅ No clippy warnings
- ✅ Code is formatted
- ✅ Workspace library and binary unit tests pass
- ✅ Workspace doctests and lingrep-web JavaScript tests pass
- ✅ Rustdoc builds with warnings denied, workspace-wide and feature-gated

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

## Performance Benchmarking

Criterion benchmarks cover matcher construction/execution and V1 prepared-corpus
open, integrity verification, projected decoding, and matching throughput.

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench -p lingrep-corpus --bench prepared_tokens

# View HTML reports
open target/criterion/report/index.html
```

### Prepared V1 Corpus Benchmark

The prepared-corpus benchmark covers the compressed V1 token format:

```bash
# Run the prepared storage/execution benchmark
cargo bench -p lingrep-corpus --bench prepared_tokens

# View results
open target/criterion/prepared_tokens/report/index.html
```

The benchmark emits file size, bytes/token, open latency, full-integrity
verification latency, projected decode throughput, and matching throughput.

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
cargo test -p lingrep --test suite -- sentence_final_characterization::
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
boundary; retain CLI tests for argument, exit-status, and stream contracts:

```bash
# Unit tests - fast, direct library access
cargo test --lib

# Matcher property tests
cargo test -p lingrep-core --test suite -- matcher_property_tests::

# Parser/matcher fuzzing (requires cargo-fuzz)
./ci.sh --fuzz
```

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

## Advanced Testing Techniques

### Test Helpers

```rust
use lingrep_core::testing::compile_test_pattern;

let matcher = compile_test_pattern("[upos=NOUN]");
let matches = matcher.find_matches(&tokens);
assert!(!matches.is_empty(), "noun matching");
```

### Golden File Testing

Use the `insta` crate for output snapshot testing:

```bash
# Create snapshots
cargo test

# Update snapshots after intentional changes
INSTA_UPDATE=1 cargo test
```

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

## Running Tests by Category

```bash
# Unit tests only (fastest)
cargo test --lib

# Integration tests only
cargo test --workspace --tests

# Focused matcher property module
cargo test -p lingrep-core --test suite -- matcher_property_tests::

# Specific test function
cargo test --lib test_pattern_matching

# Run tests serially to expose race conditions
cargo test --lib -- --test-threads=1
```

## Best Practices

1. **Test at the owning boundary** — use library tests for library behavior and CLI tests for process contracts
2. **Use property tests for complex logic**
3. **Run the matcher correctness gate after performance changes**
4. **Add golden file tests for output validation**
5. **Use `--test-threads=1` to expose races**

## Troubleshooting

### Tests Failing with "Dictionary Not Found"

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
export LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-cwj-202512.dic"
```

### Tests Timing Out

```bash
# Run with more verbose output
cargo test -- --nocapture

# Run specific test only
cargo test --lib test_specific_function

# Run with increased stack size (for deep recursion)
RUST_MIN_STACK=8388608 cargo test
```

### Flaky Tests

```bash
# Run serially to detect race conditions
cargo test -- --test-threads=1 --exact test_flaky_test

# Run multiple times
for i in {1..10}; do cargo test --lib test_flaky || break; done
```

## Related documentation

- [AGENTS.md](../AGENTS.md) — repository architecture and validation rules
- [Matcher development](fsm-development.md) — compiler and matcher contracts
- [Performance requirements](PERFORMANCE_REQUIREMENTS.md) — benchmark and
  performance-evidence selection
