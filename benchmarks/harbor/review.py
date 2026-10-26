#!/usr/bin/env python3
"""Prepare and compare blind linguistic annotation records."""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

from experiment import sha256_file as sha256
from generate_dataset import load_catalog, validate_targets

PACKET_SCHEMA = "lingrep-linguistic-eval.review-packet/v1"
ANNOTATION_SCHEMA = "lingrep-linguistic-eval.annotation/v1"
COMPARISON_SCHEMA = "lingrep-linguistic-eval.annotation-comparison/v1"
ADJUDICATION_SCHEMA = "lingrep-linguistic-eval.adjudication/v1"
CASE_TEXT_FIELDS = ("title", "capability", "phenomenon", "labels", "documents")


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
        raise FileExistsError(f"review packet already exists: {destination}")
    destination.mkdir(parents=True)

    packet_cases = []
    for case in cases:
        packet_case = {"id": case["id"], **{field: case[field] for field in CASE_TEXT_FIELDS}}
        if case.get("driver") is not None:
            packet_case["driver"] = {"documents": case["driver"]["documents"]}
        packet_cases.append(packet_case)
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
                **({"driver_targets": []} if case.get("driver") is not None else {}),
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


def canonical_targets(case: dict, targets: object, *, documents: list[dict] | None = None) -> list[dict]:
    if not isinstance(targets, list):
        raise TypeError(f"targets must be a list for {case['id']}")
    validate_targets(
        case["id"],
        case["labels"],
        case["documents"] if documents is None else documents,
        targets,
    )
    canonical = list(targets)
    if any(not target["label"] for target in canonical):
        raise ValueError(f"target labels must not be empty for {case['id']}")
    canonical.sort(
        key=lambda target: (
            target["eval_document_id"],
            target["sentence_ordinal"],
            target["start_char"],
            target["end_char"],
            target["label"],
        )
    )
    identities = [
        (
            target["eval_document_id"],
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
        case_id = review.get("id") if isinstance(review, dict) else None
        packet_case = packet_cases.get(case_id)
        if packet_case is None or case_id in reviewed:
            raise ValueError(
                f"unknown or duplicate annotation case {case_id!r}: {path}"
            )
        if set(review) != case_fields(packet_case):
            raise ValueError(f"invalid annotation case fields for {case_id}: {path}")
        if review["status"] not in {"annotate", "exclude"}:
            raise ValueError(f"invalid annotation status for {case_id}: {path}")
        if not isinstance(review["notes"], str):
            raise TypeError(f"annotation notes must be a string for {case_id}: {path}")
        resolved = resolve_targets(packet_case, review)
        if review["status"] == "exclude":
            if resolved["targets"] or resolved.get("driver_targets"):
                raise ValueError(f"excluded case must not contain targets: {case_id}")
            if not review["notes"].strip():
                raise ValueError(f"excluded case requires a reason: {case_id}")
        reviewed[case_id] = {**review, **resolved}
    if set(reviewed) != set(packet_cases):
        missing = sorted(set(packet_cases) - set(reviewed))
        raise ValueError(f"annotation is missing cases {missing}: {path}")
    return {**annotation, "cases": reviewed}


def case_fields(packet_case: dict) -> set[str]:
    fields = {"id", "status", "targets", "notes"}
    if "driver" in packet_case:
        fields.add("driver_targets")
    return fields


def resolve_targets(packet_case: dict, review: dict) -> dict:
    resolved = {"targets": canonical_targets(packet_case, review["targets"])}
    if "driver" in packet_case:
        resolved["driver_targets"] = canonical_targets(
            packet_case,
            review["driver_targets"],
            documents=packet_case["driver"]["documents"],
        )
    return resolved


def same_targets(left: dict, right: dict) -> bool:
    return left["targets"] == right["targets"] and left.get("driver_targets") == right.get(
        "driver_targets"
    )


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
        elif left["status"] == right["status"] == "annotate" and same_targets(
            left, right
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


def load_adjudication(path: Path, packet: dict, packet_sha256: str, comparison_sha256: str) -> dict:
    adjudication = json.loads(path.read_text())
    expected_fields = {"schema", "packet_sha256", "comparison_sha256", "adjudicator_id", "cases"}
    if set(adjudication) != expected_fields or adjudication["schema"] != ADJUDICATION_SCHEMA:
        raise ValueError(f"invalid adjudication record: {path}")
    if adjudication["packet_sha256"] != packet_sha256:
        raise ValueError(f"adjudication packet_sha256 does not match packet: {path}")
    if adjudication["comparison_sha256"] != comparison_sha256:
        raise ValueError(f"adjudication comparison_sha256 does not match comparison: {path}")
    adjudicator = adjudication["adjudicator_id"]
    if not isinstance(adjudicator, str) or not adjudicator.strip() or adjudicator.startswith("REPLACE_"):
        raise ValueError(f"adjudication requires an opaque adjudicator_id: {path}")
    packet_cases = {case["id"]: case for case in packet["cases"]}
    decided: dict[str, dict] = {}
    for decision in adjudication["cases"]:
        case_id = decision.get("id") if isinstance(decision, dict) else None
        packet_case = packet_cases.get(case_id)
        if packet_case is None or case_id in decided:
            raise ValueError(f"unknown or duplicate adjudication case {case_id!r}: {path}")
        expected = (case_fields(packet_case) - {"status", "notes"}) | {"status", "rationale"}
        if set(decision) != expected:
            raise ValueError(f"invalid adjudication case fields for {case_id}: {path}")
        if decision["status"] not in {"retain", "exclude"}:
            raise ValueError(f"invalid adjudication status for {case_id}: {path}")
        if not isinstance(decision["rationale"], str) or not decision["rationale"].strip():
            raise ValueError(f"adjudication requires a rationale for {case_id}: {path}")
        resolved = resolve_targets(packet_case, decision)
        if decision["status"] == "exclude" and (
            resolved["targets"] or resolved.get("driver_targets")
        ):
            raise ValueError(f"excluded case must not contain targets: {case_id}")
        decided[case_id] = {**decision, **resolved}
    return {**adjudication, "cases": decided}


def validate_review(review: Path, final_catalog: Path) -> dict:
    """Check `final_catalog` against `review`: `packet.json`, two `annotation-*.json`
    records, `comparison.json`, and `adjudication.json` when any case required it."""
    packet_path = review / "packet.json"
    packet = load_packet(packet_path)
    annotations = sorted(
        path
        for path in review.glob("annotation-*.json")
        if path.name != "annotation-template.json"
    )
    if len(annotations) != 2:
        raise ValueError(
            f"reviewed mode requires exactly two annotation records, found {len(annotations)}: {review}"
        )
    comparison_path = review / "comparison.json"
    expected_comparison = compare_annotations(packet_path, *annotations)
    if json.loads(comparison_path.read_text()) != expected_comparison:
        raise ValueError(f"comparison.json is not the comparison of the two annotation records: {review}")
    packet_digest = sha256(packet_path)
    first = load_annotation(annotations[0], packet, packet_digest)

    disputed = {case["id"] for case in expected_comparison["cases"] if case["outcome"] == "adjudication_required"}
    adjudication_path = review / "adjudication.json"
    decided: dict[str, dict] = {}
    if adjudication_path.exists():
        decided = load_adjudication(
            adjudication_path, packet, packet_digest, sha256(comparison_path)
        )["cases"]
    unresolved = sorted(disputed - set(decided))
    if unresolved:
        raise ValueError(f"unresolved adjudication for cases {unresolved}: {review}")
    undisputed = sorted(set(decided) - disputed)
    if undisputed:
        raise ValueError(f"adjudication overrides agreed cases {undisputed}: {review}")

    resolution: dict[str, dict | None] = {}
    for case in packet["cases"]:
        case_id = case["id"]
        if case_id in decided:
            decision = decided[case_id]
            resolution[case_id] = None if decision["status"] == "exclude" else decision
        elif first["cases"][case_id]["status"] == "exclude":
            resolution[case_id] = None
        else:
            resolution[case_id] = first["cases"][case_id]

    final = {case["id"]: case for case in load_catalog(final_catalog)}
    expected_ids = {case_id for case_id, resolved in resolution.items() if resolved is not None}
    if set(final) != expected_ids:
        raise ValueError(
            f"final catalog cases {sorted(final)} do not equal the reviewed cases {sorted(expected_ids)}"
        )
    packet_cases = {case["id"]: case for case in packet["cases"]}
    for case_id in expected_ids:
        case = final[case_id]
        packet_case = packet_cases[case_id]
        resolved = resolution[case_id]
        for field in CASE_TEXT_FIELDS:
            if case[field] != packet_case[field]:
                raise ValueError(f"final catalog changed {field} of reviewed case {case_id}")
        if canonical_targets(packet_case, case["targets"]) != resolved["targets"]:
            raise ValueError(f"final catalog targets for {case_id} are not the reviewed targets")
        if "driver" in packet_case:
            driver = case.get("driver")
            if driver is None or driver["documents"] != packet_case["driver"]["documents"]:
                raise ValueError(f"final catalog changed the driver documents of {case_id}")
            hidden = canonical_targets(
                packet_case, driver["targets"], documents=driver["documents"]
            )
            if hidden != resolved["driver_targets"]:
                raise ValueError(f"final catalog driver targets for {case_id} are not the reviewed targets")
        elif case.get("driver") is not None:
            raise ValueError(f"final catalog adds an unreviewed driver to {case_id}")
    return resolution


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
    validate = subparsers.add_parser("validate")
    validate.add_argument("review", type=Path)
    validate.add_argument("catalog", type=Path)
    args = parser.parse_args()

    if args.command == "prepare":
        prepare_review(
            args.catalog.resolve(), args.guide.resolve(), args.output.resolve()
        )
        print(f"wrote blind review packet to {args.output}")
    elif args.command == "validate":
        resolution = validate_review(args.review.resolve(), args.catalog.resolve())
        retained = sum(resolved is not None for resolved in resolution.values())
        print(f"reviewed catalog verified: {retained} retained of {len(resolution)} cases")
    else:
        comparison = compare_annotations(
            args.packet.resolve(), args.first.resolve(), args.second.resolve()
        )
        print(json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
