# Harbor linguistic-agent evaluation

This benchmark asks whether Terminus-2 agents perform correct Japanese linguistic analysis with the `ling` workflow versus bare MeCab plus ordinary shell/Python tooling. Runtime is secondary and comes from Harbor traces; it is not a tokenizer throughput benchmark.

The dataset generator emits paired tasks for each arm from one case catalog. Both arms receive byte-identical input and the same tool-neutral discovery guide. An experiment declares one of two protocols with `--protocol`, recorded in the manifest and in every `task.toml`. Under the default `common-analysis` both arms receive byte-identical instructions and are scored on the executable replay alone. Under `catalog-authoring` the lingrep instructions add one paragraph requiring a `query.lx` catalog, scored as a separate dimension. The guide names the assigned analyzer through the arm-local `$ANALYZER` environment variable and directs the agent to the binary's own help and output; it does not teach either workflow. A deterministic verifier in a separate container scores strict evidence rows by exactness, precision, recall, and F1. The primary outcomes are `visible_script_f1` and, where a case has a hidden driver, `hidden_script_f1`; the primary comparison is the case-level paired difference between arms within each model variant.

This is a discovery evaluation: it measures what an agent can learn from the installed analyzer. An experiment that packages arm-specific workflow guides is a guided-workflow evaluation, not a first-contact CLI comparison.

Run from the repository development shell. `./ci.sh --fast` runs the harness tests;
directly:

```bash
nix develop .#stable
cd benchmarks/harbor
uv sync --group dev
uv run --frozen python -m pytest -q tests
python generate_dataset.py
export API_BASE=http://localhost:8080/v1   # your OpenAI-compatible endpoint
python run.py --experiment smoke-001 --case direct-noun-morphology --api-base "$API_BASE"
python run.py --experiment pilot-001 --catalog /path/to/eval-v1.json \
  --api-base "$API_BASE" \
  --review-evidence /path/to/eval-v1-review \
  --repetitions 3 \
  --agent-timeout-sec 3600 \
  --model qwen3.8-27b-fp8:think
python report.py jobs/pilot-001
python report.py --summary jobs/pilot-001
```

Before spending calls on the paired pilot, run the Terminus completion gate from
the clean benchmark revision. It reuses one existing lingrep smoke task:

```bash
python run.py --experiment completion-probe-YYYYMMDD --api-base "$API_BASE" \
  --case direct-noun-morphology --arm lingrep --repetitions 3 \
  --model qwen3.8-27b-fp8:think
python report.py jobs/completion-probe-YYYYMMDD
```

The gate is diagnostic, not a tool comparison. Continue to the paired pilot only
after every trial reaches the verifier and no completion-confirmation request is
left pending until the agent deadline. Diagnose a failure at the Terminus/endpoint
boundary. Set a timeout large enough for the declared thinking-model output budget
before the experiment begins; never change it between arms or midway through a run.

`run.py` builds and loads the two explicit flake outputs, generates the selected
cases and arms, replays each generated task's packaged oracle solution through the
real verifier, and then runs trials serially. For each
case, `--repetitions` paired runs follow each other; the recorded controller seed picks
the arm that opens the first pair and later pairs alternate. Each run is one Harbor
job named `<case>-<arm>-r<repetition>`. The default model is the thinking variant.
The default per-trial agent timeout is 3600 seconds and can be changed with
`--agent-timeout-sec`; it is rendered into every task and recorded in the immutable
experiment manifest. Rerunning the same name fails unless `--resume` is supplied,
and resume succeeds only when the recorded manifest matches. Resume and `--case`
selection are validated before any task is generated or model called. Tasks are
generated into a staging tree from the stored catalog snapshot; on resume that tree
must match the existing one byte for byte, and a mismatch is reported with the
existing tree left as is. `controller.lock` in the experiment directory is held for
the run, so two controllers cannot share an experiment. The experiment keeps a
byte-identical catalog snapshot and, when `--review-evidence` is supplied, copies and
hashes its private review records and checks them with `review.py validate` (see the
annotation guide); the manifest then records `review_status: reviewed`, otherwise
`unreviewed`. The manifest also records guide hashes, repository
and Harbor revisions, image identities, dictionary source identities, models,
selected cases and arms, protocol, repetitions, timeout, endpoint, seed, and the
launching host (CPU model, logical CPUs, memory, kernel, Podman version), which
a resume must match.

Both image builds use the same pinned UniDic source. The Harbor Vibrato
dictionary is converted from that source; ordinary CLI packages use the prebuilt
dictionary release.
Before generating tasks or making model calls, `run.py` reads that source identity
from each loaded image and rejects missing or unequal identities. Older images
without the source label must be rebuilt; `--no-build` does not bypass this check.

Credible experiment identity requires committed inputs, so `run.py` rejects a dirty worktree. Direct `generate_dataset.py` and Harbor oracle runs remain available while developing catalogs or the harness.

Trials run on Harbor's Podman environment (`--env podman`), and `run.py` and the
preflight call `podman` directly. Harbor's preflight runs `podman compose ls`, which
requires a Compose V2 provider such as `docker-compose` on `PATH` and the user Podman
socket (`systemctl --user enable --now podman.socket`); `podman-compose` alone fails
it. The Linux dev shell provides `docker-compose`.

`--api-base` names the OpenAI-compatible endpoint and is required; it is recorded in the manifest. `--seed` controls which arm opens each case's first pair and is not sent to the endpoint.

`report.py` reads each child Harbor trial result and emits one JSONL row per attempt;
`--summary` emits one aggregate row per model and arm. Trials are discovered through
Harbor's `<model>/<job>/<trial>` layout, so files under `artifacts/` are never read
as results, and a trial without a readable `result.json` is listed as incomplete.
`--reconcile` emits the planned model × task × repetition matrix with per-cell status
(`complete`, `missing`, `partial`, `surplus`); `--summary` rows carry
`planned_trials` and `matrix_complete`. `job_name` and `trial_name` are the stable
trial identity; `attempt` is the chronological ordinal within a model and task. The
report joins structured task identity from `experiment.json` rather than inferring an
arm from directory names.
`--paired` emits, per model and primary metric, each case's lingrep-minus-MeCab
difference of per-arm repetition means, the mean of those differences with a
percentile bootstrap interval over cases, and the count of cases favouring each arm.
`--revisions` emits one row per revision trial beside the base trial of the same
model, arm, case and attempt: the revision's agent time, request count and Bash calls,
both trials' primary scores, and the unified-diff line count between the submitted
`run-analysis` (and `query.lx` for lingrep) and the reference artifact the revision
started from.
A trial is an infrastructure failure when Harbor recorded an exception outside the
agent's own run, the verifier produced no rewards, or any applicable verifier
dimension has outcome `infrastructure_failure`: a verifier-owned input it could not
read, which the verifier also records on the dimensions that depended on it. Such
trials are excluded from the paired differences and from the `--summary` means,
counted as `infrastructure_failures` per arm and per reconciliation cell, and leave
the cell `partial`. An agent timeout or a non-zero agent exit is still verified and
scores zero. A case with usable trials in one arm only is listed as unpaired.
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
ends when Bash returns its prompt. Each trial row also carries `phase_seconds`
(environment setup, agent setup, agent execution, verifier), the longest single
model request, and the longest gap between trajectory steps with the step it
preceded, so a stalled trial is distinguishable from a slow one.

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
SHA-256 digest, and exact source. Each row also measures the submitted artifacts as
`artifact_complexity`: bytes and non-blank lines of `run-analysis`, plus AST node and
branch counts when it is Python, and for `query.lx` bytes, definition count and the
attribute keys it references (read textually). The group report carries per-arm
medians and, beside them, the same medians for the packaged reference artifacts of
the observed cases as a floor; it otherwise only counts trials and artifacts, so
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
evidence file. Every arm also writes `verdict.json`, stating whether the phenomenon
is answerable from the material with the installed analysis; the `verdict` dimension
scores it against the case's declared `answerable` as `verdict_correct`. An
unanswerable case has empty gold, so emitting nothing is necessary but not
sufficient: the judgement itself is scored. These three files are the whole
submission; an `analysis.md` beside them is kept with the trial artifacts and not
scored.
`visible_script_f1`, optional `hidden_script_f1`, and `script_exact` report the
replay result; `script_valid` distinguishes an executable submission from a
correctly formatted static answer.

The verifier also writes `outcome.json`, recording per dimension (`evidence`,
`verdict`, `query_visible`, `query_hidden`, `script_visible`, `script_hidden`) whether it
applies and its outcome: `scored`, `invalid_submission`, `replay_failed`,
`replay_timeout`, `infrastructure_failure` or `not_reached`. Applicable reward keys
start at zero, so a hidden dimension that was never reached stays in the
denominator; `--summary` reports `hidden_*_replay_trials`, `hidden_*_scored_trials`,
`mean_hidden_*_f1` (unconditional) and `mean_hidden_*_f1_when_scored`. Each replay
has a 30-second budget; a task's verifier deadline is the sum of its replays plus 30
seconds, and the preflight rejects a `task.toml` whose `verifier.timeout_sec` does
not match.

`catalogs/smoke.json` contains self-authored fixtures that validate the machinery,
including an empty-gold negative case.
[`catalogs/worked-example.json`](catalogs/worked-example.json) is executable
documentation: its six cases cover every capability label, positive and hard
negative sentences, repeated-surface alignment, multi-token spans, and contextual
classification. Both catalogs are public examples, not held-out evaluation data and
not independently reviewed gold.

Every case carries a reference solution for each arm: `oracle_query`, a lingrep
catalog, and `oracle_mecab`, a Python `select(tokens, document)` over MeCab tokens that
`mecab_reference.py` runs. The generator packages each as the arm's `run-analysis`
with its support file, and the packaged `solve.sh` runs it on the task input, so the
reference evidence is tool output rather than copied gold. `oracle_preflight.py`
first checks every packaged catalog with host `ling check --schema
unidic-morphology`, including unavailable attributes, before any container run. It
then builds each task's environment and verifier images, runs the solution through
both, and fails if any declared reward is not reached on the visible input or on the
driver when the case has one. Put the current host `ling` on `PATH` before running
it. Under `catalog-authoring` the packaged `query.lx` is also the scored catalog:
generation writes the replay configuration and the instruction paragraph together,
and the preflight reads the requirement from the generated task.
`./ci.sh --harbor-containers` runs the preflight over both tracked catalogs and
`tests/test_containers.py`, which runs the verifier's Bubblewrap sandbox inside each
image and asserts that a replayed `run-analysis` cannot reach `/tests`, `/logs`, a
host mount, or any network interface. The lane requires Podman and the Nix image
builds and is not part of the push gate.

The `capability` value names the reasoning demand the task is designed to exercise;
it is not by itself a reward. Under `catalog-authoring`, a lingrep task
additionally replays one submitted `query.lx` in the verifier image. Published definition names
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
and rejects a run that omits either. `run-analysis` and `query.lx` replay therefore normalize identically.

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
the exact permitted vocabulary in the task. A case lists `documents`, each with an
`eval_document_id`, `sentences`, and optional scalar `metadata`; both arms receive
the documents as one JSONL line each, and `ling` exposes the metadata as
`@document.*` for `--where`. Targets name their document and use explicit
zero-based, half-open Unicode code-point spans plus an author-facing `surface`;
generation requires NFC-normalized sentences and rejects spans that are out of
bounds or select a different surface, so a sentence that repeats the same text has
no ambiguous target. An empty `targets` list is a valid negative case.

A case may declare a Soranoha `source`: the plaintext archive URL and SHA-256, its
release head, and for each document the work identifier and the sentence range
taken from it. `soranoha_source.py extract` derives `documents` and the `source`
block from a downloaded archive, framing sentences with the same rule as
`--input-format soranoha`, so neither arm segments text itself; `soranoha_source.py
verify CATALOG ARCHIVE` re-derives every case that names that archive. The manifest
records the selected cases' sources as `case_sources`. A case whose phenomenon
counts only some documents declares the `--where` clause as `oracle_where`, which
the lingrep reference script passes; such a case generates only under
`common-analysis`, because the catalog-authoring replay runs `query.lx` alone.
`catalogs/soranoha.json` holds the Soranoha-sourced cases.

A case may carry a `revision`: a changed condition (`instruction`), its `targets`
over the same documents and labels, `driver_targets` when the case has a driver, and
revised `oracle_query`, `oracle_mecab` and optional `oracle_where`. The generator
emits a second task per arm, `<case>.revision`, whose environment carries the base
case's packaged reference `run-analysis` and support files under `/data/prior/` and
whose instruction states the original phenomenon and the changed condition. Both arms
receive byte-identical revision instructions; the start state is the reference
artifact, not an agent's first attempt, so repetitions begin from the same point. The
verifier scores the revised submission against the revised gold with the ordinary
dimensions, `oracle_preflight.py` replays the revised oracle like any task, and the
manifest records under `revisions` that the agent performs the revision.

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

## Side-by-side case pages

`python export_pairs.py jobs/pilot-001 /tmp/pilot-001-pages` renders, offline, one
Markdown page per case from the report rows: the instruction, a table of every trial's
scores, time, requests and evidence agreement with gold, each arm's `run-analysis`
and `query.lx`, the evidence rows that miss or exceed gold, and for a revision the
diff from the reference artifact. The pages quote gold-derived rows, so they are for
the demo and the paper, not for distribution before the catalog is public.

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
custom rubrics and, since v0.24.0, accepts `--agent` and `--model`; it has not been
run against this benchmark's endpoint. `harbor view` serves a web viewer over a jobs
directory without presenting this benchmark's multi-dimensional rewards or
`outcome.json`.
