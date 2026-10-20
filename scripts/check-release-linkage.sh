#!/usr/bin/env bash
# scripts/check-release-linkage.sh — refuse a binary that loads a shared
# library a stock system does not have.
#
#   scripts/check-release-linkage.sh <binary>
#
# macOS: every load command must name /usr/lib or /System/Library.
# Linux: every DT_NEEDED entry must be a glibc or libgcc library.
set -euo pipefail

binary=${1:?usage: scripts/check-release-linkage.sh <binary>}

case "$(uname -s)" in
  Darwin)
    libraries=$(otool -L "$binary" | tail -n +2 | awk '{print $1}')
    allowed='^(/usr/lib/|/System/Library/)'
    ;;
  Linux)
    libraries=$(readelf -d "$binary" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')
    allowed='^(libc|libm|libdl|libpthread|librt|libgcc_s|ld-linux[^/]*)\.so'
    ;;
  *)
    echo "error: unsupported platform $(uname -s)" >&2
    exit 1
    ;;
esac

echo "$libraries"
unexpected=$(grep -Ev "$allowed" <<< "$libraries" || true)
if [ -n "$unexpected" ]; then
  echo "error: $binary loads libraries outside the base system:" >&2
  echo "$unexpected" >&2
  exit 1
fi
