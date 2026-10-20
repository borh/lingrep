import importlib.util
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "harbor_verify", ROOT / "verifier" / "verify.py"
)
VERIFY = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(VERIFY)


def event(kind, data):
    return {"schema": "lingrep.query/v2", "type": kind, "data": data}


def match_event(
    label: str,
    start_char: int,
    end_char: int,
    *,
    document_id: str | None = "doc-001",
    sentence_ordinal: int = 0,
    char_span: dict | None = None,
):
    data = {
        "sequence": 0,
        "pattern": label,
        "span": {"start": 0, "end": 1},
        "document_ordinal": 0,
        "document_sentence_ordinal": sentence_ordinal,
        "source_name": "input.jsonl",
        "surface": "猫",
        "tokens": [],
        "left_context": None,
        "right_context": None,
        "captures": [],
        "char_span": char_span
        if char_span is not None
        else {"start": start_char, "end": end_char},
    }
    if document_id is not None:
        data["source_metadata"] = {"document": {"eval_document_id": document_id}}
    return event("match", data)


def test_project_matches_reads_the_published_character_span():
    matches = [match_event("subject", 2, 3)]

    assert VERIFY.project_matches(matches) == [
        {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": "doc-001",
            "sentence_ordinal": 0,
            "start_char": 2,
            "end_char": 3,
            "label": "subject",
        }
    ]


def test_project_matches_exposes_context_consumed_by_the_query():
    matches = [match_event("subject", 0, 2)]

    assert VERIFY.project_matches(matches)[0]["end_char"] == 2


def test_project_matches_reads_the_document_identity_from_the_record():
    matches = [match_event("subject", 0, 1, document_id="hidden-001")]

    assert VERIFY.project_matches(matches)[0]["eval_document_id"] == "hidden-001"


def test_project_matches_rejects_a_run_that_withdrew_offsets():
    match = match_event("subject", 0, 1)
    del match["data"]["char_span"]

    with pytest.raises(TypeError, match="char_span"):
        VERIFY.project_matches([match])

    with pytest.raises(ValueError, match="invalid character span"):
        VERIFY.project_matches([match_event("subject", 0, 1, char_span={})])


def test_project_matches_rejects_a_record_without_projected_metadata():
    matches = [match_event("subject", 0, 1, document_id=None)]

    with pytest.raises(ValueError, match="eval_document_id"):
        VERIFY.project_matches(matches)


def test_published_names_carry_output_name_and_ignore_lexicon_bodies():
    definitions = [
        event("definition", {"name": "internal", "body": {"kind": "pattern"}}),
        event(
            "definition",
            {
                "name": "authored",
                "body": {"kind": "pattern"},
                "metadata": {"output_name": "n-subj"},
            },
        ),
    ]

    VERIFY.validate_definition_labels(definitions, ["n-subj", "internal"])
    with pytest.raises(ValueError, match="definition labels"):
        VERIFY.validate_definition_labels(definitions, ["internal"])


def test_definition_labels_must_equal_the_declared_vocabulary():
    definitions = [event("definition", {"name": "first"})]

    VERIFY.validate_definition_labels(definitions, ["first"])
    with pytest.raises(ValueError, match="definition labels"):
        VERIFY.validate_definition_labels(definitions, ["first", "second"])


def test_score_rows_keeps_visible_and_hidden_replay_separate():
    row = {
        "schema": "lingrep-linguistic-eval.evidence/v1",
        "eval_document_id": "doc-001",
        "sentence_ordinal": 0,
        "start_char": 0,
        "end_char": 1,
        "label": "target",
    }

    assert VERIFY.score_rows([row], [row]) == {
        "f1": 1.0,
        "exact": 1.0,
        "precision": 1.0,
        "recall": 1.0,
    }
    assert VERIFY.score_rows([], [row])["f1"] == 0.0


def test_character_spans_must_address_a_known_source_sentence():
    documents = [{"eval_document_id": "doc-001", "sentences": ["猫がいる。"]}]
    row = {
        "schema": "lingrep-linguistic-eval.evidence/v1",
        "eval_document_id": "doc-001",
        "sentence_ordinal": 0,
        "start_char": 0,
        "end_char": 6,
        "label": "target",
    }

    with pytest.raises(ValueError, match="outside its source sentence"):
        VERIFY.validate_rows_against_documents([row], documents)

    with pytest.raises(ValueError, match="unknown source sentence"):
        VERIFY.validate_rows_against_documents(
            [{**row, "eval_document_id": "missing", "end_char": 1}], documents
        )


def test_submitted_runner_must_be_a_regular_executable(tmp_path, monkeypatch):
    monkeypatch.setattr(VERIFY, "SUBMISSION", tmp_path)
    runner = tmp_path / "run-analysis"
    runner.write_text("#!/bin/bash\n")

    with pytest.raises(ValueError, match="executable"):
        VERIFY.submitted_runner()

    runner.chmod(0o755)
    monkeypatch.setattr(VERIFY, "MAX_RUNNER_BYTES", 1)
    with pytest.raises(ValueError, match="size limit"):
        VERIFY.submitted_runner()
    monkeypatch.setattr(VERIFY, "MAX_RUNNER_BYTES", 1024 * 1024)
    assert VERIFY.submitted_runner() == runner


def test_replay_sandbox_exposes_only_submission_and_work_paths(tmp_path):
    submission = tmp_path / "submission"
    workspace = tmp_path / "workspace"
    submission.mkdir()
    workspace.mkdir()

    command = VERIFY.sandbox_command(submission, workspace)

    assert command[0] == "bwrap"
    assert "--unshare-all" in command
    windows = [command[index : index + 3] for index in range(len(command) - 2)]
    assert ["--ro-bind", str(submission), "/submission"] in windows
    assert ["--bind", str(workspace), "/work"] in windows
    assert str(workspace) in command
    assert "/tests" not in command
    assert "/logs" not in command
    assert command[-3:] == [
        "/submission/run-analysis",
        "/work/input.jsonl",
        "/work/evidence.jsonl",
    ]
