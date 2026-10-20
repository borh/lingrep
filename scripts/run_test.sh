#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
dict="${LINGREP_DICTIONARY_PATH:?Set LINGREP_DICTIONARY_PATH to a Nix UniDic artifact}"

cargo run --release -p lingrep -- matches \
  --dict-path "$dict" \
  --patterns patterns/sfe.lx \
  --progress always \
  --tsv \
  data-archive/test-corpus.jsonl
