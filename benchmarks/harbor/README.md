# Harbor linguistic-agent evaluation

This benchmark asks whether Terminus-2 agents perform correct Japanese linguistic analysis with the `ling` workflow versus bare MeCab plus ordinary shell/Python tooling. Runtime is secondary and comes from Harbor traces; it is not a tokenizer throughput benchmark.

The dataset generator emits paired tasks for each arm from one case catalog. Both arms receive byte-identical input, instructions, and a tool-neutral discovery guide. The guide names the assigned analyzer through the arm-local `$ANALYZER` environment variable and directs the agent to the binary's own help and output; it does not teach either workflow. A deterministic verifier in a separate container scores strict evidence rows by exactness, precision, recall, and F1. The primary comparison is correctness within each model variant.

This is a discovery evaluation: it measures what an agent can learn from the installed analyzer. An experiment that packages arm-specific workflow guides is a guided-workflow evaluation, not a first-contact CLI comparison.

Run from the repository development shell:

```bash
nix develop .#stable
cd benchmarks/harbor
uv sync
python generate_dataset.py
export API_BASE=http://localhost:8080/v1   # your OpenAI-compatible endpoint
python run.py --experiment smoke-001 --case direct-noun-morphology --api-base "$API_BASE"
python run.py --experiment pilot-001 --catalog /path/to/eval-v1.json \
  --api-base "$API_BASE" \
  --review-evidence /path/to/eval-v1-review \
  --attempts 3 \
  --agent-timeout-sec 3600 \
  --model qwen3.8-27b-fp8:think
python report.py jobs/pilot-001
python report.py --summary jobs/pilot-001
```

Before spending calls on the paired pilot, run the Terminus completion gate from
the clean benchmark revision. It reuses one existing lingrep smoke task:

```bash
python run.py --experiment completion-probe-YYYYMMDD --api-base "$API_BASE" \
  --case direct-noun-morphology --arm lingrep --attempts 3 \
  --model qwen3.8-27b-fp8:think
python report.py jobs/completion-probe-YYYYMMDD
```

The gate is diagnostic, not a tool comparison. Continue to the paired pilot only
after all six trials reach the verifier and no completion-confirmation request is
left pending until the agent deadline. Diagnose a failure at the Terminus/endpoint
boundary. Set a timeout large enough for the declared thinking-model output budget
before the experiment begins; never change it between arms or midway through a run.

`run.py` builds and loads the two explicit flake outputs, runs every packaged oracle
lingrep patterns through the real verifier, randomizes arm order with a recorded controller seed,
and then runs trials serially. The default model is the thinking variant;
`--attempts` delegates repetition to Harbor's native `--n-attempts`. The default
per-trial agent timeout is 3600 seconds and can be changed with
`--agent-timeout-sec`; it is rendered into every task and recorded in the immutable
experiment manifest. Rerunning the same name fails unless `--resume` is supplied,
and resume succeeds only when the recorded manifest matches. The experiment keeps a
byte-identical catalog snapshot and, when `--review-evidence` is supplied, copies and
hashes its private review records. The manifest also records guide hashes, repository
and Harbor revisions, image identities, models, selected cases and arms, attempt
count, timeout, endpoint, and seed.

Credible experiment identity requires committed inputs, so `run.py` refuses a dirty worktree. Direct `generate_dataset.py` and Harbor oracle runs remain available while developing catalogs or the harness.

`run.py` puts `bin/` first on `PATH`, so Harbor's Docker provider runs `bin/docker`, which forwards to Podman and translates the Docker Compose `--project-directory` flag that `podman-compose` does not support.

`--api-base` names the OpenAI-compatible endpoint and is required; it is recorded in the manifest. `--seed` controls arm order only and is not sent to the endpoint.

`report.py` reads each child Harbor trial result and emits one JSONL row per attempt;
`--summary` emits one aggregate row per model and arm. It joins structured task
identity from `experiment.json` rather than inferring an arm from directory names.
The requested model comes from the immutable per-trial Harbor configuration and is
validated against the experiment manifest; Harbor's reported model name remains a
separate diagnostic field. Verifier correctness, completion, tokens, and time remain
separate.

The report counts `input_tokens`, `output_tokens`, and `cached_input_tokens`. Input tokens are cumulative across every model request,
so context sent again on later turns is counted again. Output tokens include hidden
thinking when the endpoint reports it. These are token counts, not inference phases:
use *prefill* and *decode* only when reporting phase timing or throughput. The group
report publishes totals and per-trial medians rather than treating wall time as a
proxy for model work.

Time is decomposed into exact endpoint API time, measured Bash-tool-call round trips,
and remaining agent/orchestration time. `command_analysis.py` treats ATIF's structured
`bash_command` calls as authoritative, parses their keystrokes with
`tree-sitter-bash`, and aligns each complete call against the concatenated asciinema
v3 input stream, because a large heredoc may span more than one recording input
event. A round trip starts with the call's first injected byte and
ends when Bash returns its prompt.

The Nix OCI images expose `ling` and `mecab` through one benchmark-only compiled
launcher (`tool_launcher.rs`). The launcher runs the real Nix-store binary and records actual argv, working directory, start/end time, process identity,
exit outcome, and catalog sources under `/logs/agent/tool-invocations`. It captures
`--patterns FILE`, `--patterns=FILE` and the short `-f FILE` spelling, so a launch
issued from a Python heredoc or a generated script is observed even though it is not
shell syntax. The ordinary Bash/Python FHS remains unchanged, and neither
product binary is modified.

Both arms share the same conventional shell baseline: Bash, coreutils, `awk`, `sed`,
`grep`, `find`, `jq`, `less`, process and util-linux tools, plus `clear`/`reset`/`tput`,
`file`, `diff`/`cmp`, and `strings`. The flake check
`linguistic-eval-standard-shell` checks that every command in this list is present
in the image.

Run the deterministic analysis explicitly when a durable per-trial sidecar is useful:

```bash
python command_analysis.py jobs/pilot-001
```

Each `command-analysis.json` retains ATIF step/tool-call identity, source hashes,
parse errors, simple commands, literal arguments, pipelines, redirections, heredocs,
catalog writes and uses, `ling` subcommand counts, exact aligned batch time, and
normalized runtime analyzer invocations. `report.py` computes the same facts
directly: `pattern_catalog_uses` counts catalog references, `pattern_lifecycle`
counts each catalog's writes and the subcommands that read it, and
`lingrep_operation_counts` counts the subcommands themselves. A trial recorded
without launcher telemetry reports `runtime_telemetry: "unavailable"`.

The `tool_workflows` section classifies both `ling` and `mecab` as
`direct_cli`, `python_subprocess`, `mixed`, `runtime_only`, or `not_used`. It
records direct shell command sites, calls issued from a shell pipeline, executed
Python scripts containing literal `subprocess` invocations of the tool, and the
separate runtime invocation count. Python classification recognizes only literal
executable names in standard subprocess calls; a dynamically constructed command
is classified as `runtime_only`.

Shell calls are also classified as `lingrep`, `mecab`, `python`, combined classes such
as `lingrep+python`, or `other`, but the report names these measurements
`bash_roundtrip_seconds_by_shell_category`: they describe an outer terminal batch,
not exclusive process time. Runtime spans separately publish per-tool invocation
counts, summed `runtime_tool_process_seconds`, and interval-unioned
`runtime_tool_active_wall_seconds`. These measures do not sum:
Python may contain an analyzer child and pipeline processes may overlap. Dynamic
expansion, aliases, functions, and `eval` remain unknown shell syntax rather than
guessed runtime behavior, while an actual instrumented analyzer launch is still
observed. Do not infer tokenizer throughput from agent wall time.

Launcher overhead is outside the child duration recorded in runtime telemetry but
is part of agent wall time.

Every lingrep trial is instructed to run its final catalog directly from
`/logs/artifacts/submission`. Harbor preserves those `.lx` files alongside the
evidence, and each trial-report row includes `pattern_queries` with the relative path,
SHA-256 digest, and exact source. The group report only counts trials and artifacts;
inspect trial rows or the Harbor artifact directory for query contents. The trajectory also shows
discarded drafts and debugging commands; `pattern_queries` records only the query
on the final execution path. Runtime `ling` spans also capture each readable
`--patterns` file at launch time, so `runtime_invocations[].pattern_queries` retains
temporary or Python-generated queries that were actually executed.

Every arm also submits an executable `/logs/artifacts/submission/run-analysis`
entrypoint with the interface `run-analysis INPUT_JSONL OUTPUT_JSONL`. The verifier
replays a sanitized, read-only copy with Bubblewrap in fresh user, mount, PID, IPC,
UTS, and network namespaces. The sandbox contains the selected analyzer runtime,
the submission, one input, and one writable work directory; `/tests`, `/logs`, and
gold files are absent. The authored shell/Python/`ling` workflow is therefore scored, not only the final
evidence file.
`visible_script_f1`, optional `hidden_script_f1`, and `script_exact` report the
replay result; `script_valid` distinguishes an executable submission from a
correctly formatted static answer.

`catalogs/smoke.json` contains self-authored fixtures that validate the machinery.
[`catalogs/worked-example.json`](catalogs/worked-example.json) is executable
documentation: its six cases demonstrate every capability label, positive and hard
negative sentences, repeated-surface alignment, multi-token spans, and contextual
classification. Both catalogs are public examples, not held-out evaluation data and
not independently reviewed gold.

Both tracked catalogs carry an `oracle_query` per case: a catalog that reproduces
that case's gold exactly, on the visible input and on its driver when it has one.
`oracle_preflight.py` runs each oracle query and fails if it does not reproduce the
gold. The same field enables catalog replay for the case: generation writes the
replay configuration, the oracle catalog, and the instruction paragraph from it, and
the preflight reads its requirement from the generated task. A case without an
`oracle_query` is scored only through the executable replay.

The `capability` value names the reasoning demand the task is designed to exercise;
it is not by itself a reward. A case carrying an `oracle_query` additionally replays
exactly one submitted `query.lx` in the verifier image. Published definition names
must equal the declared label vocabulary; a definition's published name is its own
name unless it declares `output_name`, which is the only way to publish a label that
is not a legal definition identifier. `visible_query_f1` compares the catalog's
published matches — its own `policy` block included — with visible gold, so consumed
context, relabeling, manual false-positive removal, and evidence not produced by the
catalog are observable. A case may carry a verifier-only `driver`; then
`hidden_query_f1` measures the same unchanged catalog on those held-out sentences.
`query_exact` requires exact visible replay and, when present, exact hidden replay.
MeCab has no corresponding declarative query artifact, so its reproducibility and
hidden generalization are measured through the common executable replay instead.

The verifier does not recompute spans. `ling` publishes each record's code-point
range as `char_span`, tracked through any span rewrite, and the query run projects
`@document.eval_document_id` into the record, so replay reads both off the record
and refuses a run that omits either. `run-analysis` and `query.lx` replay therefore normalize identically.

Both arms replay the same submitted `run-analysis` on visible input and, when a
case has a driver, on hidden input. lingrep retains direct `query.lx` replay as the
stricter check that the catalog's own publication produces the claimed records;
`run-analysis` does not replace that language-level check.

The worked sentence-final case carries a small hidden driver with punctuation
variants and an embedded negative. It exercises the mechanism and is not evidence
of broad linguistic generalization. Larger
drivers require independently reviewed gold.
The labels can overlap in real tasks, and one authored example per label does not
establish per-capability benchmark coverage.

Evaluation catalogs are supplied with `--catalog`. Each case declares a non-empty,
unique `labels` vocabulary; generation rejects target labels outside it and prints
the exact permitted vocabulary in the task. Targets use explicit zero-based,
half-open Unicode code-point spans plus an author-facing `surface`; generation
requires NFC-normalized sentences and rejects spans that are out of bounds or
select a different surface, so a sentence that repeats the same text has no
ambiguous target. An empty `targets`
list is a valid negative case.

## Diagnostic catalog design

A publishable catalog must measure analysis of the supplied data, not recall of a
textbook definition. Each retained case should satisfy all of these conditions:

- Gold depends on evidence the installed analyzer exposes, such as dictionary-version
  segmentation, feature values, or a construction assembled from multiple tokens.
- Positive and hard-negative candidates share surface material; raw string matching
  or a memorized example is insufficient.
- The input contains enough candidates that every span must be processed, while
  remaining small enough for two linguists to annotate independently.
- The phenomenon states the complete span boundary, accepted inflectional variants,
  label vocabulary, and treatment of genuinely ambiguous readings.
- At least one development model is below ceiling without being dominated by format
  mistakes or timeouts. Adjust the development set, then freeze a separate hidden
  driver set; do not tune against driver answers.

Prefer analyzer-defined distinctions when the benchmark question is whether `ling` makes
those distinctions easier to use. For higher-level linguistic judgments, require
analyzer evidence but keep the adjudicated linguistic reading—not either tool's
output—as gold. Reject cases that independent reviewers cannot resolve from the
supplied context.

Keep an evaluation catalog private while guides and annotations are calibrated, freeze and hash it for the declared run, then publish it afterward if reproducibility is desired. Do not publish scores as a general capability claim until independent linguists review and adjudicate the gold annotations. The agent container receives only the task input and instruction; gold remains in Harbor's separate verifier image.

Prepare blind reviewer materials and compare two returned annotations with:

```bash
python review.py prepare /private/eval-v1.candidate.json /private/eval-v1-review
python review.py compare \
  /private/eval-v1-review/packet.json \
  /private/annotator-a.json /private/annotator-b.json \
  > /private/eval-v1-comparison.json
```

The packet excludes authored targets and binds the source catalog plus
[`guides/annotation-guide.md`](guides/annotation-guide.md) by SHA-256. The comparison
validates both independent records and identifies cases requiring human adjudication;
it never chooses an annotation automatically.

## Reviewing ATIF traces

`report.py` and Harbor's recorded JSON remain authoritative. For qualitative local
review, export completed trials into the directory shape accepted by ATIF Trajectory
Viewer:

```bash
python export_viewer.py jobs/pilot-001 /tmp/pilot-001-viewer.zip
```

Upload that zip to a locally built checkout of
[ATIF Trajectory Viewer](https://github.com/Slimshilin/ATIF-trajectory-viewer).
Compatibility was characterized against viewer commit
`6bdcc65904f0926e04670d9fb746c0954b816661` and Harbor traces declaring
`ATIF-v1.7`. The exporter packages agent-visible task files, trajectories, trial
configuration, results, verifier logs, submitted artifacts (including final lingrep patterns),
and a generated `command-analysis.json`. Existing Harbor `analysis.json`/`.md` files
are included when present. It excludes task `tests/`, `solution/`, and
gold data. It does not translate ATIF or collapse this benchmark's multi-dimensional
rewards into the viewer's scalar reward display.

The viewer therefore shows these trials as completed with no scalar reward. Read
correctness and operational exceptions from `report.py`, not from the viewer's
status badge. Inferred filesystem state and any viewer-generated failure taxonomy
are review aids, not measured results.

Harbor's `harbor analyze` produces per-trial and aggregate `analysis.json` from
custom rubrics. In the pinned Harbor revision it runs through the Claude Agent SDK
and requires `ANTHROPIC_API_KEY`, so it cannot use this benchmark's
OpenAI-compatible endpoint.
