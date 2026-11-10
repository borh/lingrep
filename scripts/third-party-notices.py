#!/usr/bin/env python3
"""Emit the third-party notices for a `ling` release archive.

The notices are generated from the resolved dependency graph rather than
maintained by hand: a checked-in list of transitive dependencies goes stale on
the next `cargo update` and then quietly under-reports what the binary links.

Scope is what ships. The walk starts at the `lingrep` CLI crate, follows normal
dependencies only (build and dev dependencies are not in the binary), and drops
workspace members, whose license is the repository's own `LICENSE`.

Browser asset notices are included from `lingrep-web/assets/THIRD-PARTY-NOTICES.md`.

License texts are collected from each crate's source directory, which exists
once the crate has been built, so this runs after `cargo build` — that is where
`scripts/package-release.sh` calls it. Texts are deduplicated by content, which
is why a single copy of Apache-2.0 covers dozens of crates.

A git dependency's texts are also looked up at the root of its checkout. A
crate that publishes no text gets it from
`scripts/third-party-licenses/<name>-<version>/`, copied from the crate's
upstream repository.

Exit status is non-zero when a crate with no text found declares no license, or
declares one of `REPRODUCED_LICENSES`, which require their text to accompany
a binary, and is not listed in `TEXT_EXCEPTIONS`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys

ROOT_PACKAGE = "lingrep"
LICENSE_FILE_GLOBS = (
    "LICENSE*",
    "LICENCE*",
    "COPYING*",
    "NOTICE*",
    "UNLICENSE*",
    "LICENSES/*",
)
VENDORED_TEXTS = pathlib.Path(__file__).resolve().parent / "third-party-licenses"
REPRODUCED_LICENSES = {
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "MIT",
    "Unicode-3.0",
    "Unicode-DFS-2016",
}
# `"<name> <version>"` of a crate that ships without its license text, mapped
# to the reason the archive may omit it.
TEXT_EXCEPTIONS: dict[str, str] = {}


def cargo_metadata(target: str | None) -> dict:
    command = ["cargo", "metadata", "--locked", "--format-version", "1"]
    if target:
        command += ["--filter-platform", target]
    completed = subprocess.run(command, capture_output=True, text=True)
    if completed.returncode != 0:
        sys.exit(f"error: cargo metadata failed:\n{completed.stderr}")
    return json.loads(completed.stdout)


def shipped_packages(metadata: dict) -> list[dict]:
    packages = {package["id"]: package for package in metadata["packages"]}
    workspace = set(metadata["workspace_members"])
    root = next(
        (
            package_id
            for package_id in metadata["workspace_members"]
            if packages[package_id]["name"] == ROOT_PACKAGE
        ),
        None,
    )
    if root is None:
        sys.exit(f"error: no workspace package named {ROOT_PACKAGE!r}")

    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    seen: set[str] = set()
    queue = [root]
    while queue:
        package_id = queue.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        for dependency in nodes.get(package_id, {}).get("deps", []):
            kinds = {kind["kind"] for kind in dependency.get("dep_kinds", [])}
            # `None` is a normal dependency in cargo metadata's vocabulary.
            if kinds and not kinds & {None, "normal"}:
                continue
            queue.append(dependency["pkg"])

    shipped = []
    for package_id in seen:
        if package_id in workspace:
            continue
        package = packages[package_id]
        shipped.append(
            {
                "name": package["name"],
                "version": package["version"],
                "license": package["license"],
                "repository": package.get("repository"),
                "source": package.get("source"),
                "directory": pathlib.Path(package["manifest_path"]).parent,
            }
        )
    return sorted(shipped, key=lambda package: (package["name"], package["version"]))


def texts_in(directory: pathlib.Path) -> list[tuple[str, str]]:
    found = []
    if not directory.is_dir():
        return found
    for pattern in LICENSE_FILE_GLOBS:
        for path in sorted(directory.glob(pattern)):
            if path.is_file() and path.stat().st_size:
                found.append((path.name, path.read_text(errors="replace")))
    return found


def git_checkout_root(directory: pathlib.Path) -> pathlib.Path | None:
    for candidate in (directory, *directory.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


def license_texts(package: dict) -> list[tuple[str, str]]:
    found = texts_in(package["directory"])
    if not found and (package["source"] or "").startswith("git+"):
        root = git_checkout_root(package["directory"])
        if root is not None:
            found = texts_in(root)
    if not found:
        found = texts_in(VENDORED_TEXTS / f"{package['name']}-{package['version']}")
    return found


def requires_text(license_expression: str | None) -> bool:
    if not license_expression:
        return True
    identifiers = license_expression.replace("(", " ").replace(")", " ").replace("/", " ")
    return bool(set(identifiers.split()) & REPRODUCED_LICENSES)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--target",
        help="Cargo target triple to filter platform-specific dependencies by",
    )
    arguments = parser.parse_args()

    metadata = cargo_metadata(arguments.target)
    version = next(
        package["version"]
        for package in metadata["packages"]
        if package["name"] == ROOT_PACKAGE
    )
    shipped = shipped_packages(metadata)

    texts: dict[str, dict] = {}
    undocumented: list[dict] = []
    for package in shipped:
        collected = license_texts(package)
        if collected:
            for filename, text in collected:
                digest = hashlib.sha256(text.encode()).hexdigest()
                entry = texts.setdefault(
                    digest, {"text": text, "files": set(), "crates": []}
                )
                entry["files"].add(filename)
                entry["crates"].append(f"{package['name']} {package['version']}")
        elif (
            requires_text(package["license"])
            and f"{package['name']} {package['version']}" not in TEXT_EXCEPTIONS
        ):
            undocumented.append(package)

    print(f"# Third-party notices for ling {version}")
    print()
    print(
        "This archive contains the `ling` executable, which links the Rust "
        "crates below.\nEach crate is used under the license it declares. The "
        "license texts that the\ncrate sources carry are reproduced in full "
        "after the table, deduplicated by\ntext."
    )
    print()
    print("| crate | version | license |")
    print("| --- | --- | --- |")
    for package in shipped:
        license_name = package["license"] or "see the license text below"
        print(f"| `{package['name']}` | {package['version']} | {license_name} |")
    print()
    print(
        f"{len(shipped)} crates. Generated from `Cargo.lock` by "
        "`scripts/third-party-notices.py`; regenerate it rather than editing "
        "this file."
    )
    print()
    print("## Embedded data")
    print()
    print(
        "`ling` embeds a table derived from the NDC9 (日本十進分類法 新訂9版) "
        "Linked Data,\nby the Japan Library Association Classification "
        "Committee (日本図書館協会\n分類委員会) and the National Diet Library, "
        "published under CC BY at\n<https://www.jla.or.jp/committees/bunrui/ndc-data/>. "
        "The table was converted\nfrom that data to tab-separated notation and "
        "label rows."
    )

    for index, entry in enumerate(
        sorted(texts.values(), key=lambda entry: sorted(entry["crates"])[0]), start=1
    ):
        titles = ", ".join(sorted(entry["files"]))
        crates = ", ".join(sorted(set(entry["crates"])))
        print()
        print(f"## License text {index}: {titles}")
        print()
        print(f"Applies to: {crates}")
        print()
        print("```")
        print(entry["text"].rstrip())
        print("```")

    print()
    browser_notices = (
        pathlib.Path(__file__).resolve().parents[1]
        / "lingrep-web/assets/THIRD-PARTY-NOTICES.md"
    )
    print(browser_notices.read_text(encoding="utf-8"), end="")

    if undocumented:
        names = ", ".join(f"{p['name']} {p['version']}" for p in undocumented)
        print(
            f"error: no license text for: {names}; add it under "
            f"{VENDORED_TEXTS.name}/<name>-<version>/ or to TEXT_EXCEPTIONS",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
