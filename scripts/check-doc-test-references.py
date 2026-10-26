#!/usr/bin/env python3
"""Validate live guide references against existing files and Cargo test targets."""

import json
import re
import subprocess
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent

DATED_PREFIXES = (
    "third-party/",
)
DATED_FILES = {"CHANGELOG.md"}

HISTORICAL = re.compile(r"Historical (?:record|test summary|strategy)", re.IGNORECASE)
BANNER_WINDOW = 400

TEST_PATH = re.compile(r"\b((?:lingrep(?:-[a-z-]+)?|vp-[a-z]+)/tests/[A-Za-z0-9_./-]*\.rs)")
TEST_COMMAND = re.compile(r"\bcargo\s+test\b([^\n`]*)")
TEST_TARGET = re.compile(r"(?:^|\s)--test(?:=|\s+)[\"']?([A-Za-z0-9_-]+)")
PACKAGE = re.compile(r"(?:^|\s)(?:-p|--package)(?:=|\s+)[\"']?([A-Za-z0-9_-]+)")


def tracked_markdown():
    out = subprocess.run(
        ["git", "-C", str(ROOT), "ls-files", "-z", "*.md"],
        capture_output=True, text=True, check=True,
    ).stdout.split("\0")
    return [p for p in out if (ROOT / p).is_file()]


def is_live_guide(rel, text):
    if rel in DATED_FILES or rel.startswith(DATED_PREFIXES):
        return False
    return not HISTORICAL.search(text[:BANNER_WINDOW])


def real_test_targets():
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version=1"],
        cwd=ROOT, capture_output=True, text=True, check=True,
    )
    return {
        (package["name"], target["name"])
        for package in json.loads(metadata.stdout)["packages"]
        for target in package["targets"]
        if "test" in target["kind"]
    }


def relocated(basename):
    return sorted(str(path.relative_to(ROOT)) for path in ROOT.glob(f"*/tests/**/{basename}"))


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
            where = relocated(pathlib.Path(cited).name)
            hint = f"now at {', '.join(where)}" if where else "gone entirely; do not repoint it, say so"
            failures.append(f"{rel}: cites `{cited}` — {hint}")

        for command in TEST_COMMAND.finditer(text.replace("\\\n", " ")):
            args = re.split(r"\s--(?:\s|$)", command.group(1), maxsplit=1)[0]
            packages = PACKAGE.findall(args)
            for name in TEST_TARGET.findall(args):
                if any(
                    target == name and (not packages or package in packages)
                    for package, target in targets
                ):
                    continue
                failures.append(
                    f"{rel}: runs `cargo test{command.group(1)}` — "
                    f"no test target {name!r} in {', '.join(packages) or 'the workspace'}"
                )

    if failures:
        print("doc test-reference check failed:")
        for failure in failures:
            print(" -", failure)
        print()
        print(f"{len(failures)} dead reference(s) in {scanned} live guides.")
        return 1

    print(f"doc test references: {scanned} live guides, no dead paths or targets")
    return 0


if __name__ == "__main__":
    sys.exit(main())
