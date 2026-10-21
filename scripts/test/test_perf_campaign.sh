#!/usr/bin/env bash
#
# What is decidable about the campaign orchestrator without spending twenty
# minutes of measurement: its refusals, its balancing, and the pins that make
# a measurement legitimate.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/perf_campaign.sh"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

expect_refusal() {
    local expected_message=$1
    shift
    local output status
    set +e
    output=$("$@" 2>&1)
    status=$?
    set -e
    if [ "$status" -ne 2 ]; then
        echo "expected exit 2 from: $*" >&2
        echo "got $status: $output" >&2
        exit 1
    fi
    if ! grep -qF -e "$expected_message" <<< "$output"; then
        echo "expected '$expected_message' from: $*" >&2
        echo "got: $output" >&2
        exit 1
    fi
}

# A bare invocation prints usage rather than doing something.
expect_refusal "perf_campaign.sh new" "$script"

expect_refusal "--dir is required" "$script" new --campaign x
expect_refusal "--baseline REF is required" "$script" new --dir "$scratch/c" --campaign x
expect_refusal "--corpus ARTIFACT or --input FILE is required" \
    "$script" new --dir "$scratch/c" --campaign x --baseline HEAD

# Per-catalog prepared artifacts are sealed overrides of a required default.
# Refuse malformed and ambiguous declarations before building either arm.
mkdir -p "$scratch/default.lingcorpus" "$scratch/dependency.lingcorpus"
expect_refusal "--corpus and --input select different workload kinds" \
    "$script" new --dir "$scratch/both" --campaign x --baseline HEAD \
    --corpus "$scratch/default.lingcorpus" --input "$scratch/input.jsonl"
expect_refusal "overrides require a default --corpus" \
    "$script" new --dir "$scratch/direct-override" --campaign x --baseline HEAD \
    --input "$scratch/input.jsonl" \
    --catalog-corpus "patterns/ud-dependency.lx=$scratch/dependency.lingcorpus"
expect_refusal "takes CATALOG=ARTIFACT" \
    "$script" new --dir "$scratch/malformed" --campaign x --baseline HEAD \
    --corpus "$scratch/default.lingcorpus" --catalog-corpus broken
expect_refusal "non-empty CATALOG=ARTIFACT" \
    "$script" new --dir "$scratch/empty-catalog" --campaign x --baseline HEAD \
    --corpus "$scratch/default.lingcorpus" --catalog-corpus "=$scratch/dependency.lingcorpus"
expect_refusal "names patterns/ud-dependency.lx more than once" \
    "$script" new --dir "$scratch/duplicate" --campaign x --baseline HEAD \
    --corpus "$scratch/default.lingcorpus" \
    --catalog-corpus "patterns/ud-dependency.lx=$scratch/dependency.lingcorpus" \
    --catalog-corpus "patterns/ud-dependency.lx=$scratch/default.lingcorpus"
expect_refusal "catalog corpus $scratch/missing.lingcorpus does not exist" \
    "$script" new --dir "$scratch/missing" --campaign x --baseline HEAD \
    --corpus "$scratch/default.lingcorpus" \
    --catalog-corpus "patterns/ud-dependency.lx=$scratch/missing.lingcorpus"
expect_refusal "default corpus $scratch/no-default.lingcorpus does not exist" \
    "$script" new --dir "$scratch/missing-default" --campaign x --baseline HEAD \
    --corpus "$scratch/no-default.lingcorpus"

# --affinity defaults to a pinned range and taskset is Linux-only, so without
# this the first sign of trouble on any other host is exit 127 from the first
# measured command -- after both arms are built and the manifest is sealed.
# Simulated by dropping every PATH entry that supplies taskset -- every one,
# because `command -v` reports only the first and the refusal must not be
# satisfied by a later duplicate.
path_without_taskset=$(
    while IFS= read -r entry; do
        [ -x "$entry/taskset" ] || printf '%s\n' "$entry"
    done < <(printf '%s' "$PATH" | tr ':' '\n') | paste -sd: -
)
[ -z "$(PATH=$path_without_taskset command -v taskset 2>/dev/null)" ] ||
    { echo "test bug: taskset still reachable on the filtered PATH" >&2; exit 1; }
expect_refusal "--affinity needs taskset" \
    env PATH="$path_without_taskset" "$script" new --dir "$scratch/no-taskset" \
    --campaign x --baseline HEAD --corpus "$scratch/default.lingcorpus"
expect_refusal "--digest-affinity needs taskset" \
    env PATH="$path_without_taskset" "$script" new --dir "$scratch/no-taskset-digest" \
    --campaign x --baseline HEAD --corpus "$scratch/default.lingcorpus" \
    --affinity '' --digest-affinity 0-3

# A campaign directory is created once. Re-declaring in place is re-declaring
# after the numbers are in.
mkdir -p "$scratch/existing"
expect_refusal "already exists" \
    "$script" new --dir "$scratch/existing" --campaign x --baseline HEAD --corpus a.vp

expect_refusal "--dir is required" "$script" run --catalog patterns/sfe.lx --blocks 3
expect_refusal "--blocks N or --screen is required" \
    "$script" run --dir "$scratch/nope" --catalog patterns/sfe.lx
expect_refusal "is not a campaign directory" \
    "$script" run --dir "$scratch/nope" --catalog patterns/sfe.lx --blocks 3
expect_refusal "--dir is required" "$script" verdict

# Balancing. Always starting with A leaves the first position confounded with
# the arm, which is the one thing balancing exists to remove.
PERF_CAMPAIGN_LIB_ONLY=1 source "$script"
test "$(block_arms 0)" = "a b b a"
test "$(block_arms 1)" = "b a a b"
test "$(block_arms 2)" = "a b b a"
test "$(block_arms 3)" = "b a a b"

# Pins a legitimate measurement depends on. Each of these silently changes
# what is being measured if it is dropped.
grep -qF -- '--limit all' "$script"       # a truncated run measures the limit
grep -qF -- '--progress never' "$script"  # progress writes to the measured process
grep -qF -- 'taskset -c' "$script"        # the two CCDs have different caches
grep -qF -- 'CARGO_TARGET_DIR="$tool_target"' "$script"

# The tool is built into its own target directory. Sharing one with the arms
# would rebuild `ling` on every alternation at `lto = "fat"`, and is how
# an instrumented binary ends up being measured.
grep -qF -- 'cargo build --release -p lingrep-perf --features screen' "$script"
grep -qF -- 'cargo build --release -p lingrep' "$script"

# The shell holds no model of block layout. It once did, and the two models
# disagreed: a screen at block 0 is two runs and a promoted block is four, so
# the shell wrote four runs where the recorder counted two and the third was
# refused as out of sequence. Every position now comes from the planner.
grep -qF -- 'next_block next_order next_width' "$script"
grep -qF -- 'plan --campaign' "$script"
if grep -qE '^\s*(local )?width=[0-9]' "$script"; then
    echo "the orchestrator must not compute a block width; ask the planner" >&2
    exit 1
fi

# The shell script does not construct or model the command, for the same reason: every
# argument comes from the sealed template, not from a flag list in
# `workload.env`, which the manifest digest does not cover.
grep -qF -- 'template --campaign' "$script"
grep -qF -- 'command_template+=(--corpus "{corpus}")' "$script"
grep -qF -- '--default-corpus "$corpus"' "$script"
grep -qF -- 'workload_flags+=(--catalog-corpus "$entry")' "$script"
if grep -qF -- 'TARGET_FLAGS' "$script"; then
    echo "the command must come from the manifest, not from workload.env" >&2
    exit 1
fi

# The streaming pipeline must judge both processes. `$?` alone is the hasher's,
# so a failed run whose hasher succeeded records as good; `PIPESTATUS[0]` alone
# is the target's, so a hasher that died after consuming its input lets an
# empty digest be recorded against a run that succeeded.
grep -qF -- 'PIPESTATUS[@]' "$script"
grep -qF -- 'statuses[0]' "$script"
grep -qF -- 'statuses[1]' "$script"
if grep -qF -- '${PIPESTATUS[0]}' "$script"; then
    echo "the streaming pipeline must require both statuses, not only the target's" >&2
    exit 1
fi

# Affinity complement, so the hasher never lands on the measured cores.
test "$(complement_affinity 0-7)" = "8-$(($(nproc --all) - 1))"
test -z "$(complement_affinity 8-15)"
test -z "$(complement_affinity 0-$(($(nproc --all) - 1)))"
test -z "$(complement_affinity 0,2,4)"

# `suite` loops, because escalation is discovered rather than known in advance.
grep -qF -- 'did not converge' "$script"

echo "perf_campaign self-test passed"
