#!/usr/bin/env bash
# scripts/worktree-disk.sh — report and, when explicitly asked, reclaim
# per-worktree target/ disk usage.
#
# Reclamation follows worktree lifecycle rather than artifact age: clean build
# output in idle trees or remove worktrees already merged into main.
#
# Report mode (no args) is read-only and is the default. --clean-targets and
# --remove-merged are gated: they print exactly what would be affected,
# refuse any tree with a running cargo/rustc process, and prompt for
# confirmation unless --yes is given. The current tree and any dirty tree
# are never acted on, under any flag combination.
set -euo pipefail

SELF_REAL="$(realpath "$0")"
SCRIPT_NAME="$(basename "$SELF_REAL")"

usage() {
    cat <<EOF
Usage: $SCRIPT_NAME [--clean-targets] [--remove-merged] [--yes]

No arguments: report mode (read-only). Prints one row per worktree with its
target/ size, lifecycle class, and recommended action.

  --clean-targets   remove target/ dirs for MAIN and ACTIVE trees
  --remove-merged   'git worktree remove' MERGED trees (reclaims target/
                     with them)
  --yes             skip the confirmation prompt

Never acts on the current tree or a tree with uncommitted changes (DIRTY),
even with --yes. Refuses to act on a tree with a running cargo/rustc process.
EOF
}

DO_CLEAN=0
DO_REMOVE=0
ASSUME_YES=0

for arg in "$@"; do
    case "$arg" in
        --clean-targets) DO_CLEAN=1 ;;
        --remove-merged) DO_REMOVE=1 ;;
        --yes) ASSUME_YES=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "error: unrecognized argument: $arg" >&2
            usage >&2
            exit 1
            ;;
    esac
done

# --- worktree enumeration -------------------------------------------------
#
# Enumerate from shared git metadata, not a ".worktrees/*" glob, so this
# works from any worktree. The main checkout is always the first entry.

WT_PATH=()
WT_BRANCH=() # empty string means detached HEAD

cur_path=""
cur_branch=""
have_record=0
flush_record() {
    if [ "$have_record" -eq 1 ]; then
        WT_PATH+=("$cur_path")
        WT_BRANCH+=("$cur_branch")
    fi
}
while IFS= read -r line; do
    case "$line" in
        "worktree "*)
            flush_record
            cur_path="${line#worktree }"
            cur_branch=""
            have_record=1
            ;;
        "branch refs/heads/"*)
            cur_branch="${line#branch refs/heads/}"
            ;;
        "detached")
            cur_branch=""
            ;;
        *) : ;;
    esac
done < <(git worktree list --porcelain)
flush_record

if [ "${#WT_PATH[@]}" -eq 0 ]; then
    echo "error: 'git worktree list' returned no entries" >&2
    exit 1
fi

CURRENT_REAL="$(realpath "$(git rev-parse --show-toplevel)")"
MAIN_TREE_REAL="$(realpath "${WT_PATH[0]}")"

# Default branch, resolved but with a documented fallback: this repo's
# default branch is "main"; if origin/HEAD isn't set up, assume "main".
MAIN_BRANCH=""
if ref="$(git symbolic-ref -q refs/remotes/origin/HEAD 2>/dev/null)"; then
    MAIN_BRANCH="${ref#refs/remotes/origin/}"
fi
[ -n "$MAIN_BRANCH" ] || MAIN_BRANCH="main"

# --- per-tree helpers ------------------------------------------------------

# Populates global array TARGET_DIRS with every target/ dir under $1,
# pruning .worktrees so scanning one tree never descends into another's
# files.
target_dirs_for() {
    local tree="$1"
    mapfile -t TARGET_DIRS < <(find "$tree" -name .worktrees -prune -o -name target -type d -print 2>/dev/null)
}

# Sums the size (MB) of the dirs currently in TARGET_DIRS.
sum_target_mb() {
    local total=0 t sz
    for t in "${TARGET_DIRS[@]}"; do
        [ -n "$t" ] || continue
        # Guarded: `set -o pipefail` makes a bare assignment propagate du's
        # exit status (e.g. a permission-denied subtree), and `set -e` would
        # then kill the whole report with no output. The ${sz:-0} fallback
        # alone is NOT enough -- it is unreachable once set -e fires.
        if ! sz="$(du -sm "$t" 2>/dev/null | cut -f1)"; then
            sz=0
        fi
        total=$((total + ${sz:-0}))
    done
    printf '%s\n' "$total"
}

# Classifies a non-current, non-main tree as DIRTY, MERGED, or ACTIVE.
# Detached HEAD or an unresolvable comparison against $MAIN_BRANCH must
# never be classified MERGED -- deleting unmerged work is unrecoverable.
classify_tree() {
    local tree="$1" branch="$2"
    local dirty unmerged

    # A worktree whose directory no longer exists is still listed by
    # `git worktree list --porcelain` (marked "prunable") -- exactly the state
    # this tool exists to surface, and one --remove-merged can itself create if
    # interrupted. Report it distinctly instead of dying.
    if [ ! -d "$tree" ]; then
        printf 'PRUNABLE\n'
        return
    fi

    # Guarded for the same reason as sum_target_mb: under `set -o pipefail` a
    # bare assignment propagates git's exit status and `set -e` kills the
    # entire report, for every tree, with no output at all.
    if ! dirty="$(git -C "$tree" status --porcelain 2>/dev/null | wc -l)"; then
        printf 'DIRTY\n' # status unknown -> treat conservatively, never act
        return
    fi
    if [ "$dirty" -gt 0 ]; then
        printf 'DIRTY\n'
        return
    fi

    if [ -z "$branch" ]; then
        printf 'ACTIVE\n' # detached HEAD: never MERGED
        return
    fi

    if ! unmerged="$(git -C "$tree" rev-list --count "$branch" "^$MAIN_BRANCH" 2>/dev/null)"; then
        printf 'ACTIVE\n' # comparison against $MAIN_BRANCH not possible
        return
    fi

    if [ "$unmerged" -eq 0 ]; then
        printf 'MERGED\n'
    else
        printf 'ACTIVE\n'
    fi
}

# Ignored files that are NOT build output, one per line (empty if none).
#
# This closes a hole that `git status --porcelain` cannot see and
# `git worktree remove` will not stop: ignored files are excluded from
# porcelain status, so a worktree holding a gitignored `.env`, local config, or
# scratch dataset can appear clean and be removed. `target/` is excluded because
# reclaiming build output is this tool's purpose.
ignored_keepers() {
    local tree="$1"
    git -C "$tree" status --porcelain --ignored 2>/dev/null \
        | sed -n 's/^!! //p' \
        | grep -vE '(^|/)target/$' || true
}

action_for_class() {
    case "$1" in
        CURRENT) printf 'never touched, any mode\n' ;;
        PRUNABLE) printf 'directory gone -- run: git worktree prune\n' ;;
        MAIN) printf 'clean target/ only, never remove\n' ;;
        MERGED) printf 'safe to remove entirely\n' ;;
        DIRTY) printf 'report only -- never acted on, even with --yes\n' ;;
        ACTIVE) printf 'clean target/ only\n' ;;
    esac
}

# --- classify every tree once ----------------------------------------------

CLASS=()
SIZE_MB=()

for i in "${!WT_PATH[@]}"; do
    tree="${WT_PATH[$i]}"
    branch="${WT_BRANCH[$i]}"
    tree_real="$(realpath -e "$tree" 2>/dev/null || printf '%s' "$tree")"

    if [ "$tree_real" = "$CURRENT_REAL" ]; then
        class="CURRENT"
    elif [ "$tree_real" = "$MAIN_TREE_REAL" ]; then
        class="MAIN"
    else
        class="$(classify_tree "$tree" "$branch")"
    fi
    CLASS+=("$class")

    target_dirs_for "$tree"
    SIZE_MB+=("$(sum_target_mb)")
done

print_report() {
    local total_mb=0
    printf '%-58s %-24s %10s %-8s %s\n' "WORKTREE" "BRANCH" "TARGET/ MB" "CLASS" "ACTION"
    for i in "${!WT_PATH[@]}"; do
        local branch_disp="${WT_BRANCH[$i]:-(detached)}"
        printf '%-58s %-24s %10s %-8s %s\n' \
            "${WT_PATH[$i]}" "$branch_disp" "${SIZE_MB[$i]}" "${CLASS[$i]}" "$(action_for_class "${CLASS[$i]}")"
        total_mb=$((total_mb + SIZE_MB[i]))
    done
    echo
    awk -v mb="$total_mb" 'BEGIN { printf "Total target/ across all worktrees: %d MB (%.1f GB)\n", mb, mb / 1024 }'
}

if [ "$DO_CLEAN" -eq 0 ] && [ "$DO_REMOVE" -eq 0 ]; then
    print_report
    exit 0
fi

# --- action mode: gated -----------------------------------------------------

print_report
echo

command -v pgrep >/dev/null || {
    echo "error: pgrep not found; cannot verify no build is running, refusing to act" >&2
    exit 1
}

# Without procfs the cwd probe below cannot place any build in any tree, so
# the gate would pass vacuously rather than fail -- deleting a target/ out
# from under a live build. Probed by the operation the gate performs, since
# per-pid an absent /proc is indistinguishable from a clean scan.
readlink -f /proc/self/cwd >/dev/null 2>&1 || {
    echo "error: /proc is unavailable; cannot verify no build is running, refusing to act" >&2
    exit 1
}

# True if a live cargo/rustc process has its cwd inside $1 (realpath'd).
# Any process whose cwd cannot be determined is treated as a potential
# match against every tree -- refuse rather than guess.
UNKNOWN_PROCESS_STATE=0
running_build_in() {
    local tree_real="$1" pid cwd
    # Reset per call: a stale 1 from an earlier tree would mislabel a later
    # tree's genuine running-build refusal. Both cases refuse, so this only
    # affects the diagnostic text -- but a wrong diagnostic is a wrong record.
    UNKNOWN_PROCESS_STATE=0
    while IFS= read -r pid; do
        [ -n "$pid" ] || continue
        # Exited, or live with an unreadable cwd? Opposite answers -- skip
        # versus refuse -- and only the pid directory separates them: it
        # survives the EACCES hiding another uid's cwd, and vanishes with the
        # process. Test the cwd link instead and the two collapse into skip:
        # `[ -e /proc/1/cwd ]` is false for a live root-owned build, and
        # pgrep is not scoped to this uid.
        [ -d "/proc/$pid" ] || continue
        if ! cwd="$(readlink -f "/proc/$pid/cwd" 2>/dev/null)"; then
            # Re-test: an exit racing this window is still an exit.
            [ -d "/proc/$pid" ] || continue
            UNKNOWN_PROCESS_STATE=1
            return 0
        fi
        case "$cwd" in
            "$tree_real" | "$tree_real"/*) return 0 ;;
        esac
    done < <(pgrep -x cargo 2>/dev/null; pgrep -x rustc 2>/dev/null)
    return 1
}

to_clean_idx=()
to_remove_idx=()
for i in "${!WT_PATH[@]}"; do
    case "${CLASS[$i]}" in
        MAIN | ACTIVE)
            [ "$DO_CLEAN" -eq 1 ] && to_clean_idx+=("$i")
            ;;
        MERGED)
            [ "$DO_REMOVE" -eq 1 ] && to_remove_idx+=("$i")
            ;;
        *) : ;; # CURRENT, DIRTY: never acted on
    esac
done

if [ "${#to_clean_idx[@]}" -eq 0 ] && [ "${#to_remove_idx[@]}" -eq 0 ]; then
    echo "Nothing eligible to act on."
    exit 0
fi

echo "The following changes would be made:"
plan_mb=0
for i in "${to_clean_idx[@]}"; do
    printf '  clean target/  %-58s (%s MB)\n' "${WT_PATH[$i]}" "${SIZE_MB[$i]}"
    plan_mb=$((plan_mb + SIZE_MB[i]))
done
for i in "${to_remove_idx[@]}"; do
    printf '  remove worktree %-57s (%s MB)\n' "${WT_PATH[$i]}" "${SIZE_MB[$i]}"
    plan_mb=$((plan_mb + SIZE_MB[i]))
done
awk -v mb="$plan_mb" 'BEGIN { printf "Bytes at stake: %d MB (%.1f GB)\n", mb, mb / 1024 }'
echo

confirm() {
    local prompt="$1" reply=""
    if { exec 3<>/dev/tty; } 2>/dev/null; then
        read -r -p "$prompt" reply <&3
        exec 3<&-
    else
        read -r -p "$prompt" reply
    fi
    case "$reply" in
        y | Y | yes | YES) return 0 ;;
        *) return 1 ;;
    esac
}

if [ "$ASSUME_YES" -eq 0 ]; then
    if ! confirm "Proceed with the above? [y/N] "; then
        echo "Aborted; nothing changed."
        exit 0
    fi
fi

for i in "${to_clean_idx[@]}"; do
    tree="${WT_PATH[$i]}"
    tree_real="$(realpath -e "$tree" 2>/dev/null || printf '%s' "$tree")"
    if [ "$tree_real" = "$CURRENT_REAL" ]; then
        echo "refusing: ${tree} is the current tree" >&2
        continue
    fi
    if running_build_in "$tree_real"; then
        if [ "$UNKNOWN_PROCESS_STATE" -eq 1 ]; then
            echo "refusing: could not determine process state for ${tree}; refusing to guess" >&2
        else
            echo "refusing: a cargo/rustc process is running in ${tree}" >&2
        fi
        continue
    fi
    target_dirs_for "$tree"
    for t in "${TARGET_DIRS[@]}"; do
        [ -n "$t" ] || continue
        echo "removing $t"
        rm -rf -- "$t"
    done
done

for i in "${to_remove_idx[@]}"; do
    tree="${WT_PATH[$i]}"
    tree_real="$(realpath -e "$tree" 2>/dev/null || printf '%s' "$tree")"
    if [ "$tree_real" = "$CURRENT_REAL" ]; then
        echo "refusing: ${tree} is the current tree" >&2
        continue
    fi
    if running_build_in "$tree_real"; then
        if [ "$UNKNOWN_PROCESS_STATE" -eq 1 ]; then
            echo "refusing: could not determine process state for ${tree}; refusing to guess" >&2
        else
            echo "refusing: a cargo/rustc process is running in ${tree}" >&2
        fi
        continue
    fi
    keepers="$(ignored_keepers "$tree")"
    if [ -n "$keepers" ]; then
        echo "refusing: ${tree} holds ignored files that are not build output;" >&2
        echo "          'git worktree remove' would delete these permanently:" >&2
        printf '            %s\n' $keepers >&2
        echo "          Move or delete them yourself, then re-run." >&2
        continue
    fi
    echo "removing worktree $tree"
    git worktree remove "$tree"
done

echo "Done."
