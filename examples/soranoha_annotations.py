#!/usr/bin/env python3
"""Convert a bounded, saved CoNLL-U query to a Soranoha annotation layer.

Binding JSON contains view {id, text, eligible_spans}, corpus_sha256 and
sentences [{text, start, end}]. Sentence entries follow input order; start/end
are explicit UTF-8 byte positions in the view. Every CoNLL-U sentence must
carry # text. The producer exit status must be supplied separately.
"""

import argparse
import hashlib
import json
from pathlib import Path


VARIANT_FIELDS = {"run_id", "elapsed_millis", "temporary_storage_bytes"}


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def stable(value):
    if isinstance(value, dict):
        return {key: stable(item) for key, item in value.items() if key not in VARIANT_FIELDS}
    if isinstance(value, list):
        return [stable(item) for item in value]
    return value


def convert(events, binding, corpus, catalog, vocabulary, exit_code):
    if not events or any(event.get("schema") != "lingrep.query/v2" for event in events):
        raise ValueError("expected lingrep.query/v2 events")
    if events[0]["type"] != "run" or sum(event["type"] == "run" for event in events) != 1:
        raise ValueError("expected exactly one initial run")
    run = events[0]["data"]
    if run["mode"] != "matches" or (run["analyzer"]["family"], run["analyzer"]["boundary_policy"], run["analyzer"]["normalization"]) != ("pretokenized", "conllu-sentence-v1", "none"):
        raise ValueError("this consumer supports CoNLL-U matches only")
    if run["corpus_kind"] != "direct":
        raise ValueError("this consumer requires a direct, identified single-file run")
    if binding["corpus_sha256"] != digest(corpus):
        raise ValueError("sentence mapping targets different corpus bytes")
    supplied_texts = []
    for block in corpus.decode("utf-8").strip().split("\n\n"):
        texts = [line[len("# text = "):] for line in block.splitlines() if line.startswith("# text = ")]
        if len(texts) != 1:
            raise ValueError("each source sentence needs exactly one # text")
        supplied_texts.extend(texts)
    sentences = binding["sentences"]
    if [sentence["text"] for sentence in sentences] != supplied_texts:
        raise ValueError("mapping differs from source sentence text or order")
    view = binding["view"]
    text = view["text"]
    view_id = digest(canonical({"policy": "body-v1", "unit": "utf8-bytes", "text": text}).encode())
    if view["id"] != view_id:
        raise ValueError("incorrect body-v1 view identity")
    body = text.encode("utf-8")
    boundaries = {0}
    byte_position = 0
    for char in text:
        byte_position += len(char.encode())
        boundaries.add(byte_position)
    eligible = view["eligible_spans"]
    previous = 0
    for start, end in eligible:
        if start not in boundaries or end not in boundaries or not previous <= start < end:
            raise ValueError("invalid eligibility ranges")
        previous = end
    previous = 0
    for sentence in sentences:
        start, end = sentence["start"], sentence["end"]
        if start not in boundaries or end not in boundaries or not previous <= start <= end:
            raise ValueError("invalid sentence-to-view mapping")
        if body[start:end].decode() != sentence["text"]:
            raise ValueError("sentence text differs from exact view slice")
        previous = end
    complete = [event["data"] for event in events if event["type"] == "complete"]
    if len(complete) > 1 or (complete and events[-1]["type"] != "complete"):
        raise ValueError("invalid completion sequence")
    completion = complete[0] if complete else None
    if completion and "scope" in completion:
        raise ValueError("this consumer requires unscoped source sentence ordinals")
    if completion and (completion.get("inventory") or {}).get("documents", 1) != 1:
        raise ValueError("this consumer supports one source file only")
    failed = exit_code != 0 or completion is None
    partial = (not failed and (completion["coverage"] != "full" or completion["termination"] != "eof"
                              or "sample" in run or completion.get("state") != "known"))
    matches = [event["data"] for event in events if event["type"] == "match"]
    if not run["char_offsets"]:
        raise ValueError("offset publication was unavailable for this run")
    if completion and sum(match.get("char_span") is None for match in matches) != completion["counters"].get("offsets_omitted", 0):
        raise ValueError("offset omission count disagrees with records")
    if completion and completion["counters"]["records_emitted"] != len(matches):
        raise ValueError("match stream count disagrees with completion")
    identity = stable({"run": run, "complete": completion})
    features = {"lingrep": canonical(identity)}
    records = []
    unmappable = 0
    for index, match in enumerate(matches):
        if failed:
            break
        ordinal = match["corpus_sentence_ordinal"]
        if not isinstance(ordinal, int) or not 0 <= ordinal < len(sentences):
            raise ValueError("unknown sentence ordinal")
        sentence = sentences[ordinal]
        byte_positions = [0]
        for char in sentence["text"]:
            byte_positions.append(byte_positions[-1] + len(char.encode()))
        components = [("match", match)] + [(capture["name"], capture) for capture in match["captures"]]
        converted = []
        for component_index, (name, component) in enumerate(components):
            span = component.get("char_span")
            if span is None:
                break
            start, end = span["start"], span["end"]
            if not isinstance(start, int) or not isinstance(end, int) or not 0 <= start < end < len(byte_positions):
                raise ValueError("invalid code-point span")
            start, end = sentence["start"] + byte_positions[start], sentence["start"] + byte_positions[end]
            if not any(a <= start < end <= b for a, b in eligible):
                break
            converted.append({"id": f"m{index}_{component_index}", "start": start, "end": end,
                              "label": match["pattern"], "features": {
                                  **features, "component": name,
                                  "match": canonical(match),
                              }})
        else:
            records.extend(converted)
            continue
        unmappable += 1
    status = "failed" if failed else "partial" if partial or unmappable else "completed"
    inputs = {"corpus": digest(corpus), "catalog": digest(catalog),
              "producer-code": digest(Path(__file__).read_bytes()),
              "view-binding": digest(canonical(binding).encode()),
              "extraction": digest(canonical(stable(events)).encode()),
              "execution": digest(canonical({"exit_code": exit_code}).encode())}
    return {"schema": "soranoha-annotation-layer/1", "view": view_id,
            "vocabulary": digest(vocabulary),
            "producer": {"name": "lingrep-conllu", "inputs": inputs}, "dependencies": {},
            "eligible_spans": eligible, "status": status, "records": records}, unmappable


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("query", "binding", "corpus", "catalog", "vocabulary"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--exit-code", type=int, required=True)
    args = parser.parse_args()
    for name in ("query", "binding", "corpus", "catalog", "vocabulary"):
        if getattr(args, name).stat().st_size > 16 * 1024 * 1024:
            parser.error(f"{name} exceeds this example's 16 MiB input bound")
    layer, unmappable = convert(
        [json.loads(line) for line in args.query.read_text().splitlines()],
        json.loads(args.binding.read_text()), args.corpus.read_bytes(),
        args.catalog.read_bytes(), args.vocabulary.read_bytes(), args.exit_code,
    )
    print(canonical(layer))
    if unmappable:
        import sys
        print(f"{unmappable} records have unavailable or ineligible alignment", file=sys.stderr)


if __name__ == "__main__":
    main()
