#!/usr/bin/env python3
"""Validate Soranoha queries, metadata, count tables, and web results with a real corpus."""

import argparse
import io
import json
import re
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PATTERN = "particle := [text=の].\n"
SCOPE = '@document.ndc contains "914"'


def run(binary, *args, timeout=600):
    result = subprocess.run(
        [binary, *map(str, args)], capture_output=True, timeout=timeout, check=False
    )
    if result.returncode:
        raise RuntimeError(f"ling {' '.join(map(str, args))}: {result.stderr.decode()}")
    return result.stdout


def matches(binary, *input_args):
    records = run(
        binary,
        "--json",
        "matches",
        *input_args,
        "--pattern",
        PATTERN,
        "--limit",
        "20",
        "--source-metadata",
        "@document.identifier",
        "--source-metadata",
        "@document.ndc",
        "--source-metadata",
        "@document.release",
    )
    records = [json.loads(line) for line in records.splitlines()]
    analyzer = next(r["data"]["analyzer"] for r in records if r["type"] == "run")
    found = [r["data"] for r in records if r["type"] == "match"]
    assert found, "Soranoha query produced no matches"
    for match in found:
        metadata = match["source_metadata"]["document"]
        assert re.fullmatch(r"\d{6}_\d{6}", metadata["identifier"]), metadata
        assert isinstance(metadata["ndc"], list), metadata
        assert metadata["release"], metadata
    return analyzer, found


def check_web(binary, corpus, table, catalog, scratch):
    log = scratch / "server.stdout"
    errors = scratch / "server.stderr"
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
                str(corpus),
                "--patterns",
                str(catalog),
                "--pattern-root",
                str(scratch),
                "--count-table",
                str(table),
                "--metadata-labels",
                str(ROOT / "lingrep-web/labels/soranoha-catalog.json"),
            ],
            stdout=stdout,
            stderr=stderr,
        )
        try:
            deadline = time.monotonic() + 120
            while not (
                address := re.search(r"listening on (http://\S+)", log.read_text())
            ):
                if server.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("Soranoha server did not start")
                time.sleep(0.05)
            base = address.group(1)
            http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def get(path, data=None):
                with http.open(base + path, data=data, timeout=180) as response:
                    return response.read()

            meta = json.loads(get("/api/frequency/meta"))
            assert len(meta["pairs"]) == 1, meta
            pair = meta["pairs"][0]
            dimensions = {d["key"]: d for d in meta["dimensions"][pair["corpus"]]}
            assert dimensions["ndc"]["list"], dimensions
            params = {
                "corpus": pair["corpus"],
                "catalog": pair["catalog"],
                "pattern": "particle",
            }
            answer = json.loads(
                get(
                    "/api/frequency?"
                    + urllib.parse.urlencode(params | {"where": SCOPE})
                )
            )
            assert answer["status"] == "ok" and answer["occurrences"] > 0, answer
            assert answer["documents_admitted"] > 0, answer
            for path in ["/", "/frequencies", "/compare"]:
                assert get(path), path
            form = urllib.parse.urlencode(
                {
                    "pattern": PATTERN,
                    "search_scope": json.dumps({"clause": SCOPE}),
                    "retained": 20,
                }
            ).encode()
            posted = get("/api/search/soranoha-check", form)
            assert b"api/stream/soranoha-check" in posted, posted.decode()
            stream = get("/api/stream/soranoha-check").decode()
            assert "event: result" in stream, stream
            assert "の".encode() in get("/api/pattern-detail/soranoha-check/1/0")
            workbook = get("/api/export/soranoha-check/1?locale=ja")
            with zipfile.ZipFile(io.BytesIO(workbook)) as exported:
                assert "コーパス" in exported.read("xl/sharedStrings.xml").decode()
            server.terminate()
            server.wait(timeout=20)
            assert server.returncode == 0, server.returncode
        except Exception:
            print(log.read_text())
            print(errors.read_text())
            raise
        finally:
            if server.poll() is None:
                server.kill()
                server.wait()


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        usage="%(prog)s binary (--corpus CORPUS | --archive ARCHIVE"
        " [--build-timeout SECONDS] [-- ANALYZER_OPTION ...])",
        epilog="With --archive, the options after -- select the analyzer. They are"
        " passed unchanged to the direct `ling matches` run and to `ling corpus"
        " build`, for example: -- --analyzer sudachi --sudachi-dict system_core.dic",
    )
    parser.add_argument("binary", type=lambda p: str(Path(p).resolve()))
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument(
        "--corpus", type=Path, help="Prepared Soranoha corpus to validate"
    )
    inputs.add_argument(
        "--archive",
        type=Path,
        help="Real Soranoha plaintext ZIP to prepare and validate",
    )
    parser.add_argument(
        "--build-timeout",
        type=int,
        metavar="SECONDS",
        help="Deadline for ling corpus build when validating an archive (default: 600)",
    )
    argv = sys.argv[1:]
    analyzer_options = []
    if "--" in argv:
        split = argv.index("--")
        argv, analyzer_options = argv[:split], argv[split + 1 :]
    args = parser.parse_args(argv)
    if args.corpus and analyzer_options:
        parser.error(
            "analyzer options require --archive: a prepared corpus records its analyzer"
        )
    if args.build_timeout is not None and (not args.archive or args.build_timeout < 1):
        parser.error("--build-timeout requires --archive and a positive number of seconds")
    with tempfile.TemporaryDirectory(prefix="ling-soranoha-") as directory:
        scratch = Path(directory)
        corpus = args.corpus.resolve() if args.corpus else scratch / "sample.lingcorpus"
        if not args.corpus:
            archive = args.archive.resolve()
            direct = matches(
                args.binary, *analyzer_options, "--input-format", "soranoha", archive
            )
            run(
                args.binary,
                "corpus",
                "build",
                *analyzer_options,
                "--input-format",
                "soranoha",
                "--output",
                corpus,
                archive,
                timeout=args.build_timeout or 600,
            )
            assert direct == matches(args.binary, "--corpus", corpus), (
                "Direct/prepared analyzer or match records differ"
            )
        else:
            matches(args.binary, "--corpus", corpus)
        print(f"Verifying Soranoha artifact: {corpus}", flush=True)
        run(args.binary, "corpus", "inspect", "--verify", corpus)
        catalog = scratch / "soranoha-check.lx"
        catalog.write_text(PATTERN)
        print("Scanning Soranoha to build the validation count table", flush=True)
        table = scratch / "soranoha.lingtable"
        run(
            args.binary,
            "summarize",
            "--corpus",
            corpus,
            "--patterns",
            catalog,
            "--write-table",
            table,
        )
        print(
            "Checking Soranoha frequencies, search stream, concordance and export",
            flush=True,
        )
        check_web(args.binary, corpus, table, catalog, scratch)
        print(f"Soranoha validation passed: {args.corpus or args.archive}")


if __name__ == "__main__":
    main()
