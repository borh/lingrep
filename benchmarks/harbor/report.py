#!/usr/bin/env python3
"""Emit one analysis-ready row for each Harbor trial in an experiment."""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from statistics import fmean, median

from command_analysis import analyze_trial

MAX_RUNNER_SOURCE_BYTES = 1024 * 1024


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def elapsed(started: str | None, finished: str | None) -> float | None:
    if not started or not finished:
        return None
    return (
        datetime.fromisoformat(finished) - datetime.fromisoformat(started)
    ).total_seconds()


def find_experiment(path: Path) -> tuple[Path, dict]:
    current = path.resolve()
    if current.is_file():
        current = current.parent
    for candidate in (current, *current.parents):
        manifest = candidate / "experiment.json"
        if manifest.is_file():
            return candidate, json.loads(manifest.read_text())
    raise FileNotFoundError(f"no experiment.json found at or above {path}")


def load_trials(root: Path) -> list[tuple[Path, dict]]:
    trials = []
    for path in root.rglob("result.json"):
        result = json.loads(path.read_text())
        if "trial_name" in result and "task_name" in result:
            trials.append((path, result))
    return sorted(
        trials,
        key=lambda item: (item[1].get("started_at") or "", item[1]["trial_name"]),
    )


def requested_model(result_path: Path, manifest: dict) -> str:
    config_path = result_path.parent / "config.json"
    config = json.loads(config_path.read_text())
    qualified = (config.get("agent") or {}).get("model_name")
    expected = {f"openai/{model}": model for model in manifest["models"]}
    if qualified not in expected:
        raise ValueError(
            f"trial requested model {qualified!r} is absent from experiment manifest"
        )
    return expected[qualified]


def optional_sum(values: list[int | float | None]) -> int | float | None:
    present = [value for value in values if value is not None]
    return sum(present) if present else None


def optional_median(values: list[int | float | None]) -> int | float | None:
    present = [value for value in values if value is not None]
    return median(present) if present else None


def optional_mean(values: list[int | float | None]) -> float | None:
    present = [value for value in values if value is not None]
    return fmean(present) if present else None


def pattern_query_artifacts(trial_root: Path) -> list[dict[str, str]]:
    """Return the catalog sources submitted with a lingrep trial."""
    submission = trial_root / "artifacts" / "submission"
    if not submission.is_dir():
        return []
    queries = []
    for path in sorted(submission.rglob("*.lx")):
        source = path.read_text(errors="replace")
        queries.append(
            {
                "path": str(path.relative_to(submission)),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "source": source,
            }
        )
    return queries


def analysis_runner_artifact(trial_root: Path) -> dict | None:
    """Return the submitted executable analysis entrypoint for audit."""
    path = trial_root / "artifacts" / "submission" / "run-analysis"
    if path.is_symlink() or not path.is_file():
        return None
    size = path.stat().st_size
    return {
        "path": "run-analysis",
        "sha256": file_sha256(path),
        "size_bytes": size,
        "source": (
            path.read_text(errors="replace")
            if size <= MAX_RUNNER_SOURCE_BYTES
            else None
        ),
    }


def summarize_experiment(path: Path) -> list[dict]:
    root, manifest = find_experiment(path)
    tasks = manifest["tasks"]
    attempts: defaultdict[tuple[str, str], int] = defaultdict(int)
    rows = []
    for result_path, result in load_trials(root):
        task_name = result["task_name"]
        if task_name not in tasks:
            raise ValueError(
                f"trial task {task_name!r} is absent from experiment manifest"
            )
        task = tasks[task_name]
        agent_info = result.get("agent_info") or {}
        model_info = agent_info.get("model_info") or {}
        model = requested_model(result_path, manifest)
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
        command_seconds = (
            sum(command_summary["bash_roundtrip_seconds_by_shell_category"].values())
            if commands["trajectory_sha256"] is not None
            and command_summary["timed_calls"]
            == command_summary["nonempty_bash_tool_calls"]
            else None
        )
        pattern_queries = pattern_query_artifacts(result_path.parent)
        rows.append(
            {
                "schema": "lingrep-linguistic-eval.trial-report/v1",
                "experiment_id": manifest["experiment_id"],
                "model": model,
                "reported_model": model_info.get("name"),
                "agent": agent_info.get("name"),
                "arm": task["arm"],
                "case_id": task["case_id"],
                "capability": task["capability"],
                "attempt": attempts[attempt_key],
                "trial_id": result.get("id"),
                "trial_name": result["trial_name"],
                "task_checksum": result.get("task_checksum"),
                "trial_elapsed_seconds": elapsed(
                    result.get("started_at"), result.get("finished_at")
                ),
                "agent_elapsed_seconds": agent_elapsed,
                "evidence_f1": rewards.get("evidence_f1", 0.0),
                "exact": rewards.get("exact", 0.0),
                "format": rewards.get("format", 0.0),
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
                "command_analysis": command_summary,
                "runtime_invocations": commands["runtime_invocations"],
                "pattern_queries": pattern_queries,
                "analysis_runner": analysis_runner_artifact(result_path.parent),
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


def summarize_groups(rows: list[dict]) -> list[dict]:
    groups: defaultdict[tuple[str, str], list[dict]] = defaultdict(list)
    for row in rows:
        groups[(row["model"], row["arm"])].append(row)
    summaries = []
    for (model, arm), group in sorted(groups.items()):
        bash_category_counts: defaultdict[str, int] = defaultdict(int)
        bash_category_seconds: defaultdict[str, float] = defaultdict(float)
        executable_counts: defaultdict[str, int] = defaultdict(int)
        lingrep_operation_counts: defaultdict[str, int] = defaultdict(int)
        runtime_tool_counts: defaultdict[str, int] = defaultdict(int)
        runtime_tool_process_seconds: defaultdict[str, float] = defaultdict(float)
        runtime_tool_active_wall_seconds: defaultdict[str, float] = defaultdict(float)
        for row in group:
            analysis = row["command_analysis"]
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
                "operational_successes": sum(
                    row["operational_success"] for row in group
                ),
                "exact_trials": sum(row["exact"] for row in group),
                "mean_evidence_f1": fmean(row["evidence_f1"] for row in group),
                "query_replay_trials": sum(
                    row["query_valid"] is not None for row in group
                ),
                "query_valid_trials": sum(row["query_valid"] == 1.0 for row in group),
                "query_exact_trials": sum(row["query_exact"] == 1.0 for row in group),
                "mean_visible_query_f1": optional_mean(
                    [row["visible_query_f1"] for row in group]
                ),
                "mean_hidden_query_f1": optional_mean(
                    [row["hidden_query_f1"] for row in group]
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
                    [row["visible_script_f1"] for row in group]
                ),
                "mean_hidden_script_f1": optional_mean(
                    [row["hidden_script_f1"] for row in group]
                ),
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


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("experiment", type=Path)
    parser.add_argument(
        "--summary",
        action="store_true",
        help="emit one aggregate row per model and arm",
    )
    args = parser.parse_args()
    rows = summarize_experiment(args.experiment)
    if args.summary:
        rows = summarize_groups(rows)
    for row in rows:
        print(json.dumps(row, sort_keys=True))


if __name__ == "__main__":
    main()
