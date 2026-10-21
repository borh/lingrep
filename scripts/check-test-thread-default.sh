#!/usr/bin/env bash
# Guard the repository-wide default that keeps dictionary-backed test binaries
# below the host earlyoom threshold. `force = false` lets a developer opt into
# parallelism with `RUST_TEST_THREADS=<n> cargo test`.

set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

python3 -I - "$repo_root/.cargo/config.toml" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as config_file:
    config = tomllib.load(config_file)
actual = config.get("env", {}).get("RUST_TEST_THREADS")
if actual != {"value": "1", "force": False}:
    print(
        "error: .cargo/config.toml must set [env] RUST_TEST_THREADS = "
        '{ value = "1", force = false }, found ' + repr(actual),
        file=sys.stderr,
    )
    sys.exit(1)
PY

echo "✓ test harness defaults to one thread with an explicit override escape hatch"
