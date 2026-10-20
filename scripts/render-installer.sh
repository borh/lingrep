#!/usr/bin/env bash
# scripts/render-installer.sh — write the release copy of scripts/install.sh.
#
#   scripts/render-installer.sh <version> <output>
#
# Fills in the version the published installer installs by default. The
# release workflow attaches the output to the release as install.sh.
set -euo pipefail

version=${1:?usage: scripts/render-installer.sh <version> <output>}
output=${2:?usage: scripts/render-installer.sh <version> <output>}
source=$(dirname "${BASH_SOURCE[0]}")/install.sh

if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must look like 0.3.2, not '$version'" >&2
    exit 1
fi

sed "s/^RELEASE_VERSION='@LING_VERSION@'\$/RELEASE_VERSION='$version'/" "$source" >"$output"
if ! grep -qx "RELEASE_VERSION='$version'" "$output"; then
    echo "error: $source has no RELEASE_VERSION='@LING_VERSION@' line" >&2
    exit 1
fi
sh -n "$output"
