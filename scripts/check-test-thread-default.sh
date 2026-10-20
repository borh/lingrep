#!/usr/bin/env bash
# Guard the repository-wide default that keeps dictionary-backed test binaries
# below the host earlyoom threshold. `force = false` deliberately lets a
# developer opt into parallelism with `RUST_TEST_THREADS=<n> cargo test`.

set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

config=$(cargo -Z unstable-options config get env.RUST_TEST_THREADS)
if [ "$config" != $'env.RUST_TEST_THREADS.force = false\nenv.RUST_TEST_THREADS.value = "1"' ]; then
    echo "error: Cargo must effectively configure env.RUST_TEST_THREADS as value=1, force=false" >&2
    printf '%s\n' "$config" >&2
    exit 1
fi

fixture_home=$(mktemp -d)
trap 'rm -rf "$fixture_home"' EXIT
printf '%s\n' '[build]' 'RUST_TEST_THREADS = { value = "1", force = false }' > "$fixture_home/config.toml"
if (cd /tmp && CARGO_HOME="$fixture_home" cargo -Z unstable-options config get env.RUST_TEST_THREADS >/dev/null 2>&1); then
    echo "error: test-harness default must be under Cargo [env], not another config section" >&2
    exit 1
fi

echo "✓ test harness defaults to one thread with an explicit override escape hatch"
