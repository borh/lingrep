"""Independent structural census for the enhanced-propagation result.

The script reads only CoNLL-U DEPS. Pass the three release split files
explicitly; do not point it at a checkout containing historical subtrees.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path

ARGUMENT_BASES = {"nsubj", "csubj", "obj", "iobj", "obl"}


def relation_base(relation: str) -> str:
    return relation.split(":", 1)[0]


def sentences(paths: list[Path]):
    rows = []
    for path in paths:
        with path.open(encoding="utf-8") as source:
            for line in source:
                line = line.rstrip("\n")
                if not line:
                    if rows:
                        yield rows
                        rows = []
                    continue
                if line.startswith("#"):
                    continue
                fields = line.split("\t")
                if len(fields) == 10 and "-" not in fields[0]:
                    rows.append((fields[0], fields[8]))
        if rows:
            yield rows
            rows = []


def census(paths: list[Path]) -> dict[str, int]:
    coordination_pairs = 0
    pairs_with_shared_argument = 0
    pairs_with_shared_and_any_private_argument = 0
    pairs_with_shared_and_first_private_argument = 0
    first_private_argument_assignments = 0
    maximum_empty_chain_depth = 0
    anchors_at_maximum_depth = 0

    for rows in sentences(paths):
        outgoing: dict[str, set[tuple[str, str]]] = defaultdict(set)
        node_ids = {node_id for node_id, _ in rows}
        for dependent, deps in rows:
            if deps == "_":
                continue
            for edge in deps.split("|"):
                head, relation = edge.split(":", 1)
                outgoing[head].add((dependent, relation))

        pairs = {
            (head, dependent)
            for head, edges in outgoing.items()
            for dependent, relation in edges
            if relation_base(relation) == "conj"
            and head in node_ids
            and dependent in node_ids
        }
        coordination_pairs += len(pairs)
        for first, second in pairs:
            first_arguments = {
                dependent
                for dependent, relation in outgoing[first]
                if relation_base(relation) in ARGUMENT_BASES
            }
            second_arguments = {
                dependent
                for dependent, relation in outgoing[second]
                if relation_base(relation) in ARGUMENT_BASES
            }
            if not first_arguments.intersection(second_arguments):
                continue
            pairs_with_shared_argument += 1
            first_private = first_arguments.difference(second_arguments)
            any_private = first_arguments.symmetric_difference(second_arguments)
            first_private_argument_assignments += len(first_private)
            if first_private:
                pairs_with_shared_and_first_private_argument += 1
            if any_private:
                pairs_with_shared_and_any_private_argument += 1

        def empty_depth(
            node: str,
            seen: frozenset[str],
            edges: dict[str, set[tuple[str, str]]] = outgoing,
        ) -> int:
            children = [
                dependent
                for dependent, _ in edges[node]
                if "." in dependent and dependent not in seen
            ]
            return max(
                (1 + empty_depth(child, seen | {child}) for child in children),
                default=0,
            )

        sentence_depths = [
            empty_depth(node_id, frozenset())
            for node_id in node_ids
            if "." not in node_id
        ]
        sentence_maximum = max(sentence_depths, default=0)
        if sentence_maximum > maximum_empty_chain_depth:
            maximum_empty_chain_depth = sentence_maximum
            anchors_at_maximum_depth = sentence_depths.count(sentence_maximum)
        elif sentence_maximum == maximum_empty_chain_depth and sentence_maximum > 0:
            anchors_at_maximum_depth += sentence_depths.count(sentence_maximum)

    return {
        "coordination_pairs": coordination_pairs,
        "pairs_with_shared_argument": pairs_with_shared_argument,
        "pairs_with_shared_and_any_private_argument": (
            pairs_with_shared_and_any_private_argument
        ),
        "pairs_with_shared_and_first_private_argument": (
            pairs_with_shared_and_first_private_argument
        ),
        "first_private_argument_assignments": first_private_argument_assignments,
        "maximum_empty_chain_depth": maximum_empty_chain_depth,
        "anchors_at_maximum_depth": anchors_at_maximum_depth,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("conllu", nargs="+", type=Path)
    args = parser.parse_args()
    missing = [str(path) for path in args.conllu if not path.is_file()]
    if missing:
        parser.error(f"not regular files: {', '.join(missing)}")
    print(json.dumps(census(args.conllu), sort_keys=True))


if __name__ == "__main__":
    main()
