# Performance Tuning Guide

`FsmBuilder` compiles immutable, capability-classified `CompiledCatalog`
values, and plain recognition runs on the NFA. Performance work must preserve
exact canonical full `Match` results, including context fields, and must meet the requirements in
[PERFORMANCE_REQUIREMENTS.md](PERFORMANCE_REQUIREMENTS.md).

## Measure before changing a supported path

Use the
[risk-adjusted performance evidence](PERFORMANCE_REQUIREMENTS.md#risk-adjusted-performance-evidence)
tiers rather than applying the longest campaign to every change:

1. Update the candidate from current `main` before freezing comparison
   binaries, and reuse an existing representative harness where possible.
2. Record the relevant baseline and identify the bound with `perf`, a
   flamegraph, a capacity target, or explicit complexity evidence.
3. Set the minimum useful effect, guardrails, and evidence tier before the
   final measurement.
4. Change one mechanism at a time and run focused correctness checks before
   expensive measurements.
5. Execute the selected tier and stop once its decision rule is met. Add
   another corpus only when it exercises a materially different bound.
6. Run full CI once on the integrated commit. Rerun performance after
   integration only when that integration changed the measured path.

```bash
cargo test --workspace
cargo bench
./ci.sh
```

For a simplifying primitive substitution, a microbenchmark can establish that
the candidate is plausible, but it cannot establish end-to-end impact. Tier 1
still requires a fresh-process comparison for each materially different
supported mode and all applicable guardrails. If the result is not decisive,
escalate the evidence tier instead of treating one favorable run as a win.

For a change to a prepared format, a digest, or another protocol primitive,
correctness is the main risk. Characterize and test:

- version and magic rejection;
- the exact authenticated byte ranges and verification ordering;
- identities and domain separation;
- corruption behavior and error precedence;
- output parity except for explicitly versioned fields; and
- rejection of obsolete artifacts with a documented rebuild path.

Trust weakening, concurrency, unsafe code, and caching or freshness changes do
not qualify for this fast path even when a local benchmark shows a large gain.

## Memory and scheduling

The global matcher requirement is bounded memory: do not collect an entire
corpus before matching. `lingrep_core::execution::execute` owns the canonical
pull/tokenize/match/publish lifecycle. `SentenceMatcher::match_all` schedules
only the physical batch supplied by that executor on a caller-owned Rayon
pool; it preserves sentence order and does not select another matching engine.

The production defaults are:

| Limit | Default | Meaning |
|---|---:|---|
| `max_sentence_bytes` | 64 KiB | decoded UTF-8 bytes in one raw sentence |
| `max_input_record_bytes` | 1 MiB | bytes in one physical input record — a JSONL document, or a plaintext or Markdown line |
| `max_stage_sentences` | 2048 | sentences retained for one tokenization stage |
| `max_stage_input_bytes` | 128 KiB | total decoded raw UTF-8 in that stage |
| `max_tokenized_bytes` | 32 MiB | estimated `TokenInfo` inline storage plus owned string capacity in one sentence or physical match batch |

A pretokenized JSONL reader applies `max_tokenized_bytes` first to the encoded
record and again to decoded estimated residency. An oversized raw record fails
before tokenization. Sources that already own the complete raw sentence report
its exact byte length. A bounded reader that stops early reports only the
proved lower bound, normally `max_sentence_bytes + 1`; it does not drain the
record merely to produce an exact diagnostic. A tokenized sentence that alone
exceeds the residency cap fails rather than being split, because splitting
would change morphological and lingrep patterns semantics.

Physical resource batches are controlled by `--batch-sentences` and
`--batch-raw-bytes`. Several physical batches may feed one logical publication
stage without changing canonical output, sentence-final spill cadence, or
progress boundaries.

`--max-record-bytes` bounds one input record and is independent of
`--batch-raw-bytes`: a record larger than one execution batch feeds several
batches, and raising one bound never enlarges the other. `--max-record-bytes` must be at least
`--max-sentence-bytes`, because a record holds at least one sentence.
`lingrep-web` exposes the same flag, applied while its corpus artifact is staged.

Tune the batch bounds and `--workers` from measurements of the actual corpus.
A higher thread count is not itself a performance result.

## OOC execution coverage

The following paths pull bounded input and do not collect a corpus:

- `ling matches` from recursively discovered direct input or a compressed
  V1 artifact;
- `ling summarize`, with spillable exact surface aggregation;
- `ling tokenize`, `explore`, and `sentence-final`;
- `ling corpus build`, with bounded tokenization and explicit immutable
  publication;
- `lingrep-web` V1 artifact execution through the generic executor.

Direct `ling` commands accept `--max-sentence-bytes` and
`--max-tokenized-bytes`; prepared artifacts are already tokenized and are not
rebuilt to apply direct-input bounds. Traversal has separate explicit bounds.

`--batch-raw-bytes` must be at least `--max-sentence-bytes`, so that one
accepted sentence fits in a stage. Library callers construct
`ExecutionLimits` directly and may override every field.

Within paths routed through the unified executor, limit, source, tokenization,
prepared-corpus, sink, observer, and temporary-budget failures are terminal and produce
a non-zero CLI exit; malformed input is never converted to EOF. Oversize
errors identify the limit and ordinal/source context when available. They
report an exact actual value only when the complete value is already known;
early bounded readers report `at least limit + 1`. V1 decode failures
remain typed source failures through `PreparedSentenceSource`; malformed prepared
artifacts never become clean EOF.

Lingrep artifacts use `tokens.v1.zst` (token schema 2), `locations.v1`,
`provenance.sqlite` (provenance schema 4), optional `enhanced.v1.zst`,
manifest schema 1, identity scheme a1, and artifact generation 2. An artifact
of any other generation requires rebuilding from source; there is no
compatibility reader or in-place converter. Stored component and block
digests use unkeyed BLAKE3 and are checked before decompression or structural
use. These digests detect
change relative to a separately trusted manifest; they are not a MAC or a
signature, and an attacker able to rewrite both content and metadata can make
a self-consistent artifact.

Authoritative JSONL output uses protocol suite V1. Component
verification is reported as `blake3_matches`.

Cancellation is cooperative at safe execution/publication boundaries.
`RunOutcome::Cancelled` reports only fully published logical batches; callers
must discard any partial later sink state. Matcher and sentence-final emit no
aggregate output or newly completed artifact after cancellation. SIGINT exits
130, SIGTERM exits 143, and non-signal programmatic cancellation exits 1.
Artifact creation publishes its private temporary directory only after a completed
run.

`RunOutcome::Cancelled(ExecutionReport)` retains the last fully published
report. Exhaustive matches must bind the report or use `Cancelled(_)`.

## Prepared-token format

The CLI supports one compressed V1 prepared-token format. Physical readers and
writers are private; execution consumers obtain `PreparedSentenceSource` from
`CorpusArtifact`. Direct/live input consumes caller-owned bounded Vibrato
output without storage. Validate reload, throughput, file-size, and memory bars
against the requirements document.

## Useful commands

```bash
# Supported workspace validation
cargo test --workspace

# Benchmark after recording a baseline
cargo bench

# Inspect a suspected CPU bottleneck
perf record -g cargo bench
perf script | flamegraph-rs > flamegraph.svg
```

See [CLI Usage Guide](cli-usage-guide.md) for supported runtime controls and
[Matcher Development Guide](fsm-development.md) for the correctness contract.
