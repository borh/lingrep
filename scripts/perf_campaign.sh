#!/usr/bin/env bash
#
# Run a paired performance campaign.
#
# This is the orchestration half of the tooling: build and freeze the arms,
# pin to one CCD, interleave the runs, read the numbers. The other half —
# digests, output normalization, the manifest seal, and the verdict — is
# `lingrep-perf`, because a shell should not own any of them.
#
#   scripts/perf_campaign.sh new --dir CAMPAIGN --campaign slug \
#       --baseline HEAD~1 --corpus artifact.vp \
#       --catalog-corpus patterns/ud-dependency.lx=dependency-artifact.vp \
#       --bound memory-latency --profile-evidence "perf …, close 21.6%" \
#       --ladder-rung representation --primary-catalog patterns/double-wo.lx \
#       --affected-regime Lookaround
#   scripts/perf_campaign.sh suite   --dir CAMPAIGN
#   scripts/perf_campaign.sh verdict --dir CAMPAIGN
#
# `--template matches|summarize` picks the subcommand every run executes.
# `matches` is the default and pays for serializing every record; `summarize`
# does the same matching and publication without that cost, so a change to the
# read or match path is not diluted by output the change never touches.
#
# `suite` runs every catalog to the coverage its tier requires, which is what
# `lingrep-perf plan --outstanding` says. Drive one catalog by hand with:
#
#   scripts/perf_campaign.sh run --dir CAMPAIGN --catalog patterns/double-wo.lx --blocks 8
#
# `run` is resumable: it appends blocks starting after the ones already
# recorded, which is what escalating from 3 to 8 looks like.

set -euo pipefail

# The checkout this script belongs to, from its own location rather than from
# `git rev-parse --show-toplevel`. The two differ exactly when it matters: run
# a worktree's copy from anywhere else and `--show-toplevel` names the *other*
# repository, so the arms would be built from one tree and measured against
# another tree's catalogs.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# Resolve every relative campaign and catalog path from the repository root.
cd "$repo_root"

# `lingrep-perf --features screen` links `lingrep-core` with `bench-instrumentation`, and
# Cargo unifies features across one build. Sharing a target directory with the
# arms would therefore rebuild `ling` on every alternation — at
# `lto = "fat"`, minutes each way — and is the kind of setup that produces an
# instrumented binary nobody meant to measure. A separate directory removes the
# question entirely.
tool_target="$repo_root/target/perf-tool"
tool="$tool_target/release/lingrep-perf"

usage() {
    awk 'NR > 2 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "${BASH_SOURCE[0]}" >&2
}

die() {
    echo "perf_campaign: $*" >&2
    exit 2
}

build_tool() {
    CARGO_TARGET_DIR="$tool_target" \
        cargo build --release -p lingrep-perf --features screen --quiet
}

# The GNU `time` that reports maximum resident set size. The shell builtin
# does not, and a campaign without a memory guardrail is a campaign that buys
# latency with RSS and reports it as a win.
find_time_bin() {
    local candidate=${TIME_BIN:-$(type -P time || true)}
    if [ -z "$candidate" ] || [ ! -x "$candidate" ]; then
        die "GNU time not found; set TIME_BIN"
    fi
    if ! "$candidate" -v true >/dev/null 2>&1; then
        die "$candidate does not support -v; set TIME_BIN to GNU time"
    fi
    echo "$candidate"
}

# ABBA on even blocks, BAAB on odd. Alternating matters: always starting with
# A leaves the first position confounded with the arm, which is the one thing
# balancing exists to remove.
# The CPUs `--affinity` leaves free, when that is answerable.
#
# Only the simple `low-high` form starting at 0, which is what a campaign on
# this box uses: `0-7` is CCD0 with its V-Cache, and the complement is CCD1.
# Anything else prints nothing rather than guessing, because pinning the hasher
# *onto* the measured cores is worse than not pinning it at all.
complement_affinity() {
    local affinity=$1 total
    total=$(nproc --all)
    [[ $affinity =~ ^0-([0-9]+)$ ]] || return 0
    local high=${BASH_REMATCH[1]}
    if [ "$high" -lt $((total - 1)) ]; then
        printf '%d-%d' $((high + 1)) $((total - 1))
    fi
}

block_arms() {
    local block=$1
    if [ $((block % 2)) -eq 0 ]; then
        echo "a b b a"
    else
        echo "b a a b"
    fi
}

# --------------------------------------------------------------------- new

# Content addressing is the point: thirteen frozen binaries at mutable paths,
# indistinguishable from each other, is one of the failure modes this tooling
# exists to prevent. The digest comes from `lingrep-perf`, which is also what
# rehashes the arm before every run, so a frozen arm's name and its recorded
# identity cannot disagree.
freeze_arm() {
    local source_binary=$1 arms_dir=$2
    local digest frozen
    digest=$("$tool" digest "$source_binary")
    frozen="$arms_dir/${digest#blake3:}"
    cp -f "$source_binary" "$frozen"
    chmod +x "$frozen"
    echo "$frozen"
}

cmd_new() {
    local dir="" baseline="" corpus="" input="" affinity="0-7" workers=""
    local -a declare_args=() target_flags=() catalog_corpora=()
    local template=matches
    local stream_output=0 digest_affinity=
    while [ $# -gt 0 ]; do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            --baseline) baseline=$2; shift 2 ;;
            --corpus) corpus=$2; shift 2 ;;
            # Override the default prepared artifact for one catalog. The
            # mapping is repeatable and reaches the sealed manifest; `run`
            # asks Rust to resolve both holes and never carries this map in
            # mutable shell state.
            --catalog-corpus) catalog_corpora+=("$2"); shift 2 ;;
            --input) input=$2; shift 2 ;;
            --affinity) affinity=$2; shift 2 ;;
            --workers) workers=$2; shift 2 ;;
            # One extra `ling` flag, repeatable, fixed at `new` and
            # applied identically to both arms. It exists because a corpus is
            # not always measurable at the defaults: one 37 MB sentence in the
            # tracked markdown corpus exceeds `--max-tokenized-bytes`, and
            # every run over it exits non-zero and is recorded `--failed`, so
            # the campaign refuses before it adjudicates anything. Taken at
            # `new` rather than at `run` so a campaign does not change what it
            # measures halfway through, and carried into each run's execution
            # fingerprint through argv, so the two arms cannot diverge.
            #
            # It reaches the manifest as part of the sealed command template
            # below, so it is covered by the digest and `record` refuses a run
            # whose argv does not reconstruct from it.
            --target-flag) target_flags+=("$2"); shift 2 ;;
            # Which `ling` subcommand every run executes: `matches`
            # (default) or `summarize`. Sealed into the command template, so
            # two campaigns measuring different commands cannot share a digest.
            --template) template=$2; shift 2 ;;
            # Pipe each run's output through the hasher rather than writing it
            # to a file first. Nothing reads those bytes except the hasher, and
            # `distributions.lx` writes ~42 GB per run against 43 GB of free
            # disk — which is why a campaign whose bound was profiled on the
            # science corpus was measured on one 2.4x smaller.
            --stream-output) stream_output=1; shift ;;
            # Where the hasher runs. Defaults to the complement of --affinity
            # when that is a simple `low-high` range starting at 0, so the
            # consumer sits off the measured cores. Pinning does not remove
            # memory-bandwidth or shared-I/O coupling, which is what the
            # calibration measures rather than assumes.
            --digest-affinity) digest_affinity=$2; shift 2 ;;
            *) declare_args+=("$1"); shift ;;
        esac
    done
    [ -n "$dir" ] || die "--dir is required"
    [ -n "$baseline" ] || die "--baseline REF is required: the arm this change is measured against"
    [ -n "$corpus" ] || [ -n "$input" ] || die "--corpus ARTIFACT or --input FILE is required"
    [ -e "$dir" ] && die "$dir already exists; a campaign directory is created once"
    if [ -n "$corpus" ] && [ -n "$input" ]; then
        die "--corpus and --input select different workload kinds; use exactly one"
    fi
    if [ -n "$input" ] && [ ${#catalog_corpora[@]} -ne 0 ]; then
        die "--catalog-corpus overrides require a default --corpus ARTIFACT"
    fi

    # Validate the map before either release build. A malformed or duplicate
    # catalog entry is a declaration error, not a reason to spend minutes
    # compiling two arms. Artifact contents are authenticated below with the
    # candidate arm; this cheap check only establishes that every named path
    # exists and every catalog has one unambiguous override.
    local -A corpus_for_catalog=()
    local entry catalog_path artifact_path
    for entry in "${catalog_corpora[@]}"; do
        if [[ $entry != *=* ]]; then
            die "--catalog-corpus takes CATALOG=ARTIFACT, not '$entry'"
        fi
        catalog_path=${entry%%=*}
        artifact_path=${entry#*=}
        [ -n "$catalog_path" ] && [ -n "$artifact_path" ] ||
            die "--catalog-corpus takes non-empty CATALOG=ARTIFACT, not '$entry'"
        if [ "${corpus_for_catalog[$catalog_path]+present}" = present ]; then
            die "--catalog-corpus names $catalog_path more than once"
        fi
        [ -e "$artifact_path" ] || die "catalog corpus $artifact_path does not exist"
        corpus_for_catalog["$catalog_path"]=$artifact_path
    done
    if [ -n "$corpus" ]; then
        [ -e "$corpus" ] || die "default corpus $corpus does not exist"
    fi
    case "$template" in
        matches|summarize) ;;
        *) die "--template takes matches or summarize, not '$template'" ;;
    esac

    # taskset is Linux-only and --affinity defaults to a pinned range, so
    # elsewhere every measured command dies at 127 -- after both arms are
    # built and the manifest is sealed. Refused here, where a declaration
    # error is cheap. `--affinity ''` measures unpinned: a weaker campaign,
    # not a broken one, and the only honest option on a host with no
    # affinity API.
    if ! command -v taskset >/dev/null; then
        [ -z "$affinity" ] || die \
            "--affinity needs taskset, which this host lacks; pass --affinity '' to measure unpinned"
        [ -z "$digest_affinity" ] || die \
            "--digest-affinity needs taskset, which this host lacks"
    fi

    build_tool
    mkdir -p "$dir/arms"

    # Arm B first, from the working tree, because that is the change under
    # test and its build failing is the common case.
    echo "perf_campaign: building the candidate arm from the working tree" >&2
    (cd "$repo_root" && cargo build --release -p lingrep --quiet)
    local candidate_source="$repo_root/target/release/ling"
    local arm_b
    arm_b=$(freeze_arm "$candidate_source" "$dir/arms")

    # Arm A in a detached worktree, so the baseline is built from what that
    # ref actually says rather than from a stash or a reverted edit.
    local worktree="$dir/.baseline-worktree"
    echo "perf_campaign: building the baseline arm from $baseline" >&2
    git -C "$repo_root" worktree add --detach "$worktree" "$baseline" >/dev/null
    local baseline_commit
    baseline_commit=$(git -C "$worktree" rev-parse --short HEAD)
    (cd "$worktree" && cargo build --release -p lingrep --quiet)
    local arm_a
    arm_a=$(freeze_arm "$worktree/target/release/ling" "$dir/arms")
    git -C "$repo_root" worktree remove --force "$worktree"

    local candidate_commit
    candidate_commit=$(git -C "$repo_root" rev-parse --short HEAD)
    if ! git -C "$repo_root" diff --quiet; then
        candidate_commit="$candidate_commit+worktree"
    fi

    # The workload's identity, established once and sealed into the manifest.
    # A path is not an identity: a different valid artifact moved into place
    # between arms would otherwise pass every check.
    local -a workload_flags=()
    if [ -n "$corpus" ]; then
        # Full component hashing paid once, here, where the cost is
        # affordable. Per run, `record` rereads the artifact's own recorded
        # identity — component digests, dictionary, token schema, inventory —
        # which costs a manifest parse and no hashing.
        local -A verified_corpora=()
        local candidate_corpus
        local -a corpora_in_authored_order=("$corpus")
        for entry in "${catalog_corpora[@]}"; do
            corpora_in_authored_order+=("${entry#*=}")
        done
        for candidate_corpus in "${corpora_in_authored_order[@]}"; do
            if [ "${verified_corpora[$candidate_corpus]+present}" = present ]; then
                continue
            fi
            echo "perf_campaign: verifying $candidate_corpus (once, at declare time)" >&2
            "$arm_b" corpus inspect --verify "$candidate_corpus" >/dev/null
            verified_corpora["$candidate_corpus"]=1
        done
        workload_flags=(--workload-artifact "$corpus" --default-corpus "$corpus")
        local -A declared_corpora=(["$corpus"]=1)
        for entry in "${catalog_corpora[@]}"; do
            artifact_path=${entry#*=}
            if [ "${declared_corpora[$artifact_path]+present}" != present ]; then
                workload_flags+=(--workload-artifact "$artifact_path")
                declared_corpora["$artifact_path"]=1
            fi
            workload_flags+=(--catalog-corpus "$entry")
        done
    else
        workload_flags=(--workload-file "$input")
    fi

    cat > "$dir/workload.env" <<ENV
ARM_A=$arm_a
ARM_B=$arm_b
AFFINITY=$affinity
WORKERS=$workers
STREAM_OUTPUT=$stream_output
DIGEST_AFFINITY=$digest_affinity
ENV

    # The command itself, sealed into the manifest rather than reassembled at
    # `run` time. `{catalog}` is always a hole; a prepared command also carries
    # `{corpus}`, resolved from the default plus the catalog-specific overrides
    # sealed in the manifest. What used to live here was a
    # `printf %q`-quoted flag list in `workload.env`, which is a mutable file
    # the digest does not cover: two campaigns measuring different things
    # carried the same seal, and one whose flags were mangled by a quoting bug
    # measured nothing and exited 0.
    #
    # `--template` picks the subcommand. `matches` is the default and is what
    # an end-to-end record-emitting run costs; `summarize` runs the same
    # matching and publication and aggregates instead of serializing every
    # record, so it isolates a change to the read or match path from the price
    # of writing the results out.
    #
    # That difference is large enough to change a verdict. On the decode
    # owner-cursor campaign `sfe` reads 0.7388 under `matches --json`, which
    # writes 2.49 GB the change does not touch, and 0.401 under `summarize`.
    # Both are correct measurements of different commands; picking the wrong
    # one measures the serializer.
    #
    # Both emit `lingrep.query/v2`, so output parity is the same check
    # either way — `summarize --json` carries `run_id` and `elapsed_millis`
    # and nothing else that varies per run.
    local -a command_template=("$template" --patterns "{catalog}")
    if [ -n "$corpus" ]; then
        command_template+=(--corpus "{corpus}")
    else
        command_template+=("$input")
    fi
    command_template+=(
        ${target_flags[@]+"${target_flags[@]}"}
    )
    # `--limit` is a `matches` flag; `summarize` aggregates everything it
    # matches and has no such control.
    if [ "$template" = matches ]; then
        command_template+=(--limit all)
    fi
    command_template+=(
        --json
        --progress never
    )
    # An `if`, not `[ … ] && …`: under `set -e` the short form makes a campaign
    # that declares no worker count exit 1 here, before it declares anything.
    if [ -n "$workers" ]; then
        command_template+=(--workers "$workers")
    fi

    local -a stream_flag=()
    if [ "$stream_output" -eq 1 ]; then
        stream_flag=(--stream-output)
        if [ -z "$digest_affinity" ]; then
            digest_affinity=$(complement_affinity "$affinity")
        fi
    fi

    # Sealed as a number as well as a literal in the template: occupancy is
    # `user / (wall x workers)`, and the arithmetic needs the count rather
    # than the two argv words that carry it.
    local -a workers_flag=()
    if [ -n "$workers" ]; then
        workers_flag=(--workers "$workers")
    fi

    "$tool" declare \
        --out "$dir/campaign.json" \
        --arm-a "$arm_a" --arm-a-commit "$baseline_commit" \
        --arm-b "$arm_b" --arm-b-commit "$candidate_commit" \
        "${workload_flags[@]}" \
        ${workers_flag[@]+"${workers_flag[@]}"} \
        ${stream_flag[@]+"${stream_flag[@]}"} \
        "${declare_args[@]}" \
        -- "${command_template[@]}"
    echo "perf_campaign: declared. Next: $0 run --dir $dir --catalog <path> --blocks N" >&2
}

# --------------------------------------------------------------------- run

# Run the whole suite to the coverage its tier requires.
#
# The tier rules are the coverage table, and `lingrep-perf plan` is where they
# live. An operator holding twelve catalogs and three coverage rules in their
# head is an operator who measures four and publishes a short campaign, which
# is exactly what the 2026-08-18 assertion-frontier report did.
cmd_suite() {
    local dir=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            *) die "unknown flag $1" ;;
        esac
    done
    [ -n "$dir" ] || die "--dir is required"
    [ -f "$dir/campaign.json" ] || die "$dir is not a campaign directory; run `new` first"
    build_tool

    # Looped, because escalation is discovered rather than known in advance:
    # a catalog that reaches its tier minimum may then adjudicate to
    # `unresolved-guardrail`, and the next rung only becomes owed once that is
    # true. One pass would stop at minimum coverage and leave the advertised
    # 3 -> 8 -> 20 escalation to be driven by hand.
    #
    # It terminates because every pass either records runs or reports nothing
    # outstanding, and the ladder is finite. The bound is a backstop against a
    # planning bug, not a real limit.
    local pass=0
    while [ "$pass" -lt 20 ]; do
        local -a outstanding=()
        while IFS= read -r row; do
            [ -n "$row" ] || continue
            outstanding+=("$row")
        done < <("$tool" plan --campaign "$dir/campaign.json" --outstanding)

        if [ ${#outstanding[@]} -eq 0 ]; then
            echo "perf_campaign: nothing outstanding. Next: $0 verdict --dir $dir" >&2
            return 0
        fi

        local entry
        for entry in "${outstanding[@]}"; do
            IFS=$'\t' read -r catalog kind required complete partial _ _ _ reason <<< "$entry"
            echo "perf_campaign: $catalog owes $required $kind (complete $complete, partial $partial) — $reason" >&2
            if [ "$kind" = "screen" ]; then
                cmd_run --dir "$dir" --catalog "$catalog" --screen
            else
                cmd_run --dir "$dir" --catalog "$catalog" --blocks "$required"
            fi
        done
        pass=$((pass + 1))
    done
    die "planning did not converge after 20 passes; this is a bug, not a long campaign"
}

cmd_run() {
    local dir="" catalog="" blocks="" screen=0 mode=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            --catalog) catalog=$2; shift 2 ;;
            --blocks) blocks=$2; shift 2 ;;
            --screen) screen=1; shift ;;
            --mode) mode=$2; shift 2 ;;
            *) die "unknown flag $1" ;;
        esac
    done
    [ -n "$dir" ] || die "--dir is required"
    [ -n "$catalog" ] || die "--catalog is required"
    if [ "$screen" -eq 0 ] && [ -z "$blocks" ]; then
        die "--blocks N or --screen is required"
    fi
    [ -f "$dir/campaign.json" ] || die "$dir is not a campaign directory; run \`new\` first"

    build_tool
    # shellcheck disable=SC1091
    source "$dir/workload.env"
    local time_bin
    time_bin=$(find_time_bin)

    [ -n "${LINGREP_DICTIONARY_PATH:-}" ] || die "LINGREP_DICTIONARY_PATH is unset; run inside \`nix develop\`"

    # The command comes from the seal, once per catalog. Reassembling it here
    # would be a second copy of the command that can drift from the manifest,
    # which is the whole failure the seal exists to prevent — and `record`
    # would then refuse every run rather than the drift being visible here.
    #
    # Nothing about the workload is passed: a prepared artifact's identity is
    # reread by `record` from the artifact's own manifest before every run,
    # and a `File` entry is rehashed there too. Naming it again would claim
    # one identity in two places.
    local sealed_command
    sealed_command=$("$tool" template --campaign "$dir/campaign.json" --catalog "$catalog") ||
        die "$dir seals no command for $catalog; it was declared before the command was sealed \
and its runs cannot be reproduced from it"
    [ -n "$sealed_command" ] || die "the sealed command for $catalog is empty"
    mapfile -t CAMPAIGN_ARGV <<< "$sealed_command"

    # Every question about layout is the planner's. The screen at block 0 is
    # two runs and a promoted block is four; duplicating that arithmetic here
    # is how promotion came to be impossible to finish — the shell wrote four
    # runs while the recorder counted two, and the third was refused as out of
    # sequence.
    local plan_row kind required complete partial next_block next_order next_width
    local guard=0
    while true; do
        plan_row=$("$tool" plan --campaign "$dir/campaign.json" |
            grep -P "^\Q$catalog\E\t" || true)
        [ -n "$plan_row" ] || die "$catalog is not in this campaign's suite"
        IFS=$'\t' read -r _ kind required complete partial next_block next_order next_width _ \
            <<< "$plan_row"
        # A stale tool binary emitting a narrower row would otherwise be read
        # as garbage positions and produce nonsense arithmetic rather than an
        # error. Found exactly that way.
        [ "$(awk -F'\t' '{print NF}' <<< "$plan_row")" -eq 9 ] ||
            die "the plan row for $catalog has $(awk -F'\t' '{print NF}' <<< "$plan_row") \
fields, not 9; the `lingrep-perf` build in $tool_target is stale"

        # `--blocks N` and `--screen` are what the caller asked for; the plan
        # says what the campaign owes. Take the larger, so a caller can
        # escalate ahead of the verdict but never below the tier minimum.
        local wanted=$required
        if [ "$screen" -eq 0 ] && [ -n "$blocks" ] && [ "$blocks" -gt "$wanted" ]; then
            wanted=$blocks
        fi
        if [ "$complete" -ge "$wanted" ] && [ "$partial" -eq 0 ]; then
            [ "$guard" -eq 0 ] &&
                echo "perf_campaign: $catalog already has $complete of $wanted; nothing to do" >&2
            return 0
        fi

        local -a arms
        if [ "$next_width" -eq 2 ]; then
            arms=(a b)
        else
            read -r -a arms <<< "$(block_arms "$next_block")"
        fi

        local order
        for ((order = next_order; order < next_width; order++)); do
            if ! run_one "$dir" "$catalog" "${arms[order]}" "$next_block" "$order" "$mode" \
                "$time_bin"; then
                die "stopping at $catalog block $next_block order $order; nothing further \
is measured until the reason above is resolved"
            fi
        done
        echo "perf_campaign: $catalog block $next_block complete (${arms[*]})" >&2

        guard=$((guard + 1))
        [ "$guard" -gt 200 ] && die "$catalog did not converge; this is a bug, not a long run"
    done
}

run_one() {
    local dir=$1 catalog=$2 arm=$3 block=$4 order=$5 mode=$6 time_bin=$7

    local binary
    case "$arm" in
        a) binary=$ARM_A ;;
        b) binary=$ARM_B ;;
        *) die "unknown arm $arm" ;;
    esac

    local timing errors
    timing=$(mktemp)
    errors=$(mktemp)

    # Exactly what the manifest seals, read once by the caller. `record`
    # reconstructs it independently and refuses a mismatch, so a wrapper that
    # mangles this is caught rather than measured.
    local -a argv=("${CAMPAIGN_ARGV[@]}")

    local -a pin=()
    [ -n "$AFFINITY" ] && pin=(taskset -c "$AFFINITY")

    local ok=1
    local -a output_flag=()
    if [ "${STREAM_OUTPUT:-0}" -eq 1 ]; then
        # Fresh per run, and read only once both statuses are zero: a stale
        # digest from an earlier run recorded against this one would make the
        # two arms compare equal on output they never produced.
        local digest_file
        digest_file=$(mktemp)
        local -a digest_pin=()
        if [ -n "${DIGEST_AFFINITY:-}" ]; then
            digest_pin=(taskset -c "$DIGEST_AFFINITY")
        fi

        # `set +e` so the pipeline's failure does not end the script before
        # `PIPESTATUS` is read, and `PIPESTATUS` captured by the very next
        # command, because any simple command replaces it.
        set +e
        "$time_bin" -o "$timing" -v \
            "${pin[@]}" "$binary" "${argv[@]}" 2>"$errors" |
            "${digest_pin[@]}" "$tool" digest-stream > "$digest_file"
        local -a statuses=("${PIPESTATUS[@]}")
        set -e

        # Both. `$?` alone is the hasher's, so a failed run whose hasher
        # succeeded records as good; `PIPESTATUS[0]` alone is the target's, so
        # a hasher that died after consuming its input lets an empty or
        # partial digest be recorded against a run that succeeded.
        if [ "${statuses[0]}" -ne 0 ] || [ "${statuses[1]}" -ne 0 ]; then
            ok=0
        fi
        if [ "$ok" -eq 1 ]; then
            output_flag=(--output-digest "$(cat "$digest_file")")
        else
            # Never the digest of a failed run. `record --failed` needs some
            # output identity and this one is not evidence of anything, so it
            # is named as the placeholder it is; the observation is refused at
            # adjudication regardless.
            output_flag=(--output-digest "blake3:$(printf '0%.0s' {1..64})")
        fi
        rm -f "$digest_file"
    else
        local out
        out=$(mktemp)
        "$time_bin" -o "$timing" -v \
            "${pin[@]}" "$binary" "${argv[@]}" > "$out" 2>"$errors" || ok=0
        output_flag=(--output "$out")
    fi

    local wall user sys rss
    wall=$(awk -F': ' '/Elapsed \(wall clock\)/ {print $NF}' "$timing" |
        awk -F: '{ if (NF==3) print $1*3600+$2*60+$3; else if (NF==2) print $1*60+$2; else print $1 }')
    user=$(awk -F': ' '/User time/ {print $NF}' "$timing")
    sys=$(awk -F': ' '/System time/ {print $NF}' "$timing")
    rss=$(awk -F': ' '/Maximum resident set size/ {print $NF}' "$timing")

    local -a extra=()
    [ "$ok" -eq 0 ] && extra+=(--failed)
    [ -n "$mode" ] && extra+=(--mode "$mode")
    [ -n "$WORKERS" ] && extra+=(--workers "$WORKERS")

    # The dictionary is not rehashed. It lives in the Nix store, whose paths
    # are already content-addressed, so the allowlisted environment entry is
    # its identity — and hashing hundreds of megabytes before every run would
    # cost more than the measurement it protects.
    local recorded=0
    "$tool" record \
        --campaign "$dir/campaign.json" \
        --catalog "$catalog" --arm "$arm" --block "$block" --order "$order" \
        "${extra[@]}" \
        --wall "$wall" --user "$user" --sys "$sys" --peak-rss-kb "$rss" \
        --binary "$binary" "${output_flag[@]}" \
        --input "$catalog" \
        --env "LINGREP_DICTIONARY_PATH=$LINGREP_DICTIONARY_PATH" \
        --affinity "$AFFINITY" \
        -- "${argv[@]}" || recorded=1

    if [ "$ok" -eq 0 ]; then
        # Kept, and pointed at. A campaign whose runs failed is undiagnosable
        # if the reason was discarded, and this is where a harness path bug
        # shows up — the one that silently produced 0.00s rows.
        local kept="$dir/failed"
        mkdir -p "$kept"
        cp "$errors" "$kept/$(basename "$catalog")-block$block-order$order.stderr"
        echo "perf_campaign: that run exited non-zero; stderr in $kept" >&2
        tail -n 3 "$errors" >&2
    fi
    rm -f "${out:-}" "$timing" "$errors"
    # `set -e` does not apply inside a function invoked as an `if` condition,
    # so a refused `record` would otherwise be swallowed and the block
    # reported complete. It refuses precisely when the run must not be
    # counted, which makes silence the worst possible handling.
    if [ "$recorded" -ne 0 ]; then
        return 1
    fi
    return "$((1 - ok))"
}

# ----------------------------------------------------------------- verdict

cmd_plan() {
    local dir=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            *) die "unknown flag $1" ;;
        esac
    done
    [ -n "$dir" ] || die "--dir is required"
    [ -f "$dir/campaign.json" ] || die "$dir is not a campaign directory"
    build_tool
    "$tool" plan --campaign "$dir/campaign.json"
}

cmd_verdict() {
    local dir=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            *) die "unknown flag $1" ;;
        esac
    done
    [ -n "$dir" ] || die "--dir is required"
    [ -f "$dir/campaign.json" ] || die "$dir is not a campaign directory"
    build_tool
    "$tool" adjudicate --campaign "$dir/campaign.json"
}

# Sourced by the self-test, which checks the pieces that are decidable without
# spending twenty minutes of measurement.
if [ "${PERF_CAMPAIGN_LIB_ONLY:-}" = "1" ]; then
    return 0
fi

case "${1:-}" in
    new) shift; cmd_new "$@" ;;
    suite) shift; cmd_suite "$@" ;;
    run) shift; cmd_run "$@" ;;
    plan) shift; cmd_plan "$@" ;;
    verdict) shift; cmd_verdict "$@" ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
esac
