#!/usr/bin/env python3
"""Reference `run-analysis`: replays the `query.lx` beside it with `ling`, scoped
by the `--where` clause in a sibling `scope.where` when the case declares one."""

import json
import subprocess
import sys
from pathlib import Path


def events(*arguments: str) -> list[dict]:
    completed = subprocess.run(
        ["ling", "--json", *arguments],
        text=True,
        capture_output=True,
        check=True,
    )
    return [json.loads(line) for line in completed.stdout.splitlines()]


def event_data(values: list[dict], kind: str) -> list[dict]:
    return [value["data"] for value in values if value.get("type") == kind]


def main() -> None:
    corpus = Path(sys.argv[1])
    output = Path(sys.argv[2])
    solution = Path(__file__).resolve().parent
    scope = solution / "scope.where"
    matched = events(
        "matches",
        "--patterns",
        str(solution / "query.lx"),
        *(["--where", scope.read_text().strip()] if scope.exists() else []),
        "--source-metadata",
        "@document.eval_document_id",
        "--limit",
        "all",
        "--progress",
        "never",
        str(corpus),
    )

    rows = []
    for match in event_data(matched, "match"):
        char_span = match["char_span"]
        rows.append(
            {
                "schema": "lingrep-linguistic-eval.evidence/v1",
                "eval_document_id": match["source_metadata"]["document"][
                    "eval_document_id"
                ],
                "sentence_ordinal": match["document_sentence_ordinal"],
                "start_char": char_span["start"],
                "end_char": char_span["end"],
                "label": match["pattern"],
            }
        )
    rows.sort(
        key=lambda row: (
            row["eval_document_id"],
            row["sentence_ordinal"],
            row["start_char"],
            row["end_char"],
            row["label"],
        )
    )
    with output.open("w") as destination:
        for row in rows:
            destination.write(json.dumps(row, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
