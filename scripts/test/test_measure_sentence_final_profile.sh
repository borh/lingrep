#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
stderr_file=$(mktemp)
trap 'rm -f "$stderr_file"' EXIT

set +e
LINGREP_RUN_REAL_SENTENCE_FINAL_CLI_BENCH=1 \
    "$repo_root/scripts/measure_sentence_final_profile.sh" \
    >/dev/null 2>"$stderr_file"
status=$?
set -e

if [ "$status" -ne 2 ]; then
    echo "expected missing-input exit status 2, got $status" >&2
    exit 1
fi
grep -F "LINGREP_REAL_CORPUS_PATH and LINGREP_REAL_PATTERNS_PATH are required" "$stderr_file"

# The documented science corpus contains a source record larger than the
# default 128 KiB retained-raw budget. Keep the real-workload harness bound to
# the same explicit budget used to build its prepared artifacts.
test "$(grep -Fc -- '--batch-raw-bytes 262144' \
    "$repo_root/scripts/measure_sentence_final_profile.sh")" -eq 2
