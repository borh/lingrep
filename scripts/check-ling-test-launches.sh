#!/usr/bin/env bash

set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

pattern='cargo_bin(_cmd)?!?[[:space:]]*\([[:space:]]*"ling"|CARGO_BIN_EXE_ling|Command::new[[:space:]]*\([^)]*"ling"|target/(debug|release)/ling'

if bypasses=$(rg -n --glob '*.rs' --glob '!**/support/mod.rs' "$pattern" lingrep/tests); then
    echo "ling integration tests must launch the executable through tests/support" >&2
    echo "$bypasses" >&2
    exit 1
fi

echo "ling integration-test executable seam is centralized"
