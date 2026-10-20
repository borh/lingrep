#!/usr/bin/env python3
"""Compare two catalogs using ling's reference-closed comparison and completed query streams."""

import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile


def check(ling, path, other=None):
    argv = [ling, "--json", "check", "--patterns", str(path)]
    if other is not None:
        argv += ["--compare-with", str(other)]
    result = subprocess.run(argv, capture_output=True, text=True, check=True)
    return json.loads(result.stdout)["data"]


def record_key(data):
    # Query-dependent token projections are evidence, not match identity.
    return json.dumps({key: value for key, value in data.items()
                       if key not in ("sequence", "tokens", "left_context", "right_context")},
                      ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def scan(ling, catalog, source, resources, db, side, is_prepared):
    argv = [ling, "--json", "matches", "--patterns", str(catalog), "--context", "0"]
    argv += ["--corpus", str(source)] if is_prepared else [str(source), *resources]
    run = complete = None
    with tempfile.TemporaryFile() as errors:
        with subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=errors, text=True) as process:
            try:
                for line in process.stdout:
                    event = json.loads(line)
                    if event.get("schema") != "lingrep.query/v2" or complete is not None:
                        raise ValueError("invalid or trailing query event")
                    kind, data = event["type"], event["data"]
                    if kind == "run":
                        if run is not None:
                            raise ValueError("duplicate query run")
                        run = data
                    elif kind == "match":
                        if run is None:
                            raise ValueError("match before query run")
                        db.execute("insert into records values(?,?,?)", (side, data["pattern"], record_key(data)))
                    elif kind == "complete":
                        complete = data
                status = process.wait()
            except BaseException:
                process.terminate()
                process.wait()
                raise
        if status or run is None or complete is None or complete["coverage"] != "full" or complete["termination"] != "eof":
            errors.seek(0)
            raise ValueError(f"catalog search did not complete: {errors.read(65536).decode(errors='replace')}")
    db.commit()
    return {"run": run, "complete": complete}


def compare(args):
    before, after = Path(args.before), Path(args.after)
    comparison = check(args.ling, before, after)
    other = check(args.ling, after)
    descriptors = [{d["name"]: d for d in report["definitions"]} for report in (comparison, other)]
    rows = []
    for definition in comparison.get("comparison", []):
        revisions = {r["catalog"]: r for r in definition["revisions"]}
        left, right = revisions.get(str(before)), revisions.get(str(after))
        status = "added" if left is None else "removed" if right is None else (
            "unchanged" if left["body_group"] == right["body_group"] and left["kind"] == right["kind"] else "changed")
        name = definition["name"]
        rows.append({"name": name, "status": status,
                     "before": left, "after": right,
                     "metadata_changed": descriptors[0].get(name, {}).get("metadata") != descriptors[1].get(name, {}).get("metadata")})
    report = {"comparison": "reference_closed_structural", "before": str(before), "after": str(after),
              "catalog_fingerprints": [comparison["catalog_fingerprint"], other["catalog_fingerprint"]],
              "definitions": rows}
    if args.corpus or args.examples:
        with tempfile.TemporaryDirectory(prefix="lingrep-catalog-diff-") as temporary:
            root = Path(temporary)
            source = args.corpus
            if args.examples:
                claims = set()
                for catalog in descriptors:
                    for definition in catalog.values():
                        for key in ("examples", "counterexamples"):
                            entries = definition["metadata"].get(key, [])
                            if not isinstance(entries, list) or any(not isinstance(e, str) for e in entries):
                                raise ValueError(f"malformed {key} on {definition['name']}")
                            claims.update(entries)
                claims = sorted(claims)
                if not claims:
                    raise ValueError("--examples found no executable string claims")
                source = root / "examples.jsonl"
                with source.open("w") as output:
                    for claim in claims:
                        output.write(json.dumps({"metadata": {}, "sentences": [claim]}, ensure_ascii=False) + "\n")
                report["example_sentences"] = len(claims)
            resources = []
            if args.dict_path:
                resources += ["--dict-path", args.dict_path]
            db = sqlite3.connect(root / "records.sqlite")
            db.execute("pragma cache_size=-65536")
            db.execute("pragma temp_store=FILE")
            db.execute("create table records(side integer, pattern text, record text)")
            scans = [scan(args.ling, catalog, source, resources, db, side, bool(args.corpus))
                     for side, catalog in enumerate((before, after))]
            for key in ("analysis_identity", "source_identity", "token_schema_identity"):
                place = "run" if key == "token_schema_identity" else "complete"
                if scans[0][place].get(key) != scans[1][place].get(key):
                    raise ValueError(f"comparison input changed: {key}")
            db.execute("create index record_order on records(record,side)")
            differences = [{}, {}]
            for pattern, _record, n0, n1 in db.execute(
                "select pattern,record,sum(side=0),sum(side=1) from records group by record"):
                if n0 > n1:
                    differences[0][pattern] = differences[0].get(pattern, 0) + n0 - n1
                if n1 > n0:
                    differences[1][pattern] = differences[1].get(pattern, 0) + n1 - n0
            report["behavior"] = {"population": "examples" if args.examples else "prepared_corpus",
                                  "runs": scans,
                                  "only_before": differences[0], "only_after": differences[1],
                                  "identity": "published record excluding stream sequence and query-dependent token projections"}
            db.close()
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before")
    parser.add_argument("after")
    parser.add_argument("--ling", default=os.environ.get("LINGREP_BIN", "ling"))
    population = parser.add_mutually_exclusive_group()
    population.add_argument("--corpus", help="immutable prepared artifact")
    population.add_argument("--examples", action="store_true", help="union of both catalogs' example texts")
    parser.add_argument("--dict-path", default=os.environ.get("LINGREP_DICTIONARY_PATH"))
    args = parser.parse_args()
    if Path(args.before).resolve() == Path(args.after).resolve():
        parser.error("choose two distinct catalog paths")
    try:
        report = compare(args)
    except (ValueError, OSError, subprocess.CalledProcessError, sqlite3.Error) as error:
        print(f"catalog comparison failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
