#!/usr/bin/env python3
"""Compile the question interface's TypeScript browser modules."""

import argparse
import shutil
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parent
parser = argparse.ArgumentParser(description=__doc__)
mode = parser.add_mutually_exclusive_group()
mode.add_argument(
    "--check",
    action="store_true",
    help="Type-check and verify committed JavaScript is current",
)
mode.add_argument(
    "--watch", action="store_true", help="Recompile when TypeScript sources change"
)
args = parser.parse_args()
compiler = shutil.which("tsc")
if not compiler:
    parser.exit(
        1,
        "TypeScript compiler not found. Run this command inside nix develop .#default.\n",
    )
command = [compiler, "--project", str(root / "tsconfig.json")]
if args.check:
    with tempfile.TemporaryDirectory(prefix="ling-ui-build-") as directory:
        subprocess.run([*command, "--outDir", directory], check=True)
        stale = [
            path.name
            for path in Path(directory).iterdir()
            if not (root / path.name).exists()
            or path.read_bytes() != (root / path.name).read_bytes()
        ]
        if stale:
            parser.exit(
                1,
                "Generated browser assets are stale: "
                + ", ".join(sorted(stale))
                + ". Run build.py.\n",
            )
else:
    subprocess.run(
        [*command, *(["--watch", "--preserveWatchOutput"] if args.watch else [])],
        check=True,
    )
