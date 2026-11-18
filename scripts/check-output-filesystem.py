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


def rename_noreplace(source, destination):
    libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    source, destination = os.fsencode(source), os.fsencode(destination)
    if platform.system() == "Darwin":
        result = libc.renamex_np(source, destination, 0x4)  # RENAME_EXCL
    else:
        renameat2 = {"x86_64": 316, "aarch64": 276}[platform.machine()]
        at_fdcwd, rename_noreplace_flag = -100, 1
        result = libc.syscall(
            renameat2, at_fdcwd, source, at_fdcwd, destination, rename_noreplace_flag
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
    """Print what the filesystem returns for each operation ling's fallbacks depend on."""
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
            results[name] = "ok"
        except OSError as error:
            results[name] = errno.errorcode.get(error.errno, str(error.errno))
        except (sqlite3.Error, RuntimeError) as error:
            results[name] = str(error)
        print(f"probe {name}: {results[name]}", flush=True)
    return results


def ling(binary, *args, env, expect_success=True):
    print("+ ling " + " ".join(map(str, args)), flush=True)
    result = subprocess.run(
        [binary, *map(str, args)],
        check=False,
        capture_output=True,
        text=True,
        env=env,
        timeout=600,
    )
    if expect_success and result.returncode:
        raise RuntimeError(f"exit {result.returncode}\n{result.stderr}")
    return result


def wait_for_address(server, log):
    deadline = time.monotonic() + 120
    while not (address := re.search(r"listening on (http://\S+)", log.read_text())):
        if server.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError(f"server did not start (exit {server.poll()})")
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
                with http.open(base + path, data=data, timeout=180) as response:
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
            server.wait(timeout=20)
            assert server.returncode == 0, f"server exit {server.returncode}"
        except Exception:
            print(log.read_text(), errors.read_text(), sep="\n", file=sys.stderr)
            raise
        finally:
            if server.poll() is None:
                server.kill()
                server.wait()


def analyzer_preflight(binary, root, env, symlinks):
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
    assert result.returncode != 0, "analyzer prepare succeeded without wheels"
    if symlinks:
        assert SYMLINK_ERROR not in result.stderr, result.stderr
    else:
        assert SYMLINK_ERROR in result.stderr, (
            "expected the symbolic-link preflight error before reading wheels; got:\n"
            + result.stderr
        )
    assert not output.exists(), f"{output} was left behind"


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    root = Path(sys.argv[1]).absolute()
    binary = str(Path(sys.argv[2]).resolve())
    root.mkdir(parents=True, exist_ok=True)
    if any(root.iterdir()):
        sys.exit(f"{root} is not empty")

    symlinks = probe(root / "probe")["symlink"] == "ok"
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
    serve(binary, root, env, table)
    analyzer_preflight(binary, root, env, symlinks)
    print(f"Output filesystem check passed: {root}")


if __name__ == "__main__":
    main()
