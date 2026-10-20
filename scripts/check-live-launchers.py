#!/usr/bin/env python3
"""Validate live benchmark and executable launchers against Cargo metadata."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EXCLUDED = {
    Path("scripts/audit-matcher-cutover-docs.sh"),
}
BENCH_COMMAND = re.compile(r"cargo\s+bench\b(?P<args>.*?)(?=cargo\s+bench\b|$)")
BENCH_TARGET = re.compile(r"--bench(?:=|\s+)(?P<value>[^\s|;&]+)")
PACKAGE = re.compile(r"(?:^|\s)(?:-p|--package)(?:=|\s+)(?P<value>[^\s|;&]+)")
RETIRED_EXECUTABLE = re.compile(r"vp-pattern-matcher|vp_pattern_matcher")


def unquote(value: str) -> str:
    return value.strip("'\"")


def launcher_errors(
    text: str, valid_benches: set[tuple[str, str]]
) -> list[tuple[int, str]]:
    errors: list[tuple[int, str]] = []
    for line_number, line in enumerate(text.splitlines(), 1):
        if RETIRED_EXECUTABLE.search(line):
            errors.append((line_number, "retired vp-pattern-matcher executable"))
        for command in BENCH_COMMAND.finditer(line):
            args = command.group("args")
            bench_match = BENCH_TARGET.search(args)
            if bench_match is None:
                continue
            bench = unquote(bench_match.group("value"))
            package_match = PACKAGE.search(args)
            if package_match is None:
                errors.append(
                    (line_number, f"benchmark {bench!r} has no explicit Cargo package")
                )
                continue
            package = unquote(package_match.group("value"))
            if "$" in package or "$" in bench:
                errors.append(
                    (line_number, "dynamic package or benchmark target cannot be audited")
                )
            elif (package, bench) not in valid_benches:
                errors.append(
                    (
                        line_number,
                        f"unknown Cargo benchmark target {package}:{bench}",
                    )
                )
    return errors


def self_test() -> None:
    valid = {("lingrep-core", "fsm_performance")}
    assert not launcher_errors(
        "cargo bench -p lingrep-core --bench fsm_performance --no-run", valid
    )
    assert launcher_errors("cargo bench -p lingrep-core --bench deleted", valid)
    assert launcher_errors("cargo bench --bench fsm_performance", valid)
    assert launcher_errors('cargo bench -p "$package" --bench "$bench"', valid)
    assert launcher_errors("vp-pattern-matcher --help", valid)


def cargo_benches() -> set[tuple[str, str]]:
    metadata = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    workspace = json.loads(metadata.stdout)
    return {
        (package["name"], target["name"])
        for package in workspace["packages"]
        for target in package["targets"]
        if "bench" in target["kind"]
    }


def live_files() -> list[Path]:
    workflows = list((ROOT / ".github").rglob("*.yml")) + list(
        (ROOT / ".github").rglob("*.yaml")
    )
    scripts = list((ROOT / "scripts").rglob("*.sh"))
    return sorted(
        path
        for path in workflows + scripts
        if path.relative_to(ROOT) not in EXCLUDED
    )


def main() -> int:
    self_test()
    valid_benches = cargo_benches()
    failed = False
    for path in live_files():
        relative = path.relative_to(ROOT)
        for line_number, message in launcher_errors(
            path.read_text(encoding="utf-8"), valid_benches
        ):
            print(f"invalid live launcher: {relative}:{line_number}: {message}", file=sys.stderr)
            failed = True
    if failed:
        return 1
    print("Live Cargo benchmark and executable launchers are valid.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
