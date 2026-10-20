#!/usr/bin/env python3
"""Align the tracked Vibrato/UniDic ヲ格 rule with the GiNZA dependency arm.

The primary comparison in ``wo_predicate_compare.py`` keeps one token set. This
script measures the *tracked* instrument — ``sketch_wo_predicate`` in
``patterns/sketch-grammar.lx`` over a Vibrato/UniDic analysis of the same
JSONL — against ``wo_predicate_dep`` over the GiNZA CoNLL-U, so the number the
proposal cites is about the catalog the reader can run.

The two tokenizations differ, so the join key is the character position of
the を particle in the sentence text, which both agree on. Vibrato records
carry no character offsets (``supplies_char_offsets`` is false for JSONL), so
they are rebuilt here from ``ling tokenize --tsv``: each sentence's token
surfaces are aligned left to right into the sentence text, skipping whitespace
the analyzer dropped. A sentence whose surfaces cannot be aligned is counted
and excluded rather than guessed.

Inputs:

* ``--jsonl``       the exported ``aozora.jsonl`` (sentence texts)
* ``--tokens-tsv``  ``ling tokenize --tsv`` over that JSONL
* ``--vibrato-matches``  ``ling matches --json --patterns patterns/sketch-grammar.lx`` over that JSONL
* ``--dep-matches`` ``ling matches --json --patterns patterns/wo-predicate-dep.lx`` over the CoNLL-U

Output: ``summary.json`` and ``summary.md`` under ``--out-dir`` with the same
five classes as the primary comparison, plus the alignment failure count.
"""

from __future__ import annotations

import argparse
import collections
import csv
import json
import sys
from pathlib import Path


def token_char_starts(text: str, surfaces: list[str]) -> list[int] | None:
    """Character start of each token, or None when surfaces do not align."""
    starts = []
    pos = 0
    for surface in surfaces:
        while pos < len(text) and text[pos].isspace() and not surface[:1].isspace():
            pos += 1
        if not text.startswith(surface, pos):
            return None
        starts.append(pos)
        pos += len(surface)
    return starts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--jsonl", type=Path, required=True)
    parser.add_argument("--tokens-tsv", type=Path, required=True)
    parser.add_argument("--vibrato-matches", type=Path, required=True)
    parser.add_argument("--dep-matches", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args(argv)

    # Vibrato matches: (doc, sent) -> list of (collocate_end_token, head_start_token).
    vib: dict[tuple[int, int], list[tuple[int, int]]] = collections.defaultdict(list)
    vib_run = None
    with args.vibrato_matches.open(encoding="utf-8") as handle:
        for line in handle:
            record = json.loads(line)
            if record.get("type") == "run":
                vib_run = record.get("data")
            if record.get("type") != "match" or record["data"]["pattern"] != "sketch_wo_predicate":
                continue
            data = record["data"]
            captures = {c["name"]: c["span"] for c in data["captures"]}
            vib[(data["document_ordinal"], data["document_sentence_ordinal"])].append(
                (captures["collocate"]["end"], captures["head"]["start"])
            )

    # Dependency matches: (doc, sent, wo_char) -> head_char.
    dep: dict[tuple[int, int, int], int] = {}
    with args.dep_matches.open(encoding="utf-8") as handle:
        for line in handle:
            record = json.loads(line)
            if record.get("type") != "match" or record["data"]["pattern"] != "wo_predicate_dep":
                continue
            data = record["data"]
            captures = {c["name"]: c["char_span"] for c in data["captures"]}
            key = (data["document_ordinal"], data["document_sentence_ordinal"], captures["w"]["start"])
            dep[key] = captures["head"]["start"]

    # Sentence texts by (doc, sent).
    texts: dict[tuple[int, int], str] = {}
    with args.jsonl.open(encoding="utf-8") as handle:
        for doc, line in enumerate(handle):
            for sent, text in enumerate(json.loads(line)["sentences"]):
                if (doc, sent) in vib:
                    texts[(doc, sent)] = text

    # Token surfaces for sentences that have Vibrato matches, then alignment.
    surfaces: dict[tuple[int, int], list[tuple[int, str]]] = collections.defaultdict(list)
    with args.tokens_tsv.open(encoding="utf-8", newline="") as handle:
        for row in csv.reader(handle, delimiter="\t"):
            key = (int(row[2]), int(row[3]))
            if key in vib:
                surfaces[key].append((int(row[4]), row[5]))

    vib_char: dict[tuple[int, int, int], int] = {}
    unaligned_sentences = 0
    unaligned_matches = 0
    for key, matches in vib.items():
        toks = sorted(surfaces.get(key, []))
        starts = token_char_starts(texts.get(key, ""), [s for _, s in toks]) if toks else None
        if starts is None or len(starts) != len(toks):
            unaligned_sentences += 1
            unaligned_matches += len(matches)
            continue
        for wo_tok, head_tok in matches:
            if wo_tok < len(starts) and head_tok < len(starts):
                vib_char[key + (starts[wo_tok],)] = starts[head_tok]

    classes = collections.Counter()
    for key, vh in vib_char.items():
        dh = dep.get(key)
        if dh is None:
            classes["linear_only"] += 1
        elif dh == vh:
            classes["both_same"] += 1
        else:
            classes["both_differ"] += 1
    for key in dep:
        if key not in vib_char:
            classes["dep_only"] += 1

    linear_total = len(vib_char)
    summary = {
        "vibrato_run": {k: (vib_run or {}).get(k) for k in ("run_id", "catalog_fingerprint", "dictionary_identity")},
        "vibrato_matches": sum(len(v) for v in vib.values()),
        "vibrato_matches_aligned": linear_total,
        "unaligned_sentences": unaligned_sentences,
        "unaligned_matches": unaligned_matches,
        "dep_matches": len(dep),
        "classes": dict(classes),
        "dep_only_share_of_linear": (classes["dep_only"] / linear_total) if linear_total else None,
    }
    args.out_dir.mkdir(parents=True, exist_ok=True)
    (args.out_dir / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    lines = ["| class | count |", "|---|---:|"]
    for cls in ("both_same", "both_differ", "dep_only", "linear_only"):
        lines.append(f"| {cls} | {classes[cls]:,} |")
    lines.append("")
    lines.append(
        f"Vibrato `sketch_wo_predicate` {summary['vibrato_matches']:,} records, {linear_total:,} aligned "
        f"({unaligned_matches:,} in {unaligned_sentences:,} unalignable sentences); dependency {len(dep):,}; "
        f"dependency-only as share of aligned linear: {100 * (summary['dep_only_share_of_linear'] or 0):.1f}%"
    )
    (args.out_dir / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
