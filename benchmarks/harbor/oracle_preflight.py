#!/usr/bin/env python3
"""Replay a packaged oracle catalog before launching model trials."""

from __future__ import annotations

import argparse
import json
import subprocess
import tempfile
from pathlib import Path


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)


def require_oracle_reward(
    case_id: str, reward: dict, *, has_query: bool, has_hidden: bool
) -> None:
    required = {
        "evidence exact": reward.get("exact"),
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
    """What this generated task scores, read from the generated files.

    The replayed catalog is what the task scores, so it decides which rewards
    the oracle must reach. Generation writes the replay configuration, the
    oracle catalog, and the contract paragraph together; reading them back from
    the task rather than from an arm name or a catalog field is what keeps this
    gate from drifting from what the verifier will ask for.
    """
    config = json.loads((task / "tests" / "analysis-replay.json").read_text())
    has_query = (task / "tests" / "query-replay.json").is_file()
    packaged = (task / "solution" / "query.lx").is_file()
    if has_query and not packaged:
        raise RuntimeError(
            f"{task.name} replays a catalog but packages no oracle query.lx"
        )
    if packaged and not has_query:
        raise RuntimeError(
            f"{task.name} packages an oracle query.lx that is never replayed"
        )
    return has_query, config["has_hidden_driver"]


def preflight(generated: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="lingrep-harbor-oracle-") as temporary:
        temporary_root = Path(temporary)
        tasks = [
            (arm, task)
            for arm in ("lingrep", "mecab")
            for task in sorted((generated / arm).iterdir())
        ]
        for arm, task in tasks:
            has_query, has_hidden = replay_dimensions(task)
            if not has_query:
                continue

            logs = temporary_root / arm / task.name / "logs"
            logs.mkdir(parents=True)
            logs.chmod(0o777)
            run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "-v",
                    f"{task / 'solution'}:/solution:ro",
                    "-v",
                    f"{logs}:/logs",
                    f"{arm}-linguistic-eval:latest",
                    "/solution/solve.sh",
                ]
            )
            image = f"{arm}-harbor-oracle-verifier:latest"
            run(["docker", "build", "-q", "-t", image, str(task / "tests")])
            run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "-v",
                    f"{logs}:/logs",
                    image,
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
