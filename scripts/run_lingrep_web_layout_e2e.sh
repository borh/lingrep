#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

if ! command -v chromium >/dev/null || ! command -v chromedriver >/dev/null; then
    echo "browser checks require chromium and chromedriver" >&2
    exit 2
fi

layout_log="$(mktemp)"
driver_stdout="$(mktemp)"
TMPDIR="${TMPDIR:-/tmp}" chromedriver --port=0 --log-path="$layout_log" >"$driver_stdout" 2>&1 &
driver_pid=$!
cleanup() {
    kill "$driver_pid" 2>/dev/null || true
    rm -f "$layout_log" "$driver_stdout"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

driver_port=
for _ in $(seq 1 100); do
    driver_port=$(sed -n 's/^ChromeDriver was started successfully on port \([0-9][0-9]*\)\.$/\1/p' "$driver_stdout")
    if [ -n "$driver_port" ] || ! kill -0 "$driver_pid" 2>/dev/null; then
        break
    fi
    sleep 0.05
done
if [ -z "$driver_port" ]; then
    echo "Chromedriver did not start"
    cat "$driver_stdout"
    sed -n '1,120p' "$layout_log"
    exit 1
fi

export LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL="http://127.0.0.1:$driver_port"
export LINGREP_WEB_LAYOUT_REQUIRE_DRIVER=1
"$repo_root/scripts/cargo-test-selected.py" -p lingrep-web --test browser -- --test-threads=1 --nocapture
