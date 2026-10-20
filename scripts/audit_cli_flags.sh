#!/usr/bin/env bash
#
# audit_cli_flags.sh — CLI flag-migration inventory tool.
#
# Greps the tracked working tree for live references to retired or renamed CLI
# flags across source, tests, scripts, benches, completions, and docs.
#
# This is an inventory tool, not a gate: it always exits 0. Remaining hits
# should be intentional historical prose.
#
# Usage:
#   bash scripts/audit_cli_flags.sh
#   bash scripts/audit_cli_flags.sh | tee /tmp/cli_audit_before.txt
#
# Excluded from every search (historical/archival records and the internal,
# non-CLI implementation details — not migration targets):
#   - any directory literally named `archive/` (at any depth)
#   - any directory literally named `results/` (at any depth)
#   - this script itself (it necessarily quotes every retired flag spelling)

set -uo pipefail

# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 1

# git pathspec exclusions, shared by every search below.
EXCLUDE_PATHSPECS=(
  ':!**/archive/**'
  ':!data-archive/**'
  ':!**/results/**'
  ':!scripts/audit_cli_flags.sh'
)

# "label|extended-regex" pairs. Regexes are POSIX ERE, as consumed by
# `git grep -E`.
#
# --dict is matched only as a standalone flag (not part of --dict-path):
# the character right after "dict" must not be a letter or hyphen.
# --format is matched as the exact flag; this does not match
# --input-format / --output-format / --cache-format, which all have a
# single hyphen (not "--") immediately before "format".
FLAGS=(
  "retired --format|--format([ =]|\$)"
  "retired --diagnostic-format|--diagnostic-format([ =]|\$)"
  "retired --protocol-version|--protocol-version([ =]|\$)"
  "retired --no-policy|--no-policy([^a-zA-Z-]|\$)"
  "retired progress stderr|--progress[ =]+stderr"
  "retained progress never|--progress[ =]+never"
  "progress auto|--progress[ =]+auto"
  "progress always|--progress[ =]+always"
  "replacement --json|--json([^a-zA-Z-]|\$)"
  "replacement --tsv|--tsv([^a-zA-Z-]|\$)"
  "replacement --raw-matches|--raw-matches([^a-zA-Z-]|\$)"
  "sentence-final invocations|ling[[:space:]]+sentence-final"
)

total=0
summary=()

for entry in "${FLAGS[@]}"; do
  label="${entry%%|*}"
  pattern="${entry#*|}"

  # -e (not a bare positional arg) so patterns starting with "--" (e.g.
  # --patterns-file) aren't misparsed as git-grep options.
  hits="$(git grep -n -I -E -e "$pattern" -- . "${EXCLUDE_PATHSPECS[@]}" 2>/dev/null || true)"

  count=0
  if [[ -n "$hits" ]]; then
    count="$(printf '%s\n' "$hits" | wc -l)"
    printf '\n=== %s (%d hits) ===\n' "$label" "$count"
    printf '%s\n' "$hits"
  fi

  total=$((total + count))
  summary+=("$(printf '%-28s %d' "$label" "$count")")
done

printf '\n----- audit_cli_flags.sh summary -----\n'
for line in "${summary[@]}"; do
  printf '%s\n' "$line"
done
printf '%-28s %d\n' "TOTAL" "$total"
printf '\nNote: categories overlap by design (for example migration prose can\n'
printf 'name both a retired spelling and its replacement),\n'
printf 'so TOTAL is not a distinct-line count. This script always exits 0;\n'
printf 'it is an inventory, not a pass/fail gate.\n'

exit 0
