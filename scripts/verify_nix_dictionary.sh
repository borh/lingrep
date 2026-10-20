#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo 'usage: verify_nix_dictionary.sh <dictionary.dic>' >&2
  exit 64
fi

dict=$1
if [ ! -r "$dict" ]; then
  echo "dictionary is not readable: $dict" >&2
  exit 66
fi

tmp_cache=$(mktemp -d)
trap 'rm -rf "$tmp_cache"' EXIT

output=$(printf '日本語を解析します。\n' | XDG_CACHE_HOME="$tmp_cache" cargo run -q -p lingrep -- \
  tokenize --dict-path "$dict")

if [ -z "$output" ]; then
  echo 'tokenization produced no output' >&2
  exit 1
fi
