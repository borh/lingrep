#!/usr/bin/env bash
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
case_root="$workspace_root/scripts/tests/artifact-naming-cases"

# category|repository-relative path|why this file may mention all name parts
# Entries are exact file-level exceptions; each must retain a non-empty reason.
allowlist=$(cat <<'ALLOWLIST'
guard-self-test|scripts/check-artifact-naming.sh|contains detector fixtures and rules, not a corpus path construction
documentation|lingrep/src/cli.rs|documents the canonical shape for users; it does not compose a path
documentation|nix/modules/lingrep-web/nixos.nix|module option prose documents the shape; it does not compose a path
explicit-destination|lingrep-corpus/src/publication.rs|test-only fixed destination supplied by the caller
test-benchmark|lingrep-corpus/benches/prepared_tokens.rs|benchmark fixture destination, not a derived canonical name
test-benchmark|lingrep-web/src/corpus_runtime.rs|cfg(test) fixture destination, not a derived canonical name
test-benchmark|lingrep/tests/suite/prepared_protocol.rs|asserts canonical output shape without selecting the production destination
test-benchmark|lingrep-corpus/tests/suite/artifact.rs|pins the sole owner's public output contract with literal expected paths
perf-rehearsal|lingrep-perf/src/campaign/tests.rs|campaign fixture name, not corpus canonicalization
ALLOWLIST
)

python3 - "$workspace_root" "$case_root" "$allowlist" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
cases = pathlib.Path(sys.argv[2])
entries = {}
for line in sys.argv[3].splitlines():
    parts = line.split("|", 2)
    if len(parts) != 3 or not all(parts):
        raise SystemExit(f"invalid artifact-naming allowlist entry: {line!r}")
    category, path, reason = parts
    entries[path] = (category, reason)

generation = re.compile(r"CORPUS_ARTIFACT_FORMAT_GENERATION|\bgeneration\b|\bgen(?:eration)?_?id\b")
identity = re.compile(r"analysis_identity|\bidentity\b|identity_digest|\bdigest\b")
filename = re.compile(r"\.lingcorpus")
composer = re.compile(r"format!\s*\(|set_extension\s*\(|\.push(?:_str)?\s*\(|\bprintf\b|\$\{")

def without_comments(text: str, suffix: str) -> str:
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    marker = "//" if suffix == ".rs" else "#"
    return "\n".join(line.split(marker, 1)[0] for line in text.splitlines())

def offenders(path: pathlib.Path):
    text = without_comments(path.read_text(encoding="utf-8"), path.suffix)
    hits = []
    for match in filename.finditer(text):
        start = max(0, match.start() - 1200)
        end = min(len(text), match.end() + 1200)
        window = text[start:end]
        if generation.search(window) and identity.search(window) and composer.search(window):
            line = text.count("\n", 0, match.start()) + 1
            hits.append(line)
    return hits

failed = False
for sample in sorted(cases.iterdir()):
    if not sample.is_file():
        continue
    rejected = bool(offenders(sample))
    expected_reject = sample.name.startswith("reject-")
    if rejected != expected_reject:
        print(f"artifact-naming self-test failed: {sample.name} rejected={rejected}", file=sys.stderr)
        failed = True
if failed:
    raise SystemExit(1)

found = []
for path in root.rglob("*"):
    if not path.is_file() or path.suffix not in {".rs", ".nix", ".sh"}:
        continue
    relative = path.relative_to(root).as_posix()
    if relative.startswith((".git/", ".worktrees/", "target/", "scripts/tests/artifact-naming-cases/")):
        continue
    if relative == "lingrep-corpus/src/artifact.rs" or relative in entries:
        continue
    for line in offenders(path):
        found.append((relative, line))

if found:
    for path, line in found:
        print(f"artifact naming must be owned by lingrep-corpus/src/artifact.rs: {path}:{line}", file=sys.stderr)
    raise SystemExit(1)
PY
