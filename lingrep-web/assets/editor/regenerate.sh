#!/usr/bin/env bash
set -euo pipefail
# `emcc` was required here until this line was checked. `tree-sitter build
# --wasm` builds through wasi-sdk, which it downloads into `~/.cache/tree-sitter`
# on first use, so emscripten is not consulted: building the pinned revision with
# every emscripten entry removed from `PATH` produces a byte-identical WASM.
#
# The undeclared dependency is therefore the opposite one — this needs network
# access for the wasi-sdk fetch as well as for the clone below, and a check for a
# tool it does not use hid that behind a requirement that always passed.
for tool in tree-sitter git jq sha256sum; do command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }; done
asset_root=$(cd "$(dirname "$0")" && pwd -P)
repo_root=$(git -C "$asset_root" rev-parse --show-toplevel)
grammar_rev=$(sed -n '/name = "tree-sitter-lingrep"/,/^$/s/^source = .*rev=\([0-9a-f]\{40\}\).*/\1/p' "$repo_root/Cargo.lock")
[[ "$grammar_rev" =~ ^[0-9a-f]{40}$ ]] || { echo "expected one pinned tree-sitter-lingrep revision" >&2; exit 1; }
build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT
git clone --no-checkout https://github.com/borh/tree-sitter-lingrep.git "$build_dir/grammar"
git -C "$build_dir/grammar" checkout --detach "$grammar_rev"
(cd "$build_dir/grammar" && tree-sitter generate && tree-sitter build --wasm)
test -s "$build_dir/grammar/tree-sitter-lingrep.wasm"
test -s "$build_dir/grammar/queries/lingrep/highlights.scm"
install -m 0644 "$build_dir/grammar/tree-sitter-lingrep.wasm" "$asset_root/vendor/tree-sitter-lingrep.wasm"
install -m 0644 "$build_dir/grammar/queries/lingrep/highlights.scm" "$asset_root/vendor/highlights.scm"
wasm_sha=$(sha256sum "$asset_root/vendor/tree-sitter-lingrep.wasm" | cut -d' ' -f1)
query_sha=$(sha256sum "$asset_root/vendor/highlights.scm" | cut -d' ' -f1)
jq --sort-keys --arg rev "$grammar_rev" --arg wasm "$wasm_sha" --arg query "$query_sha" \
  '.grammar_revision=$rev | .sha256["tree-sitter-lingrep.wasm"]=$wasm | .sha256["highlights.scm"]=$query' \
  "$asset_root/vendor/manifest.json" > "$build_dir/manifest.json"
mv "$build_dir/manifest.json" "$asset_root/vendor/manifest.json"
