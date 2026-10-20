#!/usr/bin/env bash
# scripts/check-test-consolidation.sh — fail if a consolidated test suite has
# an undeclared source file.
#
# lingrep-core and lingrep-web integration tests use a few `tests/<target>/main.rs`
# targets, where each suite file is a `mod` line. Cargo auto-discovers
# `tests/*.rs` at the TOP level, but a `.rs` file inside `tests/<target>/` is
# compiled only if `main.rs` declares it. So a file dropped into a suite
# directory without a matching `mod` line silently compiles into nothing: no
# error, no warning, and its tests simply stop running.
#
# That is the one failure mode consolidation introduces, and it is invisible in
# a green test run. It is a live risk whenever a branch that adds tests merges
# with a branch that consolidated them — the tests land in the directory and
# nobody adds the `mod`.
#
# The check is deliberately bidirectional: an undeclared file means silently
# skipped tests, and a declared-but-missing module means a build break with a
# confusing message.
set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Every consolidated suite: a directory holding main.rs plus one module per file.
SUITES=(
    lingrep-core/tests/suite
    lingrep-cli-common/tests/suite
    lingrep-corpus/tests/suite
    lingrep/tests/suite
    lingrep-web/tests/suite
    lingrep-web/tests/browser
)

status=0

for dir in "${SUITES[@]}"; do
    main="$dir/main.rs"
    if [ ! -f "$main" ]; then
        echo "error: $main does not exist — is SUITES out of date?" >&2
        status=1
        continue
    fi

    # Files present in the directory, excluding the target root itself.
    # `sed` strips the directory prefix rather than `find -printf '%f\n'`,
    # which is a GNU extension absent from BSD/macOS find. The following
    # substitution already had to strip `.rs`, so this adds no pass.
    files="$(find "$dir" -maxdepth 1 -name '*.rs' \
        | sed 's|.*/||; s|\.rs$||' | grep -vx main | sort)"

    # Modules declared by the target root. Sibling directories such as
    # ../support and ../fixtures are pulled in with #[path] attributes and
    # legitimately have no local file, so compare only against local files.
    mods="$(grep -oE '^[[:space:]]*(pub )?mod [a-z_0-9]+' "$main" \
        | sed -E 's/.*mod //' | sort -u)"

    undeclared="$(comm -23 <(printf '%s\n' "$files") <(printf '%s\n' "$mods"))"
    if [ -n "$undeclared" ]; then
        status=1
        echo "error: $dir has .rs files that $main never declares." >&2
        echo "       Their tests are NOT compiled and NOT run:" >&2
        printf '         %s.rs\n' $undeclared >&2
        echo "       Add a 'mod <name>;' line to $main for each." >&2
    fi

    # A declared module with no local file is only a problem when it also has
    # no #[path] override pointing elsewhere.
    for m in $mods; do
        [ -f "$dir/$m.rs" ] && continue
        [ -d "$dir/$m" ] && continue
        grep -qE "^#\[path *= *\"[^\"]*\"\]" "$main" && grep -qE "path *= *\"[^\"]*/$m(/mod)?\.rs\"" "$main" && continue
        status=1
        echo "error: $main declares 'mod $m' but $dir/$m.rs does not exist" >&2
    done
done

# --- consolidation reversal --------------------------------------------------
#
# The check above catches a file that stops running. This one catches the
# opposite failure: a NEW top-level `tests/*.rs` in a consolidated crate.
# Cargo auto-discovers those, so the tests run and everything looks green --
# but each one is a separate test binary. This is silent erosion rather than
# silent loss.
#
# The allowlist below is the isolation rule made executable. A test earns its
# own binary only by observing process-scoped state -- advisory file locks
# (fs4/flock), the fd table, fixed ports, env vars, cwd, signal handlers, PID
# -- or a measured property (RSS, mmap residency, wall-clock), or by needing a
# --release-only gate. Adding a name here without one of those reasons
# silently gives back the win.
declare -A ALLOWED_STANDALONE=(
    [lingrep-core/tests/external_process_contract.rs]="observes child PIDs, process groups, signals, pipes, and fixed wall-clock grace"
    [lingrep-core/tests/real_world_validation_tests.rs]="wall-clock and large-corpus gating"
    [lingrep-web/tests/cli.rs]="trycmd harness; owns its own binary by design"
)

for dir in "${SUITES[@]}"; do
    crate_tests="$(dirname "$dir")"
    for f in "$crate_tests"/*.rs; do
        [ -e "$f" ] || continue
        if [ -z "${ALLOWED_STANDALONE[$f]:-}" ]; then
            status=1
            echo "error: $f is a standalone test target in a consolidated crate." >&2
            echo "       It builds and links its own binary, undoing the consolidation." >&2
            echo "       Either move it to $dir/ and add a 'mod' line to $dir/main.rs," >&2
            echo "       or, if it genuinely observes process-scoped state, add it to" >&2
            echo "       ALLOWED_STANDALONE in $0 with the reason." >&2
        fi
    done
done

if [ "$status" -eq 0 ]; then
    echo "✓ consolidated test suites declare every source file"
    echo "✓ no unlisted standalone test targets in consolidated crates"
fi

exit "$status"
