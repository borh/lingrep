#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
script="$repo_root/scripts/verify_nix_dictionary.sh"

set +e
output=$("$script" 2>&1)
status=$?
set -e

test "$status" -eq 64
grep -Fq 'usage: verify_nix_dictionary.sh <dictionary.dic>' <<<"$output"
