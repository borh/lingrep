#!/usr/bin/env bash
# scripts/setup-dev.sh — point git at this repo's tracked hooks.
#
# Setting `core.hooksPath` disables `.git/hooks`; all active hooks therefore
# live in the tracked `.githooks/` directory.
#
# `core.hooksPath` is repo-local config and cannot be committed, so each clone
# must run this once.
#
# There is deliberately no pre-commit test hook. Agents and humans commit
# frequently, and a test run on every commit is a tax on the wrong event;
# push is the rarer, natural checkpoint where work becomes shared.
set -euo pipefail

# The checkout this script belongs to, from its own location, so running a
# worktree's copy configures that worktree's repository rather than whichever
# one the caller was standing in.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

git config core.hooksPath .githooks
echo "✅ core.hooksPath -> .githooks"
echo

echo "Hooks now active:"
for h in .githooks/*; do
    [ -f "$h" ] || continue
    [ -x "$h" ] || {
        echo "  ⚠ $h is not executable — run: chmod +x $h"
        continue
    }
    printf '  %-24s %s\n' "$(basename "$h")" "$(sed -n '2s/^# *//p' "$h")"
done
echo

# Report inert hooks that may be mistaken for active ones.
if compgen -G ".git/hooks/*" >/dev/null 2>&1; then
    stale="$(find .git/hooks -maxdepth 1 -type f ! -name '*.sample' ! -name '*.bak' 2>/dev/null || true)"
    if [ -n "$stale" ]; then
        echo "⚠ These files in .git/hooks/ are now IGNORED because core.hooksPath is set:"
        printf '    %s\n' $stale
        echo "  Move anything you still want into .githooks/ and delete the originals."
    fi
fi
