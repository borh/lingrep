from __future__ import annotations

import json
import os
import re
from collections.abc import Mapping
from pathlib import Path
from typing import Any, Literal

MANIFEST_HASH_CHUNK_BYTES = 1 << 20
RUNTIME_RECEIPT_FILE = "lingrep-runtime.json"
RUNTIME_IDENTITY_FILE = "lingrep-runtime-identity.json"
RUNTIME_IDENTITY_FORMAT = "lingrep.adapter-runtime-identity/v2"
PLATFORM_KERNEL_DISTRIBUTIONS = frozenset({"thinc-apple-ops"})
# The adapter semantics revision identifies the output of lingrep's own
# adapter code, so their distribution versions stay out of the identity.
ADAPTER_DISTRIBUTIONS = frozenset(
    {"lingrep-adapter-protocol", "lingrep-spacy-adapter", "lingrep-stanza-adapter"}
)


def canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def digest_json(value: Any) -> str:
    import blake3

    return blake3.blake3(canonical_json(value)).hexdigest()


def adapter_semantics_digest(revision: str) -> str:
    """Return the run header's implementation digest for a bundled adapter's
    semantics revision; lingrep computes the same value."""

    import blake3

    encoded = revision.encode("utf-8")
    digest = blake3.blake3(b"lingrep.adapter-semantics/v1")
    digest.update(len(encoded).to_bytes(8, "little"))
    digest.update(encoded)
    return digest.hexdigest()


def resource_manifest_digest(
    resource_root: Path, manifest_root: Path, *, provider: Literal["spaCy", "Stanza"]
) -> str:
    label = "spaCy model" if provider == "spaCy" else "Stanza resource"
    if resource_root.is_absolute() or any(
        part in {"", ".", ".."} for part in resource_root.parts
    ):
        raise ValueError(f"{label} root must be normalized and manifest-relative")
    return _manifest_digest(
        b"lingrep.analyzer-resources/v1", manifest_root, [resource_root], provider
    )


def implementation_manifest_digest(
    manifest_root: Path, kind: Literal["spacy", "stanza"]
) -> str:
    """Return the digest lingrep computes over a staged adapter's code files."""

    project = Path(f"{kind}-adapter")
    protocol = Path("adapter-protocol")
    return _manifest_digest(
        b"lingrep.adapter-implementation/v1",
        manifest_root,
        [
            project / "bin",
            project / "src",
            project / "uv.lock",
            project / "pyproject.toml",
            protocol / "pyproject.toml",
            protocol / "src",
        ],
        "spaCy" if kind == "spacy" else "Stanza",
    )


def _manifest_digest(
    domain: bytes,
    manifest_root: Path,
    paths: list[Path],
    provider: Literal["spaCy", "Stanza"],
) -> str:
    import blake3

    root = manifest_root.resolve(strict=True)
    entries: list[tuple[str, int, bytes]] = []
    for path in paths:
        _collect_manifest_files(root, root / path, entries, provider)
    entries.sort(key=lambda entry: entry[0].encode("utf-8"))
    digest = blake3.blake3()
    digest.update(domain)
    digest.update(len(entries).to_bytes(8, "little"))
    for relative, length, content_digest in entries:
        encoded = relative.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
        digest.update(length.to_bytes(8, "little"))
        digest.update(content_digest)
    return digest.hexdigest()


def _collect_manifest_files(
    root: Path,
    current: Path,
    entries: list[tuple[str, int, bytes]],
    provider: Literal["spaCy", "Stanza"],
) -> None:
    import blake3

    label = "spaCy model" if provider == "spaCy" else "Stanza resource"
    if current.name == "__pycache__" or current.suffix == ".pyc":
        raise ValueError(f"{label} root contains a generated Python cache")
    stat = current.lstat()
    if current.is_symlink():
        raise ValueError(f"{label} path {current} contains a symlink")
    if current.is_file():
        relative = current.relative_to(root).as_posix()
        digest = blake3.blake3()
        length = 0
        with current.open("rb") as stream:
            while chunk := stream.read(MANIFEST_HASH_CHUNK_BYTES):
                length += len(chunk)
                digest.update(chunk)
        after = current.stat()
        before_identity = (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns)
        after_identity = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
        )
        if length != stat.st_size or before_identity != after_identity:
            subject = (
                "spaCy model resource" if provider == "spaCy" else "Stanza resource"
            )
            raise ValueError(f"{subject} changed while it was hashed")
        entries.append((relative, length, digest.digest()))
        return
    if not current.is_dir():
        raise ValueError(f"{label} path {current} is not regular")
    children = current.iterdir()
    if provider == "spaCy":
        children = sorted(children, key=lambda path: os.fsencode(path.name))
    for child in children:
        _collect_manifest_files(root, child, entries, provider)


def _distribution_name(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def runtime_identity(receipt: Mapping[str, Any]) -> dict[str, Any]:
    """Return the interpreter release and normalized package versions,
    excluding PLATFORM_KERNEL_DISTRIBUTIONS and ADAPTER_DISTRIBUTIONS."""

    excluded = PLATFORM_KERNEL_DISTRIBUTIONS | ADAPTER_DISTRIBUTIONS
    return {
        "format": RUNTIME_IDENTITY_FORMAT,
        "python": receipt["python_release"],
        "packages": sorted(
            [_distribution_name(name), version]
            for name, version in receipt["packages"]
            if _distribution_name(name) not in excluded
        ),
    }


def runtime_receipt_path(resource_root: Path) -> Path:
    return resource_root.parent / RUNTIME_RECEIPT_FILE


def validate_runtime(resource_root: Path) -> None:
    receipt = runtime_receipt_path(resource_root)
    if not receipt.is_file():
        return
    import importlib.metadata
    import sys

    expected = json.loads(receipt.read_text())
    actual = sorted(
        [d.metadata["Name"], d.version] for d in importlib.metadata.distributions()
    )
    if expected["python_version"] != sys.version or expected["packages"] != actual:
        raise ValueError(
            "analyzer runtime changed; rerun setup into a new output directory"
        )
