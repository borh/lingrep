#!/usr/bin/env python3
"""Per-stage totals from upstream's instrumented SF pipeline, at the pinned SHA.

Run with dm-annotations' own interpreter — it owns spaCy and ja_ginza, which
this repository deliberately does not depend on. Point `DM_ANNOTATIONS` at
that checkout and use the interpreter inside it::

    "$DM_ANNOTATIONS/.venv/bin/python" \
        scripts/upstream_sf_differential.py \
        path/to/corpus.jsonl \
        upstream-stage-baseline.json

This script is the acceptance evidence for the policy block in
``patterns/sf-experimental.lx``.

What it compares, and what it refuses to
----------------------------------------
Index-level comparison with lingrep is impossible — upstream tokenizes
with GiNZA, we tokenize with vibrato + UniDic-CWJ, and token indices are not
commensurable. Nothing here emits a `(start, end)` intended to be matched
against ours. What it emits is per-stage record counts and the multiset of
surfaces each stage removed, which is the shape parity the migration is held
to.

Each corpus sentence is fed to GiNZA on its own, and GiNZA may split it
further — 498 corpus sentences become 545 GiNZA sentences. That granularity is
not a detail: joining a document's sentences and letting GiNZA re-split gives
**301** sentences instead, because GiNZA merges across the corpus's own
boundaries, and the longer sentences push most records out of
`filter_sentence_final`'s five-token window (−646 instead of −445). Per
sentence is also what lingrep does with this JSONL, so it is the only
granularity at which the two sides are describing the same thing.
"""

from __future__ import annotations

import argparse
import collections
import json
import sys
from pathlib import Path

# The canonical stage order, from `filter_pipeline.py`. Named here so a
# reordering upstream shows up as a mismatch rather than as a silently
# different table.
STAGES = [
    "raw",
    "deduplicate_longest",
    "filter_teiru_negation",
    "filter_passive_before_aspect",
    "suppress_inner_layers",
    "split_trailing_copula_matches",
    "filter_sentence_final",
    "filter_terminal_toiu",
    "filter_s0_requires_upper_layer",
]


def load_documents(path: Path) -> list[dict]:
    """One dict per JSONL line, each with a `sentences` list."""
    documents = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                documents.append(json.loads(line))
    return documents


def trace_corpus(documents: list[dict]) -> dict:
    """Per-stage totals for the whole corpus, at the pinned commit.

    Calls `run_filter_pipeline(instrument=True)` directly rather than going
    through `trace_sentence_patterns`. That helper projects each stage to one
    row per record *entering* it, flagged `kept` — which is lossy for the one
    non-monotone stage: `split_trailing_copula_matches` replaces 31 records
    with 62, and the 31 replacements never appear in any `before` list, so a
    kept-count reads the stage as −31 where upstream is +31.

    `stage_rec["after"]` is the set the stage actually produced, so
    `len(...)` of it is upstream's own number and not a re-derivation.
    """
    # `dm_annotations` must be imported before `spacy.load`: it registers the
    # `compound_splitter` factory defaults that ja_ginza's config needs, and a
    # bare `spacy.load("ja_ginza")` fails validation without it.
    from dm_annotations.analysis.experimental_sf_detect import build_experimental_matcher
    from dm_annotations.pipeline.filter_pipeline import MAX_TOKENS_FROM_END, run_filter_pipeline
    from dm_annotations.pipeline.sf_experimental import sf_experimental_classifications

    import spacy

    nlp = spacy.load("ja_ginza")
    # NOT `create_sf_matcher` — that is the non-experimental pattern set, and
    # using it would compare against a different catalog entirely.
    matcher = build_experimental_matcher(nlp)
    table = sf_experimental_classifications

    before: collections.Counter = collections.Counter()
    after: collections.Counter = collections.Counter()
    removed: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    patterns: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    ginza_sentences = 0

    for document in documents:
        for text in document.get("sentences", []):
            if not text:
                continue
            doc = nlp(text)
            for sent in list(doc.sents) if doc.sents else [doc[:]]:
                ginza_sentences += 1
                current = [
                    (mid, start + sent.start, end + sent.start)
                    for mid, start, end in matcher(sent)
                ]
                before["raw"] += len(current)
                after["raw"] += len(current)

                result = run_filter_pipeline(
                    current,
                    doc=doc,
                    vocab=nlp.vocab,
                    classifications=table,
                    sent=sent,
                    max_tokens_from_end=MAX_TOKENS_FROM_END,
                    instrument=True,
                )
                for record in result["trace"]:
                    stage = record["stage"]
                    before[stage] += len(record["before"])
                    after[stage] += len(record["after"])
                    survivors = set(record["after"])
                    for match in record["before"]:
                        if match not in survivors:
                            mid, start, end = match
                            removed[stage][doc[start:end].text] += 1
                            patterns[stage][nlp.vocab.strings[mid]] += 1

    stages = []
    for stage in STAGES:
        if stage not in before:
            continue
        stages.append(
            {
                "stage": stage,
                "before": before[stage],
                "after": after[stage],
                "delta": after[stage] - before[stage],
                # Capped for readability; the totals above are complete.
                "removed_surfaces_top": removed[stage].most_common(15),
                "removed_patterns": sorted(patterns[stage].items()),
            }
        )
    return {"stages": stages, "ginza_sentences": ginza_sentences}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path, help="JSONL corpus with a `sentences` list per line")
    parser.add_argument("output", type=Path, help="where to write the JSON summary")
    args = parser.parse_args()

    documents = load_documents(args.corpus)
    summary = trace_corpus(documents)
    summary["documents"] = len(documents)
    summary["corpus_sentences"] = sum(len(d.get("sentences", [])) for d in documents)

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding="utf-8")

    width = max(len(stage["stage"]) for stage in summary["stages"])
    for stage in summary["stages"]:
        print(f"{stage['stage']:<{width}}  {stage['before']:>6} -> {stage['after']:>6}  {stage['delta']:+}")
    print(
        f"\n{summary['documents']} documents, "
        f"{summary['corpus_sentences']} corpus sentences, "
        f"{summary['ginza_sentences']} GiNZA sentences"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
