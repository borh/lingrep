#!/usr/bin/env python3
"""Emit the third-party notices for a `ling` release archive.

The notices are generated from the resolved dependency graph rather than
maintained by hand: a checked-in list of transitive dependencies goes stale on
the next `cargo update` and then quietly under-reports what the binary links.

Scope is what ships. The walk starts at the `lingrep` CLI crate, follows normal
dependencies only (build and dev dependencies are not in the binary), and drops
workspace members, whose license is the repository's own `LICENSE`.

License texts are collected from each crate's source directory, which exists
once the crate has been built, so this runs after `cargo build` — that is where
`scripts/package-release.sh` calls it. Texts are deduplicated by content, which
is why a single copy of Apache-2.0 covers dozens of crates.

Exit status is non-zero only when a crate declares no license *and* carries no
license file: that is a real gap in what the archive would be redistributing,
and it should be answered rather than printed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import subprocess
import sys

ROOT_PACKAGE = "lingrep"
LICENSE_FILE_GLOBS = ("LICENSE*", "LICENCE*", "COPYING*", "NOTICE*", "UNLICENSE*")


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


def license_texts(directory: pathlib.Path) -> list[tuple[str, str]]:
    found = []
    if not directory.is_dir():
        return found
    for pattern in LICENSE_FILE_GLOBS:
        for path in sorted(directory.glob(pattern)):
            if path.is_file() and path.stat().st_size:
                found.append((path.name, path.read_text(errors="replace")))
    return found


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
        collected = license_texts(package["directory"])
        if collected:
            for filename, text in collected:
                digest = hashlib.sha256(text.encode()).hexdigest()
                entry = texts.setdefault(
                    digest, {"text": text, "files": set(), "crates": []}
                )
                entry["files"].add(filename)
                entry["crates"].append(f"{package['name']} {package['version']}")
        elif not package["license"]:
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

    if undocumented:
        names = ", ".join(f"{p['name']} {p['version']}" for p in undocumented)
        print(
            f"error: no declared license and no license file for: {names}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
