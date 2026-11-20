#!/usr/bin/env bash
# Filesystem operations whose support differs between filesystems — no-replace
# rename, symbolic and hard links, no-clobber tempfile persistence, SQLite WAL
# and locking setup — go through lingrep-corpus/src/fs_capability.rs, where each
# fallback has fault-injection tests. This rejects direct uses in product
# source; test modules, test files and the allowlist below are exempt.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

# path|operation|reason
allowlist=$(cat <<'ALLOWLIST'
lingrep-corpus/src/fs_capability.rs|*|the seam itself
lingrep-core/src/dictionary.rs|rename|replaces a regular cache file in its own directory, which every supported filesystem permits; a failure is a cache miss
lingrep-perf/src/campaign.rs|rename|replaces a regular campaign file in its own directory, which every supported filesystem permits
lingrep-cli-common/src/packaged_dictionaries.rs|persist|replaces a regular dictionary file in its own directory, which every supported filesystem permits
ALLOWLIST
)

python3 - "$allowlist" lingrep-core/src lingrep-corpus/src lingrep/src lingrep-web/src \
    lingrep-cli-common/src lingrep-perf/src <<'PY'
import pathlib
import re
import sys

OPERATIONS = {
    "rename": re.compile(r"\bfs::rename\b|\blibc::rename|\brenameat2?\b|\brenamex_np\b"),
    "symlink": re.compile(r"\bsymlink(_file|_dir)?\s*\(|\bfs::symlink\b|\bsymlinkat\b"),
    "hard_link": re.compile(r"\bhard_link\b|\blibc::link(at)?\b"),
    "persist": re.compile(r"\.persist(_noclobber)?\s*\("),
    "sqlite_wal": re.compile(
        r"journal_mode[\s=(,'\"\\]*WAL|SqliteJournalMode::Wal|locking_mode", re.IGNORECASE
    ),
}

# A `use` of std::fs, std::os::unix::fs or libc that imports one of these
# functions, a glob, or a module alias makes later calls unqualified.
USE_STATEMENT = re.compile(r"\buse\s[^;]*;")
IMPORTED_NAMES = {
    "rename": re.compile(r"\b(rename|renameat2?|renamex_np)\b"),
    "symlink": re.compile(r"\b(symlink(_file|_dir|at)?|soft_link)\b"),
    "hard_link": re.compile(r"\b(hard_link|link(at)?)\b"),
}
IMPORTED_MODULE = re.compile(r"\b(fs|libc)\b")
GLOB_OR_ALIAS = re.compile(r"\b(fs|libc)::\*|\b(fs|libc|self)\s+as\b")

allowed = {}
for line in sys.argv[1].splitlines():
    path, operation, reason = line.split("|", 2)
    if not reason.strip():
        sys.exit(f"allowlist entry {path}|{operation} has no reason")
    allowed.setdefault(path, set()).add(operation)

TEST_ATTRIBUTE = re.compile(r"#\[cfg\((all\()?test\b[^\]]*\]\s*(#\[[^\]]*\]\s*)*")
TEST_MODULE_FILE = re.compile(
    r"#\[cfg\((all\()?test\b[^\]]*\]\s*(?P<attributes>(#\[[^\]]*\]\s*)*)"
    r"(pub(\([^)]*\))?\s+)?mod\s+(?P<name>\w+)\s*;"
)
PATH_ATTRIBUTE = re.compile(r'#\[path\s*=\s*"([^"]*)"\]')


def code_mask(text, blank_literals=True):
    """Blank comments and, unless `blank_literals` is false, literal contents."""
    out = list(text)
    i, n = 0, len(text)

    def blank(start, end):
        for k in range(start, end):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        if text.startswith("//", i):
            end = text.find("\n", i)
            end = n if end < 0 else end
            blank(i, end)
            i = end
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif (m := re.match(r'b?r(#*)"', text[i:])) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            closing = '"' + m.group(1)
            end = text.find(closing, i + m.end())
            end = n if end < 0 else end + len(closing)
            if blank_literals:
                blank(i + m.end(), end - len(closing))
            i = end
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            if blank_literals:
                blank(i + 1, j)
            i = j + 1
        elif m := re.match(r"'(\\.|[^\\'\n])'", text[i:]):
            if blank_literals:
                blank(i + 1, i + m.end() - 1)
            i += m.end()
        else:
            i += 1
    return "".join(out)


def test_module_ranges(masked):
    """Character ranges of inline `#[cfg(test)] mod name { ... }` blocks."""
    for match in TEST_ATTRIBUTE.finditer(masked):
        rest = masked[match.end():]
        item = re.match(r"(pub(\([^)]*\))?\s+)?mod\s+\w+\s*\{", rest)
        if not item:
            continue
        start = match.end() + item.end() - 1
        depth, j = 0, start
        while j < len(masked):
            if masked[j] == "{":
                depth += 1
            elif masked[j] == "}":
                depth -= 1
                if depth == 0:
                    break
            j += 1
        yield match.start(), j + 1


def blank_ranges(text, ranges):
    out = list(text)
    for start, end in ranges:
        for k in range(start, end):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


def module_directory(path):
    if path.name in {"lib.rs", "main.rs", "mod.rs"}:
        return path.parent
    return path.parent / path.stem


files = sorted(p for root in sys.argv[2:] for p in pathlib.Path(root).rglob("*.rs"))
masked = {path: code_mask(path.read_text()) for path in files}

test_files = set()
for path, text in masked.items():
    for match in TEST_MODULE_FILE.finditer(text):
        # `#[path]` values are blanked in the masked text, so read them from the source.
        source = path.read_text()[match.start("attributes"):match.end("attributes")]
        if explicit := PATH_ATTRIBUTE.search(source):
            test_files.add(path.parent / explicit.group(1))
            continue
        directory = module_directory(path)
        name = match.group("name")
        test_files.add(directory / f"{name}.rs")
        test_files.update((directory / name).rglob("*.rs"))

violations = []
for path in files:
    if path in test_files or "testing" in path.parts or path.parts[1] == "benches":
        continue
    exempt = allowed.get(path.as_posix(), set())
    if "*" in exempt:
        continue
    source = path.read_text()
    tests = list(test_module_ranges(masked[path]))
    code = blank_ranges(masked[path], tests).splitlines()
    # SQLite pragmas are string literals, so they are matched with literals kept.
    with_literals = blank_ranges(code_mask(source, blank_literals=False), tests).splitlines()
    for number, (line, literal_line) in enumerate(zip(code, with_literals), 1):
        for operation, pattern in OPERATIONS.items():
            searched = literal_line if operation == "sqlite_wal" else line
            if operation not in exempt and pattern.search(searched):
                violations.append(f"{path}:{number}: {operation}: {source.splitlines()[number - 1].strip()}")
    joined = "\n".join(code)
    for statement in USE_STATEMENT.finditer(joined):
        text = statement.group()
        if not IMPORTED_MODULE.search(text):
            continue
        operations = [operation for operation, pattern in IMPORTED_NAMES.items() if pattern.search(text)]
        if GLOB_OR_ALIAS.search(text):
            operations.append("fs_import")
        number = joined.count("\n", 0, statement.start()) + 1
        for operation in operations:
            if operation not in exempt:
                violations.append(f"{path}:{number}: {operation}: {source.splitlines()[number - 1].strip()}")

stale = sorted(path for path in allowed if not pathlib.Path(path).is_file())
if stale:
    print("allowlisted files do not exist:", *stale, sep="\n  ", file=sys.stderr)
    sys.exit(1)
if violations:
    print(
        "filesystem operations whose support differs by filesystem must go through "
        "lingrep_corpus::fs_capability (or carry a reasoned allowlist entry in "
        "scripts/check-fs-capability-seam.sh):",
        file=sys.stderr,
    )
    print(*dict.fromkeys(violations), sep="\n", file=sys.stderr)
    sys.exit(1)
PY

echo "filesystem-capability operations go through lingrep_corpus::fs_capability"
