from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from lingrep_adapter_protocol import _unique_object
from lingrep_adapter_protocol.resources import (
    MANIFEST_HASH_CHUNK_BYTES as MANIFEST_HASH_CHUNK_BYTES,
)
from lingrep_adapter_protocol.resources import (
    canonical_json,
)
from lingrep_adapter_protocol.resources import (
    digest_json as digest_json,
)
from lingrep_adapter_protocol.resources import (
    resource_manifest_digest as _resource_manifest_digest,
)

PROFILE_FILE = "lingrep-spacy-model.json"
PROFILE_FORMAT = "lingrep.spacy-model/v1"
PORTABLE_SCHEMA = (
    {"key": "text", "kind": "scalar"},
    {"key": "lemma", "kind": "scalar"},
    {"key": "norm", "kind": "scalar"},
    {"key": "upos", "kind": "scalar"},
    {"key": "xpos", "kind": "scalar"},
)
MORPH_TARGETS = {
    "token.lemma": lambda token: bool(token.lemma_),
    "token.pos": lambda token: bool(token.pos_),
    "token.tag": lambda token: bool(token.tag_),
}
MORPH_OUTPUTS = {"token.morph", "token.dep", "token.head"}
HUB_FACTORY = "transformer_custom"
HUB_REVISIONS = {
    "megagonlabs/transformers-ud-japanese-bert-large-ginza": (
        "7683285d102ba2098cf5f2641388cd69c7406241"
    ),
    "megagonlabs/transformers-ud-japanese-electra-base-ginza-v5.3": (
        "5858be1efff32fcac88a6d2e3747df129c9dfe4a"
    ),
}
HUB_SOURCES_FILE = "lingrep-hub-sources.json"
BUNSETU_KEY = "spacy.ginza.bunsetu"
SUDACHITRA_TOKENIZER = "modeling.ElectraSudachipyTokenizer"


def save_model(model: str, destination: Path, *, offline: bool = False) -> None:
    """Save a spaCy model with pinned GiNZA transformer weights;
    offline setup rejects models requiring those downloads."""

    import spacy

    source = _model_path(spacy, model)
    config = spacy.util.load_config(source / "config.cfg")
    sources = {}
    for name, component in config["components"].items():
        if component.get("factory") != HUB_FACTORY:
            continue
        repository = component["model"]["name"]
        if repository not in HUB_REVISIONS:
            raise ValueError(
                f"component {name!r} loads {repository} from the Hugging Face Hub, "
                "and lingrep pins no revision of it"
            )
        sources[name] = {
            "repository": repository,
            "revision": HUB_REVISIONS[repository],
        }
    if not sources:
        spacy.load(source).to_disk(destination)
        return
    if offline:
        raise ValueError(
            f"{model} needs Hugging Face downloads; rerun setup without --offline"
        )
    import shutil
    import tempfile

    from huggingface_hub import snapshot_download

    with tempfile.TemporaryDirectory(prefix="lingrep-hub-") as directory:
        hub = Path(directory)
        staged = hub / "model"
        shutil.copytree(source, staged)
        snapshots = {}
        for name, value in sources.items():
            local = (hub / "snapshots" / name).resolve()
            snapshot_download(
                value["repository"], revision=value["revision"], local_dir=local
            )
            _drop_sudachitra_auto_map(local / "tokenizer_config.json")
            _point_transformer_at(staged / name / "model", value["repository"], local)
            snapshots[name] = str(local)
        _register_sudachitra_tokenizer()
        pipeline = spacy.load(staged)
        for name, value in sources.items():
            objects = _hf_objects(pipeline.get_pipe(name))
            loaded = {
                objects.transformer.config._name_or_path,
                objects.tokenizer.name_or_path,
            }
            if loaded != {snapshots[name]}:
                raise ValueError(f"component {name!r} loaded {loaded}, not the pin")
            objects.transformer.config._name_or_path = value["repository"]
            objects.tokenizer.name_or_path = value["repository"]
        pipeline.to_disk(destination)
    saved = spacy.util.load_config(destination / "config.cfg")
    for name in sources:
        saved["components"][name]["factory"] = "transformer"
    saved.to_disk(destination / "config.cfg")
    (destination / HUB_SOURCES_FILE).write_bytes(canonical_json(sources) + b"\n")


def admit_saved_transformers(config: Mapping[str, Any]) -> None:
    """Reject Hub-loading components and register saved tokenizer classes."""

    factories = {
        component.get("factory") for component in config["components"].values()
    }
    if HUB_FACTORY in factories:
        raise ValueError(
            "model loads a transformer from the Hugging Face Hub; "
            "save it with lingrep-spacy-setup, which stores the weights in the model"
        )
    if "transformer" in factories:
        _register_sudachitra_tokenizer()


def _register_sudachitra_tokenizer() -> None:
    try:
        from sudachitra import ElectraSudachipyTokenizer
    except ImportError:
        return
    from transformers import AutoTokenizer

    AutoTokenizer.register(
        ElectraSudachipyTokenizer.__name__,
        slow_tokenizer_class=ElectraSudachipyTokenizer,
        exist_ok=True,
    )


def _model_path(spacy: Any, model: str) -> Path:
    path = Path(model)
    if path.is_dir():
        return path
    try:
        package = spacy.util.get_package_path(model)
        meta = spacy.util.get_model_meta(package)
    except (ImportError, OSError) as error:
        raise ValueError(
            "spaCy model is unavailable; install its wheel in the same "
            "uvx environment with --with MODEL_WHEEL"
        ) from error
    return package / f"{meta['lang']}_{meta['name']}-{meta['version']}"


def _drop_sudachitra_auto_map(path: Path) -> None:
    if not path.is_file():
        return
    value = json.loads(path.read_bytes())
    auto_map = value.pop("auto_map", None)
    if auto_map is None:
        return
    if auto_map != {"AutoTokenizer": [SUDACHITRA_TOKENIZER, None]}:
        raise ValueError(f"{path.parent.name} names tokenizer code lingrep lacks")
    path.write_text(json.dumps(value, ensure_ascii=False))


def _point_transformer_at(path: Path, repository: str, local: Path) -> None:


    import srsly

    value = srsly.msgpack_loads(path.read_bytes())
    replaced = 0
    for shims in value["shims"]:
        for index, data in enumerate(shims):
            shim = srsly.msgpack_loads(data)
            if shim.get("config", {}).get("_name_or_path") == repository:
                shim["config"]["_name_or_path"] = str(local)
                shims[index] = srsly.msgpack_dumps(shim)
                replaced += 1
    if replaced != 1:
        raise ValueError(f"{path} names {repository} in {replaced} shims, not one")
    path.write_bytes(srsly.msgpack_dumps(value))


def _hf_objects(component: Any) -> Any:
    for node in component.model.walk():
        if node.shims:
            return node.shims[0]._hfmodel
    raise ValueError(f"component {component.name!r} holds no transformer")


def prepare_model_bundle(
    model_root: Path,
    *,
    probe_text: str,
    morphology_keys: Sequence[str] = (),
    boundary_component: str | None = None,
) -> dict[str, Any]:
    """Seal deterministic profile metadata beside an already-saved spaCy model."""

    _validate_requested_morphology_keys(morphology_keys)
    import spacy

    root = model_root.resolve(strict=True)
    admit_saved_transformers(spacy.util.load_config(root / "config.cfg"))
    full = spacy.load(root)
    names = list(full.pipe_names)
    summary = full.analyze_pipes(pretty=False)["summary"]
    boundary_candidates = [
        name
        for name in names
        if "token.is_sent_start" in summary[name]["assigns"]
        or "doc.sents" in summary[name]["assigns"]
    ]
    if boundary_component is None:
        if len(boundary_candidates) == 1:
            boundary_component = boundary_candidates[0]
        elif len(boundary_candidates) > 1:
            raise ValueError(
                "multiple boundary producers require an explicit selection"
            )
    elif boundary_component not in boundary_candidates:
        raise ValueError(
            "selected boundary component does not produce sentence boundaries"
        )

    tokenizer_only = spacy.load(root, exclude=names)
    probe = tokenizer_only(probe_text)
    base_assignments = {
        attribute
        for attribute, available in MORPH_TARGETS.items()
        if all(available(token) for token in probe if not token.is_space)
    }
    morphology_components = _morphology_components(
        names, summary, base_assignments, None
    )
    if morphology_components is None:
        try:
            _validate_morphology_probe(root, names, names, probe_text)
        except ValueError:
            pass
        else:
            morphology_components = names
    morphology_available = morphology_components is not None

    selections: dict[str, dict[str, Any]] = {}
    selections["tokenize/input"] = _selection(
        full, [], "input-v1", [PORTABLE_SCHEMA[0]]
    )
    if boundary_component is not None:
        selections["tokenize/analyzer"] = _selection(
            full,
            _components_through(names, summary, set(), boundary_component),
            f"spacy:{boundary_component}:v1",
            [PORTABLE_SCHEMA[0]],
        )

    if morphology_available:
        schema = list(PORTABLE_SCHEMA) + [
            {"key": key, "kind": "string_set"} for key in morphology_keys
        ]
        _validate_schema(schema)
        _validate_morphology_probe(root, names, morphology_components, probe_text)
        selections["morphology/input"] = _selection(
            full, morphology_components, "input-v1", schema
        )
        if boundary_component is not None:
            combined = _morphology_components(
                names, summary, base_assignments, boundary_component
            )
            if combined is None:
                combined = names
            _validate_morphology_probe(root, names, combined, probe_text)
            selections["morphology/analyzer"] = _selection(
                full, combined, f"spacy:{boundary_component}:v1", schema
            )

    value = {
        "format": PROFILE_FORMAT,
        "language": full.lang,
        "selections": selections,
        "selection_digests": {
            key: selection_digest(selection) for key, selection in selections.items()
        },
    }
    profile_path = root / PROFILE_FILE
    profile_path.write_bytes(canonical_json(value) + b"\n")
    return value


def load_profile(model_root: Path) -> dict[str, Any]:
    path = model_root / PROFILE_FILE
    value = json.loads(path.read_bytes(), object_pairs_hook=_unique_object)
    if not isinstance(value, dict) or set(value) != {
        "format",
        "language",
        "selections",
        "selection_digests",
    }:
        raise ValueError("spaCy model profile has the wrong shape")
    if value["format"] != PROFILE_FORMAT or not isinstance(value["language"], str):
        raise ValueError("spaCy model profile identity is invalid")
    if not isinstance(value["selections"], dict) or not value["selections"]:
        raise ValueError("spaCy model profile has no selections")
    if not isinstance(value["selection_digests"], dict) or set(
        value["selection_digests"]
    ) != set(value["selections"]):
        raise ValueError("spaCy model profile selection digests have the wrong shape")
    for key, selection in value["selections"].items():
        if key not in {
            "tokenize/input",
            "tokenize/analyzer",
            "morphology/input",
            "morphology/analyzer",
        }:
            raise ValueError("spaCy model profile has an unknown selection")
        _validate_selection(selection)
        if value["selection_digests"][key] != selection_digest(selection):
            raise ValueError("spaCy model profile selection digest mismatch")
    return value


def selection_digest(selection: Mapping[str, Any]) -> str:
    return digest_json(selection)


def resource_manifest_digest(resource_root: Path, manifest_root: Path) -> str:
    return _resource_manifest_digest(resource_root, manifest_root, provider="spaCy")


def _morphology_components(
    names: Sequence[str],
    summary: Mapping[str, Mapping[str, Any]],
    base_assignments: set[str],
    boundary: str | None,
) -> list[str] | None:
    selected = _components_through(
        names, summary, set(MORPH_TARGETS) | MORPH_OUTPUTS, boundary
    )
    assigned = set(base_assignments).union(
        *(summary[name]["assigns"] for name in selected)
    )
    return selected if assigned.issuperset(MORPH_TARGETS) else None


def _components_through(
    names: Sequence[str],
    summary: Mapping[str, Mapping[str, Any]],
    targets: set[str],
    boundary: str | None,
) -> list[str]:


    last = -1
    for index, name in enumerate(names):
        if name == boundary or targets.intersection(summary[name]["assigns"]):
            last = index
    return list(names[: last + 1])


def _selection(
    pipeline: Any,
    components: Sequence[str],
    boundary_policy: str,
    schema: Sequence[Mapping[str, str]],
) -> dict[str, Any]:
    schema = list(schema)
    if len(schema) > 1 and any(
        "token.dep" in pipeline.analyze_pipes(pretty=False)["summary"][name]["assigns"]
        for name in components
    ):
        schema.extend(
            [
                {"key": "dep.head", "kind": "scalar"},
                {"key": "dep.rel", "kind": "scalar"},
                {"key": "dep.enhanced", "kind": "string_set"},
            ]
        )
    if len(schema) > 1 and any(
        pipeline.config["components"][name].get("factory") == "bunsetu_recognizer"
        for name in components
    ):
        schema.append({"key": BUNSETU_KEY, "kind": "scalar"})
    from .adapter import _projection_order

    schema.sort(key=lambda attribute: _projection_order(attribute["key"]))
    configured = []
    for name in components:
        config = _plain(pipeline.config["components"][name])
        configured.append(
            {
                "name": name,
                "factory": config.get("factory"),
                "config_digest": digest_json(config),
            }
        )
    tokenizer = _plain(pipeline.config["nlp"].get("tokenizer", {}))
    return {
        "boundary_policy": boundary_policy,
        "schema": list(schema),
        "tokenizer_config_digest": digest_json(tokenizer),
        "components": configured,
    }


def _validate_morphology_probe(
    root: Path, all_names: Sequence[str], selected: Sequence[str], probe_text: str
) -> None:
    import spacy

    pipeline = spacy.load(
        root, exclude=[name for name in all_names if name not in selected]
    )
    doc = pipeline(probe_text)
    tokens = [token for token in doc if not token.is_space]
    if not tokens or any(
        not available(token) for token in tokens for available in MORPH_TARGETS.values()
    ):
        raise ValueError("model does not produce the portable morphology capability")


def _validate_selection(value: Any) -> None:
    required = {
        "boundary_policy",
        "schema",
        "tokenizer_config_digest",
        "components",
    }
    if not isinstance(value, dict) or set(value) != required:
        raise ValueError("spaCy profile selection has the wrong shape")
    if not isinstance(value["boundary_policy"], str) or not value["boundary_policy"]:
        raise ValueError("spaCy profile boundary policy is invalid")
    _validate_schema(value["schema"])
    _validate_hex_digest(value["tokenizer_config_digest"])
    if not isinstance(value["components"], list):
        raise ValueError("spaCy profile components must be an array")
    names = []
    for component in value["components"]:
        if not isinstance(component, dict) or set(component) != {
            "name",
            "factory",
            "config_digest",
        }:
            raise ValueError("spaCy profile component has the wrong shape")
        if not isinstance(component["name"], str) or not component["name"]:
            raise ValueError("spaCy profile component name is invalid")
        if not isinstance(component["factory"], str) or not component["factory"]:
            raise ValueError("spaCy profile component factory is invalid")
        _validate_hex_digest(component["config_digest"])
        names.append(component["name"])
    if names != sorted(set(names), key=names.index):
        raise ValueError("spaCy profile component names must be unique")


def _validate_schema(value: Any) -> None:
    from .adapter import ProjectionAttribute, _validate_projection

    if not isinstance(value, list) or not value:
        raise ValueError("spaCy profile schema must be a nonempty array")
    if any(
        not isinstance(item, dict) or set(item) != {"key", "kind"} for item in value
    ):
        raise ValueError("spaCy profile schema entry has the wrong shape")
    if any(
        not isinstance(item["key"], str) or item["kind"] not in {"scalar", "string_set"}
        for item in value
    ):
        raise ValueError("spaCy profile schema entry is invalid")
    _validate_projection(tuple(ProjectionAttribute(**item) for item in value))


def _validate_requested_morphology_keys(keys: Sequence[str]) -> None:
    try:
        if any(
            not isinstance(key, str) or not key.startswith(("feat.", "spacy.morph."))
            for key in keys
        ):
            raise ValueError("invalid morphology key")
        _validate_schema(
            list(PORTABLE_SCHEMA) + [{"key": key, "kind": "string_set"} for key in keys]
        )
    except ValueError as error:
        raise ValueError(f"invalid morphology keys: {error}") from error


def _plain(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _plain(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain(item) for item in value]
    return value


def _validate_hex_digest(value: Any) -> None:
    if not isinstance(value, str) or len(value) != 64:
        raise ValueError("spaCy profile digest is invalid")
    try:
        bytes.fromhex(value)
    except ValueError as error:
        raise ValueError("spaCy profile digest is invalid") from error
