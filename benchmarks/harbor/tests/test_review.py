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

ORACLES = {
    "oracle_query": "target := [text=の].\n",
    "oracle_mecab": (
        "def select(tokens, document):\n"
        "    for index, token in enumerate(tokens):\n"
        "        if token.surface == \"の\":\n"
        "            yield index, index + 1, \"target\"\n"
    ),
}


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
                    "documents": [{"eval_document_id": "doc-001", "sentences": ["のとの"]}],
                    **ORACLES,
                    "targets": [
                        {
                            "eval_document_id": "doc-001", "sentence_ordinal": 0,
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
                "documents": [{"eval_document_id": "doc-001", "sentences": ["のとの"]}],
            }
        ],
    }
    second_target = {
        "eval_document_id": "doc-001", "sentence_ordinal": 0,
        "start_char": 2,
        "end_char": 3,
        "surface": "の",
        "label": "target",
    }
    first_target = {
        "eval_document_id": "doc-001", "sentence_ordinal": 0,
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
                "documents": [{"eval_document_id": "doc-001", "sentences": ["のとの"]}],
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


DRIVER_CASE = {
    "id": "final-no",
    "title": "Sentence-final no",
    "capability": "query_authoring",
    "phenomenon": "Label sentence-final の as target.",
    "labels": ["target"],
    "documents": [{"eval_document_id": "doc-001", "sentences": ["行くの"]}],
    **ORACLES,
    "targets": [
        {"eval_document_id": "doc-001", "sentence_ordinal": 0, "start_char": 2, "end_char": 3, "surface": "の", "label": "target"}
    ],
    "driver": {
        "documents": [{"eval_document_id": "hidden-001", "sentences": ["来るの"]}],
        "targets": [
            {"eval_document_id": "hidden-001", "sentence_ordinal": 0, "start_char": 2, "end_char": 3, "surface": "の", "label": "target"}
        ],
    },
}


def test_review_packet_carries_driver_sentences_for_blind_annotation(tmp_path):
    catalog = tmp_path / "catalog.json"
    catalog.write_text(json.dumps([DRIVER_CASE], ensure_ascii=False))
    guide = tmp_path / "guide.md"
    guide.write_text("guide\n")
    REVIEW.prepare_review(catalog, guide, tmp_path / "review")

    packet = json.loads((tmp_path / "review" / "packet.json").read_text())
    assert packet["cases"][0]["driver"] == {
        "documents": [{"eval_document_id": "hidden-001", "sentences": ["来るの"]}]
    }
    assert "targets" not in packet["cases"][0]["driver"]
    template = json.loads((tmp_path / "review" / "annotation-template.json").read_text())
    assert template["cases"][0]["driver_targets"] == []


def lineage(tmp_path, *, disagree_on_driver=False):
    tmp_path.mkdir(parents=True, exist_ok=True)
    plain = json.loads(json.dumps(DRIVER_CASE, ensure_ascii=False))
    del plain["driver"]
    plain["id"] = "plain-no"
    candidate = tmp_path / "candidate.json"
    candidate.write_text(json.dumps([plain, DRIVER_CASE], ensure_ascii=False))
    guide = tmp_path / "guide.md"
    guide.write_text("guide\n")
    review = tmp_path / "review"
    REVIEW.prepare_review(candidate, guide, review)
    packet = json.loads((review / "packet.json").read_text())
    digest = REVIEW.sha256(review / "packet.json")

    def record(annotator, driver_targets):
        return {
            "schema": REVIEW.ANNOTATION_SCHEMA,
            "source_catalog_sha256": packet["source_catalog_sha256"],
            "guide_sha256": packet["guide_sha256"],
            "packet_sha256": digest,
            "annotator_id": annotator,
            "cases": [
                {"id": "plain-no", "status": "annotate", "targets": plain["targets"], "notes": ""},
                {
                    "id": "final-no",
                    "status": "annotate",
                    "targets": DRIVER_CASE["targets"],
                    "driver_targets": driver_targets,
                    "notes": "",
                },
            ],
        }

    (review / "annotation-a.json").write_text(
        json.dumps(record("reviewer-a", DRIVER_CASE["driver"]["targets"]), ensure_ascii=False)
    )
    (review / "annotation-b.json").write_text(
        json.dumps(
            record("reviewer-b", [] if disagree_on_driver else DRIVER_CASE["driver"]["targets"]),
            ensure_ascii=False,
        )
    )
    comparison = REVIEW.compare_annotations(
        review / "packet.json", review / "annotation-a.json", review / "annotation-b.json"
    )
    (review / "comparison.json").write_text(json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    if disagree_on_driver:
        (review / "adjudication.json").write_text(
            json.dumps(
                {
                    "schema": REVIEW.ADJUDICATION_SCHEMA,
                    "packet_sha256": digest,
                    "comparison_sha256": REVIEW.sha256(review / "comparison.json"),
                    "adjudicator_id": "adjudicator",
                    "cases": [
                        {
                            "id": "final-no",
                            "status": "retain",
                            "targets": DRIVER_CASE["targets"],
                            "driver_targets": DRIVER_CASE["driver"]["targets"],
                            "rationale": "reviewer-b missed the driver target",
                        }
                    ],
                },
                ensure_ascii=False,
            )
        )
    final = tmp_path / "final.json"
    final.write_text(json.dumps([plain, DRIVER_CASE], ensure_ascii=False))
    return review, final


def test_validate_review_accepts_a_complete_lineage_and_reports_retained_cases(tmp_path):
    review, final = lineage(tmp_path)
    resolution = REVIEW.validate_review(review, final)
    assert sorted(resolution) == ["final-no", "plain-no"]
    assert resolution["final-no"]["driver_targets"] == DRIVER_CASE["driver"]["targets"]

    adjudicated, final = lineage(tmp_path / "adjudicated", disagree_on_driver=True)
    comparison = json.loads((adjudicated / "comparison.json").read_text())
    assert [case["outcome"] for case in comparison["cases"]] == ["agree", "adjudication_required"]
    assert REVIEW.validate_review(adjudicated, final)["final-no"]["status"] == "retain"


def test_validate_review_rejects_wrong_catalog_missing_hidden_review_and_unresolved_adjudication(
    tmp_path,
):
    review, final = lineage(tmp_path / "wrong-catalog")
    cases = json.loads(final.read_text())
    cases[1]["driver"]["targets"] = []
    final.write_text(json.dumps(cases, ensure_ascii=False))
    with pytest.raises(ValueError, match="driver targets for final-no are not the reviewed targets"):
        REVIEW.validate_review(review, final)

    review, final = lineage(tmp_path / "missing-hidden")
    record = json.loads((review / "annotation-a.json").read_text())
    del record["cases"][1]["driver_targets"]
    (review / "annotation-a.json").write_text(json.dumps(record, ensure_ascii=False))
    with pytest.raises(ValueError, match="invalid annotation case fields for final-no"):
        REVIEW.validate_review(review, final)

    review, final = lineage(tmp_path / "unresolved", disagree_on_driver=True)
    (review / "adjudication.json").unlink()
    with pytest.raises(ValueError, match="unresolved adjudication for cases \\['final-no'\\]"):
        REVIEW.validate_review(review, final)

    review, final = lineage(tmp_path / "edited-comparison")
    comparison = json.loads((review / "comparison.json").read_text())
    comparison["cases"][0]["outcome"] = "excluded_by_both"
    (review / "comparison.json").write_text(json.dumps(comparison))
    with pytest.raises(ValueError, match="comparison.json is not the comparison"):
        REVIEW.validate_review(review, final)
