"""Read one experiment directory: its manifest, Harbor's job/trial layout, and the planned matrix."""

from __future__ import annotations

import hashlib
import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

EXPERIMENT_SCHEMA = "lingrep-linguistic-eval.experiment/v2"
# Harbor records these and still runs the verifier, which scores the trial.
AGENT_SCOPE_EXCEPTIONS = frozenset({"AgentTimeoutError", "NonZeroAgentExitCodeError"})
RECONCILIATION_SCHEMA = "lingrep-linguistic-eval.experiment-reconciliation/v2"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


OUTCOME_SCHEMA = "lingrep-linguistic-eval.verifier-outcome/v1"


@dataclass(frozen=True)
class Trial:
    job_name: str
    path: Path
    config: dict
    result: dict | None
    error: str | None
    outcomes: dict | None

    @property
    def trial_name(self) -> str:
        return self.config["trial_name"]


def find_experiment(path: Path) -> tuple[Path, dict]:
    current = path.resolve()
    if current.is_file():
        current = current.parent
    for candidate in (current, *current.parents):
        path = candidate / "experiment.json"
        if path.is_file():
            manifest = json.loads(path.read_text())
            if manifest.get("schema") != EXPERIMENT_SCHEMA:
                raise ValueError(
                    f"unsupported experiment manifest schema {manifest.get('schema')!r} "
                    f"in {path}; expected {EXPERIMENT_SCHEMA}"
                )
            return candidate, manifest
    raise FileNotFoundError(f"no experiment.json found at or above {path}")


def discover_trials(root: Path) -> list[Trial]:
    """Trials under `root/<model>/<job>/<trial>`, identified by the `job_name` and
    `trial_name` in their `config.json`; an unreadable `result.json` yields
    `result=None` with the reason in `error`."""
    trials = []
    for job_config_path in sorted(root.glob("*/*/config.json")):
        job_config = json.loads(job_config_path.read_text())
        if not isinstance(job_config, dict) or "job_name" not in job_config:
            continue
        for trial_config_path in sorted(job_config_path.parent.glob("*/config.json")):
            trial_config = json.loads(trial_config_path.read_text())
            if not isinstance(trial_config, dict) or "trial_name" not in trial_config:
                continue
            trial_dir = trial_config_path.parent
            result_path = trial_dir / "result.json"
            result = None
            error = None
            if not result_path.is_file():
                error = "no result.json"
            else:
                try:
                    result = json.loads(result_path.read_text())
                except json.JSONDecodeError as problem:
                    error = f"result.json: {problem}"
                else:
                    if not isinstance(result, dict) or "task_name" not in result:
                        result = None
                        error = "result.json has no task_name"
            trials.append(
                Trial(
                    job_config["job_name"],
                    trial_dir,
                    trial_config,
                    result,
                    error,
                    verifier_outcomes(trial_dir),
                )
            )
    return sorted(
        trials,
        key=lambda trial: (
            (trial.result or {}).get("started_at") or "",
            trial.trial_name,
        ),
    )


def requested_model(trial: Trial, manifest: dict) -> str:
    qualified = (trial.config.get("agent") or {}).get("model_name")
    expected = {f"openai/{model}": model for model in manifest["models"]}
    if qualified not in expected:
        raise ValueError(
            f"trial requested model {qualified!r} is absent from experiment manifest"
        )
    return expected[qualified]


def verifier_outcomes(trial_root: Path) -> dict | None:
    path = trial_root / "verifier" / "outcome.json"
    if not path.is_file():
        return None
    record = json.loads(path.read_text())
    if record.get("schema") != OUTCOME_SCHEMA:
        raise ValueError(f"unknown verifier outcome schema in {path}")
    return record["dimensions"]


def infrastructure_failure(result: dict, outcomes: dict | None) -> bool:
    """A trial whose scores are not the agent's doing: a Harbor exception outside the
    agent's own run, no verifier rewards, or a verifier-owned input the verifier could
    not read for any applicable dimension."""
    exception = (result.get("exception_info") or {}).get("exception_type")
    if exception is not None and exception not in AGENT_SCOPE_EXCEPTIONS:
        return True
    if (result.get("verifier_result") or {}).get("rewards") is None:
        return True
    return any(
        record["applicable"] and record["outcome"] == "infrastructure_failure"
        for record in (outcomes or {}).values()
    )


def reconcile(manifest: dict, trials: list[Trial]) -> dict:
    tasks = manifest["tasks"]
    planned = manifest["repetitions"]
    observed: defaultdict[tuple[str, str], list[str]] = defaultdict(list)
    failures: defaultdict[tuple[str, str], int] = defaultdict(int)
    incomplete = []
    for trial in trials:
        if trial.result is None:
            incomplete.append(
                {
                    "job_name": trial.job_name,
                    "trial_name": trial.trial_name,
                    "error": trial.error,
                }
            )
            continue
        task_name = trial.result["task_name"]
        if task_name not in tasks:
            raise ValueError(
                f"trial task {task_name!r} is absent from experiment manifest"
            )
        key = (requested_model(trial, manifest), task_name)
        observed[key].append(trial.trial_name)
        failures[key] += infrastructure_failure(trial.result, trial.outcomes)

    cells = []
    for model in manifest["models"]:
        for task_name, task in tasks.items():
            names = observed.get((model, task_name), [])
            scored = len(names) - failures[(model, task_name)]
            if not names:
                status = "missing"
            elif scored == planned:
                status = "complete"
            elif scored < planned:
                status = "partial"
            else:
                status = "surplus"
            cells.append(
                {
                    "model": model,
                    "task_name": task_name,
                    "arm": task["arm"],
                    "case_id": task["case_id"],
                    "planned_repetitions": planned,
                    "observed_repetitions": len(names),
                    "infrastructure_failures": failures[(model, task_name)],
                    "status": status,
                    "trial_names": names,
                }
            )
    return {
        "schema": RECONCILIATION_SCHEMA,
        "experiment_id": manifest["experiment_id"],
        "planned_trials": len(cells) * planned,
        "observed_trials": sum(len(names) for names in observed.values()),
        "complete": all(cell["status"] == "complete" for cell in cells)
        and not incomplete,
        "cells": cells,
        "incomplete_trials": incomplete,
    }
