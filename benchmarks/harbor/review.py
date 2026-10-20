#!/usr/bin/env python3
"""Prepare and compare blind linguistic annotation records."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path

from generate_dataset import load_catalog, validate_catalog

PACKET_SCHEMA = "lingrep-linguistic-eval.review-packet/v1"
ANNOTATION_SCHEMA = "lingrep-linguistic-eval.annotation/v1"
COMPARISON_SCHEMA = "lingrep-linguistic-eval.annotation-comparison/v1"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path: Path, value: object) -> None:
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )


def prepare_review(catalog_path: Path, guide_path: Path, destination: Path) -> None:
    cases = load_catalog(catalog_path)
    identity = {
        "source_catalog_sha256": sha256(catalog_path),
        "guide_sha256": sha256(guide_path),
    }
    if destination.exists():
        raise FileExistsError(f"refusing to overwrite review packet: {destination}")
    destination.mkdir(parents=True)

    packet_cases = [
        {
            "id": case["id"],
            "title": case["title"],
            "capability": case["capability"],
            "phenomenon": case["phenomenon"],
            "labels": case["labels"],
            "sentences": case["sentences"],
        }
        for case in cases
    ]
    packet = {"schema": PACKET_SCHEMA, **identity, "cases": packet_cases}
    packet_path = destination / "packet.json"
    write_json(packet_path, packet)
    template = {
        "schema": ANNOTATION_SCHEMA,
        **identity,
        "packet_sha256": sha256(packet_path),
        "annotator_id": "REPLACE_WITH_OPAQUE_ID",
        "cases": [
            {
                "id": case["id"],
                "status": "REPLACE_WITH_annotate_OR_exclude",
                "targets": [],
                "notes": "",
            }
            for case in cases
        ],
    }
    write_json(destination / "annotation-template.json", template)
    shutil.copy2(guide_path, destination / "annotation-guide.md")


def load_packet(path: Path) -> dict:
    packet = json.loads(path.read_text())
    if packet.get("schema") != PACKET_SCHEMA or not isinstance(
        packet.get("cases"), list
    ):
        raise ValueError(f"invalid review packet: {path}")
    return packet


def canonical_targets(case: dict, targets: object) -> list[dict]:
    validated = validate_catalog([{**case, "targets": targets}])[0]
    canonical = list(validated["targets"])
    if any(not target["label"] for target in canonical):
        raise ValueError(f"target labels must not be empty for {case['id']}")
    canonical.sort(
        key=lambda target: (
            target["sentence_ordinal"],
            target["start_char"],
            target["end_char"],
            target["label"],
        )
    )
    identities = [
        (
            target["sentence_ordinal"],
            target["start_char"],
            target["end_char"],
            target["label"],
        )
        for target in canonical
    ]
    if len(identities) != len(set(identities)):
        raise ValueError(f"targets must be unique for {case['id']}")
    return canonical


def load_annotation(path: Path, packet: dict, packet_sha256: str) -> dict:
    annotation = json.loads(path.read_text())
    expected_fields = {
        "schema",
        "source_catalog_sha256",
        "guide_sha256",
        "packet_sha256",
        "annotator_id",
        "cases",
    }
    if set(annotation) != expected_fields or annotation["schema"] != ANNOTATION_SCHEMA:
        raise ValueError(f"invalid annotation record: {path}")
    for field in ("source_catalog_sha256", "guide_sha256"):
        if annotation[field] != packet[field]:
            raise ValueError(f"annotation {field} does not match packet: {path}")
    if annotation["packet_sha256"] != packet_sha256:
        raise ValueError(f"annotation packet_sha256 does not match packet: {path}")
    annotator_id = annotation["annotator_id"]
    if (
        not isinstance(annotator_id, str)
        or not annotator_id.strip()
        or annotator_id.startswith("REPLACE_")
    ):
        raise ValueError(f"annotation requires an opaque annotator_id: {path}")
    if not isinstance(annotation["cases"], list):
        raise TypeError(f"annotation cases must be a list: {path}")

    packet_cases = {case["id"]: case for case in packet["cases"]}
    reviewed: dict[str, dict] = {}
    for review in annotation["cases"]:
        if not isinstance(review, dict) or set(review) != {
            "id",
            "status",
            "targets",
            "notes",
        }:
            raise ValueError(f"invalid annotation case fields: {path}")
        case_id = review["id"]
        if case_id not in packet_cases or case_id in reviewed:
            raise ValueError(
                f"unknown or duplicate annotation case {case_id!r}: {path}"
            )
        if review["status"] not in {"annotate", "exclude"}:
            raise ValueError(f"invalid annotation status for {case_id}: {path}")
        if not isinstance(review["notes"], str):
            raise TypeError(f"annotation notes must be a string for {case_id}: {path}")
        targets = canonical_targets(packet_cases[case_id], review["targets"])
        if review["status"] == "exclude":
            if targets:
                raise ValueError(f"excluded case must not contain targets: {case_id}")
            if not review["notes"].strip():
                raise ValueError(f"excluded case requires a reason: {case_id}")
        reviewed[case_id] = {**review, "targets": targets}
    if set(reviewed) != set(packet_cases):
        missing = sorted(set(packet_cases) - set(reviewed))
        raise ValueError(f"annotation is missing cases {missing}: {path}")
    return {**annotation, "cases": reviewed}


def compare_annotations(packet_path: Path, first_path: Path, second_path: Path) -> dict:
    packet = load_packet(packet_path)
    packet_digest = sha256(packet_path)
    first = load_annotation(first_path, packet, packet_digest)
    second = load_annotation(second_path, packet, packet_digest)
    if first["annotator_id"] == second["annotator_id"]:
        raise ValueError("independent annotations require distinct annotator IDs")

    comparisons = []
    for case in packet["cases"]:
        case_id = case["id"]
        left = first["cases"][case_id]
        right = second["cases"][case_id]
        if left["status"] == right["status"] == "exclude":
            outcome = "excluded_by_both"
        elif (
            left["status"] == right["status"] == "annotate"
            and left["targets"] == right["targets"]
        ):
            outcome = "agree"
        else:
            outcome = "adjudication_required"
        comparisons.append(
            {
                "id": case_id,
                "outcome": outcome,
                "first": left,
                "second": right,
            }
        )
    return {
        "schema": COMPARISON_SCHEMA,
        "source_catalog_sha256": packet["source_catalog_sha256"],
        "guide_sha256": packet["guide_sha256"],
        "packet_sha256": packet_digest,
        "annotators": [first["annotator_id"], second["annotator_id"]],
        "cases": comparisons,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    prepare = subparsers.add_parser("prepare")
    prepare.add_argument("catalog", type=Path)
    prepare.add_argument("output", type=Path)
    prepare.add_argument(
        "--guide",
        type=Path,
        default=Path(__file__).with_name("guides") / "annotation-guide.md",
    )
    compare = subparsers.add_parser("compare")
    compare.add_argument("packet", type=Path)
    compare.add_argument("first", type=Path)
    compare.add_argument("second", type=Path)
    args = parser.parse_args()

    if args.command == "prepare":
        prepare_review(
            args.catalog.resolve(), args.guide.resolve(), args.output.resolve()
        )
        print(f"wrote blind review packet to {args.output}")
    else:
        comparison = compare_annotations(
            args.packet.resolve(), args.first.resolve(), args.second.resolve()
        )
        print(json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
