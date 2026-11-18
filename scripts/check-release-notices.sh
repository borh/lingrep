#!/usr/bin/env bash
# Runs the release archive's third-party-notices check for every target in the
# release.yml build matrix. The check reads only Cargo metadata, so a
# dependency without license text fails on any host, not only on the release
# runner of its target.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

mapfile -t targets < <(
    sed -n 's/^[[:space:]]*- target:[[:space:]]*\([^[:space:]]*\)[[:space:]]*$/\1/p' \
        .github/workflows/release.yml | sort -u
)
if ((${#targets[@]} == 0)); then
    echo "check-release-notices: no '- target:' entries in .github/workflows/release.yml" >&2
    exit 1
fi

for target in "${targets[@]}"; do
    if ! python3 scripts/third-party-notices.py --target "$target" >/dev/null; then
        echo "check-release-notices: third-party notices fail for $target" >&2
        exit 1
    fi
done
