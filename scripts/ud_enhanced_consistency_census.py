"""Census basic-to-enhanced endpoint preservation in explicit CoNLL-U files."""

from __future__ import annotations

import argparse
import json
from collections import Counter
from collections.abc import Iterator
from pathlib import Path
from typing import TextIO


def relation_base(relation: str) -> str:
    return relation.split(":", 1)[0]


def parse_deps(value: str) -> list[tuple[str, str]]:
    if value == "_":
        return []
    edges = []
    for member in value.split("|"):
        head, relation = member.split(":", 1)
        edges.append((head, relation))
    return edges


def sentence_rows(stream: TextIO) -> Iterator[list[list[str]]]:
    rows: list[list[str]] = []
    for raw_line in stream:
        line = raw_line.rstrip("\r\n")
        if not line:
            if rows:
                yield rows
                rows = []
            continue
        if line.startswith("#"):
            continue
        columns = line.split("\t")
        if len(columns) != 10:
            raise ValueError("expected exactly 10 tab-separated CoNLL-U columns")
        if "-" not in columns[0]:
            rows.append(columns)
    if rows:
        yield rows


def census(paths: list[Path]) -> dict[str, object]:
    observation_states: Counter[str] = Counter()
    missing_by_relation: Counter[str] = Counter()
    label_changes: Counter[str] = Counter()
    totals: Counter[str] = Counter()

    for path in paths:
        with path.open(encoding="utf-8") as stream:
            for rows in sentence_rows(stream):
                totals["sentences"] += 1
                words = [row for row in rows if row[0].isdigit()]
                present = [row[8] != "_" for row in words]
                if not any(present):
                    observation_states["unavailable"] += 1
                    continue
                if not all(present):
                    observation_states["incomplete"] += 1
                    continue
                observation_states["complete"] += 1

                incoming = {
                    row[0]: parse_deps(row[8])
                    for row in rows
                    if "-" not in row[0]
                }
                for row in words:
                    node_id, head, relation, deps = row[0], row[6], row[7], row[8]
                    if head == "0":
                        totals["root_rows"] += 1
                        if ("0", "root") not in parse_deps(deps):
                            totals["root_rows_missing_enhanced_root"] += 1
                        continue
                    if not head.isdigit() or relation == "_":
                        continue
                    totals["nonroot_basic_edges"] += 1
                    endpoint_edges = [
                        enhanced_relation
                        for enhanced_head, enhanced_relation in incoming[node_id]
                        if enhanced_head == head
                    ]
                    if endpoint_edges:
                        totals["same_endpoint_present"] += 1
                        if relation not in endpoint_edges:
                            totals["same_endpoint_exact_label_changed"] += 1
                            base_preserved = any(
                                relation_base(candidate) == relation_base(relation)
                                for candidate in endpoint_edges
                            )
                            label_changes[
                                "base_preserved" if base_preserved else "base_changed"
                            ] += 1
                        continue

                    totals["same_endpoint_absent"] += 1
                    missing_by_relation[relation] += 1
                    edges = incoming[node_id]
                    if any(relation_base(edge_relation) == "ref" for _, edge_relation in edges):
                        totals["relative_reference_replacement"] += 1
                    elif any("." in enhanced_head for enhanced_head, _ in edges):
                        totals["empty_node_reconstruction"] += 1
                    else:
                        totals["other_endpoint_absence"] += 1

    return {
        "files": [str(path) for path in paths],
        "observation_states": dict(sorted(observation_states.items())),
        "totals": dict(sorted(totals.items())),
        "missing_by_basic_relation": dict(sorted(missing_by_relation.items())),
        "label_change_classification": dict(sorted(label_changes.items())),
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="census basic-to-enhanced endpoint preservation"
    )
    parser.add_argument("conllu", nargs="+", type=Path)
    args = parser.parse_args()
    print(json.dumps(census(args.conllu), ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
