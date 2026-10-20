#!/usr/bin/env bash

set -euo pipefail

if [ "${LINGREP_RUN_REAL_SENTENCE_FINAL_CLI_BENCH:-}" != "1" ]; then
    echo "skipping real sentence-final CLI gate; set LINGREP_RUN_REAL_SENTENCE_FINAL_CLI_BENCH=1" >&2
    exit 0
fi

# The dictionary comes from LINGREP_DICTIONARY_PATH, which `nix develop` exports; the
# corpus and patterns are real-workload inputs the caller must still provide.
if [ -z "${LINGREP_REAL_CORPUS_PATH:-}" ] || [ -z "${LINGREP_REAL_PATTERNS_PATH:-}" ]; then
    echo "LINGREP_REAL_CORPUS_PATH and LINGREP_REAL_PATTERNS_PATH are required" >&2
    exit 2
fi

benchmark_corpus=${LINGREP_REAL_CLI_BENCH_CORPUS_PATH:-$LINGREP_REAL_CORPUS_PATH}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
mkdir -p target

cargo build --release -p lingrep

binary="$repo_root/target/release/ling"
full_arguments=(
    sentence-final
    --patterns "$LINGREP_REAL_PATTERNS_PATH"
    --dict-path "$LINGREP_DICTIONARY_PATH"
    --batch-sentences 1000
    --batch-raw-bytes 262144
    --progress always
    --tsv
    "$LINGREP_REAL_CORPUS_PATH"
)
benchmark_arguments=(
    sentence-final
    --patterns "$LINGREP_REAL_PATTERNS_PATH"
    --dict-path "$LINGREP_DICTIONARY_PATH"
    --batch-sentences 1000
    --batch-raw-bytes 262144
    --progress always
    --tsv
    "$benchmark_corpus"
)

"$binary" "${full_arguments[@]}" >/dev/null 2>target/sentence-final-profile.stderr
"$binary" "${benchmark_arguments[@]}" >/dev/null 2>target/sentence-final-profile-benchmark.stderr
completed_tokens=$(sed -n 's/.*tokens=\([0-9][0-9]*\).*/\1/p' target/sentence-final-profile-benchmark.stderr | tail -n 1)
if [ -z "$completed_tokens" ] || [ "$completed_tokens" -eq 0 ]; then
    echo "sentence-final preflight did not report a positive completed token count" >&2
    exit 1
fi

time_bin=${TIME_BIN:-$(type -P time)}
if [ ! -x "$time_bin" ]; then
    echo "GNU time executable not found at $time_bin; set TIME_BIN explicitly" >&2
    exit 2
fi
"$time_bin" -o target/sentence-final-profile-time.log -v \
    "$binary" "${full_arguments[@]}" >/dev/null 2>target/sentence-final-profile.stderr
