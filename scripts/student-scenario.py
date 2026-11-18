#!/usr/bin/env python3
"""Run the class 6 workflow against a ling binary at the macOS default open-file limit.

usage: student-scenario.py PATH_TO_LING WORK_DIRECTORY [--downloads DIRECTORY]

The binary must find the cwj dictionary the way a student's does: through
`ling dictionary install`, or LINGREP_DICTIONARY_PATH. WORK_DIRECTORY must be
absent or empty. The Soranoha author archives are downloaded into
--downloads (default WORK_DIRECTORY/downloads) unless a copy with the pinned
SHA-256 is already there.
"""

import argparse
import hashlib

import io
import json
import os
import re
import resource
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ElementTree
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEXTS_BASE_URL = "https://soranoha.org/bulk/authors/"
TEXTS_DIGESTS = Path(__file__).resolve().with_name("student-scenario-texts.sha256")
TEXTS = {
    "natsume": "soranoha-Natsume_Soseki-000148-plaintext.zip",
    "mori": "soranoha-Mori_Ogai-000129-plaintext.zip",
}

# The soft limit a macOS login shell starts with (`launchctl limit maxfiles`).
MACOS_DEFAULT_OPEN_FILES = 256

# The class 6 catalog's definitions that bind to the Vibrato/UniDic token
# schema. Its `npv` definition reads GiNZA's dep.* attributes.
CATALOG = """\
set CASE := {が, を, に, で, から, より, と, へ}.
set NOMINAL := {NOUN, PROPN, PRON, NUM}.

key: [upos]
pos := [].

key: [xpos, upos]
tags := [].

key: [lemma]
nouns := [upos in @NOMINAL].

key: [$n.lemma, $p.lemma, $v.lemma]
npv_adjacent := (?<n> [upos in @NOMINAL]) (?<p> [lemma in @CASE]) (?<v> [upos=VERB]).
"""

# Counts for the pinned Soranoha archives analyzed by Vibrato with unidic-cwj-202512.
# `pos` and `tags` match every token, so their totals are the token count.
EXPECTED = {
    "natsume": {
        "documents": 112,
        "sentences": 127084,
        "tokens": 2630590,
        "matches": 6019714,
        "nouns": 655427,
        "npv_adjacent": 103107,
    },
    "mori": {
        "documents": 139,
        "sentences": 110687,
        "tokens": 2020912,
        "matches": 4756533,
        "nouns": 639286,
        "npv_adjacent": 75423,
    },
}
QUESTION_CORPUS = "natsume"
QUESTION_PATTERN = "npv_adjacent"
COMPOUND_LIMIT_MESSAGES = ["Derive compounds", "Too many results to retain"]
STREAM_DEADLINE_SECONDS = 300


class ScenarioError(Exception):
    pass


def step(name):
    print(f"--- {name}", flush=True)
    return time.monotonic()


def done(started):
    print(f"    {time.monotonic() - started:.1f} s", flush=True)


def check(condition, message):
    if not condition:
        raise ScenarioError(message)


def limit_open_files():
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    if soft == resource.RLIM_INFINITY or soft > MACOS_DEFAULT_OPEN_FILES:
        soft = MACOS_DEFAULT_OPEN_FILES
        resource.setrlimit(resource.RLIMIT_NOFILE, (soft, hard))
    print(f"open-file limit: soft {soft}, hard {hard}", flush=True)


def ling(binary, *args):
    result = subprocess.run(
        [binary, *map(str, args)], capture_output=True, timeout=900, check=False
    )
    if result.returncode:
        raise ScenarioError(
            f"ling {' '.join(map(str, args))} exited {result.returncode}:\n"
            + result.stderr.decode(errors="replace")
        )
    return result.stdout.decode()


def records(output, kind):
    return [r["data"] for r in map(json.loads, output.splitlines()) if r["type"] == kind]


def pinned_digests():
    digests = {}
    for line in TEXTS_DIGESTS.read_text().splitlines():
        digest, name = line.split()
        digests[name] = digest
    return digests


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for block in iter(lambda: file.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_texts(downloads):
    """Returns each corpus's archive, downloading what is absent from `downloads`."""
    started = step("0. Soranoha author archives")
    downloads.mkdir(parents=True, exist_ok=True)
    digests = pinned_digests()
    archives = {}
    for corpus, name in TEXTS.items():
        archive = downloads / name
        if not (archive.is_file() and sha256(archive) == digests[name]):
            partial = downloads / (name + ".partial")
            with urllib.request.urlopen(TEXTS_BASE_URL + name, timeout=300) as response:
                partial.write_bytes(response.read())
            actual = sha256(partial)
            if actual != digests[name]:
                partial.unlink()
            # soranoha.org serves the current release under each archive name.
            check(
                actual == digests[name],
                f"{TEXTS_BASE_URL}{name} has SHA-256 {actual}, not the pinned "
                f"{digests[name]}. Soranoha has probably published a new release: "
                f"update {TEXTS_DIGESTS.name} and the counts in EXPECTED.",
            )
            partial.replace(archive)
            print(f"    downloaded {name}", flush=True)
        archives[corpus] = archive
    done(started)
    return archives


def extract_works(archive, directory):
    directory.mkdir(parents=True)
    with zipfile.ZipFile(archive) as works:
        for name in works.namelist():
            if name.endswith(".txt"):
                check("/" not in name and not name.startswith("."), f"{archive.name}: member {name}")
                (directory / name).write_bytes(works.read(name))


def prepare(binary, work, archives):
    started = step("1. ling corpus build --input-format plain, one directory per author")
    for corpus, archive in archives.items():
        extract_works(archive, work / "texts" / corpus)
    (work / "patterns").mkdir()
    (work / "patterns/class6.lx").write_text(CATALOG, encoding="utf-8")
    for corpus in EXPECTED:
        ling(
            binary, "corpus", "build", "--input-format", "plain",
            "--output", work / f"{corpus}.lingcorpus", work / "texts" / corpus,
        )
    done(started)

    started = step("2. ling summarize --write-table")
    for corpus, expected in EXPECTED.items():
        # Eight workers is summarize's shard ceiling (`workers.clamp(1, 8)`),
        # so the number of spill files does not depend on the runner's cores.
        output = ling(
            binary, "--json", "summarize", "--workers", "8",
            "--corpus", work / f"{corpus}.lingcorpus",
            "--patterns", work / "patterns/class6.lx",
            "--write-table", work / f"{corpus}.lingtable",
        )
        counters = records(output, "complete")[0]["counters"]
        observed = {
            "documents": counters["documents_admitted"],
            "sentences": counters["sentences_admitted"],
            "tokens": counters["tokens_scanned"],
            "matches": counters["matches_found"],
        }
        print(f"    {corpus}: {observed}", flush=True)
        check(
            observed == {key: expected[key] for key in observed},
            f"{corpus}: summarize counted {observed}, expected {expected}",
        )
    done(started)


class Server:
    def __init__(self, binary, work):
        self.work = work
        # A prepared corpus needs no dictionary, so none is reachable.
        environment = dict(os.environ, XDG_DATA_HOME=str(work / "no-dictionary"))
        environment.pop("LINGREP_DICTIONARY_PATH", None)
        command = [binary, "serve", "--host", "127.0.0.1", "--port", "0"]
        for corpus in EXPECTED:
            command += ["--corpus", f"{corpus}={work / f'{corpus}.lingcorpus'}"]
            command += ["--count-table", str(work / f"{corpus}.lingtable")]
        command += [
            "--patterns", str(work / "patterns/class6.lx"),
            "--pattern-root", str(work / "patterns"),
        ]
        self.log = work / "server.stdout"
        self.errors = work / "server.stderr"
        with self.log.open("w") as stdout, self.errors.open("w") as stderr:
            self.process = subprocess.Popen(
                command, stdout=stdout, stderr=stderr, env=environment
            )
        deadline = time.monotonic() + 60
        while True:
            address = re.search(r"listening on (http://\S+)", self.log.read_text())
            if address:
                break
            self.check_alive()
            check(time.monotonic() < deadline, "ling serve did not listen within 60 s")
            time.sleep(0.05)
        self.base = address.group(1)
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def check_alive(self):
        check(
            self.process.poll() is None,
            f"ling serve exited ({self.process.returncode})",
        )

    def open(self, path, data=None, headers=None, timeout=300):
        request = urllib.request.Request(self.base + path, data=data, headers=headers or {})
        try:
            return self.http.open(request, timeout=timeout)
        except (urllib.error.URLError, ConnectionError) as error:
            self.check_alive()
            raise ScenarioError(f"{path}: {error}") from error

    def read(self, path, data=None, headers=None):
        with self.open(path, data, headers) as response:
            check(response.status == 200, f"{path}: HTTP {response.status}")
            return response.read()

    def json(self, path, payload=None):
        if payload is None:
            return json.loads(self.read(path))
        return json.loads(
            self.read(path, json.dumps(payload).encode(), {"Content-Type": "application/json"})
        )

    def stop(self):
        self.process.terminate()
        self.process.wait(timeout=30)
        check(self.process.returncode == 0, f"ling serve shutdown exited {self.process.returncode}")

    def report(self):
        for path in [self.log, self.errors]:
            print(f"--- {path.name}", file=sys.stderr)
            print(path.read_text(errors="replace")[-20000:], file=sys.stderr)


def workbench_search(server, workbench, corpus, derive_compounds):
    form = urllib.parse.urlencode(
        {
            "pattern": CATALOG,
            "corpus": corpus,
            "derive_compounds": "true" if derive_compounds else "false",
        }
    ).encode()
    server.read(f"/api/search/{workbench}", form)
    events = []
    name, data = None, []
    deadline = time.monotonic() + STREAM_DEADLINE_SECONDS
    with server.open(f"/api/stream/{workbench}", timeout=STREAM_DEADLINE_SECONDS) as stream:
        for line in stream:
            check(time.monotonic() < deadline, f"{workbench}: no result within {STREAM_DEADLINE_SECONDS} s")
            line = line.decode().rstrip("\r\n")
            if line.startswith("event:"):
                name = line[len("event:"):].strip()
            elif line.startswith("data:"):
                data.append(line[len("data:"):])
            elif not line and name:
                events.append((name, "".join(data)))
                name, data = None, []
    server.check_alive()
    terminal = [event for event in events if event[0] not in ("progress", "close")]
    check(terminal, f"{workbench}: the stream ended without a result: {[e[0] for e in events]}")
    return terminal[-1]


def question_run(server):
    started = step(f"4. question page run of {QUESTION_PATTERN} over {QUESTION_CORPUS}")
    run = server.json(
        "/api/questions/runs",
        {"source": CATALOG, "dataset": QUESTION_CORPUS, "pattern": QUESTION_PATTERN},
    )["id"]
    deadline = time.monotonic() + STREAM_DEADLINE_SECONDS
    while True:
        snapshot = server.json(f"/api/questions/runs/{run}")
        if snapshot["phase"] != "running":
            break
        check(time.monotonic() < deadline, f"question run still running after {STREAM_DEADLINE_SECONDS} s")
        time.sleep(0.25)
    check(
        snapshot["phase"] == "complete",
        f"question run ended {snapshot['phase']}: {snapshot['error_code']} {snapshot['error']}",
    )
    expected = EXPECTED[QUESTION_CORPUS][QUESTION_PATTERN]
    check(
        snapshot["counts"] == {QUESTION_PATTERN: expected},
        f"question run counted {snapshot['counts']}, expected {expected}",
    )
    done(started)

    started = step("5. question page JSONL export")
    kinds = []
    last = None
    with server.open(f"/api/questions/runs/{run}/export") as response:
        for line in response:
            last = json.loads(line)
            kinds.append(last["type"])
    matches = kinds.count("match")
    print(f"    {matches} match records", flush=True)
    check(kinds[:1] == ["run"], f"export does not start with a run record: {kinds[:1]}")
    check(matches == expected, f"export has {matches} match records, expected {expected}")
    check(
        last["type"] == "complete" and last["data"]["counters"]["matches_found"] == expected,
        f"export does not end with a complete record for {expected} matches: {last}",
    )
    done(started)


def workbench_compounds(server):
    started = step("6. Workbench search of the catalog with compound derivation (d=1)")
    for corpus in EXPECTED:
        name, body = workbench_search(server, f"compounds-{corpus}", corpus, True)
        limited = name == "error" and any(message in body for message in COMPOUND_LIMIT_MESSAGES)
        print(f"    {corpus}: {'result_limit' if limited else name}", flush=True)
        check(
            name == "result" or limited,
            f"{corpus}: compound search ended with {name}: {re.sub(r'<[^>]+>', ' ', body)[:500]}",
        )
    done(started)


def frequencies(server):
    started = step("7. Frequencies from the count tables")
    server.read("/frequencies")
    for corpus, expected in EXPECTED.items():
        totals = frequency_totals(expected)
        for pattern, total in totals.items():
            query = urllib.parse.urlencode(
                {"corpus": corpus, "catalog": "class6.lx", "pattern": pattern}
            )
            answer = server.json(f"/api/frequency?{query}")
            check(answer["status"] == "ok", f"{corpus} {pattern}: {answer}")
            check(
                answer["occurrences"] == total,
                f"{corpus} {pattern}: {answer['occurrences']} occurrences, expected {total}",
            )
    done(started)


def frequency_totals(expected):
    return {
        "pos": expected["tokens"],
        "tags": expected["tokens"],
        "nouns": expected["nouns"],
        "npv_adjacent": expected["npv_adjacent"],
    }


def workbook_export(server):
    corpus = QUESTION_CORPUS
    started = step(f"8. Workbench xlsx export over {corpus}")
    name, _ = workbench_search(server, "export", corpus, False)
    check(name == "result", f"Workbench search without compounds ended with {name}")
    workbook = zipfile.ZipFile(io.BytesIO(server.read("/api/export/export/1")))
    sheets = read_workbook(workbook)
    about = sheets["About"]
    header = ["Pattern", "Sheet", "Total matches", "Exported rows"]
    check(header in about, f"the About sheet has no pattern table: {about}")
    exported = {
        row[0]: (int(row[2]), int(row[3])) for row in about[about.index(header) + 1:]
    }
    expected = EXPECTED[corpus]
    for pattern, total in frequency_totals(expected).items():
        check(pattern in exported, f"the workbook has no {pattern} row")
        matches, rows = exported[pattern]
        check(matches == total, f"workbook {pattern}: {matches} matches, expected {total}")
        check(
            0 < rows == len(sheets[pattern]) - 1,
            f"workbook {pattern}: {len(sheets[pattern]) - 1} rows on its sheet, About reports {rows}",
        )
    done(started)


def read_workbook(workbook):
    namespace = {"x": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    relationships = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id"
    shared = [
        "".join(text.text or "" for text in item.iter(f"{{{namespace['x']}}}t"))
        for item in ElementTree.fromstring(workbook.read("xl/sharedStrings.xml")).findall("x:si", namespace)
    ]
    targets = {
        rel.get("Id"): rel.get("Target")
        for rel in ElementTree.fromstring(workbook.read("xl/_rels/workbook.xml.rels"))
    }
    sheets = {}
    for sheet in ElementTree.fromstring(workbook.read("xl/workbook.xml")).iter(f"{{{namespace['x']}}}sheet"):
        root = ElementTree.fromstring(workbook.read("xl/" + targets[sheet.get(relationships)]))
        rows = []
        for row in root.iter(f"{{{namespace['x']}}}row"):
            cells = []
            for cell in row.findall("x:c", namespace):
                value = cell.find("x:v", namespace)
                if cell.get("t") == "s":
                    cells.append(shared[int(value.text)])
                elif cell.get("t") == "inlineStr":
                    cells.append("".join(t.text or "" for t in cell.iter(f"{{{namespace['x']}}}t")))
                else:
                    cells.append(value.text if value is not None else "")
            rows.append(cells)
        sheets[sheet.get("name")] = rows
    return sheets


def serve(binary, work):
    started = step("3. ling serve --corpus --count-table, without a dictionary")
    server = Server(binary, work)
    try:
        for path in ["/", "/workbench", "/workbench?d=1", "/frequencies", "/api/frequency/meta"]:
            check(server.read(path), f"{path}: empty response")
        datasets = server.json("/api/questions/config")["datasets"]
        for corpus, expected in EXPECTED.items():
            observed = {key: datasets[corpus][key] for key in ["documents", "sentences", "tokens"]}
            check(
                observed == {key: expected[key] for key in observed},
                f"{corpus}: the server reports {observed}, expected {expected}",
            )
        done(started)
        question_run(server)
        workbench_compounds(server)
        frequencies(server)
        workbook_export(server)
        server.check_alive()
        server.stop()
    except Exception:
        server.report()
        raise
    finally:
        if server.process.poll() is None:
            server.process.kill()
            server.process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("binary")
    parser.add_argument("work", type=Path)
    parser.add_argument("--downloads", type=Path)
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    work = args.work.resolve()
    downloads = (args.downloads or work / "downloads").resolve()
    work.mkdir(parents=True, exist_ok=True)
    if any(work.iterdir()):
        sys.exit(f"{work} is not empty")
    limit_open_files()
    print(ling(binary, "--version").strip(), flush=True)
    started = time.monotonic()
    try:
        prepare(binary, work, fetch_texts(downloads))
        serve(binary, work)
    except ScenarioError as error:
        sys.exit(f"student scenario failed: {error}")
    print(f"student scenario passed in {time.monotonic() - started:.1f} s", flush=True)


if __name__ == "__main__":
    main()
