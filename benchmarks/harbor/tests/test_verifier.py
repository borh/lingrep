import importlib.util
import json
import os
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
    documents = [
        {"eval_document_id": "doc-001", "metadata": {"ndc": "NDC 913"}, "sentences": ["猫がいる。"]}
    ]
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
    with pytest.raises(ValueError, match="exceeds"):
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


GOLD_ROW = {
    "schema": "lingrep-linguistic-eval.evidence/v1",
    "eval_document_id": "doc-001",
    "sentence_ordinal": 0,
    "start_char": 0,
    "end_char": 1,
    "label": "target",
}


def write_verifier_tree(root, monkeypatch):
    submission = root / "submission"
    tests = root / "tests"
    submission.mkdir()
    tests.mkdir()
    (tests / "visible.jsonl").write_text(
        json.dumps({"eval_document_id": "doc-001", "sentences": ["猫がいる。"]}) + "\n"
    )
    (tests / "gold.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    monkeypatch.setattr(VERIFY, "SUBMISSION", submission)
    monkeypatch.setattr(VERIFY, "TESTS", tests)
    monkeypatch.setattr(VERIFY, "REWARD_PATH", root / "logs" / "reward.json")
    monkeypatch.setattr(VERIFY, "OUTCOME_PATH", root / "logs" / "outcome.json")
    return submission, tests


def test_evidence_symlinked_to_verifier_gold_scores_nothing(tmp_path, monkeypatch):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").symlink_to(tests / "gold.jsonl")

    VERIFY.main()

    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert reward["evidence_f1"] == 0.0
    assert reward["format"] == 0.0


def test_evidence_is_scored_without_result_or_analysis_artifacts(tmp_path, monkeypatch):
    submission, _ = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    assert sorted(path.name for path in submission.iterdir()) == ["evidence.jsonl"]

    VERIFY.main()

    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert (reward["format"], reward["evidence_f1"]) == (1.0, 1.0)


def test_submission_reads_reject_special_and_oversized_files(tmp_path, monkeypatch):
    fifo = tmp_path / "evidence.jsonl"
    os.mkfifo(fifo)
    with pytest.raises(ValueError, match="regular file"):
        VERIFY.read_submission_bytes(fifo, 1024)

    link = tmp_path / "link.jsonl"
    link.symlink_to(tmp_path / "missing")
    with pytest.raises(ValueError, match="symbolic link"):
        VERIFY.read_submission_bytes(link, 1024)

    large = tmp_path / "large.jsonl"
    large.write_bytes(b"x" * 11)
    with pytest.raises(ValueError, match="exceeds"):
        VERIFY.read_submission_bytes(large, 10)
    assert VERIFY.read_submission_bytes(large, 11) == b"x" * 11


def test_replay_output_symlink_is_not_followed(tmp_path):
    gold = tmp_path / "gold.jsonl"
    gold.write_text(json.dumps(GOLD_ROW) + "\n")
    evidence = tmp_path / "evidence.jsonl"
    evidence.symlink_to(gold)

    with pytest.raises(ValueError, match="symbolic link"):
        VERIFY.load_rows(evidence)


def declare_script_replay(tests, *, has_hidden):
    (tests / "analysis-replay.json").write_text(
        json.dumps({"has_hidden_driver": has_hidden, "replay_timeout_sec": 30}) + "\n"
    )
    if has_hidden:
        (tests / "hidden.jsonl").write_text(
            json.dumps({"eval_document_id": "hidden-001", "sentences": ["犬がいる。"]})
            + "\n"
        )
        (tests / "hidden-gold.jsonl").write_text(
            json.dumps({**GOLD_ROW, "eval_document_id": "hidden-001"}) + "\n"
        )


def outcomes():
    record = json.loads(VERIFY.OUTCOME_PATH.read_text())
    assert record["schema"] == "lingrep-linguistic-eval.verifier-outcome/v1"
    return {
        name: (dimension["applicable"], dimension["outcome"])
        for name, dimension in record["dimensions"].items()
    }


def test_missing_runner_keeps_the_hidden_dimension_in_the_denominator(
    tmp_path, monkeypatch
):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    declare_script_replay(tests, has_hidden=True)

    VERIFY.main()

    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert reward["evidence_f1"] == 1.0
    assert reward["script_valid"] == 0.0
    assert reward["visible_script_f1"] == 0.0
    assert reward["hidden_script_f1"] == 0.0
    assert reward["hidden_script_exact"] == 0.0
    assert reward["script_exact"] == 0.0
    assert outcomes() == {
        "evidence": (True, "scored"),
        "verdict": (False, "not_applicable"),
        "query_visible": (False, "not_applicable"),
        "query_hidden": (False, "not_applicable"),
        "script_visible": (True, "invalid_submission"),
        "script_hidden": (True, "not_reached"),
    }


def test_no_driver_case_is_inapplicable_rather_than_failed(tmp_path, monkeypatch):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    declare_script_replay(tests, has_hidden=False)

    VERIFY.main()

    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert "hidden_script_f1" not in reward
    assert outcomes()["script_hidden"] == (False, "not_applicable")


def test_replay_timeout_and_verifier_errors_are_distinct_outcomes(
    tmp_path, monkeypatch
):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    runner = submission / "run-analysis"
    runner.write_text("#!/bin/sh\nexit 0\n")
    runner.chmod(0o755)
    declare_script_replay(tests, has_hidden=True)

    def expire(command, **kwargs):
        raise VERIFY.subprocess.TimeoutExpired(command, kwargs["timeout"])

    monkeypatch.setattr(VERIFY.subprocess, "run", expire)
    VERIFY.main()
    assert outcomes()["script_visible"] == (True, "replay_timeout")
    assert outcomes()["script_hidden"] == (True, "not_reached")

    (tests / "gold.jsonl").unlink()
    VERIFY.main()
    assert outcomes()["evidence"] == (True, "infrastructure_failure")
    assert outcomes()["script_visible"] == (True, "infrastructure_failure")
    assert outcomes()["script_hidden"] == (True, "infrastructure_failure")
    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert reward["hidden_script_f1"] == 0.0


def test_unreadable_contract_is_an_infrastructure_failure_of_every_dimension(
    tmp_path, monkeypatch
):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    (tests / "analysis-replay.json").write_text("{not json")

    VERIFY.main()

    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert reward == {"evidence_f1": 0.0, "exact": 0.0, "format": 0.0}
    assert outcomes() == {
        name: (True, "infrastructure_failure")
        for name in (
            "evidence",
            "verdict",
            "query_visible",
            "query_hidden",
            "script_visible",
            "script_hidden",
        )
    }
    record = json.loads(VERIFY.OUTCOME_PATH.read_text())
    assert "analysis-replay.json" in record["dimensions"]["evidence"]["detail"]


def declare_verdict(tests, *, answerable):
    (tests / "verdict.json").write_text(json.dumps({"answerable": answerable}) + "\n")


def write_verdict(submission, answerable, **extra):
    verdict = {"schema": VERIFY.VERDICT_SCHEMA, "answerable": answerable, "reason": "r", **extra}
    (submission / "verdict.json").write_text(json.dumps(verdict) + "\n")


def test_verdict_dimension_scores_the_answerability_judgement(tmp_path, monkeypatch):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")

    VERIFY.main()
    assert outcomes()["verdict"] == (False, "not_applicable")
    assert "verdict_correct" not in json.loads(VERIFY.REWARD_PATH.read_text())

    declare_verdict(tests, answerable=False)
    VERIFY.main()
    assert outcomes()["verdict"] == (True, "invalid_submission")
    assert json.loads(VERIFY.REWARD_PATH.read_text())["verdict_correct"] == 0.0

    write_verdict(submission, False)
    VERIFY.main()
    assert outcomes()["verdict"] == (True, "scored")
    reward = json.loads(VERIFY.REWARD_PATH.read_text())
    assert reward["verdict_correct"] == 1.0
    assert reward["evidence_f1"] == 1.0

    write_verdict(submission, True)
    VERIFY.main()
    assert outcomes()["verdict"] == (True, "scored")
    assert json.loads(VERIFY.REWARD_PATH.read_text())["verdict_correct"] == 0.0


@pytest.mark.parametrize(
    "content",
    [
        '{"schema": "lingrep-linguistic-eval.verdict/v1", "answerable": "yes", "reason": "r"}',
        '{"schema": "lingrep-linguistic-eval.verdict/v1", "answerable": true}',
        '{"schema": "other", "answerable": true, "reason": "r"}',
        '{"schema": "lingrep-linguistic-eval.verdict/v1", "answerable": true, "reason": "r", "x": 1}',
        "[]",
        "{not json",
    ],
)
def test_malformed_verdicts_are_invalid_submissions(tmp_path, monkeypatch, content):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    declare_verdict(tests, answerable=True)
    (submission / "verdict.json").write_text(content)
    VERIFY.main()
    assert outcomes()["verdict"] == (True, "invalid_submission")
    assert json.loads(VERIFY.REWARD_PATH.read_text())["verdict_correct"] == 0.0


def test_unreadable_verdict_contract_is_an_infrastructure_failure(tmp_path, monkeypatch):
    submission, tests = write_verifier_tree(tmp_path, monkeypatch)
    (submission / "evidence.jsonl").write_text(json.dumps(GOLD_ROW) + "\n")
    (tests / "verdict.json").write_text("{not json")
    VERIFY.main()
    assert outcomes()["verdict"] == (True, "infrastructure_failure")
