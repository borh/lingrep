from __future__ import annotations

import argparse
import importlib
import importlib.metadata
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path


def _copy_package(package: str, destination: Path) -> Path:
    source = Path(importlib.import_module(package).__file__).parent
    destination.mkdir(parents=True)
    for file in source.glob("*.py"):
        shutil.copyfile(file, destination / file.name)
    return source


def _project_file(source: Path, name: str) -> Path:
    bundled = source / "_project" / name
    if bundled.is_file():
        return bundled
    return source.parents[1] / name


def _stage_code(root: Path, kind: str) -> None:
    package = f"lingrep_{kind}_adapter"
    project = root / f"{kind}-adapter"
    source = _copy_package(package, project / "src" / package)
    for name in ("pyproject.toml", "uv.lock", f"bin/lingrep-{kind}-adapter"):
        target = project / name
        target.parent.mkdir(parents=True, exist_ok=True)
        original = (
            source / "uv.lock"
            if name == "uv.lock" and (source / "uv.lock").is_file()
            else _project_file(source, name)
        )
        if not original.is_file():
            raise ValueError(
                f"adapter distribution is missing {name}; reinstall its wheel"
            )
        shutil.copyfile(original, target)
    protocol = root / "adapter-protocol"
    source = _copy_package(
        "lingrep_adapter_protocol", protocol / "src" / "lingrep_adapter_protocol"
    )
    shutil.copyfile(
        _project_file(source, "pyproject.toml"), protocol / "pyproject.toml"
    )


def _packages(values: list[str]) -> dict[str, str]:
    result = {}
    for value in values:
        key, separator, package = value.partition("=")
        if not separator or not key or not package or key in result:
            raise ValueError("--package must be a unique PROCESSOR=PACKAGE pair")
        result[key] = package
    return result


def setup(kind: str, argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog=f"lingrep-{kind}-setup")
    parser.add_argument(
        "--output", required=True, type=Path, help="persistent analyzer directory"
    )
    parser.add_argument("--backend", choices=("cpu", "cuda", "rocm"), default="cpu")
    parser.add_argument("--probe-text", required=True)
    parser.add_argument(
        "--offline",
        action="store_true",
        help="use only an installed spaCy model or a local Stanza resource root",
    )
    if kind == "spacy":
        parser.add_argument(
            "--model",
            required=True,
            help="installed model package or local model directory",
        )
    else:
        parser.add_argument("--language", required=True)
        parser.add_argument("--package", action="append", required=True)
        parser.add_argument("--resource-root", type=Path)
    args = parser.parse_args(argv)
    try:
        output = args.output.absolute()
        config = output / "analyzer.toml"
        if output.exists():
            if not config.is_file():
                raise ValueError(
                    f"output already exists without an analyzer configuration: {output}"
                )
            previous = json.loads((output / "setup.json").read_text())
            requested = _request(kind, args)
            if previous != requested:
                raise ValueError(
                    "output belongs to a different setup; "
                    "choose a new --output directory"
                )
            if not Path(
                json.loads((output / "model" / "lingrep-runtime.json").read_text())[
                    "python"
                ]
            ).is_file():
                raise ValueError(
                    "cached Python environment was removed; "
                    "choose a new --output and rerun setup"
                )
            from .resources import validate_runtime

            validate_runtime(output / "model")
            print(config)
            return 0
        packages = _packages(args.package) if kind == "stanza" else {}
        if kind == "stanza" and args.offline and args.resource_root is None:
            raise ValueError("--offline requires --resource-root for Stanza")
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix=".lingrep-setup-", dir=output.parent
        ) as temporary:
            root = Path(temporary)
            _stage_code(root, kind)
            model = root / "model"
            if kind == "spacy":
                from lingrep_spacy_adapter.adapter import initialize_spacy_backend

                spacy, *_ = initialize_spacy_backend(args.backend)
                from lingrep_spacy_adapter.bundle import prepare_model_bundle

                try:
                    pipeline = spacy.load(args.model)
                except OSError as error:
                    raise ValueError(
                        "spaCy model is unavailable; install its wheel in the same "
                        "uvx environment with --with MODEL_WHEEL"
                    ) from error
                pipeline.to_disk(model)
                prepare_model_bundle(model, probe_text=args.probe_text)
            else:
                from lingrep_stanza_adapter.adapter import _initialize_backend

                stanza, *_ = _initialize_backend(args.backend)
                from lingrep_stanza_adapter.bundle import prepare_model_bundle

                if args.resource_root is not None:
                    shutil.copytree(
                        args.resource_root,
                        model,
                        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
                    )
                else:
                    stanza.download(
                        args.language,
                        model_dir=str(model),
                        processors=packages,
                        package=None,
                        verbose=False,
                    )
                prepare_model_bundle(
                    model,
                    language=args.language,
                    probe_text=args.probe_text,
                    packages=packages,
                )
            runtime = {
                "python": sys.executable,
                "python_version": sys.version,
                "packages": sorted(
                    (d.metadata["Name"], d.version)
                    for d in importlib.metadata.distributions()
                ),
            }
            (model / "lingrep-runtime.json").write_text(
                json.dumps(runtime, sort_keys=True)
            )
            (root / "setup.json").write_text(
                json.dumps(_request(kind, args), sort_keys=True)
            )
            config_text = (
                f'version = 1\nkind = "{kind}"\npython = {json.dumps(sys.executable)}\n'
                f'entrypoint = "{kind}-adapter/bin/lingrep-{kind}-adapter"\n'
                f'model = "model"\nbackend = "{args.backend}"\n'
            )
            (root / "analyzer.toml").write_text(config_text)
            os.rename(root, output)
        print(config)
        return 0
    except Exception as error:
        print(f"lingrep-{kind}-setup: {error}", file=sys.stderr)
        return 1


def _request(kind: str, args: argparse.Namespace) -> dict:
    return {
        key: str(value) if isinstance(value, Path) else value
        for key, value in {"kind": kind, **vars(args)}.items()
        if key not in {"output", "offline"}
    }
