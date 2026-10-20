#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = [
#   "datasets==5.0.1",
#   "ud-hf-parquet-tools==1.2.0",
# ]
#
# [tool.uv]
# # Keep this here rather than in a user-global ~/.config/uv/uv.toml: the
# # sidecar lock records `exclude-newer-span`, and uv re-resolves when the
# # active value does not match what the lock recorded. Untracked,
# # `uv lock --check --script` passes only on the machine that wrote it.
# exclude-newer = "P3D"
# ///
"""Stream a Hugging Face dataset into a ling corpus format."""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from collections.abc import Iterable, Mapping
from itertools import islice
from pathlib import Path
from typing import Any, TextIO


class ProjectionError(ValueError):
    """A selected dataset row cannot be represented as a ling document."""


CONLLU_WORD_FIELDS = (
    "tokens",
    "lemmas",
    "upos",
    "xpos",
    "feats",
    "head",
    "deprel",
    "deps",
    "misc",
)


def nested_field(row: Mapping[str, Any], path: str, row_number: int) -> Any:
    """Resolve a dot-separated field path from one dataset row."""
    value: Any = row
    for component in path.split("."):
        if not component or not isinstance(value, Mapping) or component not in value:
            raise ProjectionError(
                f"row {row_number}: field {path!r} is missing at {component!r}"
            )
        value = value[component]
    return value


def project_row(
    row: Mapping[str, Any],
    *,
    row_number: int,
    text_field: str,
    metadata_fields: list[str],
) -> dict[str, Any]:
    """Project one Hugging Face row to the strict ling JSONL shape."""
    text = nested_field(row, text_field, row_number)
    if not isinstance(text, str):
        raise ProjectionError(
            f"row {row_number}: text field {text_field!r} must be a string, "
            f"got {type(text).__name__}"
        )

    document: dict[str, Any] = {"text": text}
    if metadata_fields:
        document["metadata"] = {
            field: nested_field(row, field, row_number) for field in metadata_fields
        }
    return document


def write_rows(
    rows: Iterable[Mapping[str, Any]],
    output: TextIO,
    *,
    text_field: str,
    metadata_fields: list[str],
    limit: int | None,
) -> int:
    """Write projected rows incrementally and return their count."""
    count = 0
    selected_rows = rows if limit is None else islice(rows, limit)
    for row_number, row in enumerate(selected_rows, start=1):
        document = project_row(
            row,
            row_number=row_number,
            text_field=text_field,
            metadata_fields=metadata_fields,
        )
        try:
            encoded = json.dumps(document, ensure_ascii=False, separators=(",", ":"))
        except (TypeError, ValueError) as error:
            raise ProjectionError(
                f"row {row_number}: selected fields are not JSON-serializable: {error}"
            ) from error
        output.write(encoded)
        output.write("\n")
        count += 1
    return count


def validate_conllu_row(row: Mapping[str, Any], *, row_number: int) -> None:
    """Validate the parallel word columns consumed by the CoNLL-U encoder."""
    for field in CONLLU_WORD_FIELDS:
        if field not in row:
            raise ProjectionError(
                f"row {row_number}: CoNLL-U field {field!r} is missing"
            )
        if not isinstance(row[field], list):
            raise ProjectionError(
                f"row {row_number}: CoNLL-U field {field!r} must be a list"
            )

    word_count = len(row["tokens"])
    for field in CONLLU_WORD_FIELDS[1:]:
        field_count = len(row[field])
        if field_count != word_count:
            raise ProjectionError(
                f"row {row_number}: CoNLL-U field {field!r} has {field_count} "
                f"values; expected {word_count}"
            )


def encode_conllu_row(row: Mapping[str, Any], upos_names: list[str] | None) -> str:
    """Encode one validated row with the maintained UD implementation."""
    from ud_hf_parquet_tools import example_to_conllu

    return example_to_conllu(row, upos_names)


def write_conllu_rows(
    rows: Iterable[Mapping[str, Any]],
    output: TextIO,
    *,
    limit: int | None,
    upos_names: list[str] | None,
) -> int:
    """Stream canonical UD rows through the maintained CoNLL-U encoder."""
    count = 0
    selected_rows = rows if limit is None else islice(rows, limit)
    for row_number, row in enumerate(selected_rows, start=1):
        validate_conllu_row(row, row_number=row_number)
        if upos_names is None and any(isinstance(value, int) for value in row["upos"]):
            raise ProjectionError(
                f"row {row_number}: integer UPOS values require dataset label names"
            )
        try:
            encoded = encode_conllu_row(row, upos_names)
        except (IndexError, KeyError, TypeError, ValueError) as error:
            raise ProjectionError(
                f"row {row_number}: cannot encode CoNLL-U: {error}"
            ) from error
        if not isinstance(encoded, str):
            raise ProjectionError(
                f"row {row_number}: CoNLL-U encoder returned "
                f"{type(encoded).__name__}, expected str"
            )
        output.write(encoded)
        count += 1
    return count


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return parsed


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description=(
            "Project a Hugging Face dataset split to a ling corpus format. "
            "Rows are streamed and never collected in memory."
        ),
        epilog=(
            "Direct text query: huggingface_dataset.py DATASET --split train | "
            "ling matches --input-format jsonl --patterns PATTERNS -\n"
            "Pretokenized UD query: add --output-format conllu and pass "
            "--input-format conllu to ling.\n"
            "Reusable source: add --output corpus.jsonl (or corpus.conllu), then pass it "
            "to ling with --cache."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    result.add_argument("dataset", help="dataset repository, for example owner/name")
    result.add_argument(
        "--inspect",
        action="store_true",
        help="print configurations and the selected configuration's schema as JSON",
    )
    result.add_argument("--config", help="dataset configuration/subset name")
    result.add_argument(
        "--split", help="one dataset split to stream (required for rows)"
    )
    result.add_argument(
        "--revision",
        help="Hub branch, tag, or commit; pin a commit for reproducible materialization",
    )
    result.add_argument(
        "--text-field",
        help="dot-separated UTF-8 text field path (default: text)",
    )
    result.add_argument(
        "--metadata-field",
        action="append",
        default=[],
        help="dot-separated field to preserve as document metadata; repeatable",
    )
    result.add_argument("--limit", type=positive_int, help="stop after this many rows")
    result.add_argument(
        "--output-format",
        choices=("jsonl", "conllu"),
        default="jsonl",
        help="projection written to stdout or --output (default: jsonl)",
    )
    result.add_argument(
        "--output",
        type=Path,
        help="materialize to a new file instead of stdout",
    )
    return result


def validate_projection_options(arguments: argparse.Namespace) -> None:
    """Reject projection options that do not belong to the selected format."""
    if arguments.output_format != "conllu":
        return
    if arguments.text_field is not None:
        raise ProjectionError("--text-field is only valid with JSONL output")
    if arguments.metadata_field:
        raise ProjectionError("--metadata-field is only valid with JSONL output")


def dataset_rows(arguments: argparse.Namespace) -> Iterable[Mapping[str, Any]]:
    """Open the selected split with the maintained Hugging Face client."""
    from datasets import load_dataset

    options: dict[str, Any] = {
        "split": arguments.split,
        "streaming": True,
    }
    if arguments.revision is not None:
        options["revision"] = arguments.revision
    return load_dataset(arguments.dataset, arguments.config, **options)


def conllu_upos_names(rows: Any) -> list[str] | None:
    """Read optional ClassLabel names from a Hugging Face dataset schema."""
    features = getattr(rows, "features", None)
    if features is None:
        return None
    upos = features.get("upos")
    feature = getattr(upos, "feature", None)
    names = getattr(feature, "names", None)
    return None if names is None else list(names)


def write_selected_rows(
    rows: Iterable[Mapping[str, Any]],
    output: TextIO,
    arguments: argparse.Namespace,
) -> int:
    """Write rows using the explicitly selected standard corpus projection."""
    if arguments.output_format == "conllu":
        return write_conllu_rows(
            rows,
            output,
            limit=arguments.limit,
            upos_names=conllu_upos_names(rows),
        )
    return write_rows(
        rows,
        output,
        text_field=arguments.text_field or "text",
        metadata_fields=arguments.metadata_field,
        limit=arguments.limit,
    )


def inspect_dataset(
    arguments: argparse.Namespace,
    *,
    config_names: Any = None,
    split_names: Any = None,
    load_builder: Any = None,
) -> dict[str, Any]:
    """Return bounded schema metadata without reading any dataset rows."""
    if config_names is None or split_names is None or load_builder is None:
        from datasets import (
            get_dataset_config_names,
            get_dataset_split_names,
            load_dataset_builder,
        )

        config_names = get_dataset_config_names
        split_names = get_dataset_split_names
        load_builder = load_dataset_builder

    revision_options = (
        {} if arguments.revision is None else {"revision": arguments.revision}
    )
    configurations = config_names(arguments.dataset, **revision_options)
    selected = arguments.config
    if selected is None and len(configurations) == 1:
        selected = configurations[0]
    if selected is not None and selected not in configurations:
        raise ProjectionError(
            f"configuration {selected!r} does not exist; available: "
            + ", ".join(configurations)
        )

    result: dict[str, Any] = {
        "dataset": arguments.dataset,
        "requested_revision": arguments.revision,
        "configurations": configurations,
        "selected_configuration": selected,
    }
    if selected is None:
        result["hint"] = "rerun with --inspect --config CONFIG to show its fields"
        return result

    builder = load_builder(arguments.dataset, selected, **revision_options)
    features = builder.info.features
    result["splits"] = split_names(arguments.dataset, selected, **revision_options)
    result["features"] = None if features is None else features.to_dict()
    return result


def write_new_file(
    destination: Path,
    rows: Iterable[Mapping[str, Any]],
    arguments: argparse.Namespace,
) -> int:
    """Publish a complete file atomically without replacing an existing source."""
    destination = destination.absolute()
    parent = destination.parent
    if not parent.is_dir():
        raise FileNotFoundError(f"output directory does not exist: {parent}")

    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="\n",
            prefix=f".{destination.name}.",
            suffix=".partial",
            dir=parent,
            delete=False,
        ) as output:
            temporary_path = Path(output.name)
            count = write_selected_rows(rows, output, arguments)
            output.flush()
            os.fsync(output.fileno())
        try:
            os.link(temporary_path, destination)
        except FileExistsError as error:
            raise FileExistsError(f"output already exists: {destination}") from error
        return count
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)


def run(arguments: argparse.Namespace) -> int:
    if arguments.inspect:
        if arguments.split is not None:
            raise ProjectionError("--inspect does not accept --split")
        if arguments.output is not None:
            raise ProjectionError("--inspect does not accept --output")
        if arguments.limit is not None or arguments.metadata_field:
            raise ProjectionError(
                "--inspect does not accept row projection or limit options"
            )
        json.dump(inspect_dataset(arguments), sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write("\n")
        return 0
    if arguments.split is None:
        raise ProjectionError("--split is required unless --inspect is used")
    validate_projection_options(arguments)
    rows = dataset_rows(arguments)
    if arguments.output is None:
        count = write_selected_rows(rows, sys.stdout, arguments)
    else:
        count = write_new_file(arguments.output, rows, arguments)
    print(f"projected {count} dataset rows", file=sys.stderr)
    return 0


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        return run(arguments)
    except BrokenPipeError:
        try:
            stdout_fd = sys.stdout.fileno()
            null_fd = os.open(os.devnull, os.O_WRONLY)
            os.dup2(null_fd, stdout_fd)
            os.close(null_fd)
        except (AttributeError, OSError):
            pass
        return 0
    except (OSError, ProjectionError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
