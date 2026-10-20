#!/usr/bin/env bash
# Two checks over the tracked lingrep patterns catalogs and the sources that embed them.
#
# 1. No `.lx` file may be a symlink. A symlink to an absolute path outside
#    the repository makes the checkout non-reproducible: `git checkout` of an
#    older commit restores the link, not the content it pointed at, so a
#    commit that was green when authored can fail later for reasons no diff
#    explains. `patterns/sfe.lx` was exactly that until it was vendored.
#
# 2. Fails if any tracked source still writes a regex operand with `=` or `!=`,
# which the lingrep patterns compiler rejects. Match mode is explicit: `=`/`!=` take
# literals, `in`/`not in` take sets, `~`/`!~` take a literal (substring
# containment) or a regex (substring search).
#
# The operand must appear inside a `[...]` node pattern. Without that anchor
# the check also flags ordinary text such as a URL-encoded `dict_path=/tmp/x`.
#
# Lines marked `legacy-lx-syntax-ok` are exempt. That marker is for the few
# fixtures whose whole point is the rejected spelling — a test asserting that
# `!=` refuses a regex operand cannot be written any other way.
#
# Only tracked files are scanned, so untracked scratch work in a shared
# workspace never fails the build.
set -euo pipefail
# The checkout this script belongs to, from its own location. `git rev-parse
# --show-toplevel` would name whichever repository the caller happens to be
# standing in, so a worktree's copy run from elsewhere would check the wrong
# tree and report a clean result for a file it never read.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# 1. Catalogs must be regular files. Mode 120000 is a symlink in the index.
symlinked=$(git ls-files -s '*.lx' | awk '$1 == 120000 { print $4 }')
if [[ -n "$symlinked" ]]; then
  echo "lingrep patterns catalog tracked as a symlink (checkouts are not reproducible):" >&2
  echo "$symlinked" >&2
  echo >&2
  echo "Vendor the content as a regular file instead." >&2
  exit 1
fi

# 2. Regex operands belong to `~`/`!~`.
pattern='\[[^]]*[a-z0-9_]+[[:space:]]*!?=[[:space:]]*/[^/]*/'

offenders=$(git ls-files -z '*.rs' '*.lx' \
  | xargs -0 grep -nE "$pattern" 2>/dev/null \
  | grep -v 'legacy-lx-syntax-ok' || true)

if [[ -n "$offenders" ]]; then
  echo "Legacy lingrep patterns syntax (regex operand with = or !=):" >&2
  echo "$offenders" >&2
  echo >&2
  echo "Use '~' for a substring search, or '~ /\\A(?:...)\\z/' for a whole-value match." >&2
  exit 1
fi

echo "✓ lingrep patterns catalogs are regular files and free of legacy regex operands"
