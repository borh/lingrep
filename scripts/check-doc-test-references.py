#!/usr/bin/env python3
"""Fail if a live guide cites a test file or test target that does not exist.

Why this exists: `scripts/check-test-consolidation.sh` guards the code side of
the test consolidation — a `.rs` dropped into a suite directory without a `mod`
line. Nothing guarded the prose. Seven live guides went on citing top-level
`<crate>/tests/*.rs` targets for months after those files became modules under
`tests/suite/`, and every `cargo test --test <name>` they printed failed with
"no test target named ...". A reader following the documentation could not run
the tests the documentation named, and a green CI run said nothing about it.

Two checks, both against the working tree rather than a list maintained here:

1. A `<crate>/tests/…rs` path in prose must exist on disk.
2. A `cargo test … --test NAME` must name a real integration-test target —
   a `<crate>/tests/NAME.rs` file, or a `<crate>/tests/NAME/main.rs` suite.

Scope is LIVE GUIDES only. A dated record — a plan, a results write-up, a task
report, the changelog, or any file opening with a "Historical record" banner —
is a statement about what was true on a date. Rewriting a path inside one
falsifies it, so those are skipped rather than fixed. That is not a loophole:
177 of the 186 dead paths in this tree are theirs and belong to them.

A failure names where the file actually lives now, because the fix is almost
always `tests/X.rs` -> `tests/suite/X.rs` and the command form
`--test X` -> `--test suite -- X::`.
"""

import os
import re
import subprocess
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Directories whose contents are dated records, not instructions for today.
DATED_PREFIXES = (
    "docs/testing/",
    "third-party/",
)
DATED_FILES = {"CHANGELOG.md"}

# A file may also opt out by opening with the repository's historical banner.
HISTORICAL = re.compile(r"Historical (?:record|test summary|strategy)", re.IGNORECASE)
BANNER_WINDOW = 400

TEST_PATH = re.compile(r"\b((?:lingrep(?:-[a-z-]+)?|vp-[a-z]+)/tests/[A-Za-z0-9_./-]*\.rs)")
TEST_TARGET = re.compile(r"cargo test[^\n`]*?--test\s+([A-Za-z0-9_]+)")


def tracked_markdown():
    out = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "*.md"],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    return [p for p in out if (ROOT / p).is_file()]


def is_live_guide(rel, text):
    if rel in DATED_FILES or rel.startswith(DATED_PREFIXES):
        return False
    return not HISTORICAL.search(text[:BANNER_WINDOW])


def real_test_targets():
    """Integration-test target names cargo would actually accept, per crate."""
    targets = set()
    for crate in sorted(os.listdir(ROOT)):
        tests = ROOT / crate / "tests"
        if not tests.is_dir():
            continue
        for entry in tests.iterdir():
            if entry.suffix == ".rs" and entry.is_file():
                targets.add(entry.stem)
            elif entry.is_dir() and (entry / "main.rs").exists():
                targets.add(entry.name)
    return targets


def relocated(basename):
    """Where a moved test file lives now, to make the failure actionable."""
    hits = []
    for crate in sorted(os.listdir(ROOT)):
        tests = ROOT / crate / "tests"
        if not tests.is_dir():
            continue
        for found in tests.rglob(basename):
            hits.append(str(found.relative_to(ROOT)))
    return hits


def main():
    targets = real_test_targets()
    failures = []
    scanned = 0

    for rel in tracked_markdown():
        text = (ROOT / rel).read_text(encoding="utf-8", errors="replace")
        if not is_live_guide(rel, text):
            continue
        scanned += 1

        for match in TEST_PATH.finditer(text):
            cited = match.group(1)
            if (ROOT / cited).exists():
                continue
            where = relocated(os.path.basename(cited))
            hint = f"now at {', '.join(where)}" if where else "gone entirely; do not repoint it, say so"
            failures.append(f"{rel}: cites `{cited}` — {hint}")

        for match in TEST_TARGET.finditer(text):
            name = match.group(1)
            if name in targets:
                continue
            where = relocated(f"{name}.rs")
            hint = (
                f"consolidated into {', '.join(where)}; use `--test suite -- {name}::`"
                if where
                else "no such target, and no file of that name survives"
            )
            failures.append(f"{rel}: runs `cargo test --test {name}` — {hint}")

    if failures:
        print("doc test-reference check failed:")
        for failure in failures:
            print(" -", failure)
        print()
        print(f"{len(failures)} dead reference(s) in {scanned} live guides.")
        print("A dated record (a task report or a file")
        print('opening with a "Historical record" banner) is exempt by design.')
        return 1

    print(f"doc test references: {scanned} live guides, no dead paths or targets")
    return 0


if __name__ == "__main__":
    sys.exit(main())
