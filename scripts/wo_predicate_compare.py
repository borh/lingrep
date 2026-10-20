#!/usr/bin/env python3
"""Compare the linear and dependency ヲ格名詞句–述語 arms over one CoNLL-U corpus.

Reads the match JSONL (``ling matches --json``) of
``patterns/wo-predicate-dep.lx`` — which emits ``wo_predicate_linear`` and
``wo_predicate_dep`` over the same tokens — together with the CoNLL-U file the
matches came from, and classifies every を case particle in the corpus:

* ``both_same``      — both arms found it and named the same predicate token
* ``both_differ``    — both found it, different predicate tokens
* ``dep_only``       — only the dependency arm found it
* ``linear_only``    — only the linear arm found it
* ``neither``        — a を case particle no arm attached (the oracle still
                       records the parser's head for it)

A third, independent oracle is computed directly from the CoNLL-U columns:
for every ``を`` with ``xpos`` starting ``助詞-格助詞`` and ``deprel=case``,
its head is the nominal it marks, and that nominal's head is the predicate.
The oracle is what any head-sharing query could express, free of lingrep patterns's
capture and adjacency requirements; ``dep_vs_oracle`` reports how much of it
the dependency catalog reaches and why it misses (nominal not adjacent to を,
head not a VERB, relation outside the catalog's set).

Outputs, under ``--out-dir``:

* ``summary.json`` — all counts, the distance distributions, and provenance
  (input digests, match run record).
* ``summary.md``   — the same as a Markdown table for the results record.
* ``review-<class>.tsv`` — stratified random samples for manual adjudication,
  one per class, with the sentence text and the three token positions marked.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import random
import sys
from dataclasses import dataclass, field
from pathlib import Path

WO_XPOS_PREFIX = "助詞-格助詞"


@dataclass
class Sentence:
    doc: int
    ordinal: int
    doc_id: str
    sent_id: str
    text: str
    forms: list[str] = field(default_factory=list)
    lemmas: list[str] = field(default_factory=list)
    upos: list[str] = field(default_factory=list)
    xpos: list[str] = field(default_factory=list)
    heads: list[int] = field(default_factory=list)
    rels: list[str] = field(default_factory=list)


def read_conllu(path: Path):
    """Yield sentences with document and in-document ordinals as ling assigns them."""
    doc = -1
    ordinal = 0
    doc_id = ""
    sent_id = ""
    text = ""
    rows: list[list[str]] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if line.startswith("# newdoc"):
                doc += 1
                ordinal = 0
                doc_id = line.partition("=")[2].strip()
                continue
            if line.startswith("# sent_id"):
                sent_id = line.partition("=")[2].strip()
                continue
            if line.startswith("# text ="):
                text = line[len("# text =") :].strip()
                continue
            if line.startswith("#"):
                continue
            if not line:
                if rows:
                    yield _sentence(doc, ordinal, doc_id, sent_id, text, rows)
                    ordinal += 1
                    rows = []
                continue
            cols = line.split("\t")
            if "-" in cols[0] or "." in cols[0]:
                continue
            rows.append(cols)
    if rows:
        yield _sentence(doc, ordinal, doc_id, sent_id, text, rows)


def _sentence(doc, ordinal, doc_id, sent_id, text, rows) -> Sentence:
    s = Sentence(doc, ordinal, doc_id, sent_id, text)
    for cols in rows:
        s.forms.append(cols[1])
        s.lemmas.append(cols[2])
        s.upos.append(cols[3])
        s.xpos.append(cols[4])
        s.heads.append(int(cols[6]) if cols[6].isdigit() else 0)
        s.rels.append(cols[7])
    return s


def read_matches(path: Path):
    """Map (doc, sentence, を token index) -> head token index, per pattern."""
    linear: dict[tuple[int, int, int], int] = {}
    dep: dict[tuple[int, int, int], tuple[int, str]] = {}
    run = None
    counts = collections.Counter()
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            record = json.loads(line)
            kind = record.get("type")
            if kind == "run":
                run = record.get("data")
                continue
            if kind != "match":
                continue
            data = record["data"]
            captures = {c["name"]: c["span"] for c in data["captures"]}
            key = (data["document_ordinal"], data["document_sentence_ordinal"])
            counts[data["pattern"]] += 1
            if data["pattern"] == "wo_predicate_linear":
                wo = captures["collocate"]["end"]
                linear[key + (wo,)] = captures["head"]["start"]
            elif data["pattern"] == "wo_predicate_dep":
                wo = captures["w"]["start"]
                rel = next(
                    (k["value"] for k in data.get("key", []) if k["name"].endswith("dep.rel")),
                    "",
                )
                dep[key + (wo,)] = (captures["head"]["start"], rel)
    return linear, dep, run, counts


def _nest(counter: collections.Counter) -> dict[str, dict[str, int]]:
    nested: dict[str, dict[str, int]] = collections.defaultdict(dict)
    for (group, cls), n in sorted(counter.items(), key=lambda kv: (-kv[1], str(kv[0]))):
        nested[str(group)][cls] = n
    return {group: dict(sorted(row.items())) for group, row in sorted(nested.items(), key=lambda kv: -sum(kv[1].values()))}


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def linear_blocker(s: Sentence, wo: int, head: int) -> str:
    """Name the first reason the linear rule could not reach the parser's head."""
    if wo == 0 or not (s.xpos[wo - 1].startswith(("名詞", "代名詞", "接尾辞-名詞的"))):
        return "nominal_shape"
    if head < wo:
        return "head_precedes_wo"
    between = range(wo + 1, head)
    if len(between) > 5:
        return "gap_over_5"
    for j in between:
        x = s.xpos[j]
        if x.startswith("補助記号"):
            return "punctuation_in_gap"
        if x.startswith(("動詞", "助動詞")):
            return "verb_or_aux_in_gap"
        if x.startswith(("助詞-格助詞", "助詞-係助詞", "助詞-接続助詞")):
            return "particle_in_gap"
        if "サ変可能" in x:
            return "sahen_in_gap"
    head_x = s.xpos[head]
    if head_x.startswith("動詞"):
        return "unexplained"
    if "サ変可能" in head_x:
        nxt = head + 1
        if nxt < len(s.lemmas) and s.lemmas[nxt] == "する":
            return "unexplained"
        return "sahen_without_suru"
    return "head_shape_" + head_x.split("-")[0]


def mark(s: Sentence, wo: int, linear_head: int | None, dep_head: int | None) -> str:
    """Render the sentence with を bracketed and the two heads tagged."""
    out = []
    for i, form in enumerate(s.forms):
        tag = ""
        if i == wo:
            tag = "【を】"
            out.append(tag)
            continue
        if i == linear_head and i == dep_head:
            out.append(f"⟦{form}⟧")
        elif i == linear_head:
            out.append(f"⟨L:{form}⟩")
        elif i == dep_head:
            out.append(f"⟨D:{form}⟩")
        else:
            out.append(form)
    return "".join(out)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--conllu", type=Path, required=True)
    parser.add_argument("--matches", type=Path, required=True, help="ling matches --json output")
    parser.add_argument("--out-dir", type=Path, required=True)
    parser.add_argument(
        "--jsonl",
        type=Path,
        default=None,
        help="the exported aozora.jsonl; attaches orthography and translation status per document",
    )
    parser.add_argument("--review-size", type=int, default=100)
    parser.add_argument("--seed", type=int, default=20260914)
    args = parser.parse_args(argv)

    linear, dep, run, pattern_counts = read_matches(args.matches)

    doc_meta: list[dict] = []
    if args.jsonl is not None:
        with args.jsonl.open(encoding="utf-8") as handle:
            for line in handle:
                record = json.loads(line)
                doc_meta.append(
                    {
                        "orthography": record.get("orthography", ""),
                        "translated": bool(record.get("translated")),
                    }
                )

    classes = collections.Counter()
    classes_by_orthography = collections.Counter()
    classes_by_translation = collections.Counter()
    dep_only_blockers = collections.Counter()
    oracle = collections.Counter()
    dep_vs_oracle = collections.Counter()
    dep_rel = collections.Counter()
    linear_distance = collections.Counter()
    dep_distance = collections.Counter()
    dep_only_distance = collections.Counter()
    review: dict[str, list[dict]] = collections.defaultdict(list)
    rng = random.Random(args.seed)
    reservoir_seen = collections.Counter()
    sentences = 0
    tokens = 0
    docs = set()
    wo_total = 0

    def offer(cls: str, row: dict) -> None:
        # Reservoir sampling per class, so the review files are uniform samples.
        reservoir_seen[cls] += 1
        n = reservoir_seen[cls]
        if len(review[cls]) < args.review_size:
            review[cls].append(row)
        else:
            j = rng.randrange(n)
            if j < args.review_size:
                review[cls][j] = row

    for s in read_conllu(args.conllu):
        sentences += 1
        tokens += len(s.forms)
        docs.add(s.doc)
        for i, form in enumerate(s.forms):
            if form != "を" or not s.xpos[i].startswith(WO_XPOS_PREFIX):
                continue
            wo_total += 1
            key = (s.doc, s.ordinal, i)
            lh = linear.get(key)
            dh = dep.get(key)
            dep_head = dh[0] if dh else None
            if lh is not None and dh is not None:
                cls = "both_same" if lh == dep_head else "both_differ"
            elif dh is not None:
                cls = "dep_only"
            elif lh is not None:
                cls = "linear_only"
            else:
                cls = "neither"
            classes[cls] += 1
            if doc_meta and s.doc < len(doc_meta):
                classes_by_orthography[(doc_meta[s.doc]["orthography"], cls)] += 1
                classes_by_translation[(doc_meta[s.doc]["translated"], cls)] += 1
            if cls == "dep_only":
                dep_only_blockers[linear_blocker(s, i, dep_head)] += 1
            if dh:
                dep_rel[dh[1]] += 1
                dep_distance[min(dep_head - i, 30)] += 1
                if cls == "dep_only":
                    dep_only_distance[min(dep_head - i, 30)] += 1
            if lh is not None:
                linear_distance[min(lh - i, 30)] += 1

            # Oracle: を -case-> nominal; nominal -rel-> predicate.
            nominal = s.heads[i] - 1 if s.rels[i] == "case" and s.heads[i] > 0 else None
            pred = None
            if nominal is not None and s.heads[nominal] > 0:
                pred = s.heads[nominal] - 1
            if nominal is None:
                oracle["wo_not_case"] += 1
                o_cls = "wo_not_case"
            elif pred is None:
                oracle["nominal_is_root"] += 1
                o_cls = "nominal_is_root"
            else:
                oracle["predicate_found"] += 1
                o_cls = "predicate_found"
                if dh is not None:
                    dep_vs_oracle["same" if dep_head == pred else "different_head"] += 1
                else:
                    # Why did the catalog not reach what the oracle expresses?
                    if nominal != i - 1:
                        dep_vs_oracle["miss:nominal_not_adjacent_to_wo"] += 1
                    elif s.upos[nominal] not in {"NOUN", "PROPN", "PRON", "NUM"}:
                        dep_vs_oracle["miss:nominal_upos_" + s.upos[nominal]] += 1
                    elif pred < i:
                        dep_vs_oracle["miss:predicate_precedes_wo"] += 1
                    elif s.upos[pred] != "VERB":
                        dep_vs_oracle["miss:predicate_upos_" + s.upos[pred]] += 1
                    else:
                        dep_vs_oracle["miss:relation_" + s.rels[nominal]] += 1
            offer(
                cls,
                {
                    "class": cls,
                    "doc_id": s.doc_id,
                    "sent_id": s.sent_id,
                    "wo_index": i,
                    "linear_head": "" if lh is None else s.forms[lh],
                    "dep_head": "" if dep_head is None else s.forms[dep_head],
                    "dep_rel": dh[1] if dh else "",
                    "oracle": o_cls,
                    "oracle_head": "" if pred is None else s.forms[pred],
                    "marked": mark(s, i, lh, dep_head),
                    "text": s.text,
                    "verdict": "",
                },
            )

    args.out_dir.mkdir(parents=True, exist_ok=True)
    for cls, rows in review.items():
        with (args.out_dir / f"review-{cls}.tsv").open("w", encoding="utf-8") as out:
            cols = list(rows[0].keys())
            out.write("\t".join(cols) + "\n")
            for row in rows:
                out.write("\t".join(str(row[c]) for c in cols) + "\n")

    linear_total = sum(v for k, v in classes.items() if k in {"both_same", "both_differ", "linear_only"})
    dep_total = sum(v for k, v in classes.items() if k in {"both_same", "both_differ", "dep_only"})
    summary = {
        "inputs": {
            "conllu": str(args.conllu),
            "conllu_sha256": sha256(args.conllu),
            "matches": str(args.matches),
            "matches_sha256": sha256(args.matches),
            "catalog_fingerprint": (run or {}).get("catalog_fingerprint"),
            "run_id": (run or {}).get("run_id"),
        },
        "corpus": {"documents": len(docs), "sentences": sentences, "tokens": tokens},
        "wo_case_particles": wo_total,
        "pattern_records": dict(pattern_counts),
        "classes": dict(classes),
        "linear_total": linear_total,
        "dep_total": dep_total,
        "dep_only_share_of_linear": (classes["dep_only"] / linear_total) if linear_total else None,
        "dep_rel": dict(dep_rel.most_common()),
        "oracle": dict(oracle),
        "dep_vs_oracle": dict(dep_vs_oracle.most_common()),
        "distance_wo_to_head": {
            "linear": dict(sorted(linear_distance.items())),
            "dep": dict(sorted(dep_distance.items())),
            "dep_only": dict(sorted(dep_only_distance.items())),
        },
        "dep_only_blockers": dict(dep_only_blockers.most_common()),
        "classes_by_orthography": _nest(classes_by_orthography),
        "classes_by_translation": _nest(
            collections.Counter({(("translated" if k[0] else "original"), k[1]): v for k, v in classes_by_translation.items()})
        ),
        "review": {cls: len(rows) for cls, rows in review.items()},
        "seed": args.seed,
    }
    (args.out_dir / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    lines = [
        "| class | count | share of を |",
        "|---|---:|---:|",
    ]
    for cls in ("both_same", "both_differ", "dep_only", "linear_only", "neither"):
        n = classes[cls]
        lines.append(f"| {cls} | {n:,} | {100 * n / wo_total:.1f}% |" if wo_total else f"| {cls} | {n:,} | |")
    lines += [
        "",
        f"を case particles: {wo_total:,}; linear arm {linear_total:,}; dependency arm {dep_total:,}; "
        f"dependency-only as share of linear: {100 * (summary['dep_only_share_of_linear'] or 0):.1f}%",
        "",
        "| dependency vs oracle | count |",
        "|---|---:|",
    ]
    for k, v in dep_vs_oracle.most_common():
        lines.append(f"| {k} | {v:,} |")
    lines += ["", "| dependency-only: what blocks the linear rule | count |", "|---|---:|"]
    for k, v in dep_only_blockers.most_common():
        lines.append(f"| {k} | {v:,} |")
    if classes_by_orthography:
        lines += ["", "| orthography | を | both_same | both_differ | dep_only | linear_only | neither |", "|---|---:|---:|---:|---:|---:|---:|"]
        for orth, row in summary["classes_by_orthography"].items():
            total = sum(row.values())
            cells = " | ".join(f"{100 * row.get(c, 0) / total:.1f}%" for c in ("both_same", "both_differ", "dep_only", "linear_only", "neither"))
            lines.append(f"| {orth or '(none)'} | {total:,} | {cells} |")
    (args.out_dir / "summary.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
