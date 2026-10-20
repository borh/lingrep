#!/usr/bin/env bash
# Durability retirement gate.
#
# The rule is: a generation-and-scheme this package reads keeps a stored
# fixture, and retiring one requires the same change to add a migration test.
# "Unless the same change adds a migration test" cannot be enforced from a diff
# without a naming convention, so there is one:
#
#   lingrep-corpus/tests/fixtures/durability/g<generation>-<scheme>/
#   lingrep-corpus/tests/suite/migration_g<generation>_<scheme>.rs
#
# Removing the fixture directory without adding the correspondingly named
# module fails. That is the whole gate: it makes the promise checkable rather
# than aspirational.
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
fixtures="$workspace_root/lingrep-corpus/tests/fixtures/durability"
suite="$workspace_root/lingrep-corpus/tests/suite"

python3 - "$fixtures" "$suite" <<'PY'
import pathlib
import re
import subprocess
import sys

fixtures = pathlib.Path(sys.argv[1])
suite = pathlib.Path(sys.argv[2])
name_pattern = re.compile(r"^g(\d+)-(legacy|a\d+)$")

failures = []

if not fixtures.is_dir():
    failures.append(
        f"{fixtures} does not exist. At least one durable generation must be "
        "retained; an empty set is a retirement, not a clean slate."
    )
    present = set()
else:
    present = set()
    for entry in sorted(fixtures.iterdir()):
        if not entry.is_dir():
            failures.append(f"unexpected file in durability fixtures: {entry}")
            continue
        if not name_pattern.match(entry.name):
            failures.append(
                f"durability fixture {entry.name!r} does not follow "
                "g<generation>-<scheme>; the gate matches on that name"
            )
            continue
        present.add(entry.name)
        for component in ("manifest.json", "tokens.v1.zst", "locations.v1",
                          "provenance.sqlite"):
            if not (entry / component).is_file():
                failures.append(
                    f"durability fixture {entry.name} is missing {component}; "
                    "a fixture that cannot be opened proves nothing"
                )

# What the previous commit retained. Comparing against HEAD is what turns this
# from an inventory check into a *retirement* check: a fixture that was never
# there is not a retirement, and one that disappeared is.
try:
    listing = subprocess.run(
        ["git", "ls-tree", "--name-only", "HEAD",
         "lingrep-corpus/tests/fixtures/durability/"],
        capture_output=True, text=True, check=True,
    ).stdout
except subprocess.CalledProcessError:
    listing = ""
previous = {
    pathlib.PurePosixPath(line).name
    for line in listing.splitlines()
    if line.strip()
}

migrations = {
    path.stem for path in suite.glob("migration_g*.rs")
} if suite.is_dir() else set()

for retired in sorted(previous - present):
    match = name_pattern.match(retired)
    if not match:
        continue
    expected = f"migration_g{match.group(1)}_{match.group(2)}"
    if expected not in migrations:
        failures.append(
            f"durability fixture {retired} was removed without "
            f"lingrep-corpus/tests/suite/{expected}.rs.\n"
            "       Retiring a readable generation or identity scheme is a "
            "breaking change for every artifact already on disk.\n"
            "       Add the migration test, or restore the fixture."
        )

if failures:
    print("error: durability fixture gate failed:", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    raise SystemExit(1)

print(f"✓ {len(present)} durable generation/scheme fixture(s) retained")
PY
