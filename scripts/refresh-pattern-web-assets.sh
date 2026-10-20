#!/usr/bin/env bash
set -euo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would act on the wrong
# tree.
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
vendor_dir="$repo_root/lingrep-web/assets/editor/vendor"
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

manifest_path=$(cargo metadata --format-version 1 | jq -r '
  [.packages[] | select(.name == "tree-sitter-lingrep") | .manifest_path] | first // empty
')
if [[ -z "$manifest_path" ]]; then
  echo "tree-sitter-lingrep is absent from cargo metadata" >&2
  exit 1
fi
grammar_dir=$(dirname "$manifest_path")
grammar_wasm="$grammar_dir/tree-sitter-lingrep.wasm"
highlight_query="$grammar_dir/queries/lingrep/highlights.scm"
[[ -f "$grammar_wasm" && -f "$highlight_query" ]] || {
  echo "the pinned grammar checkout lacks browser artifacts" >&2
  exit 1
}

mkdir -p "$vendor_dir"
cp "$grammar_wasm" "$vendor_dir/tree-sitter-lingrep.wasm"
cp "$highlight_query" "$vendor_dir/highlights.scm"

(
  cd "$tmp_dir"
  npm pack --silent web-tree-sitter@0.26.11 >/dev/null
  tar -xzf web-tree-sitter-0.26.11.tgz
)
cp "$tmp_dir/package/web-tree-sitter.js" "$vendor_dir/web-tree-sitter.js"
cp "$tmp_dir/package/web-tree-sitter.wasm" "$vendor_dir/web-tree-sitter.wasm"
chmod 0644 "$vendor_dir"/{web-tree-sitter.js,web-tree-sitter.wasm,tree-sitter-lingrep.wasm,highlights.scm}

grammar_revision=$(sed -n 's/.*tree-sitter-lingrep.*rev = "\([0-9a-f]*\)".*/\1/p' "$repo_root/Cargo.toml" | head -1)
[[ -n "$grammar_revision" ]] || {
  echo "unable to read the pinned tree-sitter-lingrep revision" >&2
  exit 1
}

jq -n -S \
  --arg grammar_revision "$grammar_revision" \
  --arg runtime_version "0.26.11" \
  --arg runtime_js "$(sha256sum "$vendor_dir/web-tree-sitter.js" | cut -d' ' -f1)" \
  --arg runtime_wasm "$(sha256sum "$vendor_dir/web-tree-sitter.wasm" | cut -d' ' -f1)" \
  --arg grammar_wasm_hash "$(sha256sum "$vendor_dir/tree-sitter-lingrep.wasm" | cut -d' ' -f1)" \
  --arg highlights "$(sha256sum "$vendor_dir/highlights.scm" | cut -d' ' -f1)" \
  '{
    grammar_revision: $grammar_revision,
    runtime_version: $runtime_version,
    sha256: {
      "highlights.scm": $highlights,
      "tree-sitter-lingrep.wasm": $grammar_wasm_hash,
      "web-tree-sitter.js": $runtime_js,
      "web-tree-sitter.wasm": $runtime_wasm
    }
  }' > "$vendor_dir/manifest.json"
