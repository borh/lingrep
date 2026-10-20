#!/usr/bin/env python3
"""Calculate adjacent-pair gravity from lingrep patterns-selected ling matches."""

from __future__ import annotations

from collections import Counter
import json
import math
import sys
from typing import TextIO


SCHEMA = "lingrep.query/v2"


def token_text(token: object) -> str:
    if not isinstance(token, dict) or not isinstance(token.get("index"), int):
        raise ValueError("token evidence must have an integer `index`")
    attributes = token.get("attributes")
    if not isinstance(attributes, dict) or not isinstance(attributes.get("text"), str):
        raise ValueError("token evidence is missing string attribute `text`")
    return attributes["text"]


def count_pairs(
    source: TextIO,
) -> tuple[Counter[tuple[str, str]], Counter[str], Counter[str]]:
    pairs: Counter[tuple[str, str]] = Counter()
    outgoing: Counter[str] = Counter()
    incoming: Counter[str] = Counter()
    saw_run = False
    complete = False
    current_ordinal: int | None = None
    current_tokens: dict[int, str] = {}
    closed_ordinal = -1

    def finish_sentence() -> None:
        nonlocal current_tokens, closed_ordinal
        if current_ordinal is None:
            return
        sentence = [current_tokens[index] for index in sorted(current_tokens)]
        for left, right in zip(sentence, sentence[1:]):
            pairs[(left, right)] += 1
            outgoing[left] += 1
            incoming[right] += 1
        closed_ordinal = current_ordinal
        current_tokens = {}

    for line_number, line in enumerate(source, 1):
        if not line.strip():
            continue
        if complete:
            raise ValueError("event follows the complete event")
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"line {line_number}: invalid JSON: {error.msg}") from error
        if not isinstance(event, dict) or event.get("schema") != SCHEMA:
            raise ValueError(f"line {line_number}: expected {SCHEMA}")
        event_type = event.get("type")
        data = event.get("data")

        if not saw_run:
            if event_type != "run" or not isinstance(data, dict):
                raise ValueError("stream must begin with a ling run event")
            if data.get("mode") != "matches":
                raise ValueError("gravity accepts only ling matches output")
            saw_run = True
        elif event_type == "run":
            raise ValueError("stream has more than one run event")
        elif event_type == "match":
            if not isinstance(data, dict) or not isinstance(data.get("tokens"), list):
                raise ValueError(f"line {line_number}: match is missing `tokens`")
            ordinal = data.get("corpus_sentence_ordinal")
            if not isinstance(ordinal, int) or ordinal < 0:
                raise ValueError(f"line {line_number}: match has invalid sentence ordinal")
            if current_ordinal != ordinal:
                finish_sentence()
                if ordinal <= closed_ordinal:
                    raise ValueError("match events are not in sentence order")
                current_ordinal = ordinal
            for token in data["tokens"]:
                text = token_text(token)
                index = token["index"]
                previous = current_tokens.setdefault(index, text)
                if previous != text:
                    raise ValueError("overlapping match evidence disagrees on token text")
        elif event_type == "complete":
            if not isinstance(data, dict):
                raise ValueError("complete event is missing data")
            if data.get("coverage") != "full" or data.get("termination") != "eof":
                raise ValueError("gravity requires full coverage terminating at eof")
            complete = True
        elif event_type != "definition":
            raise ValueError(f"line {line_number}: unexpected event type `{event_type}`")

    finish_sentence()
    if not complete:
        raise ValueError("gravity requires the final ling complete event")
    return pairs, outgoing, incoming


def gravity_score(
    left: str,
    right: str,
    count: int,
    outgoing: Counter[str],
    incoming: Counter[str],
) -> float:
    left_total = outgoing[left] + incoming[left]
    right_total = outgoing[right] + incoming[right]
    return math.log(count * outgoing[left] / left_total) + math.log(
        count * incoming[right] / right_total
    )


def analyze(source: TextIO, output: TextIO) -> None:
    pairs, outgoing, incoming = count_pairs(source)
    for (left, right), count in sorted(pairs.items()):
        json.dump(
            {
                "left": left,
                "right": right,
                "count": count,
                "score": gravity_score(left, right, count, outgoing, incoming),
            },
            output,
            ensure_ascii=False,
            separators=(",", ":"),
        )
        output.write("\n")


def main() -> int:
    if len(sys.argv) != 1:
        print("gravity: no options; selection belongs in the lingrep patterns catalog", file=sys.stderr)
        return 2
    try:
        analyze(sys.stdin, sys.stdout)
    except (OSError, ValueError) as error:
        print(f"gravity: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
