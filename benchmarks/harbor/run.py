#!/usr/bin/env python3
"""Build/load images and launch a balanced paired Harbor job."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import subprocess
from datetime import UTC, datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent
HARBOR_COMMIT = "1b1dbc43729edff705071a797bb0f98a90cf62f5"
DEFAULT_AGENT_TIMEOUT_SEC = 3600
DEFAULT_MODELS = ["qwen3.8-27b-fp8:think"]
DISCOVERY_GUIDE = ROOT / "guides" / "analyzer-guide.md"


def run(command: list[str], **kwargs) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True, **kwargs)


def output(command: list[str]) -> str:
    return subprocess.run(
        command, check=True, text=True, capture_output=True
    ).stdout.strip()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_experiment_manifest(directory: Path, manifest: dict, *, resume: bool) -> None:
    path = directory / "experiment.json"
    if path.exists():
        if not resume:
            raise FileExistsError(
                f"experiment already exists: {directory}; pass --resume to continue it"
            )
        existing = json.loads(path.read_text())
        comparable_existing = {
            key: value for key, value in existing.items() if key != "created_at"
        }
        comparable_new = {
            key: value for key, value in manifest.items() if key != "created_at"
        }
        if comparable_existing != comparable_new:
            raise ValueError(
                f"experiment manifest does not match existing experiment: {directory}"
            )
        return
    directory.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )


def snapshot_experiment_inputs(
    directory: Path, catalog: Path, review_evidence: Path | None
) -> dict:
    inputs = directory / "inputs"
    inputs.mkdir(parents=True, exist_ok=True)
    catalog_snapshot = inputs / "catalog.json"
    if (
        catalog_snapshot.exists()
        and catalog_snapshot.read_bytes() != catalog.read_bytes()
    ):
        raise ValueError("experiment catalog snapshot does not match requested catalog")
    if not catalog_snapshot.exists():
        shutil.copy2(catalog, catalog_snapshot)

    evidence_hashes = {}
    if review_evidence is not None:
        if not review_evidence.is_dir():
            raise ValueError(f"review evidence is not a directory: {review_evidence}")
        evidence_snapshot = inputs / "review"
        sources = {
            source.name: source
            for source in review_evidence.iterdir()
            if source.is_file() and not source.is_symlink()
        }
        if evidence_snapshot.exists():
            existing = {path.name for path in evidence_snapshot.iterdir()}
            if existing != set(sources):
                raise ValueError("experiment review snapshot file set does not match")
        else:
            evidence_snapshot.mkdir()
        for source in sorted(sources.values()):
            destination = evidence_snapshot / source.name
            if destination.exists() and destination.read_bytes() != source.read_bytes():
                raise ValueError(
                    f"experiment review snapshot does not match {source.name}"
                )
            if not destination.exists():
                shutil.copy2(source, destination)
            evidence_hashes[source.name] = sha256(source)
    return {
        "catalog_snapshot": str(catalog_snapshot.relative_to(directory)),
        "review_evidence_sha256": evidence_hashes,
    }


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return parsed


def safe_component(value: str, *, field: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", value):
        raise ValueError(f"{field} is not a safe path component: {value!r}")
    return value


def model_directory(model: str) -> str:
    readable = re.sub(r"[^A-Za-z0-9._-]+", "-", model).strip("-.") or "model"
    if readable == model:
        return readable
    suffix = hashlib.sha256(model.encode()).hexdigest()[:8]
    return f"{readable}-{suffix}"


def benchmark_commit() -> str:
    dirty = output(["git", "status", "--porcelain", "--untracked-files=normal"])
    if dirty:
        raise RuntimeError(
            "refusing to start an experiment from a dirty worktree; commit the benchmark state first"
        )
    return output(["git", "rev-parse", "HEAD"])


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", action="append", default=[])
    parser.add_argument(
        "--api-base",
        required=True,
        help="OpenAI-compatible endpoint, e.g. http://localhost:8080/v1",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=8272026,
        help="Controls arm order only; it is not sent to the model endpoint",
    )
    parser.add_argument("--attempts", type=positive_int, default=1)
    parser.add_argument(
        "--agent-timeout-sec",
        type=positive_int,
        default=DEFAULT_AGENT_TIMEOUT_SEC,
        help="Per-trial agent deadline, recorded in the experiment manifest",
    )
    parser.add_argument(
        "--experiment",
        default=None,
        help="Stable experiment identity; defaults to a UTC timestamp",
    )
    parser.add_argument(
        "--catalog", type=Path, default=ROOT / "catalogs" / "smoke.json"
    )
    parser.add_argument(
        "--review-evidence",
        type=Path,
        help="Private review directory to snapshot with the experiment",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="Continue only when the existing experiment manifest matches",
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--case", action="append", default=[])
    parser.add_argument("--arm", action="append", choices=["lingrep", "mecab"], default=[])
    args = parser.parse_args()
    os.environ["PATH"] = f"{ROOT / 'bin'}:{os.environ['PATH']}"
    os.environ.setdefault("OPENAI_API_KEY", "local-benchmark")

    catalog = args.catalog.resolve()
    models = args.model or DEFAULT_MODELS
    revision = benchmark_commit()
    experiment_id = args.experiment or datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    safe_component(experiment_id, field="experiment id")
    experiment_root = ROOT / "jobs" / experiment_id
    if experiment_root.exists() and not args.resume:
        raise FileExistsError(
            f"experiment already exists: {experiment_root}; pass --resume to continue it"
        )
    snapshots = snapshot_experiment_inputs(
        experiment_root,
        catalog,
        args.review_evidence.resolve() if args.review_evidence else None,
    )

    image_outputs: dict[str, str] = {}
    for flake_output in ("linguistic-eval-lingrep-image", "linguistic-eval-mecab-image"):
        archive = ROOT / f".{flake_output}.tar.gz"
        if archive.exists():
            image_outputs[flake_output] = str(archive.resolve())
    if not args.no_build:
        for flake_output in ("linguistic-eval-lingrep-image", "linguistic-eval-mecab-image"):
            archive = ROOT / f".{flake_output}.tar.gz"
            run(["nix", "build", f".#{flake_output}", "--out-link", str(archive)])
            image_outputs[flake_output] = str(archive.resolve())
            with archive.open("rb") as image:
                run(["docker", "load"], stdin=image)
    base_image_ids = {
        arm: output(
            [
                "docker",
                "image",
                "inspect",
                f"{arm}-linguistic-eval:latest",
                "--format",
                "{{.Id}}",
            ]
        )
        for arm in ("lingrep", "mecab")
    }
    base_image_refs = {
        arm: image_id if image_id.startswith("sha256:") else f"sha256:{image_id}"
        for arm, image_id in base_image_ids.items()
    }
    generated = ROOT / ".generated" / experiment_id
    run(
        [
            "python3",
            str(ROOT / "generate_dataset.py"),
            "--output",
            str(generated),
            "--catalog",
            str(catalog),
            "--agent-timeout-sec",
            str(args.agent_timeout_sec),
            "--lingrep-image",
            base_image_refs["lingrep"],
            "--mecab-image",
            base_image_refs["mecab"],
        ]
    )
    run(["python3", str(ROOT / "oracle_preflight.py"), str(generated)])
    catalog_cases = json.loads(catalog.read_text())
    catalog_by_id = {case["id"]: case for case in catalog_cases}
    cases = args.case or list(catalog_by_id)
    unknown_cases = sorted(set(cases) - set(catalog_by_id))
    if unknown_cases:
        raise ValueError(f"unknown cases: {unknown_cases}")
    rng = random.Random(args.seed)
    arms = args.arm or ["lingrep", "mecab"]
    image_ids = {arm: base_image_ids[arm] for arm in arms}
    tasks = {
        f"lingrep/{arm}-{case_id}": {
            "arm": arm,
            "case_id": case_id,
            "capability": catalog_by_id[case_id]["capability"],
        }
        for case_id in cases
        for arm in arms
    }
    manifest = {
        "schema": "lingrep-linguistic-eval.experiment/v1",
        "experiment_id": experiment_id,
        "created_at": datetime.now(UTC).isoformat(),
        "catalog": str(catalog),
        "catalog_sha256": sha256(catalog),
        **snapshots,
        "guide_sha256": {
            arm: sha256(DISCOVERY_GUIDE) for arm in arms
        },
        "harbor_commit": HARBOR_COMMIT,
        "benchmark_commit": revision,
        "models": models,
        "api_base": args.api_base,
        "attempts": args.attempts,
        "agent_timeout_sec": args.agent_timeout_sec,
        "controller_seed": args.seed,
        "arms": arms,
        "cases": cases,
        "image_outputs": image_outputs,
        "image_ids": image_ids,
        "tasks": tasks,
    }
    write_experiment_manifest(experiment_root, manifest, resume=args.resume)

    for model in models:
        job_root = experiment_root / model_directory(model)
        job_root.mkdir(parents=True, exist_ok=True)
        for case in cases:
            order = arms[:]
            rng.shuffle(order)
            for arm in order:
                run(
                    [
                        "uv",
                        "run",
                        "harbor",
                        "run",
                        "-p",
                        str(generated / arm / case),
                        "-a",
                        "terminus-2",
                        "-m",
                        f"openai/{model}",
                        "--agent-kwarg",
                        f"api_base={args.api_base}",
                        "--jobs-dir",
                        str(job_root),
                        "--job-name",
                        f"{case}-{arm}",
                        "--n-attempts",
                        str(args.attempts),
                        "--n-concurrent",
                        "1",
                        "--yes",
                    ],
                    cwd=ROOT,
                )


if __name__ == "__main__":
    main()
