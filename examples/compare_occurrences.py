#!/usr/bin/env python3
"""Compare two patterns' occurrences from complete `ling matches --json --evidence` streams.

An occurrence is identified by its sentence (source name, document ordinal,
document sentence ordinal) and the words its named roles occupy: capture
names, graph-node names and named edges. Two records with the same sentence
and the same word for every compared role are the same occurrence, whatever
their spans. The output is one TSV row per occurrence, marked `shared`,
`a_only` or `b_only`, with both sides' record counts, the role words and the
sentence. Both streams must be complete, unsampled runs over the same
analysis; anything else exits with status 2.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import Counter
from typing import Any, TextIO

SCHEMA = "lingrep.query/v2"


class InputError(Exception):
    pass


def read_stream(source: TextIO, label: str) -> dict[str, Any]:
    run: dict[str, Any] | None = None
    complete: dict[str, Any] | None = None
    patterns: list[str] = []
    records: list[dict[str, Any]] = []
    for line_number, line in enumerate(source, 1):
        if not line.strip():
            continue
        where = f"{label} line {line_number}"
        if complete is not None:
            raise InputError(f"{where}: event follows the complete event")
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise InputError(f"{where}: invalid JSON: {error.msg}") from error
        if not isinstance(event, dict) or event.get("schema") != SCHEMA:
            raise InputError(f"{where}: expected a {SCHEMA} event")
        kind, data = event.get("type"), event.get("data")
        if not isinstance(data, dict):
            raise InputError(f"{where}: event has no data object")
        if run is None:
            if kind != "run" or data.get("mode") != "matches":
                raise InputError(f"{label}: stream must begin with a `ling matches` run event")
            if data.get("sample") is not None:
                raise InputError(f"{label}: the run sampled the corpus; rerun without --sample")
            run = data
        elif kind == "definition":
            if data.get("body", {}).get("kind") == "pattern":
                patterns.append(data["name"])
        elif kind == "match":
            if "evidence" not in data:
                raise InputError(f"{where}: match has no evidence; rerun with --evidence")
            records.append(data)
        elif kind == "complete":
            complete = data
        else:
            raise InputError(f"{where}: unexpected `{kind}` event")
    if run is None:
        raise InputError(f"{label}: empty stream")
    if complete is None:
        raise InputError(f"{label}: no complete event; the run failed or was interrupted")
    if complete.get("coverage") != "full" or complete.get("termination") != "eof":
        raise InputError(f"{label}: the run stopped early (for example at --limit); rerun with --limit all")
    if complete.get("retained_matches") is not None:
        raise InputError(f"{label}: the run kept a --retain sample; rerun without --retain")
    if not complete.get("analysis_identity"):
        raise InputError(f"{label}: the run has no corpus analysis identity (stdin input)")
    return {"complete": complete, "patterns": patterns, "records": records}


def identity(stream: dict[str, Any]) -> tuple[Any, ...]:
    complete = stream["complete"]
    return (
        complete.get("corpus_kind"),
        complete.get("source_identity"),
        complete.get("analysis_identity"),
        json.dumps(complete.get("scope"), sort_keys=True),
    )


def select(stream: dict[str, Any], pattern: str | None, label: str) -> list[dict[str, Any]]:
    if pattern is None:
        if len(stream["patterns"]) != 1:
            names = ", ".join(stream["patterns"]) or "none"
            raise InputError(f"{label}: the run has several patterns ({names}); name one")
        pattern = stream["patterns"][0]
    elif pattern not in stream["patterns"]:
        raise InputError(f"{label}: the run has no pattern `{pattern}`")
    return [record for record in stream["records"] if record["pattern"] == pattern]


def roles(record: dict[str, Any]) -> dict[str, str]:
    """Each named role's position: word numbers, an empty node `N.M`, or `HEAD>DEPENDENT`."""

    def node(value: dict[str, Any]) -> str:
        if value["node_kind"] == "word":
            return str(value["token_index"] + 1)
        return f"{value['boundary']}.{value['subindex']}"

    named: dict[str, str] = {}
    for capture in record.get("captures", []):
        span = capture["span"]
        named[capture["name"]] = ",".join(str(index + 1) for index in range(span["start"], span["end"]))
    for binding in record.get("graph_bindings", []):
        if binding["binding_kind"] == "node":
            named[binding["name"]] = node(binding["node"])
        else:
            named[binding["name"]] = f"{node(binding['head'])}>{node(binding['dependent'])}"
    return named


def unit(record: dict[str, Any], compared: list[str], label: str) -> tuple[Any, ...]:
    named = roles(record)
    missing = [role for role in compared if role not in named]
    if missing:
        raise InputError(
            f"{label}: record {record['sequence']} has no role {', '.join(missing)}; "
            "name the roles both patterns bind with --roles"
        )
    return (
        record["source_name"],
        record["document_ordinal"],
        record["document_sentence_ordinal"],
        tuple(f"{role}={named[role]}" for role in compared),
    )


def words(record: dict[str, Any], compared: list[str]) -> str:
    """The compared roles' words as `role=text(word,rel→head)`."""
    rendered = []
    for role in compared:
        for word in record["evidence"]["words"]:
            if role in word["roles"]:
                rendered.append(
                    f"{role}={word['text'] or ''}({word['word']},{word['rel'] or ''}→{word['head'] or ''})"
                )
    return " ".join(rendered)


def compare(a: TextIO, b: TextIO, a_pattern: str | None, b_pattern: str | None,
            compared: list[str] | None, output: TextIO) -> Counter[str]:
    left, right = read_stream(a, "A"), read_stream(b, "B")
    if identity(left) != identity(right):
        raise InputError(
            "A and B differ in corpus, analysis or --where scope; "
            "compare runs over the same corpus and scope"
        )
    a_records = select(left, a_pattern, "A")
    b_records = select(right, b_pattern, "B")
    if compared is None:
        names = [set(roles(record)) for record in a_records + b_records]
        compared = sorted(set.intersection(*names)) if names else []
        if names and not compared:
            raise InputError("the two patterns share no role name; choose roles with --roles")
    counts: dict[tuple[Any, ...], list[int]] = {}
    shown: dict[tuple[Any, ...], dict[str, Any]] = {}
    for side, records, label in ((0, a_records, "A"), (1, b_records, "B")):
        for record in records:
            key = unit(record, compared, label)
            counts.setdefault(key, [0, 0])[side] += 1
            shown.setdefault(key, record)
    totals: Counter[str] = Counter()
    output.write("status\tcount_a\tcount_b\tsource_name\tdocument_ordinal\t"
                 "document_sentence_ordinal\tunit\twords\tsentence\n")
    for key in sorted(counts, key=lambda key: (key[1], key[2], key[3], key[0])):
        count_a, count_b = counts[key]
        status = "shared" if count_a and count_b else "a_only" if count_a else "b_only"
        totals[status] += 1
        record = shown[key]
        fields = [status, str(count_a), str(count_b), key[0], str(key[1]), str(key[2]),
                  " ".join(key[3]), words(record, compared), record["evidence"]["sentence"]]
        output.write("\t".join(field.replace("\t", " ").replace("\n", " ") for field in fields) + "\n")
    return totals


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("a", help="first `ling matches --json --evidence` stream")
    parser.add_argument("b", help="second stream; may be the same file")
    parser.add_argument("--a-pattern", help="pattern to read from A (default: its only pattern)")
    parser.add_argument("--b-pattern", help="pattern to read from B (default: its only pattern)")
    parser.add_argument("--roles", help="comma-separated roles to compare (default: those every record binds)")
    arguments = parser.parse_args(argv)
    compared = arguments.roles.split(",") if arguments.roles else None
    try:
        with open(arguments.a, encoding="utf-8") as a, open(arguments.b, encoding="utf-8") as b:
            totals = compare(a, b, arguments.a_pattern, arguments.b_pattern, compared, sys.stdout)
    except (InputError, OSError) as error:
        print(f"compare_occurrences: {error}", file=sys.stderr)
        return 2
    print(
        f"shared {totals['shared']}, a_only {totals['a_only']}, b_only {totals['b_only']}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
