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

### Kani prepared-corpus proofs

On x86-64 Linux, run the corpus proofs from the workspace root. The verification
shell supplies Kani 0.68.0 and the nightly compiler it was built against:

```bash
nix develop .#verification --command cargo kani -p lingrep-corpus
```

| Boundary | Inputs | Property |
| --- | --- | --- |
| Location record lookup | Any `u64` sentence count and ordinal | Accept exactly the in-range ordinals. An accepted ordinal identifies an existing block and a slot within that block, including a final partial block. |
| Location block directory | Any `u64` metadata fields for the block selected by a valid record ordinal | Accept exactly the canonical start, record count, and byte offset. Reject inconsistent entries without panicking. |
| Location slicing | Any `u64` offset and length, with buffer lengths from 0 to 16 bytes | Accept exactly the in-bounds ranges and return the requested length. |
| Token directory | Any `u64` corpus sentence count, block start, and block sentence count | Accept exactly the nonempty ranges within the corpus. The end of an accepted range fits in `u64` and advances beyond its start. |

For new proofs, state the input domain and compare the production function
with an independent specification. Use wider arithmetic for the specification
when testing overflow. Keep the structural tests for real mappings, file layout,
and checksums.

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
the browser layout checks. The browser checks need Chromium and ChromeDriver on
Linux and only ChromeDriver on macOS, whose dev shell provides the matching
Chrome; see [development-environment.md](development-environment.md).
Fuzz and performance are independent `--fuzz` and `--performance` modes.

### Soranoha corpus validation

Run Soranoha validation locally for corpus or web behavior changes. This is a
development check, separate from GitHub CI. Use a real Soranoha plaintext author
archive during iteration; it exercises catalog metadata and literary text that
Markdown and constructed fixtures do not cover.

Inside the development shell:

```bash
cargo build -p lingrep --bin ling
python3 scripts/check-soranoha.py target/debug/ling \
  --archive /path/to/soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip
```

The check compares the first 20 complete direct and prepared match records,
verifies artifact components, scans the corpus to build a count table, and checks
list-valued NDC
metadata, filtered frequencies, the web search stream, concordance, and Japanese
XLSX export. It starts and stops its own loopback server and removes its temporary
artifacts. Existing Markdown and synthetic tests retain their format-specific
and boundary coverage. For a UI change, also check the changed view in a browser
using that Soranoha corpus.

Before a new version or release, run the same check against the full Soranoha
corpus:

```bash
cargo build --release -p lingrep --bin ling
python3 scripts/check-soranoha.py target/release/ling --corpus /path/to/soranoha.lingcorpus
```

The full check verifies every artifact component and scans every sentence when
building its count table; displayed concordance and export rows are bounded.
It fails on an unavailable or incompatible artifact rather than substituting
Markdown or an author subset. Rebuild an incompatible artifact from the full
Soranoha ZIP into a new destination with `ling corpus build --input-format
soranoha --output NEW.lingcorpus ARCHIVE.zip`. Report the corpus path and the
validation result when finishing work or preparing a release; report unavailable
validation as pending.

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

The document reader's comparison with the source rendering is ignored in
routine tests because it reads a local Markdown corpus. It needs three
variables: `LINGREP_READER_ARTIFACT`, the corpus's prepared `.lingcorpus`
artifact; `LINGREP_READER_SIDECAR`, the reading sidecar `ling corpus render`
wrote for it; and `LINGREP_READER_SOURCE_ROOT`, the Markdown tree it was built
from. It prints only counts, so a private corpus's text stays out of the
output:

```bash
LINGREP_READER_ARTIFACT=corpus.lingcorpus LINGREP_READER_SIDECAR=corpus.reading \
  LINGREP_READER_SOURCE_ROOT=corpus/ nix develop -c cargo test -p lingrep \
  --test suite -- document_reader::the_reader_matches_the_source_rendering_on_a_local_corpus \
  --ignored --exact --nocapture
```

It passes when every document with a sentence shows the headings and
paragraphs that rendering its source shows, over every reader page.

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

`lingrep_core::test_scale::ScaleKnob` lowers a size threshold (spill runs,
merge fan-in, batch output, postings runs, compound derivation) so a small
fixture reaches the code that otherwise runs only on large inputs. A `ling`
child process reads each knob from its `LINGREP_TEST_*` variable; an
in-process test calls `ScaleKnob::set`, which is process-wide, so such a test
needs its own binary. Setting `LINGREP_TEST_SPILL_FILE_REPORT` makes `ling`
write the most spill files it held open at once. Only builds with
lingrep-core's `testing` feature read these; `lingrep/tests/suite/scale_bounds.rs`
and `lingrep-web/tests/compound_derivation_bounds.rs` use them.

### Browser tests

`lingrep-web/tests/browser/` runs on Linux Chromium and macOS Google Chrome.
Use the shared helpers to keep input and viewport handling consistent:

- Open sessions with `crate::session` to close Chrome after a test failure.
- Use `crate::MOD` for CodeMirror's `Mod-` shortcuts: Command on macOS and
  Control on Linux.
- Use `crate::resize` to set the page width, including widths below 500 px.
- Use `two_frames` before inspecting layout updated by `ResizeObserver`,
  media-query changes, or `fullscreenchange`; poll for any further asynchronous
  state that the assertion requires instead of sleeping for a fixed time.
- After an htmx swap, wait for `.htmx-added` to clear before clicking an
  element with `hx-*` attributes.
- For hover previews, assert how long the pointer must remain over a bar
  before a request, rather than the number of requests during a WebDriver sweep.

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
