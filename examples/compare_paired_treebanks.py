#!/usr/bin/env python3
"""Compare one pattern's occurrences across two annotations of the same sentences.

A and B are two CoNLL-U files that annotate the same words differently, for
example a UD treebank and its SUD conversion. Each side is queried with its own
pattern by `ling --json matches --evidence --limit all --source-metadata
@sentence.sent_id`, one file per run. Repeat `--b` to pool records from
several runs over B. An occurrence is a `sent_id` and the
word ID of every compared role, so the two patterns must name those roles alike.
The output is one TSV row per occurrence, marked `shared`, `a_only` or `b_only`,
with each side's analysis of the role words.

The two files must hold the same `sent_id` values, each once, and the same
ID, FORM and UPOS sequence in every sentence. Every stream must be a complete,
unsampled run over its side's file. Anything else exits with status 2.
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from collections import Counter
from typing import Any

from compare_occurrences import InputError, identity, read_stream, roles, select


def read_conllu(path: str, label: str) -> dict[str, Any]:
    """Read integer-ID rows and reject missing or repeated sentence IDs."""
    sentences: list[dict[str, Any]] = []
    current: dict[str, Any] | None = None
    with open(path, encoding="utf-8") as source:
        for line in [*source, ""]:
            line = line.rstrip("\n")
            if not line:
                if current is not None:
                    sentences.append(current)
                current = None
                continue
            if current is None:
                current = {"sent_id": None, "text": None, "words": []}
            if line.startswith("# sent_id = "):
                current["sent_id"] = line[len("# sent_id = "):]
            elif line.startswith("# text = "):
                current["text"] = line[len("# text = "):]
            elif not line.startswith("#"):
                fields = line.split("\t")
                if fields[0].isdigit():
                    current["words"].append(fields)
    by_id: dict[str, int] = {}
    for ordinal, sentence in enumerate(sentences):
        sent_id = sentence["sent_id"]
        if sent_id is None:
            raise InputError(f"{label}: sentence {ordinal + 1} of {path} has no sent_id")
        if sent_id in by_id:
            raise InputError(f"{label}: sent_id {sent_id} occurs more than once in {path}")
        by_id[sent_id] = ordinal
    return {"path": path, "sentences": sentences, "by_id": by_id}


def require_aligned(a: dict[str, Any], b: dict[str, Any]) -> None:
    missing = sorted(set(a["by_id"]) ^ set(b["by_id"]))
    if missing:
        raise InputError(
            f"Sentence IDs differ. First unmatched sent_id is {missing[0]} "
            f"({len(missing)} unmatched)."
        )
    unaligned = [
        sent_id
        for sent_id, ordinal in a["by_id"].items()
        if [(word[0], word[1], word[3]) for word in a["sentences"][ordinal]["words"]]
        != [(word[0], word[1], word[3]) for word in b["sentences"][b["by_id"][sent_id]]["words"]]
    ]
    if unaligned:
        raise InputError(
            f"Word ID, FORM or UPOS sequences differ. First differing sent_id is "
            f"{unaligned[0]} ({len(unaligned)} affected)."
        )


def read_side(paths: list[str], conllu: dict[str, Any], label: str) -> dict[str, Any]:
    """Check each match against its side's CoNLL-U file before grouping records."""
    streams = []
    for path in paths:
        with open(path, encoding="utf-8") as source:
            name = os.path.basename(path)
            streams.append((name, read_stream(source, f"{label} {name}")))
    if len({identity(stream) for _, stream in streams}) != 1:
        raise InputError(f"{label}: the runs differ in corpus, analysis or --where scope")
    records = []
    for path, stream in streams:
        admitted = stream["complete"]["counters"]["sentences_admitted"]
        if admitted != len(conllu["sentences"]):
            raise InputError(
                f"{label} {path}: the run read {admitted} sentences, "
                f"{conllu['path']} has {len(conllu['sentences'])}"
            )
        for record in select(stream, None, f"{label} {path}"):
            sent_id = record.get("source_metadata", {}).get("sentence", {}).get("sent_id")
            if sent_id is None:
                raise InputError(
                    f"{label} {path}: record {record['sequence']} has no sent_id; "
                    "rerun with --source-metadata @sentence.sent_id"
                )
            if (
                record["source_name"] != os.path.basename(conllu["path"])
                or sent_id not in conllu["by_id"]
                or conllu["sentences"][conllu["by_id"][sent_id]]["text"]
                not in (None, record["evidence"]["sentence"])
            ):
                raise InputError(f"{label} {path}: record {record['sequence']} is not from {conllu['path']}")
            records.append((path, sent_id, record))
    return {"label": label, "conllu": conllu, "paths": [name for name, _ in streams], "records": records,
            "complete": streams[0][1]["complete"]}


def unit(record: dict[str, Any], compared: list[str], label: str) -> tuple[str, ...]:
    named = roles(record)
    missing = [role for role in compared if role not in named]
    if missing:
        raise InputError(f"{label}: record {record['sequence']} has no role {', '.join(missing)}")
    return tuple(named[role] for role in compared)


def analysis(conllu: dict[str, Any], sent_id: str, record: dict[str, Any] | None,
             compared: list[str], positions: tuple[str, ...]) -> str:
    """Format matched roles, falling back to compared roles when there is no record."""
    words = conllu["sentences"][conllu["by_id"][sent_id]]["words"]
    if record is None:
        named = [(int(position), role) for role, position in zip(compared, positions)
                 if position.isdigit()]
    else:
        named = [(word["word"], role) for word in record["evidence"]["words"] for role in word["roles"]]
    return " ".join(
        f"{role}={words[number - 1][1]}({number},{words[number - 1][7]}→{words[number - 1][6]})"
        for number, role in sorted(named)
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--a-conllu", required=True, help="the CoNLL-U file the A run read")
    parser.add_argument("--b-conllu", required=True, help="the CoNLL-U file the B runs read")
    parser.add_argument("--a", required=True, help="`ling --json matches --evidence` stream over A")
    parser.add_argument("--b", required=True, action="append",
                        help="stream over B; repeat to pool several patterns' records")
    parser.add_argument("--roles", required=True, help="comma-separated roles that identify an occurrence")
    arguments = parser.parse_args(argv)
    compared = arguments.roles.split(",")
    try:
        a_conllu = read_conllu(arguments.a_conllu, "A")
        b_conllu = read_conllu(arguments.b_conllu, "B")
        require_aligned(a_conllu, b_conllu)
        sides = (read_side([arguments.a], a_conllu, "A"), read_side(arguments.b, b_conllu, "B"))
    except (InputError, OSError) as error:
        print(f"compare_paired_treebanks: {error}", file=sys.stderr)
        return 2
    counts: dict[tuple[str, tuple[str, ...]], list[int]] = {}
    shown: dict[tuple[str, tuple[str, ...]], list[dict[str, Any] | None]] = {}
    per_stream: Counter[str] = Counter()
    try:
        for index, side in enumerate(sides):
            for path, sent_id, record in side["records"]:
                key = (sent_id, unit(record, compared, f"{side['label']} {path}"))
                counts.setdefault(key, [0, 0])[index] += 1
                shown.setdefault(key, [None, None])[index] = record
                per_stream[path] += 1
    except InputError as error:
        print(f"compare_paired_treebanks: {error}", file=sys.stderr)
        return 2
    totals: Counter[str] = Counter()
    sys.stdout.write("status\tcount_a\tcount_b\tsent_id\tunit\tanalysis_a\tanalysis_b\tsentence\n")
    order = a_conllu["by_id"]
    for key in sorted(counts, key=lambda key: (order[key[0]], [int(n) for n in re.findall(r"\d+", " ".join(key[1]))])):
        sent_id, positions = key
        count_a, count_b = counts[key]
        status = "shared" if count_a and count_b else "a_only" if count_a else "b_only"
        totals[status] += 1
        fields = [
            status, str(count_a), str(count_b), sent_id,
            " ".join(f"{role}={position}" for role, position in zip(compared, positions)),
            analysis(a_conllu, sent_id, shown[key][0], compared, positions),
            analysis(b_conllu, sent_id, shown[key][1], compared, positions),
            a_conllu["sentences"][order[sent_id]]["text"] or "",
        ]
        sys.stdout.write("\t".join(field.replace("\t", " ") for field in fields) + "\n")
    for side in sides:
        streams = ", ".join(f"{path} {per_stream[path]}" for path in side["paths"])
        print(
            f"{side['label']}: {len(side['records'])} records ({streams}), "
            f"source_identity {side['complete']['source_identity']}, "
            f"analysis_identity {side['complete']['analysis_identity']}",
            file=sys.stderr,
        )
    print(
        f"shared {totals['shared']}, a_only {totals['a_only']}, b_only {totals['b_only']}",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
