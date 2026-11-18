#!/usr/bin/env python3
"""Run a short ling scenario with its input, outputs and temporary files on one filesystem.

Usage: check-output-filesystem.py OUTPUT_DIR LING

OUTPUT_DIR must be a new or empty directory on the filesystem under test. ling
finds its dictionary through --dict-path's usual fallbacks: LINGREP_DICTIONARY_PATH
or the dictionary `ling dictionary install` installed.
"""

import ctypes
import ctypes.util
import errno
import io
import json
import os
import platform
import re
import sqlite3
import subprocess
import sys
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

TEXT = "猫の名前はまだない。\n犬が庭の中を走る。\n"
PATTERN = "particle := [text=の].\n"
SYMLINK_ERROR = "cannot create symbolic links in"
DESTINATION_EXISTS = "the destination already exists and was left unchanged"
# The errno values lingrep_corpus::fs_capability::Capability::unsupported_by
# treats as missing support. ENOTSUP and EOPNOTSUPP are one value on Linux.
UNSUPPORTED = {
    "rename_noreplace": {errno.ENOSYS, errno.EINVAL, errno.EOPNOTSUPP, errno.ENOTSUP},
    "symlink": {
        errno.EPERM,
        errno.EACCES,
        errno.ENOSYS,
        errno.EOPNOTSUPP,
        errno.ENOTSUP,
    },
    "hard_link": {errno.EPERM, errno.ENOSYS, errno.EOPNOTSUPP, errno.ENOTSUP},
}
# A hung ling call fails the script, with its stderr, before a CI job's timeout.
DEADLINE = time.monotonic() + 8 * 60


def seconds_left(limit):
    left = min(limit, DEADLINE - time.monotonic())
    if left <= 0:
        raise TimeoutError("the scenario exceeded its 8-minute budget")
    return left


def rename_noreplace(source, destination):
    libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    source, destination = os.fsencode(source), os.fsencode(destination)
    if platform.system() == "Darwin":
        result = libc.renamex_np(source, destination, 0x4)  # RENAME_EXCL
    else:
        numbers = {"x86_64": 316, "aarch64": 276}
        if platform.machine() not in numbers:
            raise RuntimeError(f"no renameat2 syscall number for {platform.machine()}")
        at_fdcwd, rename_noreplace_flag = -100, 1
        result = libc.syscall(
            numbers[platform.machine()],
            at_fdcwd,
            source,
            at_fdcwd,
            destination,
            rename_noreplace_flag,
        )
    if result:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def sqlite_wal(path, exclusive):
    connection = sqlite3.connect(path)
    try:
        if exclusive:
            connection.execute("PRAGMA locking_mode=EXCLUSIVE")
        mode = connection.execute("PRAGMA journal_mode=WAL").fetchone()[0]
        connection.execute("CREATE TABLE t(x)")
        connection.commit()
        if mode != "wal":
            raise RuntimeError(f"journal_mode is {mode}")
    finally:
        connection.close()


def probe(directory):
    """Print and return what the filesystem returns for each operation ling's fallbacks
    depend on: None for success, the errno for an OS error, or a message."""
    directory.mkdir()
    (directory / "target").touch()
    (directory / "staging").mkdir()
    (directory / "Case").touch()

    def case_collision():
        try:
            os.close(os.open(directory / "case", os.O_CREAT | os.O_EXCL | os.O_WRONLY))
        except FileExistsError:
            raise RuntimeError("case-insensitive") from None

    checks = {
        "rename_noreplace": lambda: rename_noreplace(
            directory / "staging", directory / "published"
        ),
        "symlink": lambda: os.symlink("target", directory / "link"),
        "hard_link": lambda: os.link(directory / "target", directory / "hard"),
        "sqlite_wal": lambda: sqlite_wal(directory / "shared.sqlite", exclusive=False),
        "sqlite_wal_exclusive": lambda: sqlite_wal(
            directory / "exclusive.sqlite", exclusive=True
        ),
        "distinct_case": case_collision,
    }
    results = {}
    for name, check in checks.items():
        try:
            check()
            results[name], shown = None, "ok"
        except OSError as error:
            results[name] = error.errno
            shown = errno.errorcode.get(error.errno, str(error.errno))
        except (sqlite3.Error, RuntimeError) as error:
            results[name] = shown = str(error)
        print(f"probe {name}: {shown}", flush=True)
    return results


def ling(binary, *args, env, expect_success=True):
    print("+ ling " + " ".join(map(str, args)), flush=True)
    try:
        result = subprocess.run(
            [binary, *map(str, args)],
            check=False,
            capture_output=True,
            text=True,
            env=env,
            timeout=seconds_left(180),
        )
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(
            f"timed out after {error.timeout:.0f} s\n{error.stderr}"
        ) from None
    if expect_success and result.returncode:
        raise RuntimeError(f"exit {result.returncode}\n{result.stderr}")
    return result


def expect_failure(result, *fragments):
    assert result.returncode != 0, f"expected a failure containing {fragments}"
    for fragment in fragments:
        assert fragment in result.stderr, f"expected {fragment!r} in:\n{result.stderr}"


def wait_for_address(server, log):
    while not (address := re.search(r"listening on (http://\S+)", log.read_text())):
        if server.poll() is not None:
            raise RuntimeError(f"server exited before listening ({server.returncode})")
        seconds_left(1)
        time.sleep(0.05)
    return address.group(1)


def serve(binary, root, env, table):
    log, errors = root / "server.stdout", root / "server.stderr"
    print("+ ling serve", flush=True)
    with log.open("w") as stdout, errors.open("w") as stderr:
        server = subprocess.Popen(
            [
                binary,
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--input",
                root / "input" / "novel.txt",
                "--corpus",
                f"prepared={root / 'novel.lingcorpus'}",
                "--count-table",
                table,
                "--patterns",
                root / "catalog" / "check.lx",
            ],
            stdout=stdout,
            stderr=stderr,
            env=env,
        )
        try:
            base = wait_for_address(server, log)
            http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def get(path, data=None):
                with http.open(
                    base + path, data=data, timeout=seconds_left(120)
                ) as response:
                    return response.read()

            meta = json.loads(get("/api/frequency/meta"))
            assert len(meta["pairs"]) == 1, meta
            # The default corpus is the plain-text input, which the server
            # prepares beside its source on the filesystem under test.
            form = urllib.parse.urlencode({"pattern": PATTERN, "retained": 20}).encode()
            assert b"api/stream/check" in get("/api/search/check", form)
            stream = get("/api/stream/check").decode()
            assert "event: result" in stream, stream
            with zipfile.ZipFile(
                io.BytesIO(get("/api/export/check/1?locale=en"))
            ) as workbook:
                assert "の" in workbook.read("xl/sharedStrings.xml").decode()
            server.terminate()
            server.wait(timeout=seconds_left(20))
            assert server.returncode == 0, f"server exit {server.returncode}"
        except Exception:
            print(log.read_text(), errors.read_text(), sep="\n", file=sys.stderr)
            raise
        finally:
            if server.poll() is None:
                server.kill()
                server.wait()


def analyzer_preflight(binary, root, env, symlink):
    """Run `ling analyzer prepare` with an empty wheel directory, so it stops before any download."""
    (root / "no-wheels").mkdir()
    output = root / "analyzer"
    result = ling(
        binary,
        "analyzer",
        "prepare",
        "ginza",
        "--output",
        output,
        "--wheels",
        root / "no-wheels",
        env=env,
        expect_success=False,
    )
    if symlink is None:
        # The preflight passed; reading SHA256SUMS from the empty directory failed.
        expect_failure(result, f"(os error {errno.ENOENT})")
        assert SYMLINK_ERROR not in result.stderr, result.stderr
    elif symlink in UNSUPPORTED["symlink"]:
        expect_failure(result, SYMLINK_ERROR, os.strerror(symlink))
    else:
        expect_failure(result, f"(os error {symlink})")
        assert SYMLINK_ERROR not in result.stderr, result.stderr
    assert not output.exists(), f"{output} was left behind"


def ending_report(binary, root, env, corpus, probed):
    """Write `matches --ending-report` twice: the first creates the report through no-replace
    rename, a hard link or exclusive creation, and the second leaves it unchanged."""
    report = root / "endings.json"
    rename, hard_link = probed["rename_noreplace"], probed["hard_link"]
    if not (
        rename is None
        or (rename in UNSUPPORTED["rename_noreplace"] and hard_link is None)
        or (
            rename in UNSUPPORTED["rename_noreplace"]
            and hard_link in UNSUPPORTED["hard_link"]
        )
    ):
        raise RuntimeError(
            f"no expectation for rename {rename} and hard link {hard_link}"
        )
    arguments = [
        "matches",
        "--corpus",
        corpus,
        "--patterns",
        root / "catalog" / "check.lx",
        "--ending-report",
        report,
    ]
    ling(binary, *arguments, env=env)
    written = report.read_bytes()
    assert json.loads(written)["version"] == 1
    again = ling(binary, *arguments, env=env, expect_success=False)
    expect_failure(again, "already exists")
    assert report.read_bytes() == written, f"{report} was changed"


def case_variant(binary, root, env, corpus, case_insensitive):
    """Build a second corpus whose name differs from the first only by case."""
    variant = root / corpus.name.upper()
    result = ling(
        binary,
        "corpus",
        "build",
        "--input-format",
        "plain",
        "--output",
        variant,
        root / "input" / "novel.txt",
        env=env,
        expect_success=False,
    )
    names = set(os.listdir(root))
    if case_insensitive:
        expect_failure(result, DESTINATION_EXISTS)
        assert corpus.name in names and variant.name not in names, sorted(names)
        ling(binary, "corpus", "inspect", "--verify", corpus, env=env)
    else:
        assert result.returncode == 0, result.stderr
        assert {corpus.name, variant.name} <= names, sorted(names)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    root = Path(sys.argv[1]).absolute()
    binary = str(Path(sys.argv[2]).resolve())
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        sys.exit(f"{root} is not empty")

    probed = probe(root / "probe")
    (root / "input").mkdir()
    (root / "input" / "novel.txt").write_text(TEXT, encoding="utf-8")
    (root / "catalog").mkdir()
    (root / "catalog" / "check.lx").write_text(PATTERN, encoding="utf-8")
    (root / "tmp").mkdir()
    env = os.environ | {"TMPDIR": str(root / "tmp")}

    corpus = root / "novel.lingcorpus"
    table = root / "novel.lingtable"
    ling(
        binary,
        "corpus",
        "build",
        "--input-format",
        "plain",
        "--output",
        corpus,
        root / "input" / "novel.txt",
        env=env,
    )
    ling(binary, "corpus", "inspect", "--verify", "--documents", corpus, env=env)
    ling(
        binary,
        "summarize",
        "--corpus",
        corpus,
        "--patterns",
        root / "catalog" / "check.lx",
        "--write-table",
        table,
        env=env,
    )
    ling(
        binary,
        "table",
        "--corpus",
        corpus,
        "--patterns",
        root / "catalog" / "check.lx",
        table,
        env=env,
    )
    ending_report(binary, root, env, corpus, probed)
    serve(binary, root, env, table)
    analyzer_preflight(binary, root, env, probed["symlink"])
    case_variant(
        binary, root, env, corpus, probed["distinct_case"] == "case-insensitive"
    )
    print(f"Output filesystem check passed: {root}")


if __name__ == "__main__":
    main()
