#!/usr/bin/env bash
set -euo pipefail

if ! command -v chromium >/dev/null || ! command -v chromedriver >/dev/null; then
    echo "SKIP: Chromium or Chromedriver is unavailable"
    exit 0
fi

layout_log="$(mktemp)"
TMPDIR=/tmp chromedriver --port=9516 --log-path="$layout_log" &
driver_pid=$!
cleanup() {
    kill "$driver_pid" 2>/dev/null || true
    rm -f "$layout_log"
}
trap cleanup EXIT INT TERM

driver_ready=false
for _ in $(seq 1 100); do
    if bash -c 'exec 3<>/dev/tcp/127.0.0.1/9516' 2>/dev/null; then
        driver_ready=true
        break
    fi
    sleep 0.05
done
if [ "$driver_ready" != true ]; then
    echo "Chromedriver did not start"
    sed -n '1,120p' "$layout_log"
    exit 1
fi

export LINGREP_WEB_LAYOUT_CHROMEDRIVER_URL="http://127.0.0.1:9516"
cargo test -p lingrep-web --test browser -- layout_e2e_test:: --test-threads=1 --nocapture
cargo test -p lingrep-web --test browser -- query_library_e2e_test:: --test-threads=1 --nocapture
cargo test -p lingrep-web --test browser -- corpus_context_e2e_test:: --test-threads=1 --nocapture
