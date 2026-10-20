from __future__ import annotations

import json
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path
from typing import Any

PROFILE_FILE = "lingrep-stanza-model.json"
PROFILE_FORMAT = "lingrep.stanza-model/v1"
MANIFEST_HASH_CHUNK_BYTES = 1 << 20
PROCESSOR_ORDER = ("tokenize", "mwt", "pos", "lemma")
BASE_UD_FEATURE_NAMES = (
    "Abbr",
    "AdpType",
    "AdvType",
    "Animacy",
    "Aspect",
    "Case",
    "Clusivity",
    "ConjType",
    "Definite",
    "Degree",
    "Deixis",
    "DeixisRef",
    "Echo",
    "Evident",
    "ExtPos",
    "Foreign",
    "Gender",
    "Hyph",
    "Mood",
    "NameType",
    "NounClass",
    "NounType",
    "Number",
    "NumForm",
    "NumType",
    "PartType",
    "Person",
    "Polarity",
    "Polite",
    "Poss",
    "PrepCase",
    "PronType",
    "PunctSide",
    "PunctType",
    "Reflex",
    "Style",
    "Subcat",
    "Tense",
    "Typo",
    "Variant",
    "VerbForm",
    "VerbType",
    "Voice",
)
TOKENIZE_SCHEMA = ({"key": "text", "kind": "scalar"},)
MORPHOLOGY_SCALARS = (
    {"key": "text", "kind": "scalar"},
    {"key": "lemma", "kind": "scalar"},
    {"key": "upos", "kind": "scalar"},
    {"key": "xpos", "kind": "scalar"},
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


def prepare_model_bundle(
    resource_root: Path,
    *,
    language: str,
    probe_text: str,
    packages: Mapping[str, str],
    layered_feature_keys: Sequence[str] = (),
) -> dict[str, Any]:
    root = resource_root.resolve(strict=True)
    resources = root / "resources.json"
    if not resources.is_file():
        raise ValueError("Stanza resource root has no resources.json")
    _validate_language(language)
    package_map = _validate_packages(packages)

    value = declared_model_profile(
        language=language,
        packages=package_map,
        layered_feature_keys=layered_feature_keys,
    )
    for name, selection in value["selections"].items():
        profile, segmentation = name.split("/")
        _validate_model_selection(
            root,
            language,
            probe_text,
            selection,
            profile=profile,
            segmentation=segmentation,
        )
    (root / PROFILE_FILE).write_bytes(canonical_json(value) + b"\n")
    return value


def declared_model_profile(
    *,
    language: str,
    packages: Mapping[str, str],
    layered_feature_keys: Sequence[str] = (),
) -> dict[str, Any]:
    """Builds the deterministic declaration; preparation separately proves it loads."""

    _validate_language(language)
    package_map = _validate_packages(packages)
    feature_keys = _feature_keys(layered_feature_keys)
    tokenize_processors = ("tokenize", "mwt") if "mwt" in package_map else ("tokenize",)
    selections = {
        "tokenize/input": _declared_selection(
            package_map, tokenize_processors, "input", TOKENIZE_SCHEMA
        ),
        "tokenize/analyzer": _declared_selection(
            package_map, tokenize_processors, "analyzer", TOKENIZE_SCHEMA
        ),
    }
    if set(package_map) != {"tokenize"}:
        schema = MORPHOLOGY_SCALARS + tuple(
            {"key": key, "kind": "string_set"} for key in feature_keys
        )
        selections["morphology/input"] = _declared_selection(
            package_map, PROCESSOR_ORDER, "input", schema
        )
        selections["morphology/analyzer"] = _declared_selection(
            package_map, PROCESSOR_ORDER, "analyzer", schema
        )
    return {
        "format": PROFILE_FORMAT,
        "language": language,
        "selections": selections,
        "selection_digests": {
            name: selection_digest(selection) for name, selection in selections.items()
        },
    }


def load_profile(resource_root: Path) -> dict[str, Any]:
    value = json.loads(
        (resource_root / PROFILE_FILE).read_bytes(), object_pairs_hook=_unique_object
    )
    if not isinstance(value, dict) or set(value) != {
        "format",
        "language",
        "selections",
        "selection_digests",
    }:
        raise ValueError("Stanza model profile has the wrong shape")
    if value["format"] != PROFILE_FORMAT:
        raise ValueError("Stanza model profile format is invalid")
    _validate_language(value["language"])
    if not isinstance(value["selections"], dict) or not value["selections"]:
        raise ValueError("Stanza model profile has no selections")
    allowed = {
        "tokenize/input",
        "tokenize/analyzer",
        "morphology/input",
        "morphology/analyzer",
    }
    if not set(value["selections"]).issubset(allowed):
        raise ValueError("Stanza model profile has an unknown selection")
    if not isinstance(value["selection_digests"], dict) or set(
        value["selection_digests"]
    ) != set(value["selections"]):
        raise ValueError("Stanza selection digests have the wrong shape")
    for name, selection in value["selections"].items():
        _validate_selection(name, selection)
        if value["selection_digests"][name] != selection_digest(selection):
            raise ValueError("Stanza selection digest mismatch")
    return value


def selection_digest(selection: Mapping[str, Any]) -> str:
    return digest_json(selection)


def resource_manifest_digest(resource_root: Path, manifest_root: Path) -> str:
    import blake3

    root = manifest_root.resolve(strict=True)
    if resource_root.is_absolute() or any(
        part in {"", ".", ".."} for part in resource_root.parts
    ):
        raise ValueError(
            "Stanza resource root must be normalized and manifest-relative"
        )
    entries: list[tuple[str, int, bytes]] = []
    _collect_manifest_files(root, root / resource_root, entries)
    entries.sort(key=lambda entry: entry[0].encode("utf-8"))
    digest = blake3.blake3()
    digest.update(b"lingrep.analyzer-resources/v1")
    digest.update(len(entries).to_bytes(8, "little"))
    for relative, length, content_digest in entries:
        encoded = relative.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
        digest.update(length.to_bytes(8, "little"))
        digest.update(content_digest)
    return digest.hexdigest()


def _declared_selection(
    packages: Mapping[str, str],
    processors: Sequence[str],
    segmentation: str,
    schema: Sequence[Mapping[str, str]],
) -> dict[str, Any]:
    boundary = "input-v1" if segmentation == "input" else "stanza:tokenize:v1"
    alignment = "identity-v1" if tuple(schema) == TOKENIZE_SCHEMA else "stanza-mwt/v1"
    return {
        "processors": list(processors),
        "packages": {name: packages[name] for name in processors},
        "boundary_policy": boundary,
        "alignment_policy": alignment,
        "schema": list(schema),
    }


def _validate_model_selection(
    root: Path,
    language: str,
    probe_text: str,
    selection: Mapping[str, Any],
    *,
    profile: str,
    segmentation: str,
) -> None:
    import stanza

    processors = tuple(selection["processors"])
    pipeline = stanza.Pipeline(
        lang=language,
        dir=str(root),
        processors=",".join(processors),
        package=selection["packages"],
        download_method=None,
        resources_filepath=str(root / "resources.json"),
        tokenize_no_ssplit=segmentation == "input",
        use_gpu=False,
        verbose=False,
    )
    document = pipeline(probe_text)
    sentences = list(document.sentences)
    if not sentences or not any(sentence.tokens for sentence in sentences):
        raise ValueError("Stanza selection produced no probe tokens")
    if profile != "tokenize":
        for sentence in sentences:
            for token in sentence.tokens:
                if not token.words or any(
                    not word.text or not word.lemma or not word.upos
                    for word in token.words
                ):
                    raise ValueError(
                        "Stanza model does not produce the portable "
                        "morphology capability"
                    )


def _validate_selection(name: str, value: Any) -> None:
    if not isinstance(value, dict) or set(value) != {
        "processors",
        "packages",
        "boundary_policy",
        "alignment_policy",
        "schema",
    }:
        raise ValueError("Stanza selection has the wrong shape")
    profile, segmentation = name.split("/")
    expected = list(PROCESSOR_ORDER)
    if profile == "tokenize":
        expected = value["processors"]
        if expected not in (["tokenize"], ["tokenize", "mwt"]):
            raise ValueError("Stanza tokenize processor closure is invalid")
    if value["processors"] != expected:
        raise ValueError("Stanza selection processor closure is invalid")
    packages = _validate_packages(value["packages"], allow_tokenize_mwt=True)
    if set(packages) != set(expected):
        raise ValueError("Stanza selection packages differ from its processors")
    expected_boundary = "input-v1" if segmentation == "input" else "stanza:tokenize:v1"
    expected_alignment = "identity-v1" if profile == "tokenize" else "stanza-mwt/v1"
    if value["boundary_policy"] != expected_boundary:
        raise ValueError("Stanza boundary policy is invalid")
    if value["alignment_policy"] != expected_alignment:
        raise ValueError("Stanza alignment policy is invalid")
    _validate_schema(value["schema"], profile)


def _validate_schema(value: Any, profile: str) -> None:
    if not isinstance(value, list) or not value:
        raise ValueError("Stanza schema must be a nonempty array")
    expected_scalars = [item["key"] for item in TOKENIZE_SCHEMA]
    if profile == "morphology":
        expected_scalars = [item["key"] for item in MORPHOLOGY_SCALARS]
    keys: list[str] = []
    for attribute in value:
        if not isinstance(attribute, dict) or set(attribute) != {"key", "kind"}:
            raise ValueError("Stanza schema entry has the wrong shape")
        key = attribute["key"]
        if not isinstance(key, str) or attribute["kind"] not in {
            "scalar",
            "string_set",
        }:
            raise ValueError("Stanza schema entry is invalid")
        if key in expected_scalars:
            if attribute["kind"] != "scalar":
                raise ValueError("Stanza scalar schema kind is invalid")
        elif not _is_feature_key(key) or attribute["kind"] != "string_set":
            raise ValueError("Stanza feature schema entry is invalid")
        keys.append(key)
    canonical = expected_scalars + sorted(
        set(keys).difference(expected_scalars), key=lambda key: key.encode("utf-8")
    )
    if keys != canonical or len(keys) != len(set(keys)):
        raise ValueError("Stanza schema is not unique and canonical")


def _validate_language(value: Any) -> None:
    if (
        not isinstance(value, str)
        or not value
        or value.startswith("-")
        or value.endswith("-")
        or "--" in value
        or not all(
            character.isascii() and (character.isalnum() or character == "-")
            for character in value
        )
    ):
        raise ValueError("Stanza language must be a nonempty language tag")


def _validate_packages(
    value: Any, *, allow_tokenize_mwt: bool = False
) -> dict[str, str]:
    if not isinstance(value, Mapping) or not value or "tokenize" not in value:
        raise ValueError("Stanza packages must name the tokenize processor")
    result: dict[str, str] = {}
    for name, package in value.items():
        if name not in PROCESSOR_ORDER or not isinstance(package, str) or not package:
            raise ValueError("Stanza package has an unknown processor or empty package")
        result[name] = package
    allowed = ({"tokenize"}, set(PROCESSOR_ORDER))
    if allow_tokenize_mwt:
        allowed += ({"tokenize", "mwt"},)
    if set(result) not in allowed:
        raise ValueError("Stanza morphology requires tokenize,mwt,pos,lemma processors")
    return result


def _feature_keys(layered: Sequence[str]) -> list[str]:
    base = [f"feat.{name}" for name in BASE_UD_FEATURE_NAMES]
    if any(not _is_feature_key(key) or "[" not in key for key in layered):
        raise ValueError(
            "layered Stanza feature keys must be canonical feat.*[layer] keys"
        )
    combined = base + list(layered)
    if len(combined) != len(set(combined)):
        raise ValueError("Stanza feature keys must be unique")
    return sorted(combined, key=lambda key: key.encode("utf-8"))


def _is_feature_key(key: str) -> bool:
    if not key.startswith("feat."):
        return False
    name = key.removeprefix("feat.")
    if "[" not in name:
        return name in BASE_UD_FEATURE_NAMES
    base, separator, layer = name.partition("[")
    return (
        separator == "["
        and base in BASE_UD_FEATURE_NAMES
        and layer.endswith("]")
        and bool(layer[:-1])
        and layer[:-1].isascii()
        and layer[:-1].isalnum()
    )


def _collect_manifest_files(
    root: Path, current: Path, entries: list[tuple[str, int, bytes]]
) -> None:
    import blake3

    if current.name == "__pycache__" or current.suffix == ".pyc":
        raise ValueError("Stanza resource root contains a generated Python cache")
    before = current.lstat()
    if current.is_symlink():
        raise ValueError(f"Stanza resource path {current} contains a symlink")
    if current.is_file():
        digest = blake3.blake3()
        length = 0
        with current.open("rb") as stream:
            while chunk := stream.read(MANIFEST_HASH_CHUNK_BYTES):
                length += len(chunk)
                digest.update(chunk)
        after = current.stat()
        before_identity = (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        )
        after_identity = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
        if before_identity != after_identity or length != before.st_size:
            raise ValueError("Stanza resource changed while it was hashed")
        entries.append((current.relative_to(root).as_posix(), length, digest.digest()))
        return
    if not current.is_dir():
        raise ValueError(f"Stanza resource path {current} is not regular")
    for child in current.iterdir():
        _collect_manifest_files(root, child, entries)


def _unique_object(pairs: Iterable[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result
