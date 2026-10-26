#!/usr/bin/env python3
"""Build/load images and launch a balanced paired Harbor job."""

from __future__ import annotations

import argparse
import fcntl
import filecmp
import hashlib
import json
import os
import platform
import random
import re
import shutil
import subprocess
from datetime import UTC, datetime
from pathlib import Path

from experiment import EXPERIMENT_SCHEMA
from experiment import sha256_file as sha256
from generate_dataset import (
    COMMON_ANALYSIS,
    PROTOCOL_SCHEMA,
    PROTOCOLS,
    generate,
    load_catalog,
    select_cases,
    task_specs,
)
from oracle_preflight import preflight
from review import validate_review

ROOT = Path(__file__).resolve().parent
DEFAULT_AGENT_TIMEOUT_SEC = 3600
DEFAULT_MODELS = ["qwen3.8-27b-fp8:think"]
DISCOVERY_GUIDE = ROOT / "guides" / "analyzer-guide.md"


def run(command: list[str], **kwargs) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True, **kwargs)


def output(command: list[str], **kwargs) -> str:
    return subprocess.run(
        command, check=True, text=True, capture_output=True, **kwargs
    ).stdout.strip()


def dictionary_sources(image_refs: dict[str, str]) -> dict:
    sources = {}
    for arm, image in image_refs.items():
        labels = json.loads(output([
            "podman", "image", "inspect", image,
            "--format", "{{json .Config.Labels}}",
        ])) or {}
        try:
            sources[arm] = json.loads(labels.get("org.lingrep.unidic-source", "null"))
        except (TypeError, json.JSONDecodeError):
            sources[arm] = None
    if (
        any(
            not isinstance(source, dict)
            or any(
                not isinstance(source.get(key), str) or not source[key]
                for key in ("url", "hash")
            )
            for source in sources.values()
        )
        or sources["lingrep"] != sources["mecab"]
    ):
        raise ValueError(
            f"dictionary source mismatch: lingrep={sources['lingrep']!r}; "
            f"mecab={sources['mecab']!r}; rebuild both benchmark images"
        )
    return sources


def check_experiment_manifest(directory: Path, manifest: dict, *, resume: bool) -> bool:
    """True when a matching manifest exists; raises if one exists without `resume` or differs."""
    path = directory / "experiment.json"
    if not path.exists():
        return False
    if not resume:
        raise FileExistsError(
            f"experiment already exists: {directory}; pass --resume to continue it"
        )
    existing = json.loads(path.read_text())
    comparable_existing = {
        key: value for key, value in existing.items() if key not in {"created_at", "image_outputs"}
    }
    comparable_new = {
        key: value for key, value in manifest.items() if key not in {"created_at", "image_outputs"}
    }
    if comparable_existing != comparable_new:
        raise ValueError(
            f"experiment manifest does not match existing experiment: {directory}"
        )
    return True


def write_experiment_manifest(directory: Path, manifest: dict, *, resume: bool) -> None:
    if check_experiment_manifest(directory, manifest, resume=resume):
        return
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "experiment.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )


def hold_experiment(directory: Path):
    """Take the experiment's controller lock; the returned file keeps it until closed."""
    directory.mkdir(parents=True, exist_ok=True)
    lock = (directory / "controller.lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        raise RuntimeError(
            f"another controller holds experiment {directory.name}; wait for it to finish"
        ) from None
    return lock


def tree_differences(expected: Path, actual: Path) -> list[str]:
    comparison = filecmp.dircmp(expected, actual, ignore=[], hide=[])
    differences = []

    def walk(node: filecmp.dircmp, prefix: Path) -> None:
        for name in (*node.left_only, *node.right_only, *node.funny_files):
            differences.append(str(prefix / name))
        for name in node.common_files:
            if not filecmp.cmp(Path(node.left) / name, Path(node.right) / name, shallow=False):
                differences.append(str(prefix / name))
        for name, child in node.subdirs.items():
            walk(child, prefix / name)

    walk(comparison, Path())
    return sorted(differences)


def publish_generated_inputs(
    generated: Path,
    catalog: Path,
    *,
    agent_timeout_sec: int,
    image_refs: dict[str, str],
    protocol: str,
    cases: list[str],
    arms: list[str],
) -> None:
    """Keep an existing `generated` tree only when a fresh generation is byte-identical."""
    staging = generated.with_name(f"{generated.name}.staging-{os.getpid()}")
    try:
        generate(
            staging,
            catalog,
            agent_timeout_sec=agent_timeout_sec,
            lingrep_image=image_refs["lingrep"],
            mecab_image=image_refs["mecab"],
            protocol=protocol,
            cases=cases,
            arms=arms,
        )
        if not generated.exists():
            staging.rename(generated)
            return
        differences = tree_differences(generated, staging)
        if differences:
            raise ValueError(
                f"generated inputs differ from {generated}: {', '.join(differences[:5])}"
            )
    finally:
        if staging.exists():
            shutil.rmtree(staging)


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
    review_status = "unreviewed"
    if review_evidence is not None:
        validate_review(evidence_snapshot, catalog_snapshot)
        review_status = "reviewed"
    return {
        "catalog_snapshot": str(catalog_snapshot.relative_to(directory)),
        "review_evidence_sha256": evidence_hashes,
        "review_status": review_status,
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


HARBOR_IDENTITY = """
import importlib.metadata, json
distribution = importlib.metadata.distribution("harbor")
direct_url = json.loads(distribution.read_text("direct_url.json") or "{}")
commit = (direct_url.get("vcs_info") or {}).get("commit_id")
print(json.dumps({"version": distribution.version, "commit": commit}))
"""


def installed_harbor() -> dict[str, str]:
    """Version and Git commit of the Harbor that `uv run --frozen harbor` launches,
    from the installer's direct_url.json (PEP 610)."""
    identity = json.loads(
        output(["uv", "run", "--frozen", "python", "-c", HARBOR_IDENTITY], cwd=ROOT)
    )
    if not identity["commit"]:
        raise RuntimeError("installed harbor is not a pinned Git checkout")
    return identity


def host_identity() -> dict:
    cpu_model = next(
        (
            line.split(":", 1)[1].strip()
            for line in Path("/proc/cpuinfo").read_text().splitlines()
            if line.startswith("model name")
        ),
        None,
    )
    memory_kib = next(
        int(line.split()[1])
        for line in Path("/proc/meminfo").read_text().splitlines()
        if line.startswith("MemTotal:")
    )
    uname = platform.uname()
    return {
        "cpu_model": cpu_model,
        "logical_cpus": os.cpu_count(),
        "memory_bytes": memory_kib * 1024,
        "kernel": f"{uname.system} {uname.release}",
        "machine": uname.machine,
        "container_runtime": output(["podman", "--version"]),
    }


def benchmark_commit() -> str:
    dirty = output(["git", "status", "--porcelain", "--untracked-files=normal"])
    if dirty:
        raise RuntimeError(
            "dirty worktree; commit the benchmark state before starting an experiment"
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
        help="Controls which arm opens each case's first pair; not sent to the model endpoint",
    )
    parser.add_argument(
        "--protocol",
        choices=PROTOCOLS,
        default=COMMON_ANALYSIS,
        help="Scoring protocol for every task of the experiment; recorded in the manifest",
    )
    parser.add_argument(
        "--repetitions",
        type=positive_int,
        default=1,
        help="Paired runs per case and model; each is one Harbor job per arm",
    )
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
    os.environ.setdefault("OPENAI_API_KEY", "local-benchmark")

    catalog = args.catalog.resolve()
    models = args.model or DEFAULT_MODELS
    revision = benchmark_commit()
    harbor = installed_harbor()
    experiment_id = args.experiment or datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    safe_component(experiment_id, field="experiment id")
    experiment_root = ROOT / "jobs" / experiment_id
    controller = hold_experiment(experiment_root)
    if (experiment_root / "experiment.json").exists() and not args.resume:
        raise FileExistsError(
            f"experiment already exists: {experiment_root}; pass --resume to continue it"
        )
    snapshots = snapshot_experiment_inputs(
        experiment_root,
        catalog,
        args.review_evidence.resolve() if args.review_evidence else None,
    )
    catalog_snapshot = experiment_root / snapshots["catalog_snapshot"]
    catalog_by_id = {case["id"]: case for case in load_catalog(catalog_snapshot)}
    cases = [
        case["id"] for case in select_cases(list(catalog_by_id.values()), args.case or None)
    ]
    arms = args.arm or ["lingrep", "mecab"]

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
                run(["podman", "load"], stdin=image)
    base_image_ids = {
        arm: output(
            [
                "podman",
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
    bound_dictionary_sources = dictionary_sources(base_image_refs)
    image_ids = {arm: base_image_ids[arm] for arm in arms}
    specs = task_specs([catalog_by_id[case_id] for case_id in cases])
    tasks = {
        f"lingrep/{arm}-{spec['id']}": {
            "arm": arm,
            "case_id": spec["id"],
            "base_case_id": spec["base_case_id"],
            "stage": spec["stage"],
            "capability": spec["capability"],
        }
        for spec in specs
        for arm in arms
    }
    manifest = {
        "schema": EXPERIMENT_SCHEMA,
        "experiment_id": experiment_id,
        "protocol": args.protocol,
        "protocol_schema": PROTOCOL_SCHEMA,
        "created_at": datetime.now(UTC).isoformat(),
        "catalog": str(catalog),
        "catalog_sha256": sha256(catalog),
        **snapshots,
        "guide_sha256": {
            arm: sha256(DISCOVERY_GUIDE) for arm in arms
        },
        "harbor_version": harbor["version"],
        "harbor_commit": harbor["commit"],
        "benchmark_commit": revision,
        "host": host_identity(),
        "models": models,
        "api_base": args.api_base,
        "repetitions": args.repetitions,
        "agent_timeout_sec": args.agent_timeout_sec,
        "controller_seed": args.seed,
        "arms": arms,
        "cases": cases,
        "case_sources": {
            case_id: catalog_by_id[case_id]["source"]
            for case_id in cases
            if catalog_by_id[case_id].get("source") is not None
        },
        "revisions": {
            "performed_by": "agent",
            "start_state": "reference artifact of the base case",
            "cases": [spec["base_case_id"] for spec in specs if spec["stage"] == "revision"],
        },
        "image_outputs": image_outputs,
        "image_ids": image_ids,
        "dictionary_sources": bound_dictionary_sources,
        "tasks": tasks,
    }
    check_experiment_manifest(experiment_root, manifest, resume=args.resume)

    generated = ROOT / ".generated" / experiment_id
    publish_generated_inputs(
        generated,
        catalog_snapshot,
        agent_timeout_sec=args.agent_timeout_sec,
        image_refs=base_image_refs,
        protocol=args.protocol,
        cases=cases,
        arms=arms,
    )
    preflight(generated)
    write_experiment_manifest(experiment_root, manifest, resume=args.resume)
    rng = random.Random(args.seed)

    for model in models:
        job_root = experiment_root / model_directory(model)
        job_root.mkdir(parents=True, exist_ok=True)
        for case in [spec["id"] for spec in specs]:
            pair = arms[:]
            rng.shuffle(pair)
            for repetition in range(1, args.repetitions + 1):
                order = pair if repetition % 2 else pair[::-1]
                for arm in order:
                    run(
                        [
                            "uv",
                            "run",
                            "--frozen",
                            "harbor",
                            "run",
                            "-p",
                            str(generated / arm / case),
                            "-a",
                            "terminus-2",
                            "--env",
                            "podman",
                            "-m",
                            f"openai/{model}",
                            "--agent-kwarg",
                            f"api_base={args.api_base}",
                            "--jobs-dir",
                            str(job_root),
                            "--job-name",
                            f"{case}-{arm}-r{repetition}",
                            "--n-attempts",
                            "1",
                            "--n-concurrent",
                            "1",
                            "--yes",
                        ],
                        cwd=ROOT,
                    )
    controller.close()


if __name__ == "__main__":
    main()
