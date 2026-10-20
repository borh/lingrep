#!/usr/bin/env bash

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT

mkdir -p "$temporary/bin" "$temporary/fuzz" "$temporary/logs"

cat >"$temporary/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"$FAKE_CARGO_LOG"

if [[ "$1 $2" == "fuzz list" ]]; then
    printf '%s\n' fuzz_alpha fuzz_beta
    exit 0
fi

if [[ "$1 $2" != "fuzz run" ]]; then
    exit 64
fi

target=${6:-}
if [[ ${3:-} != "--release" || ${4:-} != "-s" || ${5:-} != "none" || \
      ${7:-} != "--" || ${8:-} != "-max_total_time=7" ]]; then
    exit 65
fi

if [[ ${LINGREP_FUZZ_RUNS:-} == 0 ]]; then
    [[ $# -eq 8 ]] || exit 65
elif [[ ${9:-} != "-runs=${LINGREP_FUZZ_RUNS:-}" || $# -ne 9 ]]; then
    exit 65
fi

if [[ $target == "${FAIL_TARGET:-}" ]]; then
    exit 42
fi
EOF
chmod +x "$temporary/bin/cargo"

export PATH="$temporary/bin:$PATH"
export FAKE_CARGO_LOG="$temporary/cargo.log"
export LINGREP_FUZZ_DIR="$temporary/fuzz"
export LINGREP_FUZZ_LOG_DIR="$temporary/logs"
export LINGREP_FUZZ_MAX_TOTAL_TIME=7
export LINGREP_FUZZ_RUNS=11

"$repo_root/scripts/run_fuzz_smoke.sh"

grep -Fx "fuzz run --release -s none fuzz_alpha -- -max_total_time=7 -runs=11" \
    "$FAKE_CARGO_LOG"
grep -Fx "fuzz run --release -s none fuzz_beta -- -max_total_time=7 -runs=11" \
    "$FAKE_CARGO_LOG"

: >"$FAKE_CARGO_LOG"
export LINGREP_FUZZ_RUNS=0
"$repo_root/scripts/run_fuzz_smoke.sh"

grep -Fx "fuzz run --release -s none fuzz_alpha -- -max_total_time=7" \
    "$FAKE_CARGO_LOG"
grep -Fx "fuzz run --release -s none fuzz_beta -- -max_total_time=7" \
    "$FAKE_CARGO_LOG"
if grep -F -- "-runs=" "$FAKE_CARGO_LOG"; then
    echo "LINGREP_FUZZ_RUNS=0 must omit the libFuzzer run-count argument" >&2
    exit 1
fi

: >"$FAKE_CARGO_LOG"
export LINGREP_FUZZ_RUNS=11
export FAIL_TARGET=fuzz_alpha
if "$repo_root/scripts/run_fuzz_smoke.sh"; then
    echo "fuzz smoke runner must propagate target failures" >&2
    exit 1
fi

grep -Fx "fuzz run --release -s none fuzz_alpha -- -max_total_time=7 -runs=11" \
    "$FAKE_CARGO_LOG"
grep -Fx "fuzz run --release -s none fuzz_beta -- -max_total_time=7 -runs=11" \
    "$FAKE_CARGO_LOG"
