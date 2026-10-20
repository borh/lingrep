#!/usr/bin/env bash
# scripts/check-test-filters.sh — fail if a CI gate filters on a test module
# that no longer exists.
#
# Several gates run `cargo test ... -- <module>:: --quiet` to
# select one module out of a consolidated suite binary. A libtest name filter
# that matches ZERO tests is not an error — the harness prints
# "running 0 tests" and exits 0. So when the module is renamed or deleted, the
# gate keeps reporting green while running nothing at all, and the `&&` chain
# it sits in stays satisfied.
#
# This is the same absence-based failure that
# `check-test-consolidation.sh` guards: an obsolete selector looks green unless
# the repository verifies it independently.
set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Files that may contain filtered `cargo test` invocations.
SOURCES=(ci.sh)
while IFS= read -r script; do
    SOURCES+=("$script")
done < <(find scripts -maxdepth 1 -name '*.sh' | sort)

status=0
checked=0

for source in "${SOURCES[@]}"; do
    [ -f "$source" ] || continue

    # Match `cargo test [...] -p <crate> [...] --test <target> [...] -- <module>::`.
    # Only module-qualified filters (trailing `::`) are checked: a bare
    # substring filter is a deliberate prefix match, not a module reference.
    while IFS= read -r line; do
        crate="$(sed -nE 's/.*-p +([a-z0-9_-]+).*/\1/p' <<<"$line")"
        target="$(sed -nE 's/.*--test +([a-z0-9_-]+).*/\1/p' <<<"$line")"
        module="$(sed -nE 's/.*-- +([a-z0-9_]+)::.*/\1/p' <<<"$line")"

        [ -n "$crate" ] && [ -n "$target" ] && [ -n "$module" ] || continue
        checked=$((checked + 1))

        dir="$crate/tests/$target"
        if [ ! -d "$dir" ]; then
            status=1
            echo "error: $source filters on $crate --test $target, but $dir/ does not exist" >&2
            continue
        fi
        if [ ! -f "$dir/$module.rs" ]; then
            status=1
            echo "error: $source filters tests by '$module::' but $dir/$module.rs does not exist." >&2
            echo "       A libtest filter matching zero tests EXITS 0, so this gate is green" >&2
            echo "       while running nothing. Either point it at the successor module or" >&2
            echo "       delete the line — and say in the commit what covers it instead." >&2
        fi
    done < <(grep -nE 'cargo test .*-- +[a-z0-9_]+::' "$source" || true)
done

if [ "$status" -eq 0 ]; then
    echo "✓ all $checked module-filtered test gates name a module that exists"
fi

exit "$status"
