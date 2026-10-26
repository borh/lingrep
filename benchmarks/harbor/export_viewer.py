#!/usr/bin/env python3
"""Export an immutable Harbor experiment for local ATIF trajectory review."""

from __future__ import annotations

import argparse
import json
import zipfile
from pathlib import Path, PurePosixPath

from command_analysis import analyze_trial
from experiment import discover_trials, find_experiment

TASK_FILES = ("instruction.md", "task.toml")
TASK_DIRECTORIES = ("environment",)
TRIAL_FILES = ("config.json", "result.json", "analysis.json", "analysis.md")
TRIAL_DIRECTORIES = ("verifier", "artifacts")
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)


def archive_name(*parts: str) -> str:
    path = PurePosixPath(*parts)
    if path.is_absolute() or ".." in path.parts:
        raise ValueError(f"unsafe archive path: {path}")
    return str(path)


def add_bytes(archive: zipfile.ZipFile, name: str, content: bytes) -> None:
    info = zipfile.ZipInfo(name, ZIP_TIMESTAMP)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o100644 << 16
    archive.writestr(info, content)


def add_file(archive: zipfile.ZipFile, source: Path, name: str) -> None:
    if source.is_file() and not source.is_symlink():
        add_bytes(archive, name, source.read_bytes())


def add_tree(archive: zipfile.ZipFile, source: Path, prefix: str) -> None:
    if not source.is_dir():
        return
    for path in sorted(
        candidate
        for candidate in source.rglob("*")
        if candidate.is_file() and not candidate.is_symlink()
    ):
        add_file(archive, path, archive_name(prefix, str(path.relative_to(source))))


def task_source(trial: Path) -> Path:
    config = json.loads((trial / "config.json").read_text())
    source = config.get("task", {}).get("path")
    if not isinstance(source, str):
        raise TypeError(f"trial does not record a local task path: {trial}")
    path = Path(source).resolve()
    if not path.is_dir():
        raise FileNotFoundError(f"recorded task path is unavailable: {path}")
    return path


def validate_trajectory(path: Path) -> None:
    trajectory = json.loads(path.read_text())
    version = trajectory.get("schema_version")
    if not isinstance(version, str) or not version.startswith("ATIF-v1."):
        raise ValueError(f"viewer export requires an ATIF-v1.x trajectory: {path}")
    if not isinstance(trajectory.get("steps"), list):
        raise TypeError(f"ATIF trajectory has no steps array: {path}")


def export_experiment(experiment: Path, destination: Path) -> int:
    root, manifest = find_experiment(experiment)
    if destination.exists():
        raise FileExistsError(f"viewer bundle already exists: {destination}")
    destination.parent.mkdir(parents=True, exist_ok=True)

    trials = [trial for trial in discover_trials(root) if trial.result is not None]
    if not trials:
        raise ValueError(f"experiment has no completed Harbor trials: {root}")

    task_sources: dict[str, Path] = {}
    trial_rows: list[tuple[str, Path]] = []
    for completed in trials:
        task_name = completed.result["task_name"]
        task = manifest.get("tasks", {}).get(task_name)
        if not isinstance(task, dict):
            raise TypeError(
                f"trial task is absent from experiment manifest: {task_name}"
            )
        task_key = f"{task['arm']}-{task['case_id']}"
        trial = completed.path
        trajectory = trial / "agent" / "trajectory.json"
        validate_trajectory(trajectory)
        source = task_source(trial)
        previous = task_sources.setdefault(task_key, source)
        if previous != source:
            raise ValueError(f"task {task_key} has multiple recorded source paths")
        trial_rows.append((task_key, trial))

    with zipfile.ZipFile(destination, "x") as archive:
        add_bytes(
            archive,
            "experiment.json",
            json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True).encode()
            + b"\n",
        )
        for task_key, source in sorted(task_sources.items()):
            for filename in TASK_FILES:
                add_file(
                    archive,
                    source / filename,
                    archive_name(task_key, filename),
                )
            for directory in TASK_DIRECTORIES:
                add_tree(
                    archive,
                    source / directory,
                    archive_name(task_key, directory),
                )

        for task_key, trial in sorted(trial_rows):
            run_key = trial.name
            prefix = archive_name(task_key, "jobs", run_key)
            for filename in TRIAL_FILES:
                add_file(archive, trial / filename, archive_name(prefix, filename))
            add_file(
                archive,
                trial / "agent" / "trajectory.json",
                archive_name(prefix, "agent", "trajectory.json"),
            )
            add_bytes(
                archive,
                archive_name(prefix, "command-analysis.json"),
                json.dumps(
                    analyze_trial(trial), ensure_ascii=False, indent=2, sort_keys=True
                ).encode()
                + b"\n",
            )
            for directory in TRIAL_DIRECTORIES:
                add_tree(
                    archive,
                    trial / directory,
                    archive_name(prefix, directory),
                )
    return len(trial_rows)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("experiment", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    count = export_experiment(args.experiment, args.output)
    print(f"exported {count} completed trials to {args.output}")


if __name__ == "__main__":
    main()
