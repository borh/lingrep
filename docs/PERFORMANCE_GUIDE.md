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
corpus before matching. `lingrep_core::execution::execute_analyzed` runs the
canonical pull/tokenize/match/publish lifecycle.
`BoundSentenceMatcher::match_in_windows` schedules the supplied physical batch
on a caller-owned Rayon pool and preserves sentence order.

The production defaults are:

| Limit | Default | Meaning |
|---|---:|---|
| `max_sentence_bytes` | 1 MiB in the library; automatic in the CLI | decoded UTF-8 bytes in one raw sentence; CLI defaults to the input-record memory bound |
| `max_input_record_bytes` | 1 MiB | bytes in one physical input record — a JSONL document, or a plaintext or Markdown line |
| `max_stage_sentences` | 2048 | sentences retained for one tokenization stage |
| `max_stage_input_bytes` | 128 KiB | target decoded raw UTF-8 in a stage; a larger accepted sentence runs alone |
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
batches, or one larger sentence runs alone. The batch target stays unchanged
when the record bound changes. An explicit sentence bound is independent.
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
- `lingrep-web` queries over V1 artifacts through the generic executor.

Direct `ling` commands accept `--max-sentence-bytes` and
`--max-tokenized-bytes`; prepared artifacts are already tokenized and are not
rebuilt to apply direct-input bounds. Traversal has separate explicit bounds.

`--batch-raw-bytes` is a batching target, so it may be smaller than an accepted
sentence. Library callers construct `ExecutionLimits` directly and may override
every field.

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

## Qualify a corpus build under memory pressure

The pressure harness requires Linux cgroup v2, a systemd user manager with
memory accounting, and disk storage that supports reflinks. Build optimized
binaries, then run a fixed three-block comparison:

```bash
cargo build --release -p lingrep -p lingrep-perf
python scripts/bench-corpus-pressure.py \
    --binary target/release/ling --estimator target/release/lingrep-perf \
    --source soranoha-plaintext.zip --dictionary "$LINGREP_DICTIONARY_PATH" \
    --output /var/tmp/ling-pressure-qualification
```

Each arm uses fresh private input and dictionary inodes, with swap disabled.
The limited arm uses 2 GiB; the control has no cgroup memory limit. The
harness rejects inherited limits that invalidate the comparison. It records
the plan before measuring and compares complete artifact bytes and identities,
excluding only the build timestamp. The 95% Student-t interval uses balanced
block log-ratios; qualification requires an upper wall-time ratio of at most
1.10 and successful completion of every run.

The output directory must not exist. It retains plans, metrics and logs;
private inputs and built artifacts are removed after each run. If service
termination cannot be verified, the run directory is retained and reported.
After retaining any evidence you need, delete the output directory to reclaim
its space. Choose the sample size before running; an inconclusive result does
not authorize extending the same campaign until it passes.

## Native campaigns on Linux and macOS

Run `scripts/perf_campaign.sh` inside `nix develop`, which supplies Bash 4+
and Python 3. The existing
[campaign commands](PERFORMANCE_REQUIREMENTS.md#running-one) support both
platforms. Linux defaults to `--affinity 0-7`; macOS defaults to unpinned
execution because it has no equivalent CPU-affinity API.

Before a comparison, declare an A/A calibration with the same optimized binary
supplied to both `declare --arm-a` and `--arm-b`. Declare a single block count
with `--ladder N` and an acceptable wall-time interval with
`--calibration-wall-tolerance-pct PCT`. For example, `--ladder 6` with
`--calibration-wall-tolerance-pct 2` requires the full 95% interval to lie within
0.98–1.02 after exactly six balanced blocks. Calibration covers the primary
catalog only.

`--warmup-runs-per-arm N` seals the warm-up count when declaring a campaign
(default: 1; minimum: 1). Before recording measurements for a catalog, `run`
executes that many untimed runs of each arm with the measured command,
environment and affinity. Warm-up receipts remain under `warmups/` and do
not enter estimates or block counts. A failed warm-up stops the run.
Resuming unfinished work repeats both arms' warm-ups and labels their receipts
`resumed-warmup`; resuming completed work does nothing. Campaigns without a
sealed warm-up count must be declared again before running.

When declaring a comparison, pass `--calibration /path/to/calibration/campaign.json`
to `new` or `lingrep-perf declare`. The comparison seals that calibration's
manifest digest and the digest of its complete evidence file. Retain the file
at its recorded path; deletion or modification produces an unqualified label.

Comparison reports list whether the calibration passed and whether hostname,
CPU/kernel/toolchain, workers, affinity, warm-up count, timing backend and
primary catalog match. Missing configuration in older evidence is unknown,
not a match. New declarations seal the rig settings and a Unix timestamp;
the report shows the calibration's timestamp and its age at comparison
declaration when both timestamps are present. There is no automatic expiry.
Choose a fresh calibration after changes to host load or operating conditions.

Calibration qualification is a label separate from the optimization verdict.
A missing, unreadable, changed, failed or mismatched calibration produces an
unqualified label; it does not suppress the comparison or change its exit
status. `--verdict-only` includes this label too. Invalid comparison evidence
still fails validation. Library callers must load the calibration assessment
before rendering; an unchecked reference is labeled unqualified.

Run `verdict` after calibration: exit 0 means qualified, 1 means outside the
interval, and 2 means invalid evidence. A failed calibration does not qualify
the setup, even if a later campaign has a narrow interval. Use separate sealed
campaigns for confirmation and candidate comparisons; do not pool their runs.
The existing evidence tiers and fixed-confirmation rules apply on both systems.

Each campaign retains `host.json`, raw measurements and stderr under
`measurements/`. The host snapshot and frozen measurement helper are covered
by the campaign seal. Both platforms freeze GNU `time`, which supplies wall
time, CPU time and peak RSS. Campaigns without the recorded backend cannot
resume; declare a new campaign after changing backends.

Receipts store RSS in bytes; the recorder receives KiB. Full host probes run
once, when declaring the campaign. Darwin runs collect only thermal and host
swap observations before and after each timing. These probes run outside the
timed interval. The campaign records requested target affinity; the host
snapshot records the observer's effective affinity.

RSS is process residency, not physical footprint or compressed-memory usage.
Host swap observations cannot establish that a particular process never
swapped. Native measurements produce timing evidence; use the Linux pressure
harness above for enforced-memory qualification. A Linux VM on a Mac measures
the Linux guest, not the Darwin binary.

Keep the evidence directory on disk. Temporary query output is removed after
each run, including interrupted runs. Retain the receipts you need, then delete
the campaign directory to reclaim its frozen binaries, measurements and logs.

## Profile CPU time

```bash
perf record -g cargo bench
perf script | flamegraph-rs > flamegraph.svg
```

On macOS, sample a running native process separately from timing runs:

```bash
sample PID 10 -file /path/on/disk/ling.sample.txt
footprint -p PID
```

Collect a memory breakdown separately from timing:

```bash
python scripts/perf_measure.py memory PID --output /path/on/disk/ling-memory
```

On macOS, the snapshot retains byte-valued `footprint` data, region residency
from `vmmap`, current RSS from `ps`, and host compression and swap counters
from `vm_stat`. The `footprint` swapped column combines compressed and swapped
memory; these tools do not separate those quantities per process. On Linux,
the snapshot retains process `status` and `smaps_rollup` plus host `meminfo`.
Probe errors remain in `snapshot.json`; unavailable values are not zeroes.
Snapshots are sequential observations, not peaks or enforced-capacity tests.

Check access against the target process first; permissions can prevent
attachment. `xctrace` requires full Xcode; Command Line Tools alone do not
provide a working Time Profiler. Run profiling and memory inspection
separately from timing campaigns.

See [CLI Usage Guide](cli-usage-guide.md) for supported runtime controls and
[Matcher Development Guide](fsm-development.md) for the correctness contract.
