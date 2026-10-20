import importlib.util
import json
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location("review", ROOT / "review.py")
REVIEW = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(REVIEW)


def write_catalog(path: Path) -> None:
    path.write_text(
        json.dumps(
            [
                {
                    "id": "second-no",
                    "title": "Find the second no",
                    "capability": "segmentation_alignment",
                    "phenomenon": "Label the second の as target.",
                    "labels": ["target"],
                    "sentences": ["のとの"],
                    "targets": [
                        {
                            "sentence_ordinal": 0,
                            "start_char": 2,
                            "end_char": 3,
                            "surface": "の",
                            "label": "target",
                        }
                    ],
                }
            ],
            ensure_ascii=False,
        )
    )


def annotation(
    packet: dict, packet_sha256: str, annotator_id: str, targets: list[dict]
) -> dict:
    return {
        "schema": REVIEW.ANNOTATION_SCHEMA,
        "source_catalog_sha256": packet["source_catalog_sha256"],
        "guide_sha256": packet["guide_sha256"],
        "packet_sha256": packet_sha256,
        "annotator_id": annotator_id,
        "cases": [
            {
                "id": "second-no",
                "status": "annotate",
                "targets": targets,
                "notes": "",
            }
        ],
    }


def test_prepare_review_packet_omits_authored_targets_and_binds_inputs(tmp_path):
    catalog = tmp_path / "catalog.json"
    guide = tmp_path / "guide.md"
    write_catalog(catalog)
    guide.write_text("Annotation guide\n")

    output = tmp_path / "review"
    REVIEW.prepare_review(catalog, guide, output)

    packet = json.loads((output / "packet.json").read_text())
    assert packet["source_catalog_sha256"] == REVIEW.sha256(catalog)
    assert packet["guide_sha256"] == REVIEW.sha256(guide)
    assert "targets" not in packet["cases"][0]
    template = json.loads((output / "annotation-template.json").read_text())
    assert template["packet_sha256"] == REVIEW.sha256(output / "packet.json")
    assert (output / "annotation-guide.md").read_bytes() == guide.read_bytes()


def test_prepare_review_validates_inputs_before_creating_destination(tmp_path):
    catalog = tmp_path / "catalog.json"
    write_catalog(catalog)
    output = tmp_path / "review"

    with pytest.raises(FileNotFoundError):
        REVIEW.prepare_review(catalog, tmp_path / "missing-guide.md", output)

    assert not output.exists()


def test_compare_annotations_ignores_target_order_and_flags_real_disagreement(tmp_path):
    packet = {
        "schema": REVIEW.PACKET_SCHEMA,
        "source_catalog_sha256": "catalog",
        "guide_sha256": "guide",
        "cases": [
            {
                "id": "second-no",
                "title": "Find the second no",
                "capability": "segmentation_alignment",
                "phenomenon": "Label の.",
                "labels": ["target", "different_label"],
                "sentences": ["のとの"],
            }
        ],
    }
    second_target = {
        "sentence_ordinal": 0,
        "start_char": 2,
        "end_char": 3,
        "surface": "の",
        "label": "target",
    }
    first_target = {
        "sentence_ordinal": 0,
        "start_char": 0,
        "end_char": 1,
        "surface": "の",
        "label": "target",
    }
    packet_path = tmp_path / "packet.json"
    first_path = tmp_path / "first.json"
    second_path = tmp_path / "second.json"
    packet_path.write_text(json.dumps(packet))
    packet_sha256 = REVIEW.sha256(packet_path)
    first_path.write_text(
        json.dumps(
            annotation(
                packet, packet_sha256, "reviewer-a", [second_target, first_target]
            )
        )
    )
    second_path.write_text(
        json.dumps(
            annotation(
                packet, packet_sha256, "reviewer-b", [first_target, second_target]
            )
        )
    )

    comparison = REVIEW.compare_annotations(packet_path, first_path, second_path)
    assert comparison["cases"][0]["outcome"] == "agree"

    changed = {**first_target, "label": "different_label"}
    second_path.write_text(
        json.dumps(
            annotation(packet, packet_sha256, "reviewer-b", [changed, second_target])
        )
    )
    comparison = REVIEW.compare_annotations(packet_path, first_path, second_path)
    assert comparison["cases"][0]["outcome"] == "adjudication_required"

    packet["cases"][0]["phenomenon"] = "tampered"
    packet_path.write_text(json.dumps(packet))
    with pytest.raises(ValueError, match="packet_sha256 does not match"):
        REVIEW.compare_annotations(packet_path, first_path, second_path)


def test_compare_annotations_requires_independent_reviewer_ids(tmp_path):
    packet = {
        "schema": REVIEW.PACKET_SCHEMA,
        "source_catalog_sha256": "catalog",
        "guide_sha256": "guide",
        "cases": [
            {
                "id": "second-no",
                "title": "Find the second no",
                "capability": "segmentation_alignment",
                "phenomenon": "Label の.",
                "labels": ["target"],
                "sentences": ["のとの"],
            }
        ],
    }
    packet_path = tmp_path / "packet.json"
    first_path = tmp_path / "first.json"
    second_path = tmp_path / "second.json"
    packet_path.write_text(json.dumps(packet))
    record = annotation(packet, REVIEW.sha256(packet_path), "same-reviewer", [])
    first_path.write_text(json.dumps(record))
    second_path.write_text(json.dumps(record))

    with pytest.raises(ValueError, match="distinct annotator IDs"):
        REVIEW.compare_annotations(packet_path, first_path, second_path)
