#!/usr/bin/env python3
"""Replay a packaged oracle catalog before launching model trials."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
import tomllib
from pathlib import Path

from generate_dataset import REPLAY_TIMEOUT_SEC, verifier_timeout_sec


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)


def build(context: Path) -> str:
    """Image id of a `podman build` of `context`; no tag, so concurrent preflights
    cannot select each other's images."""
    return subprocess.run(
        ["podman", "build", "-q", str(context)],
        check=True,
        text=True,
        capture_output=True,
    ).stdout.strip()


def require_oracle_reward(
    case_id: str, reward: dict, *, has_query: bool, has_hidden: bool
) -> None:
    required = {
        "evidence exact": reward.get("exact"),
        "verdict correct": reward.get("verdict_correct"),
        "script valid": reward.get("script_valid"),
        "visible script exact": reward.get("visible_script_exact"),
        "script exact": reward.get("script_exact"),
    }
    if has_query:
        required.update(
            {
                "query valid": reward.get("query_valid"),
                "visible query exact": reward.get("visible_query_exact"),
                "query exact": reward.get("query_exact"),
            }
        )
    if has_hidden:
        required["hidden script exact"] = reward.get("hidden_script_exact")
        if has_query:
            required["hidden query exact"] = reward.get("hidden_query_exact")
    failures = [name for name, value in required.items() if value != 1.0]
    if failures:
        raise RuntimeError(
            f"oracle preflight failed for {case_id}: {', '.join(failures)}; "
            f"reward={json.dumps(reward, sort_keys=True)}"
        )


def replay_dimensions(task: Path) -> tuple[bool, bool]:
    """`(has_query, has_hidden)` from the generated task's replay configuration."""
    config = json.loads((task / "tests" / "analysis-replay.json").read_text())
    query_config_path = task / "tests" / "query-replay.json"
    has_query = query_config_path.is_file()
    has_hidden = config["has_hidden_driver"]
    configs = [config]
    if has_query:
        configs.append(json.loads(query_config_path.read_text()))
    if any(replay.get("replay_timeout_sec") != REPLAY_TIMEOUT_SEC for replay in configs):
        raise RuntimeError(f"{task.name} declares a replay timeout the generator did not set")
    expected = float(verifier_timeout_sec(has_query=has_query, has_hidden=has_hidden))
    declared = tomllib.loads((task / "task.toml").read_text())["verifier"]["timeout_sec"]
    if declared != expected:
        raise RuntimeError(
            f"{task.name} verifier timeout {declared} does not cover its {expected} s replay budget"
        )
    return has_query, has_hidden


def preflight(generated: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lingrep-harbor-oracle-") as temporary:
        temporary_root = Path(temporary)
        tasks = [
            (arm_root.name, task)
            for arm_root in sorted(generated.iterdir())
            for task in sorted(arm_root.iterdir())
        ]
        dimensions = {task: replay_dimensions(task) for _, task in tasks}
        for arm, task in tasks:
            if arm != "lingrep":
                continue
            catalog = task / "solution" / "query.lx"
            try:
                result = subprocess.run(
                    [
                        "ling",
                        "check",
                        "--patterns",
                        str(catalog),
                        "--schema",
                        "unidic-morphology",
                        "--json",
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                )
            except subprocess.CalledProcessError as error:
                raise RuntimeError(
                    f"invalid oracle catalog {catalog}: {error.stderr}"
                ) from error
            binding = json.loads(result.stdout)["data"]["binding"]
            answerable = json.loads((task / "tests" / "verdict.json").read_text())["answerable"]
            if binding.get("refusal") or (binding["unavailable"] and answerable):
                raise RuntimeError(f"invalid oracle catalog {catalog}: {binding}")
        for arm, task in tasks:
            has_query, has_hidden = dimensions[task]
            logs = temporary_root / arm / task.name / "logs"
            logs.mkdir(parents=True)
            logs.chmod(0o777)
            environment = build(task / "environment")
            run(
                [
                    "podman",
                    "run",
                    "--rm",
                    "-v",
                    f"{task / 'solution'}:/solution:ro",
                    "-v",
                    f"{logs}:/logs",
                    environment,
                    "/solution/solve.sh",
                ]
            )
            verifier = build(task / "tests")
            run(
                [
                    "podman",
                    "run",
                    "--rm",
                    "-v",
                    f"{logs}:/logs",
                    verifier,
                    "/tests/test.sh",
                ]
            )
            reward = json.loads((logs / "verifier" / "reward.json").read_text())
            require_oracle_reward(
                task.name,
                reward,
                has_query=has_query,
                has_hidden=has_hidden,
            )
            print(f"oracle preflight passed: {arm}/{task.name}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("generated", type=Path)
    args = parser.parse_args()
    preflight(args.generated.resolve())


if __name__ == "__main__":
    main()
