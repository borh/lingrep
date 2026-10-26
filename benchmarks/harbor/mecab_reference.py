"""Shared half of a MeCab-backed reference `run-analysis`.

A case script calls `main(select)`; `select(tokens, document)` receives one
sentence's tokens and the input document (with its `metadata`) and yields
`(first_token, end_token, label)` with `end_token` exclusive.
"""

import csv
import json
import subprocess
import sys
from collections.abc import Callable, Iterable
from pathlib import Path

# UniDic feature columns: an unknown word has only the first six.
FIELDS = {"pos1": 0, "pos2": 1, "pos3": 2, "pos4": 3, "ctype": 4, "cform": 5, "lemma": 7}


class Token(dict):
    __getattr__ = dict.__getitem__


def analyze(sentences: list[str]) -> list[list[Token]]:
    """Tokenize every sentence in one MeCab run; offsets are code points."""
    completed = subprocess.run(
        ["mecab"],
        input="".join(sentence + "\n" for sentence in sentences),
        text=True,
        capture_output=True,
        check=True,
    )
    analyses: list[list[Token]] = []
    tokens: list[Token] = []
    for line in completed.stdout.splitlines():
        if line == "EOS":
            analyses.append(tokens)
            tokens = []
            continue
        surface, feature = line.split("\t", 1)
        columns = next(csv.reader([feature]))
        features = {
            name: columns[index] if index < len(columns) and columns[index] != "*" else ""
            for name, index in FIELDS.items()
        }
        tokens.append(Token(surface=surface, **features))
    if len(analyses) != len(sentences):
        raise RuntimeError(f"mecab returned {len(analyses)} analyses for {len(sentences)} sentences")
    for sentence, tokens in zip(sentences, analyses):
        cursor = 0
        for token in tokens:
            start = sentence.index(token.surface, cursor)
            token["start"], token["end"] = start, start + len(token.surface)
            cursor = token["end"]
    return analyses


def sentence_final(tokens: list[Token], index: int) -> bool:
    return index + 2 == len(tokens) and tokens[index + 1].pos1 == "補助記号"


def predicate_start(tokens: list[Token], verb: int) -> int:
    if (
        tokens[verb].lemma == "為る"
        and verb > 0
        and tokens[verb - 1].pos1 == "名詞"
        and tokens[verb - 1].pos3 == "サ変可能"
    ):
        return verb - 1
    return verb


def auxiliary_run(tokens: list[Token], start: int) -> int:
    end = start
    while end < len(tokens) and tokens[end].pos1 == "助動詞":
        end += 1
    return end


def main(select: Callable[[list[Token], dict], Iterable[tuple[int, int, str]]]) -> None:
    documents = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines() if line]
    rows = []
    for document in documents:
        for ordinal, tokens in enumerate(analyze(document["sentences"])):
            for first, end, label in select(tokens, document):
                rows.append(
                    {
                        "schema": "lingrep-linguistic-eval.evidence/v1",
                        "eval_document_id": document["eval_document_id"],
                        "sentence_ordinal": ordinal,
                        "start_char": tokens[first].start,
                        "end_char": tokens[end - 1].end,
                        "label": label,
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
    Path(sys.argv[2]).write_text(
        "".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows)
    )
