#!/usr/bin/env python3
"""Strict verifier for evidence, the submitted catalog, and sandboxed analysis replay."""

from __future__ import annotations

import json
import os
import resource
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path

SUBMISSION = Path("/logs/artifacts/submission")
TESTS = Path("/tests")
REWARD_PATH = Path("/logs/verifier/reward.json")
OUTCOME_PATH = Path("/logs/verifier/outcome.json")
OUTCOME_SCHEMA = "lingrep-linguistic-eval.verifier-outcome/v1"
DIMENSIONS = (
    "evidence",
    "verdict",
    "query_visible",
    "query_hidden",
    "script_visible",
    "script_hidden",
)
VERDICT_SCHEMA = "lingrep-linguistic-eval.verdict/v1"
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
MAX_TEXT_ARTIFACT_BYTES = 1024 * 1024


def open_submission_file(path: Path) -> int:
    """Open a path the agent or its sandboxed runner wrote, without following a
    link; it may point into `/tests` or be a FIFO."""
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError as error:
        if path.is_symlink():
            raise ValueError(f"{path.name} is a symbolic link") from error
        raise
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise ValueError(f"{path.name} is not a regular file")
    return descriptor


def read_submission_bytes(path: Path, limit: int) -> bytes:
    with os.fdopen(open_submission_file(path), "rb") as source:
        if os.fstat(source.fileno()).st_size > limit:
            raise ValueError(f"{path.name} exceeds {limit} bytes")
        return source.read(limit + 1)


def read_submission_text(path: Path, limit: int) -> str:
    return read_submission_bytes(path, limit).decode()


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
    text = read_submission_text(path, MAX_EVIDENCE_BYTES)
    return validate_rows([json.loads(line) for line in text.splitlines()])


def load_gold(path: Path) -> list[dict]:
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
        if not {"eval_document_id", "sentences"} <= set(document) <= {
            "eval_document_id",
            "metadata",
            "sentences",
        }:
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


def run_lingrep(*arguments: str, timeout: float) -> list[dict]:
    completed = subprocess.run(
        ["ling-verifier", *arguments],
        text=True,
        capture_output=True,
        check=False,
        timeout=timeout,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip() or completed.stdout.strip()
        raise ReplayFailure(f"ling replay failed: {detail}")
    return [json.loads(line) for line in completed.stdout.splitlines()]


def replay(
    query: Path, corpus: Path, labels: list[str], *, timeout: float
) -> list[dict]:
    documents = verifier_input(load_documents, corpus)
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
        timeout=timeout,
    )
    validate_definition_labels(matches, labels)
    return validate_rows_against_documents(project_matches(matches), documents)


def submitted_query() -> Path:
    queries = sorted(SUBMISSION.rglob("*.lx"))
    expected = SUBMISSION / "query.lx"
    if queries != [expected]:
        raise ValueError("submitted catalog must be exactly one query.lx")
    read_submission_bytes(expected, MAX_TEXT_ARTIFACT_BYTES)
    return expected


def submitted_runner() -> Path:
    runner = SUBMISSION / "run-analysis"
    read_submission_bytes(runner, MAX_RUNNER_BYTES)
    if not os.access(runner, os.X_OK):
        raise ValueError("submission must contain a regular executable run-analysis")
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


def output_tail(path: Path) -> str:
    try:
        with os.fdopen(open_submission_file(path), "rb") as source:
            source.seek(max(0, os.fstat(source.fileno()).st_size - 4096))
            return source.read().decode(errors="replace").strip()
    except (OSError, ValueError) as error:
        return f"<{path.name} unreadable: {error}>"


def replay_analysis(corpus: Path, *, timeout: float) -> list[dict]:
    submitted_runner()
    documents = verifier_input(load_documents, corpus)
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
                timeout=timeout,
                preexec_fn=limit_files,
            )
        if completed.returncode != 0:
            detail = output_tail(stderr_path) or output_tail(stdout_path)
            raise ReplayFailure(f"run-analysis replay failed: {detail}")
        evidence = workspace / "evidence.jsonl"
        return validate_rows_against_documents(load_rows(evidence), documents)


class ReplayFailure(ValueError):
    """The submitted runner or catalog executed and failed."""


class VerifierFailure(Exception):
    """A verifier-owned input under `/tests` was unreadable."""


def verifier_input(read, *arguments):
    try:
        return read(*arguments)
    except Exception as error:
        raise VerifierFailure(f"{error.__class__.__name__}: {error}") from error


def classify(error: Exception) -> str:
    if isinstance(error, VerifierFailure):
        return "infrastructure_failure"
    if isinstance(error, subprocess.TimeoutExpired):
        return "replay_timeout"
    if isinstance(error, ReplayFailure):
        return "replay_failed"
    return "invalid_submission"


SUBMISSION_ERRORS = (
    OSError,
    ValueError,
    TypeError,
    KeyError,
    IndexError,
    subprocess.TimeoutExpired,
    VerifierFailure,
)


def load_json(path: Path) -> dict:
    return json.loads(path.read_text())


def task_contract() -> dict:
    contract = {"query": None, "script": None, "verdict": None}
    for kind, name in (
        ("query", "query-replay.json"),
        ("script", "analysis-replay.json"),
        ("verdict", "verdict.json"),
    ):
        path = TESTS / name
        if path.exists():
            try:
                contract[kind] = verifier_input(load_json, path)
            except VerifierFailure as error:
                raise VerifierFailure(f"{name}: {error}") from error
    return contract


def declare_dimensions(contract: dict) -> tuple[dict, dict]:
    reward = {"evidence_f1": 0.0, "exact": 0.0, "format": 0.0}
    outcomes = {"evidence": {"applicable": True, "outcome": "not_reached"}}
    has_verdict = contract["verdict"] is not None
    if has_verdict:
        reward["verdict_correct"] = 0.0
    outcomes["verdict"] = {
        "applicable": has_verdict,
        "outcome": "not_reached" if has_verdict else "not_applicable",
    }
    for kind in ("query", "script"):
        config = contract[kind]
        applicable = config is not None
        has_hidden = applicable and bool(config["has_hidden_driver"])
        if applicable:
            reward.update(
                {
                    f"{kind}_valid": 0.0,
                    f"visible_{kind}_f1": 0.0,
                    f"visible_{kind}_exact": 0.0,
                    f"{kind}_exact": 0.0,
                }
            )
        if has_hidden:
            reward.update({f"hidden_{kind}_f1": 0.0, f"hidden_{kind}_exact": 0.0})
        for scope, present in (("visible", applicable), ("hidden", has_hidden)):
            outcomes[f"{kind}_{scope}"] = {
                "applicable": present,
                "outcome": "not_reached" if present else "not_applicable",
            }
    return reward, outcomes


def score_evidence(reward: dict, outcome: dict) -> None:
    try:
        visible_documents = verifier_input(load_documents, TESTS / "visible.jsonl")
        expected = verifier_input(load_gold, TESTS / "gold.jsonl")
        predicted = validate_rows_against_documents(
            load_rows(SUBMISSION / "evidence.jsonl"), visible_documents
        )
    except SUBMISSION_ERRORS as error:
        outcome.update({"outcome": classify(error), "detail": str(error)})
        print(f"evidence verification failed: {error}")
        return
    score = score_rows(predicted, expected)
    reward.update(
        {
            "evidence_f1": score["f1"],
            "exact": score["exact"],
            "format": 1.0,
            "precision": score["precision"],
            "recall": score["recall"],
        }
    )
    outcome["outcome"] = "scored"


def load_verdict(path: Path) -> bool:
    """The submitted `verdict.json`: exactly `schema`, `answerable` and `reason`."""
    verdict = json.loads(read_submission_text(path, MAX_TEXT_ARTIFACT_BYTES))
    if not isinstance(verdict, dict) or set(verdict) != {"schema", "answerable", "reason"}:
        raise ValueError("verdict.json must be an object with schema, answerable and reason")
    if verdict["schema"] != VERDICT_SCHEMA:
        raise ValueError(f"verdict.json schema must be {VERDICT_SCHEMA}")
    if not isinstance(verdict["answerable"], bool) or not isinstance(verdict["reason"], str):
        raise TypeError("verdict.json answerable must be a boolean and reason a string")
    return verdict["answerable"]


def score_verdict(expected: dict, reward: dict, outcome: dict) -> None:
    try:
        answerable = verifier_input(lambda: bool(expected["answerable"]))
        submitted = load_verdict(SUBMISSION / "verdict.json")
    except SUBMISSION_ERRORS as error:
        outcome.update({"outcome": classify(error), "detail": str(error)})
        print(f"verdict verification failed: {error}")
        return
    reward["verdict_correct"] = float(submitted == answerable)
    outcome["outcome"] = "scored"


def score_replay(kind: str, prepare, *, has_hidden: bool, reward: dict, outcomes: dict) -> None:
    """`prepare` validates the submission and returns `corpus -> rows`; the hidden
    replay runs only after a scored visible one, else records `not_reached`."""
    visible = outcomes[f"{kind}_visible"]
    hidden = outcomes[f"{kind}_hidden"]
    try:
        run = prepare()
        gold = verifier_input(load_gold, TESTS / "gold.jsonl")
        rows = run(TESTS / "visible.jsonl")
    except SUBMISSION_ERRORS as error:
        visible.update({"outcome": classify(error), "detail": str(error)})
        if has_hidden and isinstance(error, VerifierFailure):
            hidden.update({"outcome": "infrastructure_failure", "detail": str(error)})
        print(f"{kind} replay verification failed: {error}")
        return
    reward[f"{kind}_valid"] = 1.0
    score = score_rows(rows, gold)
    reward[f"visible_{kind}_f1"] = score["f1"]
    reward[f"visible_{kind}_exact"] = score["exact"]
    visible["outcome"] = "scored"
    all_exact = score["exact"] == 1.0
    if has_hidden:
        try:
            hidden_gold = verifier_input(load_gold, TESTS / "hidden-gold.jsonl")
            hidden_rows = run(TESTS / "hidden.jsonl")
        except SUBMISSION_ERRORS as error:
            hidden.update({"outcome": classify(error), "detail": str(error)})
            print(f"hidden {kind} replay verification failed: {error}")
            return
        hidden_score = score_rows(hidden_rows, hidden_gold)
        reward[f"hidden_{kind}_f1"] = hidden_score["f1"]
        reward[f"hidden_{kind}_exact"] = hidden_score["exact"]
        hidden["outcome"] = "scored"
        all_exact = all_exact and hidden_score["exact"] == 1.0
    reward[f"{kind}_exact"] = float(all_exact)


def write_results(reward: dict, outcomes: dict) -> None:
    REWARD_PATH.parent.mkdir(parents=True, exist_ok=True)
    REWARD_PATH.write_text(json.dumps(reward, sort_keys=True) + "\n")
    OUTCOME_PATH.write_text(
        json.dumps(
            {"schema": OUTCOME_SCHEMA, "dimensions": outcomes},
            sort_keys=True,
        )
        + "\n"
    )


def main() -> None:
    try:
        contract = task_contract()
    except VerifierFailure as error:
        print(f"task contract unreadable: {error}")
        write_results(
            {"evidence_f1": 0.0, "exact": 0.0, "format": 0.0},
            {
                name: {
                    "applicable": True,
                    "outcome": "infrastructure_failure",
                    "detail": str(error),
                }
                for name in DIMENSIONS
            },
        )
        return
    reward, outcomes = declare_dimensions(contract)
    score_evidence(reward, outcomes["evidence"])
    if contract["verdict"] is not None:
        score_verdict(contract["verdict"], reward, outcomes["verdict"])

    query_config = contract["query"]
    if query_config is not None:

        def prepare_query():
            query = submitted_query()
            labels = verifier_input(lambda: list(query_config["labels"]))
            timeout = verifier_input(lambda: float(query_config["replay_timeout_sec"]))
            return lambda corpus: replay(query, corpus, labels, timeout=timeout)

        score_replay(
            "query",
            prepare_query,
            has_hidden=bool(query_config["has_hidden_driver"]),
            reward=reward,
            outcomes=outcomes,
        )

    script_config = contract["script"]
    if script_config is not None:

        def prepare_script():
            submitted_runner()
            timeout = verifier_input(lambda: float(script_config["replay_timeout_sec"]))
            return lambda corpus: replay_analysis(corpus, timeout=timeout)

        score_replay(
            "script",
            prepare_script,
            has_hidden=bool(script_config["has_hidden_driver"]),
            reward=reward,
            outcomes=outcomes,
        )

    write_results(reward, outcomes)


if __name__ == "__main__":
    main()
