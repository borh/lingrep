#!/usr/bin/env python3
"""Strict verifier for evidence, the submitted catalog, and sandboxed analysis replay."""

from __future__ import annotations

import json
import os
import resource
import shutil
import subprocess
import tempfile
from pathlib import Path

SUBMISSION = Path("/logs/artifacts/submission")
TESTS = Path("/tests")
REQUIRED = {
    "schema",
    "eval_document_id",
    "sentence_ordinal",
    "start_char",
    "end_char",
    "label",
}
ORDER = (
    "eval_document_id",
    "sentence_ordinal",
    "start_char",
    "end_char",
    "label",
)
MAX_EVIDENCE_BYTES = 16 * 1024 * 1024
MAX_REPLAY_FILE_BYTES = 512 * 1024 * 1024
MAX_RUNNER_BYTES = 1024 * 1024


def row_key(row: dict) -> tuple:
    return tuple(row[key] for key in ORDER)


def validate_rows(rows: list[dict]) -> list[dict]:
    for row in rows:
        if set(row) != REQUIRED or row["schema"] != "lingrep-linguistic-eval.evidence/v1":
            raise ValueError("invalid evidence schema")
        if not all(
            isinstance(row[key], int)
            for key in ("sentence_ordinal", "start_char", "end_char")
        ):
            raise TypeError("offsets and ordinal must be integers")
        if (
            row["sentence_ordinal"] < 0
            or row["start_char"] < 0
            or row["end_char"] <= row["start_char"]
        ):
            raise ValueError("invalid half-open span")
    keys = [row_key(row) for row in rows]
    if keys != sorted(keys) or len(keys) != len(set(keys)):
        raise ValueError("evidence must be canonical and unique")
    return rows


def load_rows(path: Path) -> list[dict]:
    return validate_rows([json.loads(line) for line in path.read_text().splitlines()])


def validate_rows_against_documents(
    rows: list[dict], documents: list[dict]
) -> list[dict]:
    by_id = {document["eval_document_id"]: document for document in documents}
    if len(by_id) != len(documents):
        raise ValueError("replay document identifiers must be unique")
    for row in rows:
        document = by_id.get(row["eval_document_id"])
        ordinal = row["sentence_ordinal"]
        if document is None or ordinal >= len(document["sentences"]):
            raise ValueError("evidence has an unknown source sentence")
        if row["end_char"] > len(document["sentences"][ordinal]):
            raise ValueError(
                "evidence character span falls outside its source sentence"
            )
    return rows


def score_rows(predicted: list[dict], expected: list[dict]) -> dict[str, float]:
    predicted_keys = {row_key(row) for row in predicted}
    expected_keys = {row_key(row) for row in expected}
    true_positive = len(predicted_keys & expected_keys)
    precision = (
        true_positive / len(predicted_keys)
        if predicted_keys
        else float(not expected_keys)
    )
    recall = (
        true_positive / len(expected_keys)
        if expected_keys
        else float(not predicted_keys)
    )
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {
        "f1": f1,
        "exact": float(predicted_keys == expected_keys),
        "precision": precision,
        "recall": recall,
    }


def load_jsonl(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines()]


def load_documents(path: Path) -> list[dict]:
    documents = load_jsonl(path)
    for document in documents:
        if set(document) != {"eval_document_id", "sentences"}:
            raise ValueError("invalid replay document schema")
        if not isinstance(document["eval_document_id"], str) or not isinstance(
            document["sentences"], list
        ):
            raise TypeError("invalid replay document values")
        if not all(isinstance(sentence, str) for sentence in document["sentences"]):
            raise TypeError("replay sentences must be strings")
    return documents


def event_data(events: list[dict], kind: str) -> list[dict]:
    return [event["data"] for event in events if event.get("type") == kind]


def match_char_span(match: dict) -> tuple[int, int]:
    """Read the record's own code-point offsets, which the tool publishes.

    `ling` computes these from the published span, so a `detach` or a span
    rewrite moves them with it. Rebuilding them from token indices would be a
    second implementation of the same projection, and one that silently scores
    zero when a sentence withdraws offsets (`counters.offsets_omitted`).
    """
    char_span = match.get("char_span")
    if not isinstance(char_span, dict):
        raise TypeError(
            "match has no `char_span`; the run withdrew character offsets "
            "(see the run record's `counters.offsets_omitted`)"
        )
    start = char_span.get("start")
    end = char_span.get("end")
    if not isinstance(start, int) or not isinstance(end, int) or end <= start:
        raise ValueError("match has an invalid character span")
    return start, end


def match_document_id(match: dict) -> str:
    """Read the document identity the run projected from its own metadata."""
    source_metadata = match.get("source_metadata")
    document = (
        source_metadata.get("document") if isinstance(source_metadata, dict) else None
    )
    document_id = (
        document.get("eval_document_id") if isinstance(document, dict) else None
    )
    if not isinstance(document_id, str) or not document_id:
        raise ValueError(
            "match does not carry `@document.eval_document_id`; the query run must "
            "pass `--source-metadata @document.eval_document_id`"
        )
    return document_id


def project_matches(match_events: list[dict]) -> list[dict]:
    rows = []
    for match in event_data(match_events, "match"):
        start_char, end_char = match_char_span(match)
        rows.append(
            {
                "schema": "lingrep-linguistic-eval.evidence/v1",
                "eval_document_id": match_document_id(match),
                "sentence_ordinal": match["document_sentence_ordinal"],
                "start_char": start_char,
                "end_char": end_char,
                "label": match["pattern"],
            }
        )
    rows.sort(key=row_key)
    return validate_rows(rows)


def published_label(definition: dict) -> str:
    """The name a definition publishes its records under.

    `output_name` is compiler-owned metadata that rewrites the published name
    (`lingrep_core::fsm::policy::POLICY_KEYS`), and it is the only way to
    publish a label that is not a legal definition identifier.
    """
    metadata = definition.get("metadata")
    output_name = metadata.get("output_name") if isinstance(metadata, dict) else None
    return output_name if isinstance(output_name, str) else definition["name"]


def validate_definition_labels(events: list[dict], labels: list[str]) -> None:
    names = [
        published_label(definition) for definition in event_data(events, "definition")
    ]
    if len(names) != len(set(names)) or set(names) != set(labels):
        raise ValueError(
            f"emitting definition labels {sorted(names)} do not equal "
            f"declared labels {sorted(labels)}"
        )


def run_lingrep(*arguments: str) -> list[dict]:
    completed = subprocess.run(
        ["ling-verifier", *arguments],
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise ValueError(f"ling replay failed: {detail}")
    return [json.loads(line) for line in completed.stdout.splitlines()]


def replay(query: Path, corpus: Path, labels: list[str]) -> list[dict]:
    documents = load_documents(corpus)
    matches = run_lingrep(
        "--json",
        "matches",
        "--patterns",
        str(query),
        "--source-metadata",
        "@document.eval_document_id",
        "--limit",
        "all",
        "--progress",
        "never",
        str(corpus),
    )
    validate_definition_labels(matches, labels)
    return validate_rows_against_documents(project_matches(matches), documents)


def submitted_query() -> Path:
    queries = sorted(SUBMISSION.rglob("*.lx"))
    expected = SUBMISSION / "query.lx"
    if queries != [expected] or expected.is_symlink() or not expected.is_file():
        raise ValueError(
            "submitted catalog must be exactly one regular query.lx"
        )
    return expected


def submitted_runner() -> Path:
    runner = SUBMISSION / "run-analysis"
    if runner.is_symlink() or not runner.is_file() or not os.access(runner, os.X_OK):
        raise ValueError("submission must contain a regular executable run-analysis")
    if runner.stat().st_size > MAX_RUNNER_BYTES:
        raise ValueError("run-analysis exceeds the submission size limit")
    return runner


def sandbox_command(submission: Path, workspace: Path) -> list[str]:
    command = [
        "bwrap",
        "--unshare-all",
        "--new-session",
        "--die-with-parent",
        "--uid",
        "65534",
        "--gid",
        "65534",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/nix/store",
        "/nix/store",
        "--ro-bind",
        "/bin",
        "/bin",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/etc",
        "/etc",
        "--dir",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home",
        "--dir",
        "/home/agent",
        "--ro-bind",
        str(submission),
        "/submission",
        "--bind",
        str(workspace),
        "/work",
        "--clearenv",
        "--setenv",
        "HOME",
        "/home/agent",
        "--setenv",
        "PATH",
        "/usr/local/bin:/usr/bin:/bin",
        "--setenv",
        "LANG",
        "C.UTF-8",
        "--setenv",
        "LC_ALL",
        "C.UTF-8",
    ]
    dictionary = os.environ.get("LINGREP_DICTIONARY_PATH")
    if dictionary:
        command.extend(["--setenv", "LINGREP_DICTIONARY_PATH", dictionary])
    command.extend(
        [
            "--chdir",
            "/submission",
            "/submission/run-analysis",
            "/work/input.jsonl",
            "/work/evidence.jsonl",
        ]
    )
    return command


def replay_analysis(corpus: Path) -> list[dict]:
    submitted_runner()
    documents = load_documents(corpus)
    with tempfile.TemporaryDirectory(prefix="lingrep-harbor-replay-") as temporary:
        workspace = Path(temporary)
        workspace.chmod(0o777)
        input_path = workspace / "input.jsonl"
        shutil.copyfile(corpus, input_path)
        input_path.chmod(0o644)

        def limit_files() -> None:
            resource.setrlimit(
                resource.RLIMIT_FSIZE,
                (MAX_REPLAY_FILE_BYTES, MAX_REPLAY_FILE_BYTES),
            )

        stdout_path = workspace / "stdout"
        stderr_path = workspace / "stderr"
        with stdout_path.open("w") as stdout, stderr_path.open("w") as stderr:
            completed = subprocess.run(
                sandbox_command(SUBMISSION, workspace),
                text=True,
                stdout=stdout,
                stderr=stderr,
                check=False,
                timeout=30,
                preexec_fn=limit_files,
            )
        if completed.returncode != 0:
            detail = stderr_path.read_text(errors="replace")[-4096:].strip()
            if not detail:
                detail = stdout_path.read_text(errors="replace")[-4096:].strip()
            raise ValueError(f"run-analysis replay failed: {detail}")
        evidence = workspace / "evidence.jsonl"
        if evidence.stat().st_size > MAX_EVIDENCE_BYTES:
            raise ValueError("run-analysis evidence exceeds replay size limit")
        return validate_rows_against_documents(load_rows(evidence), documents)


def main() -> None:
    reward = {"evidence_f1": 0.0, "exact": 0.0, "format": 0.0}
    try:
        result = json.loads((SUBMISSION / "result.json").read_text())
        if result.get("schema") != "lingrep-linguistic-eval.result/v1":
            raise ValueError("invalid result schema")
        if not (SUBMISSION / "analysis.md").read_text().strip():
            raise ValueError("analysis.md is empty")
        visible_documents = load_documents(TESTS / "visible.jsonl")
        predicted = validate_rows_against_documents(
            load_rows(SUBMISSION / "evidence.jsonl"), visible_documents
        )
        expected = load_rows(TESTS / "gold.jsonl")
        score = score_rows(predicted, expected)
        reward = {
            "evidence_f1": score["f1"],
            "exact": score["exact"],
            "format": 1.0,
            "precision": score["precision"],
            "recall": score["recall"],
        }
    except (OSError, ValueError, TypeError, json.JSONDecodeError) as error:
        print(f"evidence verification failed: {error}")

    config_path = TESTS / "query-replay.json"
    if config_path.exists():
        reward.update(
            {
                "query_valid": 0.0,
                "visible_query_f1": 0.0,
                "visible_query_exact": 0.0,
                "query_exact": 0.0,
            }
        )
        try:
            config = json.loads(config_path.read_text())
            query = submitted_query()
            visible = replay(query, TESTS / "visible.jsonl", config["labels"])
            reward["query_valid"] = 1.0
            visible_score = score_rows(visible, load_rows(TESTS / "gold.jsonl"))
            reward["visible_query_f1"] = visible_score["f1"]
            reward["visible_query_exact"] = visible_score["exact"]
            all_exact = visible_score["exact"] == 1.0
            if config["has_hidden_driver"]:
                reward["hidden_query_f1"] = 0.0
                reward["hidden_query_exact"] = 0.0
                hidden = replay(query, TESTS / "hidden.jsonl", config["labels"])
                hidden_score = score_rows(
                    hidden, load_rows(TESTS / "hidden-gold.jsonl")
                )
                reward["hidden_query_f1"] = hidden_score["f1"]
                reward["hidden_query_exact"] = hidden_score["exact"]
                all_exact = all_exact and hidden_score["exact"] == 1.0
            reward["query_exact"] = float(all_exact)
        except (
            OSError,
            ValueError,
            TypeError,
            KeyError,
            IndexError,
            json.JSONDecodeError,
        ) as error:
            print(f"query replay verification failed: {error}")

    analysis_config_path = TESTS / "analysis-replay.json"
    if analysis_config_path.exists():
        reward.update(
            {
                "script_valid": 0.0,
                "visible_script_f1": 0.0,
                "visible_script_exact": 0.0,
                "script_exact": 0.0,
            }
        )
        try:
            config = json.loads(analysis_config_path.read_text())
            visible = replay_analysis(TESTS / "visible.jsonl")
            reward["script_valid"] = 1.0
            visible_score = score_rows(visible, load_rows(TESTS / "gold.jsonl"))
            reward["visible_script_f1"] = visible_score["f1"]
            reward["visible_script_exact"] = visible_score["exact"]
            all_exact = visible_score["exact"] == 1.0
            if config["has_hidden_driver"]:
                reward["hidden_script_f1"] = 0.0
                reward["hidden_script_exact"] = 0.0
                hidden = replay_analysis(TESTS / "hidden.jsonl")
                hidden_score = score_rows(
                    hidden, load_rows(TESTS / "hidden-gold.jsonl")
                )
                reward["hidden_script_f1"] = hidden_score["f1"]
                reward["hidden_script_exact"] = hidden_score["exact"]
                all_exact = all_exact and hidden_score["exact"] == 1.0
            reward["script_exact"] = float(all_exact)
        except (
            OSError,
            ValueError,
            TypeError,
            KeyError,
            subprocess.TimeoutExpired,
            json.JSONDecodeError,
        ) as error:
            print(f"run-analysis replay verification failed: {error}")

    output = Path("/logs/verifier/reward.json")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(reward, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
