# Performance and Functionality Requirements

> These requirements describe goals to develop towards. They are
> directional targets that prioritize where optimization effort should go,
> not hard merge-blocking gates. Repeated use of "must" below reflects intent
> and priority, not a rule that work is rejected if a number is missed. When a
> target is not met, record the actual measurement, the trade-off involved, and
> a plan for closing the gap, then proceed if the change is a net improvement.
>
> These requirements cover the supported `CompiledCatalog` matcher and compressed
> V1 prepared artifacts.

## Core Principles

1. **Aim to be faster than the vibrato baseline** - prioritize this direction
2. **Aim for memory growth that is sub-linear or constant** in dataset size
3. **Prepared-artifact creation may trade one bounded build for faster repeated queries**
4. **All binaries should work on all corpus sizes** - From <100K to >10M sentences

---

## Global Requirements

### Performance Baseline (vibrato)

All processing is compared against vibrato tokenization plus matching:

```
Baseline: 200K-300K tokens/sec (single-threaded, no cache)
```

**Aim:** Features should be faster than this baseline when measured end-to-end.

### Memory Requirements

**Forbidden patterns:**
- ❌ Loading entire corpus into memory before processing
- ❌ Collecting all sentences in Vec before matching
- ❌ O(n) memory growth where n = corpus size

**Required patterns:**
- ✅ Streaming processing (constant memory)
- ✅ Chunked processing (sub-linear memory)
- ✅ Lazy loading (only load what's needed)
- ✅ Memory-mapped files (OS-managed memory)

**Working-set budget:** peak RSS must stay under max(2× the input file
size, the declared working-set budget). The default working-set budget is
2 GiB; a command that exposes a budget knob may be run above or below it,
and the knob's value is then the declared budget for that run.

The invariant being protected is the forbidden list above — memory must not
grow with the corpus. A bounded, corpus-independent working set (resident
aggregation buffers, per-worker batch buffers, reorder windows) does not
violate that invariant however the input size compares to it, and may be
purchased for measured speed. The 2× arm gives large inputs a proportional
ceiling; the budget arm governs small inputs.

---

## Binary-Specific Requirements

### ling

#### Use Cases
- **Interactive:** TUI exploration (small corpora, frequent runs)
- **Batch:** Large corpus processing (one-time runs)

#### Performance Bars

| Metric | Minimum | Target | Excellent |
|--------|---------|--------|-----------|
| First-run throughput | >250K tok/s | >500K tok/s | >1M tok/s |
| Prepared-artifact query throughput | >1M tok/s | >2M tok/s | >5M tok/s |
| Interactive startup time | <2s | <1s | <0.5s |
| Memory overhead (vs input) | <2× | <1.5× | <1× |

The memory row is evaluated against max(the stated multiple of input, the
declared working-set budget) — see Memory Requirements above. On an input
smaller than the budget, the budget arm governs.

#### Functionality Requirements
- ✅ Support all .lx pattern syntax
- ✅ TUI for interactive exploration
- ✅ Batch processing with progress reporting
- ✅ Supported compressed V1 prepared artifact format

#### Prepared V1 artifact requirements

- Build and query without collecting the corpus in memory.
- Preserve exact output parity with direct execution for the same admitted
  source, analyzer identity, token schema, and lingrep patterns catalog.
- Open lazily for ordinary queries; reserve complete component hashing for
  explicit verification.
- Aim for more than 500K tokens/sec when matching a prepared corpus.
- Keep the compressed token component below 25 bytes per token.


### N-gram lingrep patterns profile

`ling summarize --patterns patterns/ngrams.lx` is the supported n-gram
workflow. It inherits the `ling` streaming, memory, and prepared-artifact
requirements above. The catalog covers orders one through five and matching is
sentence-local, so no window crosses a sentence boundary.

---

#### Sentence-final lingrep patterns profile

`ling sentence-final` is the supported sentence-final
workflow. It should stream bounded batches, aim to exceed 300K tokens/sec
end-to-end, and keep peak RSS below both 2 GiB and 2× the input-file size.
Long-running invocations report progress at least every 100K sentences.

---

### External gravity example

`examples/gravity.py` demonstrates corpus-global analysis over a complete
`ling matches` JSONL stream. It is not a packaged production component
and has no throughput or memory acceptance bar. Its distinct-term and
adjacent-pair maps are in memory; use is limited to moderate corpora. See
[the gravity analysis guide](gravity-analysis.md).

The supported performance contract ends at the `ling` command producing
that stream. Large-corpus gravity is outside this repository; a downstream
implementation must establish its own measured requirements and resource
bounds.

---


## Development Workflow

### Risk-adjusted performance evidence

Scale measurement effort to uncertainty and permanent engineering risk. Every
performance change still needs all of the following:

1. A representative workload and one primary metric.
2. A measured bound, or another explicit justification such as a capacity
   target, known scale increase, or complexity analysis.
3. A minimum useful effect and guardrails chosen before final measurement.
4. Correctness evidence proportional to the semantic risk.
5. At least one end-to-end comparison from a legitimate optimized build.

Choose the evidence tier before the final campaign:

| Tier | Use when | Default measurement |
|---|---|---|
| **Tier 1 — decisive simplifying substitution** | The candidate removes or preserves complexity, changes one mechanism, and adds no concurrency, unsafe code, weakened trust model, or persistent strategy surface. | One fresh-process A/B comparison for each materially different supported mode. Stop when every mode improves by at least 20% and all guardrails pass. |
| **Tier 2 — moderate effect** | The expected or first observed effect is 5–20%, or ordinary run variance could change the decision. | At least three balanced paired blocks, such as ABBA/BAAB; report the paired effect with an interval. |
| **Tier 3 — marginal, noisy, or costly** | The effect is near the minimum useful bar, the workload is unstable, or the change adds permanent complexity. | At least five balanced paired blocks, an interval, and the broader applicable guardrail matrix. |

Tier 1 is an early-stop rule, not permission to replace system evidence with a
microbenchmark. If its comparison is not decisive, escalate to Tier 2 or Tier
3. Do not select a lower tier after seeing a favorable result.

**Guardrails must match the candidate's ladder rung.** A work-elimination
candidate should hold user CPU and peak RSS near parity, and relative ceilings
(±2% user, +5% RSS) are appropriate. A parallelism-rung candidate spends
CPU and working memory to buy wall time — that is the mechanism, not a side
effect — so sealing relative user-CPU or RSS ceilings rejects the mechanism
itself. For a parallelism candidate seal instead: wall as primary; the
absolute working-set budget above as the memory guardrail; and a
parallel-efficiency floor — added CPU-seconds per wall-second saved, with
its ceiling declared in the manifest (2× is a reasonable default: a candidate
that burns more than two extra CPU-seconds per wall-second saved is buying
speed the machine cannot afford under load).

### Matcher catalog coverage

A matcher optimization must not treat one hot catalog as representative of the
whole lingrep patterns workload. The stratified guardrail suite is every catalog under
`patterns/`, each carrying a role and the runtime regime it guards. The
assignment is pinned in `every_pattern_catalog_has_a_pinned_guardrail_role`
(`lingrep-core/tests/suite/tracked_catalogs_test.rs`), which takes the set from
the tree and fails until a new catalog is given a role.

Two roles:

- **Substantive** — carries paired blocks under the tier rules and enters the
  equal-weight aggregate.
- **Floor** — a decode and publication floor and a correctness smoke case.
  Measured, but never evidence that an optimization is broadly effective.
  `test-simple.lx` is the only one.

Before retaining a matcher optimization:

1. Choose the profile-backed primary catalog and metric before implementation.
2. Compare complete canonical output for every substantive catalog in
   compressed V1 mode. Output parity is mandatory. Neutralize the fields
   `lingrep::protocol::query::RUN_VARIANT_FIELDS` names before comparing, and
   nothing else. That list is part of the `lingrep.query/v2` contract;
   `two_runs_of_one_binary_differ_only_in_the_named_run_variant_fields` checks
   that it is complete.
3. For Tier 2, run at least three balanced paired blocks on the primary and on
   every catalog whose pinned runtime regime the change can affect. Run at
   least one fresh-process paired guardrail on every remaining catalog.
4. For Tier 3, run at least five balanced paired blocks on the primary and at
   least three on every other substantive catalog.
5. A guardrail whose interval admits a regression beyond its predeclared
   ceiling is unresolved even when its mean passes. Gather more evidence on
   that catalog — escalation is per catalog, not per campaign — or reject the
   candidate; do not call it neutral.
6. Report every catalog separately and report an equal-weight geometric mean
   across the substantive catalogs. The aggregate describes broad effectiveness
   but never overrides a per-catalog regression guardrail.

### Running one

`scripts/perf_campaign.sh` executes those six rules:

```bash
just perf-campaign new --dir target/campaign-<slug> --campaign <slug> \
    --baseline HEAD~1 --corpus <morphology-artifact>.lingcorpus \
    --catalog-corpus patterns/double-wo-dep.lx=<dependency-artifact>.lingcorpus \
    --catalog-corpus patterns/ud-dependency.lx=<dependency-artifact>.lingcorpus \
    --bound memory-latency --runner-up-bound work \
    --profile-evidence "perf -F 499 …, AssertionSession::consume 47.7%" \
    --ladder-rung representation \
    --primary-catalog patterns/double-wo.lx --affected-regime Lookaround \
    --workers 8 --affinity 0-7
just perf-campaign suite   --dir target/campaign-<slug>
just perf-campaign verdict --dir target/campaign-<slug>
```

`new` seals a manifest naming the bound, the profile that named it, the rung,
the minimum useful effect, the guardrail ceilings and the command every run
executes, before anything is measured. Because the command is sealed, two
campaigns that measure different things carry different digests; `run` executes
the manifest's command, and `record` refuses a run whose argv does not
reconstruct from the seal. The worker pool is sealed too, and
`verdict` reports `occupancy = mean(user) / (mean(wall) x workers)` per catalog
beside a planning estimate of what the declared minimum implies about user
time. Occupancy is not a bound on what a CPU change can buy — `user` covers
threads outside the pool, CPU and I/O overlap, and relieving contention can
move wall by more than any linear projection — but read it before choosing
`--primary-metric`.

`--corpus` is the prepared-artifact fallback for the suite. Repeat
`--catalog-corpus CATALOG=ARTIFACT` for a catalog that needs a different stored
schema; the two dependency-relation catalogs use a dependency-bearing
artifact while the remaining catalogs use the morphology fallback. `new`
fully verifies each distinct artifact once, records every identity, and seals
the resolved catalog-to-artifact map. Each run then re-observes every declared
artifact manifest and reconstructs both `{catalog}` and the selected `{corpus}`
from that seal. The map is not stored in `workload.env`, so editing runner
state cannot change which corpus an existing campaign measures.

`--stream-output` pipes each run's stdout through `lingrep-perf digest-stream`
instead of writing it to a file and hashing that. The digest is identical
either way, so no baseline moves; the run pays for a hashing consumer, pinned
to `--digest-affinity` (by default the complement of `--affinity`), instead of
the writes. The choice is sealed, and `record` refuses a run that disagrees
with the seal.

`--stream-output` is not the default: the hasher can become the pipeline's rate
limiter, and a campaign streamed on a high-output catalog then measures
`digest-stream` rather than the matcher. Use it when disk capacity is the
binding constraint, on catalogs that do not publish at volume.

Two checks cover this. `declare` warns when `--stream-output` is combined with
a `wall` primary, because whether a catalog outruns the hasher depends on its
output rate and is not known at declare time. Adjudication then lists every
affected catalog under `Wall is externally paced`: a catalog whose
`user` interval excludes parity while its `wall` interval still admits it is
being paced by something outside the process, since a rate limiter applies
equally to both arms. The test has no threshold and also flags a catalog bound
by writing its own output on an un-streamed campaign.

`--template matches|summarize` picks the subcommand every run executes.
`matches` is the default and reports what an end-to-end record-emitting run
costs; `summarize` performs the same matching and publication without
serializing every record. Prefer it for a change to the read or match path,
where `matches --json`'s output writing is a constant in both arms'
denominators that dilutes the effect. The two templates measure different
quantities, so their ratios differ for the same change. Output parity
is the same check either way, because both emit `lingrep.query/v2`.

`suite` measures every catalog to the coverage its tier requires and continues
while the verdict names a catalog needing escalation (3 → 8 → 20 blocks).
`verdict` renders a measurement report for an external evidence archive and
leaves two sections for the author to fill in: where the bound went, and what
was ruled out.

An invalid campaign is refused rather than adjudicated, with an exit status
distinct from every verdict: a dev-profile build, an edited manifest, a
workload that changed after sealing, unbalanced arms, short coverage, or broken
output parity.

Before spending measurement time, screen the counted work:

```bash
just perf-work --json > before.json    # change something, then:
just perf-work --baseline before.json
```

If no counter moves, the change reduces cost per unit rather than eliminating
work, and needs a timing campaign. The per-catalog totals are pinned by the
`ci.sh` screen lane, so a work regression fails CI.

The `lingrep-perf.work/v1` screen includes assertion state visits and frontier
high-water plus publication-policy candidate visits, record drops, span
narrowings, and capture drops. It also includes tree-evaluator candidate
volume, as the pair `candidates_produced` / `candidates_retained` — the
candidates `evaluate` constructs and those surviving deduplication. It does not label process-global
allocator traffic, output bytes, or scheduler-dependent queue occupancy as
deterministic catalog work. V1 decode allocations need a prepared-pipeline
probe; serialization bytes belong at the real `matches`/`summarize` writer;
and a reorder high-water belongs to the stage that owns that buffer.

Synthetic patterns remain appropriate for characterization tests and
counted-work screens. They cannot replace this repository-catalog matrix in a
retention decision.

Trust weakening, concurrency, unsafe code, caching or freshness changes, and
similar semantic risks always receive full design and correctness scrutiny;
speed never lowers that bar. Their measurement defaults to Tier 3 unless a
stricter change-specific plan applies. A documented harness limitation may
justify different evidence, but the claim must be narrowed to what was
actually measured rather than treating weak evidence as a win.

### Before Writing Code

1. **Check requirements** - Find the relevant section above
2. **Identify bars** - What minimum/target/excellent metrics apply?
3. **Measure baseline** - Run current code and record performance
4. **Set targets** - Know what "done" looks like

### During Development

1. **Profile first** - Use `perf record -g` and flamegraphs
2. **Use the selected evidence tier** - Run focused benchmarks continuously;
   reserve a longer paired campaign for the tier that requires it
3. **Test on real data** - Japanese corpora, not synthetic data
4. **Validate correctness** - Run the matcher correctness gate (`./scripts/test_matcher_correctness.sh`) after changes

### Before Merging

**Required (correctness/CI gates):**
```bash
# 1. All tests pass
cargo test --lib

# 2. Matcher correctness gate passes (correctness validation)
./scripts/test_matcher_correctness.sh

# 3. No clippy warnings
cargo clippy -- -D warnings

# 4. Code is formatted
cargo fmt --check

# 5. Full CI validation
./ci.sh
```

Run the applicable focused benchmark and the evidence tier selected above;
`cargo bench` is useful when the whole benchmark suite is relevant, but it is
not required merely as ceremony for an unrelated path.

**Documentation required:**
- Update current requirements when targets or acceptance criteria change.
- Record measurements and trade-offs in the commit message or pull request.

---

## Performance Regression Testing

### What Counts as Regression?


**Critical regression:** >20% slower on any benchmark
- Should be fixed before merge when feasible
- Requires explanation and a follow-up plan in the commit/PR

**Moderate regression:** 5-20% slower on any benchmark
- Prefer to fix before merge
- Requires justification in the PR

**Minor variance:** <5% difference
- Acceptable measurement noise
- No action required

### Benchmark Suite

Run before merging any performance-related change:

```bash
# Prepared-corpus V1 storage/execution
cargo bench -p lingrep-corpus --bench prepared_tokens

# Matching throughput
cargo bench -p lingrep-core --bench fsm_performance

# Recursive source admission and first-document latency
cargo bench -p lingrep-core --bench corpus_discovery
```

Criterion stores baselines under `target/criterion/`.


## Quick Reference Checklist

### Before claiming a feature is "done":

- [ ] Approaches the minimum performance target (see tables above)
- [ ] Memory usage is sub-linear or constant
- [ ] Faster than vibrato baseline (end-to-end)
- [ ] All tests pass
- [ ] Benchmarks show improvement (not degradation)
- [ ] No clippy warnings
- [ ] Documentation updated
- [ ] Real corpus tested (not just synthetic data)

### If performance doesn't meet the targets:

1. **Document the gap** — record actual numbers and the trade-off
2. **Profile** — Find the bottleneck
3. **Optimize** — Improve the hot path
4. **Measure** — Verify improvement
5. **Repeat** — Iterate toward the targets

---

## Results Export Endpoint (XLSX)

The results-export endpoint (`GET /api/export/{workbench_id}/{revision}`) runs a
dedicated export scan over the prepared corpus. This is matching-only work,
performed off the hot path, and it runs under the coordinator's scan permit. At
the default scan concurrency of `1`, at most one export or display scan runs at
a time; a higher `with_scan_concurrency` bound relaxes this serialization.

Memory for the export is bounded. Retained rows are capped by `MAX_EXPORT_ROWS`
(100,000) × `EXPORT_KWIC_CHAR_CAP` (256 chars per field), plus the in-memory
workbook buffer. The writer also enforces Excel's 32,767-character per-cell
limit. The ignored `measure_peak_rss_full_export` harness in
`lingrep-web/src/export/workbook.rs` measures peak RSS for a full export. The
scan permit serializes the scan phase, so scans do not stack; an operator who
needs a tighter bound can lower `MAX_EXPORT_ROWS`.

### Permalinks

Permalink decode (`GET /?v=1&q=…`) inflates through a reader capped at the 1 MiB
pattern limit (`permalink::MAX_PATTERN_BYTES`), so a crafted small blob cannot
expand without bound. The complete request target is bounded by
`MAX_REQUEST_TARGET_LEN` (32,000 characters); larger patterns are refused at
copy time rather than truncated.

## Summary

These are goals to develop towards, not hard gates.

**Primary goal:** Be faster than the vibrato baseline.

**Secondary goal:** Keep memory growth sub-linear or constant in corpus size.

**Testing goal:** Keep the matcher correctness gate passing; correctness is a
merge gate.
