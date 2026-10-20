#!/usr/bin/env bash

set -uo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
fuzz_dir=${LINGREP_FUZZ_DIR:-"$repo_root/lingrep-core"}
log_dir=${LINGREP_FUZZ_LOG_DIR:-/tmp}
max_total_time=${LINGREP_FUZZ_MAX_TOTAL_TIME:-30}
runs=${LINGREP_FUZZ_RUNS:-5000}
sanitizer=${LINGREP_FUZZ_SANITIZER:-none}

if ! fuzz_list=$(cd "$fuzz_dir" && cargo fuzz list); then
    echo "failed to discover cargo-fuzz targets in $fuzz_dir" >&2
    exit 1
fi

mapfile -t fuzz_targets <<<"$fuzz_list"
if [[ ${#fuzz_targets[@]} -eq 0 || -z ${fuzz_targets[0]} ]]; then
    echo "cargo fuzz list found no targets in $fuzz_dir" >&2
    exit 1
fi

status=0
for target in "${fuzz_targets[@]}"; do
    log_file="$log_dir/cargo-fuzz-$target.log"
    fuzzer_args=("-max_total_time=$max_total_time")
    if [[ $runs != 0 ]]; then
        fuzzer_args+=("-runs=$runs")
        bound_description="max ${max_total_time}s or ${runs} runs"
    else
        bound_description="max ${max_total_time}s with no run-count limit"
    fi
    echo "Running fuzz target $target ($bound_description)"
    if (cd "$fuzz_dir" && cargo fuzz run --release -s "$sanitizer" "$target" -- \
        "${fuzzer_args[@]}") 2>&1 | tee "$log_file"; then
        echo "PASS: $target"
    else
        echo "FAIL: $target (see $log_file)" >&2
        status=1
    fi
done

exit "$status"
