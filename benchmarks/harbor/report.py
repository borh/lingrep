#!/usr/bin/env python3
"""Emit one analysis-ready row for each Harbor trial in an experiment."""

from __future__ import annotations

import argparse
import ast
import difflib
import json
import math
import random
import re
from collections import defaultdict
from datetime import datetime
from itertools import pairwise
from pathlib import Path
from statistics import fmean, median

from command_analysis import analyze_trial
from experiment import (
    discover_trials,
    find_experiment,
    infrastructure_failure,
    reconcile,
    requested_model,
    sha256_file,
)
from generate_dataset import ROOT as HARBOR_ROOT
from generate_dataset import mecab_reference_runner

MAX_RUNNER_SOURCE_BYTES = 1024 * 1024
BRANCH_NODES = (ast.If, ast.IfExp, ast.For, ast.While, ast.Try, ast.Match, ast.comprehension)
DEFINITION_LINE = re.compile(r"^\s*(?:pattern|fragment|lexicon|set)?\s*[^\s:#]+(?:\s+by\s+\[[^\]]*\])?\s*:=")
ATTRIBUTE_KEY = re.compile(r"(?<![\w.$@])([A-Za-z_][\w.]*)\s*(?:!?[=~]|\s+(?:not\s+)?in\b)")
PAIRED_SCHEMA = "lingrep-linguistic-eval.paired-comparison/v1"
REVISION_SCHEMA = "lingrep-linguistic-eval.revision-report/v1"
PRIMARY_METRICS = ("visible_script_f1", "hidden_script_f1")


def elapsed(started: str | None, finished: str | None) -> float | None:
    if not started or not finished:
        return None
    return (
        datetime.fromisoformat(finished) - datetime.fromisoformat(started)
    ).total_seconds()


def optional_max(values: list[int | float | None]) -> int | float | None:
    present = [value for value in values if value is not None]
    return max(present) if present else None


def longest_step_gap(trial_root: Path) -> dict | None:
    """Largest wall-clock gap between consecutive trajectory steps and the step it
    preceded; None without two timestamped steps."""
    path = trial_root / "agent" / "trajectory.json"
    if not path.is_file():
        return None
    steps = [
        (datetime.fromisoformat(step["timestamp"]), step.get("step_id"))
        for step in json.loads(path.read_text()).get("steps", [])
        if step.get("timestamp")
    ]
    if len(steps) < 2:
        return None
    gaps = [
        ((later - earlier).total_seconds(), step_id)
        for (earlier, _), (later, step_id) in pairwise(steps)
    ]
    seconds, step_id = max(gaps)
    return {"seconds": seconds, "before_step_id": step_id}


def optional_sum(values: list[int | float | None]) -> int | float | None:
    present = [value for value in values if value is not None]
    return sum(present) if present else None


def optional_median(values: list[int | float | None]) -> int | float | None:
    present = [value for value in values if value is not None]
    return median(present) if present else None


def optional_mean(values: list[int | float | None]) -> float | None:
    present = [value for value in values if value is not None]
    return fmean(present) if present else None


def submission_dir(trial_root: Path) -> Path:
    """Harbor mirrors the collected `/logs/artifacts/submission` under `artifacts/`."""
    return trial_root / "artifacts" / "logs" / "artifacts" / "submission"


def pattern_query_artifacts(trial_root: Path) -> list[dict[str, str]]:
    submission = submission_dir(trial_root)
    if not submission.is_dir():
        return []
    queries = []
    for path in sorted(submission.rglob("*.lx")):
        if path.is_symlink() or not path.is_file():
            continue
        source = path.read_text(errors="replace")
        queries.append(
            {
                "path": str(path.relative_to(submission)),
                "sha256": sha256_file(path),
                "source": source,
            }
        )
    return queries


def hidden_scored(row: dict, kind: str) -> bool:
    if row["infrastructure_failure"]:
        return False
    outcomes = row["verifier_outcomes"]
    if outcomes is None:
        return row[f"hidden_{kind}_f1"] is not None
    return outcomes[f"{kind}_hidden"]["outcome"] == "scored"


def scored_values(group: list[dict], metric: str) -> list[float | None]:
    return [row[metric] for row in group if not row["infrastructure_failure"]]


def analysis_runner_artifact(trial_root: Path) -> dict | None:
    path = submission_dir(trial_root) / "run-analysis"
    if path.is_symlink() or not path.is_file():
        return None
    size = path.stat().st_size
    return {
        "path": "run-analysis",
        "sha256": sha256_file(path),
        "size_bytes": size,
        "source": (
            path.read_text(errors="replace")
            if size <= MAX_RUNNER_SOURCE_BYTES
            else None
        ),
    }


def non_blank_lines(source: str) -> int:
    return sum(1 for line in source.splitlines() if line.strip())


def runner_complexity(source: str | None, size_bytes: int) -> dict:
    """Size and, for Python, AST size and branch count of a `run-analysis`."""
    measures: dict = {"bytes": size_bytes, "non_blank_lines": None, "language": "other"}
    if source is None:
        return measures
    measures["non_blank_lines"] = non_blank_lines(source)
    shebang = source.split("\n", 1)[0] if source.startswith("#!") else ""
    try:
        tree = ast.parse(source)
    except (SyntaxError, ValueError):
        if re.search(r"\b(ba|z|da)?sh\b", shebang):
            measures["language"] = "shell"
        return measures
    if "python" in shebang or not shebang:
        nodes = list(ast.walk(tree))
        measures["language"] = "python"
        measures["ast_nodes"] = len(nodes)
        measures["branches"] = sum(isinstance(node, BRANCH_NODES) for node in nodes)
    elif re.search(r"\b(ba|z|da)?sh\b", shebang):
        measures["language"] = "shell"
    return measures


def query_complexity(source: str) -> dict:
    """Size, definition count and the attribute keys a `.lx` text references.

    Keys are read textually from token tests, so a key inside a string literal
    or a `where` clause counts too; `ling check --json` is the exact reading."""
    code_lines = [line for line in source.splitlines() if not line.lstrip().startswith("#")]
    keys = {
        key
        for line in code_lines
        for bracket in re.findall(r"\[([^\]]*)\]", line)
        for key in ATTRIBUTE_KEY.findall(bracket)
        if key not in {"in", "not"}
    }
    return {
        "bytes": len(source.encode()),
        "non_blank_lines": non_blank_lines(source),
        "definitions": sum(bool(DEFINITION_LINE.match(line)) for line in code_lines),
        "attribute_keys": sorted(keys),
    }


def artifact_complexity(
    runner: dict | None, pattern_queries: list[dict[str, str]]
) -> dict:
    query = next((q for q in pattern_queries if q["path"] == "query.lx"), None)
    return {
        "run_analysis": (
            runner_complexity(runner["source"], runner["size_bytes"])
            if runner is not None
            else None
        ),
        "query": query_complexity(query["source"]) if query is not None else None,
    }


def reference_artifact_complexity(root: Path, arm: str, case_ids: set[str]) -> dict | None:
    """The packaged reference artifacts' measures for the cases in `case_ids`, as a floor."""
    catalog = root / "inputs" / "catalog.json"
    if not catalog.is_file():
        return None
    cases = {case["id"]: case for case in json.loads(catalog.read_text())}
    if arm == "lingrep":
        runner_source = (HARBOR_ROOT / "oracle_lingrep_runner.py").read_text()
        runners = [runner_source for _ in case_ids]
        queries = [query_complexity(cases[case_id]["oracle_query"]) for case_id in sorted(case_ids)]
    else:
        runners = [
            mecab_reference_runner(cases[case_id]["oracle_mecab"]) for case_id in sorted(case_ids)
        ]
        queries = []
    runner_measures = [runner_complexity(source, len(source.encode())) for source in runners]
    return {
        "run_analysis": median_measures(runner_measures, ("bytes", "non_blank_lines", "ast_nodes", "branches")),
        "query": median_measures(queries, ("bytes", "non_blank_lines", "definitions")),
    }


def median_measures(measures: list[dict], keys: tuple[str, ...]) -> dict | None:
    if not measures:
        return None
    return {key: optional_median([m.get(key) for m in measures]) for key in keys}


def reconcile_experiment(path: Path) -> dict:
    root, manifest = find_experiment(path)
    return reconcile(manifest, discover_trials(root))


def summarize_experiment(path: Path) -> list[dict]:
    root, manifest = find_experiment(path)
    tasks = manifest["tasks"]
    attempts: defaultdict[tuple[str, str], int] = defaultdict(int)
    rows = []
    for trial in discover_trials(root):
        if trial.result is None:
            continue
        result = trial.result
        result_path = trial.path / "result.json"
        task_name = result["task_name"]
        if task_name not in tasks:
            raise ValueError(
                f"trial task {task_name!r} is absent from experiment manifest"
            )
        task = tasks[task_name]
        agent_info = result.get("agent_info") or {}
        model_info = agent_info.get("model_info") or {}
        model = requested_model(trial, manifest)
        attempt_key = (task_name, model)
        attempts[attempt_key] += 1
        exception = result.get("exception_info") or {}
        rewards = (result.get("verifier_result") or {}).get("rewards") or {}
        agent_result = result.get("agent_result") or {}
        agent_metadata = agent_result.get("metadata") or {}
        agent_execution = result.get("agent_execution") or {}
        commands = analyze_trial(result_path.parent)
        command_summary = commands["summary"]
        api_request_times = agent_metadata.get("api_request_times_msec")
        agent_elapsed = elapsed(
            agent_execution.get("started_at"),
            agent_execution.get("finished_at"),
        )
        model_api_seconds = (
            sum(api_request_times) / 1000 if api_request_times is not None else None
        )
        phase_seconds = {
            phase: elapsed(
                (result.get(phase) or {}).get("started_at"),
                (result.get(phase) or {}).get("finished_at"),
            )
            for phase in ("environment_setup", "agent_setup", "agent_execution", "verifier")
        }
        command_seconds = (
            sum(command_summary["bash_roundtrip_seconds_by_shell_category"].values())
            if commands["trajectory_sha256"] is not None
            and command_summary["timed_calls"]
            == command_summary["nonempty_bash_tool_calls"]
            else None
        )
        pattern_queries = pattern_query_artifacts(result_path.parent)
        analysis_runner = analysis_runner_artifact(result_path.parent)
        rows.append(
            {
                "schema": "lingrep-linguistic-eval.trial-report/v1",
                "experiment_id": manifest["experiment_id"],
                "model": model,
                "reported_model": model_info.get("name"),
                "agent": agent_info.get("name"),
                "arm": task["arm"],
                "case_id": task["case_id"],
                "base_case_id": task.get("base_case_id", task["case_id"]),
                "stage": task.get("stage", "base"),
                "capability": task["capability"],
                "attempt": attempts[attempt_key],
                "trial_id": result.get("id"),
                "job_name": trial.job_name,
                "trial_name": trial.trial_name,
                "task_checksum": result.get("task_checksum"),
                "trial_elapsed_seconds": elapsed(
                    result.get("started_at"), result.get("finished_at")
                ),
                "agent_elapsed_seconds": agent_elapsed,
                "evidence_f1": rewards.get("evidence_f1", 0.0),
                "exact": rewards.get("exact", 0.0),
                "format": rewards.get("format", 0.0),
                "verdict_correct": rewards.get("verdict_correct"),
                "precision": rewards.get("precision", 0.0),
                "recall": rewards.get("recall", 0.0),
                "query_valid": rewards.get("query_valid"),
                "visible_query_f1": rewards.get("visible_query_f1"),
                "visible_query_exact": rewards.get("visible_query_exact"),
                "hidden_query_f1": rewards.get("hidden_query_f1"),
                "hidden_query_exact": rewards.get("hidden_query_exact"),
                "query_exact": rewards.get("query_exact"),
                "script_valid": rewards.get("script_valid"),
                "visible_script_f1": rewards.get("visible_script_f1"),
                "visible_script_exact": rewards.get("visible_script_exact"),
                "hidden_script_f1": rewards.get("hidden_script_f1"),
                "hidden_script_exact": rewards.get("hidden_script_exact"),
                "script_exact": rewards.get("script_exact"),
                "verifier_outcomes": trial.outcomes,
                "infrastructure_failure": infrastructure_failure(result, trial.outcomes),
                "operational_success": not exception
                and result.get("finished_at") is not None,
                "exception": exception.get("exception_type"),
                "input_tokens": agent_result.get("n_input_tokens"),
                "cached_input_tokens": agent_result.get("n_cache_tokens"),
                "output_tokens": agent_result.get("n_output_tokens"),
                "model_api_requests": (
                    len(api_request_times) if api_request_times is not None else None
                ),
                "model_api_seconds": model_api_seconds,
                "model_api_request_seconds_max": (
                    max(api_request_times) / 1000 if api_request_times else None
                ),
                "phase_seconds": phase_seconds,
                "longest_step_gap": longest_step_gap(result_path.parent),
                "command_analysis": command_summary,
                "runtime_invocations": commands["runtime_invocations"],
                "pattern_queries": pattern_queries,
                "analysis_runner": analysis_runner,
                "artifact_complexity": artifact_complexity(analysis_runner, pattern_queries),
                "agent_other_seconds": (
                    max(0.0, agent_elapsed - model_api_seconds - command_seconds)
                    if agent_elapsed is not None
                    and model_api_seconds is not None
                    and command_seconds is not None
                    else None
                ),
                "result_path": str(result_path.relative_to(root)),
            }
        )
    return rows


def planned_by_group(manifest: dict) -> dict[tuple[str, str], dict]:
    planned: dict[tuple[str, str], dict] = {}
    for model in manifest["models"]:
        for task in manifest["tasks"].values():
            group = planned.setdefault((model, task["arm"]), {"cells": 0, "trials": 0})
            group["cells"] += 1
            group["trials"] += manifest["repetitions"]
    return planned


def summarize_groups(rows: list[dict], manifest: dict, root: Path | None = None) -> list[dict]:
    groups: defaultdict[tuple[str, str], list[dict]] = defaultdict(list)
    for row in rows:
        groups[(row["model"], row["arm"])].append(row)
    planned = planned_by_group(manifest)
    summaries = []
    for (model, arm), group in sorted(groups.items()):
        plan = planned[(model, arm)]
        observed_cells = {row["case_id"] for row in group}
        matrix_complete = len(observed_cells) == plan["cells"] and all(
            sum(row["case_id"] == case_id and not row["infrastructure_failure"] for row in group)
            == manifest["repetitions"]
            for case_id in observed_cells
        )
        bash_category_counts: defaultdict[str, int] = defaultdict(int)
        bash_category_seconds: defaultdict[str, float] = defaultdict(float)
        executable_counts: defaultdict[str, int] = defaultdict(int)
        lingrep_operation_counts: defaultdict[str, int] = defaultdict(int)
        runtime_tool_counts: defaultdict[str, int] = defaultdict(int)
        runtime_tool_process_seconds: defaultdict[str, float] = defaultdict(float)
        runtime_tool_active_wall_seconds: defaultdict[str, float] = defaultdict(float)
        outcome_counts: defaultdict[str, defaultdict[str, int]] = defaultdict(
            lambda: defaultdict(int)
        )
        for row in group:
            analysis = row["command_analysis"]
            for dimension, record in (row["verifier_outcomes"] or {}).items():
                if record["applicable"]:
                    outcome_counts[dimension][record["outcome"]] += 1
            for category, count in analysis[
                "bash_call_counts_by_shell_category"
            ].items():
                bash_category_counts[category] += count
            for category, seconds in analysis[
                "bash_roundtrip_seconds_by_shell_category"
            ].items():
                bash_category_seconds[category] += seconds
            for executable, count in analysis["executable_counts"].items():
                executable_counts[executable] += count
            for operation, count in analysis["lingrep_operation_counts"].items():
                lingrep_operation_counts[operation] += count
            for tool, count in analysis["runtime_tool_invocation_counts"].items():
                runtime_tool_counts[tool] += count
            for tool, seconds in analysis["runtime_tool_process_seconds"].items():
                runtime_tool_process_seconds[tool] += seconds
            for tool, seconds in analysis["runtime_tool_active_wall_seconds"].items():
                runtime_tool_active_wall_seconds[tool] += seconds
        summaries.append(
            {
                "schema": "lingrep-linguistic-eval.group-report/v1",
                "experiment_id": group[0]["experiment_id"],
                "model": model,
                "arm": arm,
                "trials": len(group),
                "planned_trials": plan["trials"],
                "matrix_complete": matrix_complete,
                "operational_successes": sum(
                    row["operational_success"] for row in group
                ),
                "infrastructure_failures": sum(
                    row["infrastructure_failure"] for row in group
                ),
                "exact_trials": sum(row["exact"] for row in group),
                "verdict_correct_trials": optional_sum(
                    [row["verdict_correct"] for row in group]
                ),
                "mean_evidence_f1": optional_mean(scored_values(group, "evidence_f1")),
                "query_replay_trials": sum(
                    row["query_valid"] is not None for row in group
                ),
                "query_valid_trials": sum(row["query_valid"] == 1.0 for row in group),
                "query_exact_trials": sum(row["query_exact"] == 1.0 for row in group),
                "mean_visible_query_f1": optional_mean(
                    scored_values(group, "visible_query_f1")
                ),
                "mean_hidden_query_f1": optional_mean(
                    scored_values(group, "hidden_query_f1")
                ),
                "hidden_query_replay_trials": sum(
                    row["hidden_query_f1"] is not None for row in group
                ),
                "hidden_query_scored_trials": sum(
                    hidden_scored(row, "query") for row in group
                ),
                "mean_hidden_query_f1_when_scored": optional_mean(
                    [row["hidden_query_f1"] for row in group if hidden_scored(row, "query")]
                ),
                "script_replay_trials": sum(
                    row["script_valid"] is not None for row in group
                ),
                "script_valid_trials": sum(
                    row["script_valid"] == 1.0 for row in group
                ),
                "script_exact_trials": sum(
                    row["script_exact"] == 1.0 for row in group
                ),
                "mean_visible_script_f1": optional_mean(
                    scored_values(group, "visible_script_f1")
                ),
                "mean_hidden_script_f1": optional_mean(
                    scored_values(group, "hidden_script_f1")
                ),
                "hidden_script_replay_trials": sum(
                    row["hidden_script_f1"] is not None for row in group
                ),
                "hidden_script_scored_trials": sum(
                    hidden_scored(row, "script") for row in group
                ),
                "mean_hidden_script_f1_when_scored": optional_mean(
                    [row["hidden_script_f1"] for row in group if hidden_scored(row, "script")]
                ),
                "verifier_outcome_trials": sum(
                    row["verifier_outcomes"] is not None for row in group
                ),
                "dimension_outcomes": {
                    dimension: dict(sorted(counts.items()))
                    for dimension, counts in sorted(outcome_counts.items())
                },
                "median_trial_elapsed_seconds": optional_median(
                    [row["trial_elapsed_seconds"] for row in group]
                ),
                "input_tokens_total": optional_sum(
                    [row["input_tokens"] for row in group]
                ),
                "input_tokens_median": optional_median(
                    [row["input_tokens"] for row in group]
                ),
                "cached_input_tokens_total": optional_sum(
                    [row["cached_input_tokens"] for row in group]
                ),
                "output_tokens_total": optional_sum(
                    [row["output_tokens"] for row in group]
                ),
                "output_tokens_median": optional_median(
                    [row["output_tokens"] for row in group]
                ),
                "trials_with_pattern_queries": sum(
                    bool(row["pattern_queries"]) for row in group
                ),
                "pattern_query_artifacts": sum(len(row["pattern_queries"]) for row in group),
                "analysis_runner_artifacts": sum(
                    row["analysis_runner"] is not None for row in group
                ),
                "artifact_complexity_median": {
                    "run_analysis": median_measures(
                        [
                            row["artifact_complexity"]["run_analysis"]
                            for row in group
                            if row["artifact_complexity"]["run_analysis"] is not None
                        ],
                        ("bytes", "non_blank_lines", "ast_nodes", "branches"),
                    ),
                    "query": median_measures(
                        [
                            row["artifact_complexity"]["query"]
                            for row in group
                            if row["artifact_complexity"]["query"] is not None
                        ],
                        ("bytes", "non_blank_lines", "definitions"),
                    ),
                },
                "reference_artifact_complexity_median": (
                    reference_artifact_complexity(root, arm, observed_cells)
                    if root is not None
                    else None
                ),
                "model_api_requests": optional_sum(
                    [row["model_api_requests"] for row in group]
                ),
                "model_api_seconds": optional_sum(
                    [row["model_api_seconds"] for row in group]
                ),
                "agent_elapsed_seconds": optional_sum(
                    [row["agent_elapsed_seconds"] for row in group]
                ),
                "agent_other_seconds": optional_sum(
                    [row["agent_other_seconds"] for row in group]
                ),
                "model_api_request_seconds_max": optional_max(
                    [row["model_api_request_seconds_max"] for row in group]
                ),
                "step_gap_seconds_max": optional_max(
                    [
                        row["longest_step_gap"]["seconds"]
                        if row["longest_step_gap"] is not None
                        else None
                        for row in group
                    ]
                ),
                "verifier_seconds": optional_sum(
                    [row["phase_seconds"]["verifier"] for row in group]
                ),
                "bash_tool_calls": sum(
                    row["command_analysis"]["bash_tool_calls"] for row in group
                ),
                "shell_commands": sum(
                    row["command_analysis"]["shell_commands"] for row in group
                ),
                "shell_parse_error_calls": sum(
                    row["command_analysis"]["parse_error_calls"] for row in group
                ),
                "timed_shell_calls": sum(
                    row["command_analysis"]["timed_calls"] for row in group
                ),
                "unaligned_shell_calls": sum(
                    row["command_analysis"]["unaligned_calls"] for row in group
                ),
                "bash_call_counts_by_shell_category": dict(
                    sorted(bash_category_counts.items())
                ),
                "bash_roundtrip_seconds_by_shell_category": {
                    key: round(value, 6)
                    for key, value in sorted(bash_category_seconds.items())
                },
                "shell_executable_counts": dict(sorted(executable_counts.items())),
                "lingrep_operation_counts": dict(sorted(lingrep_operation_counts.items())),
                "runtime_telemetry_trials": sum(
                    row["command_analysis"]["runtime_telemetry"] == "available"
                    for row in group
                ),
                "runtime_incomplete_invocations": sum(
                    row["command_analysis"]["runtime_incomplete_invocations"]
                    for row in group
                ),
                "runtime_tool_invocation_counts": dict(
                    sorted(runtime_tool_counts.items())
                ),
                "runtime_tool_process_seconds": {
                    key: round(value, 6)
                    for key, value in sorted(runtime_tool_process_seconds.items())
                },
                "runtime_tool_active_wall_seconds": {
                    key: round(value, 6)
                    for key, value in sorted(runtime_tool_active_wall_seconds.items())
                },
            }
        )
    return summaries


def changed_lines(before: str, after: str) -> int:
    """Added plus removed lines of the unified diff, headers excluded."""
    diff = difflib.unified_diff(before.splitlines(), after.splitlines(), lineterm="", n=0)
    return sum(
        1
        for line in diff
        if line.startswith(("+", "-")) and not line.startswith(("+++", "---"))
    )


def reference_artifacts(root: Path, arm: str, case_id: str) -> dict[str, str] | None:
    """The packaged reference `run-analysis` and, for lingrep, `query.lx` of a case."""
    catalog = root / "inputs" / "catalog.json"
    if not catalog.is_file():
        return None
    case = next(
        (case for case in json.loads(catalog.read_text()) if case["id"] == case_id), None
    )
    if case is None:
        return None
    if arm == "lingrep":
        return {
            "run-analysis": (HARBOR_ROOT / "oracle_lingrep_runner.py").read_text(),
            "query.lx": case["oracle_query"],
        }
    return {"run-analysis": mecab_reference_runner(case["oracle_mecab"])}


def revision_report(rows: list[dict], manifest: dict, root: Path) -> list[dict]:
    """Each revision trial beside the base trial of the same model, arm, case and
    attempt: the revision's time and requests, its scores, and how far its submitted
    artifacts moved from the packaged reference artifact it started from."""
    by_cell: dict[tuple[str, str, str, str, int], dict] = {
        (row["model"], row["arm"], row["base_case_id"], row["stage"], row["attempt"]): row
        for row in rows
    }
    report = []
    for (model, arm, case_id, stage, attempt), revision in sorted(by_cell.items()):
        if stage != "revision":
            continue
        base = by_cell.get((model, arm, case_id, "base", attempt))
        prior = reference_artifacts(root, arm, case_id)
        submitted_runner = revision["analysis_runner"]
        submitted_query = next(
            (q for q in revision["pattern_queries"] if q["path"] == "query.lx"), None
        )
        report.append(
            {
                "schema": REVISION_SCHEMA,
                "experiment_id": manifest["experiment_id"],
                "model": model,
                "arm": arm,
                "case_id": case_id,
                "attempt": attempt,
                "base_trial_id": base["trial_id"] if base else None,
                "revision_trial_id": revision["trial_id"],
                "revision_infrastructure_failure": revision["infrastructure_failure"],
                "revision_agent_elapsed_seconds": revision["agent_elapsed_seconds"],
                "revision_model_api_requests": revision["model_api_requests"],
                "revision_model_api_seconds": revision["model_api_seconds"],
                "revision_bash_tool_calls": revision["command_analysis"]["bash_tool_calls"],
                **{
                    f"{stage_name}_{metric}": (row[metric] if row else None)
                    for stage_name, row in (("base", base), ("revision", revision))
                    for metric in ("visible_script_f1", "hidden_script_f1", "script_exact")
                },
                "run_analysis_changed_lines": (
                    changed_lines(prior["run-analysis"], submitted_runner["source"])
                    if prior
                    and submitted_runner is not None
                    and submitted_runner["source"] is not None
                    else None
                ),
                "query_changed_lines": (
                    changed_lines(prior["query.lx"], submitted_query["source"])
                    if prior and "query.lx" in prior and submitted_query is not None
                    else None
                ),
            }
        )
    return report


def percentile(sorted_values: list[float], fraction: float) -> float:
    position = (len(sorted_values) - 1) * fraction
    lower, upper = math.floor(position), math.ceil(position)
    return sorted_values[lower] + (sorted_values[upper] - sorted_values[lower]) * (
        position - lower
    )


def paired_comparison(
    rows: list[dict], manifest: dict, *, resamples: int = 10_000, seed: int = 0
) -> list[dict]:
    """Per model and primary metric: each case's treatment-minus-control difference
    of per-arm repetition means, their mean, and a percentile bootstrap over cases.
    Trials excluded as infrastructure failures are counted per arm; a case with
    usable trials in one arm only is listed as unpaired."""
    arms = list(manifest.get("arms") or sorted({row["arm"] for row in rows}))
    if len(arms) != 2:
        raise ValueError(f"paired comparison requires two arms, got {arms}")
    treatment = "lingrep" if "lingrep" in arms else arms[0]
    control = next(arm for arm in arms if arm != treatment)
    comparisons = []
    for model in manifest["models"]:
        for metric in PRIMARY_METRICS:
            values: defaultdict[str, dict[str, list[float]]] = defaultdict(
                lambda: {treatment: [], control: []}
            )
            excluded = {treatment: 0, control: 0}
            for row in rows:
                if row["model"] != model:
                    continue
                if row["infrastructure_failure"]:
                    excluded[row["arm"]] += 1
                elif row[metric] is not None:
                    values[row["case_id"]][row["arm"]].append(row[metric])
            differences = {
                case: fmean(by_arm[treatment]) - fmean(by_arm[control])
                for case, by_arm in sorted(values.items())
                if by_arm[treatment] and by_arm[control]
            }
            if not differences and not any(excluded.values()):
                continue
            observed = list(differences.values())
            interval = None
            if len(observed) >= 2:
                rng = random.Random(seed)
                means = sorted(
                    fmean(rng.choices(observed, k=len(observed))) for _ in range(resamples)
                )
                interval = [percentile(means, 0.025), percentile(means, 0.975)]
            comparisons.append(
                {
                    "schema": PAIRED_SCHEMA,
                    "experiment_id": manifest["experiment_id"],
                    "model": model,
                    "metric": metric,
                    "treatment_arm": treatment,
                    "control_arm": control,
                    "cases": len(observed),
                    "case_differences": differences,
                    "mean_difference": fmean(observed) if observed else None,
                    "bootstrap_interval_95": interval,
                    "bootstrap_resamples": resamples,
                    "bootstrap_seed": seed,
                    "cases_favouring": {
                        treatment: sum(value > 0 for value in observed),
                        control: sum(value < 0 for value in observed),
                    },
                    "ties": sum(value == 0 for value in observed),
                    "unpaired_cases": sorted(set(values) - set(differences)),
                    "excluded_trials": excluded,
                }
            )
    return comparisons


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("experiment", type=Path)
    parser.add_argument(
        "--summary",
        action="store_true",
        help="emit one aggregate row per model and arm",
    )
    parser.add_argument(
        "--reconcile",
        action="store_true",
        help="emit the planned-versus-observed trial matrix instead of trial rows",
    )
    parser.add_argument(
        "--paired",
        action="store_true",
        help="emit one case-level paired comparison per model and primary metric",
    )
    parser.add_argument(
        "--revisions",
        action="store_true",
        help="emit one row per revision trial beside its base trial",
    )
    args = parser.parse_args()
    if args.reconcile:
        print(json.dumps(reconcile_experiment(args.experiment), sort_keys=True))
        return
    rows = summarize_experiment(args.experiment)
    if args.paired:
        _, manifest = find_experiment(args.experiment)
        rows = paired_comparison(rows, manifest)
    elif args.revisions:
        root, manifest = find_experiment(args.experiment)
        rows = revision_report(rows, manifest, root)
    elif args.summary:
        root, manifest = find_experiment(args.experiment)
        rows = summarize_groups(rows, manifest, root)
    for row in rows:
        print(json.dumps(row, sort_keys=True))


if __name__ == "__main__":
    main()
