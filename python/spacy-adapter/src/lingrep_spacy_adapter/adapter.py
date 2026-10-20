from __future__ import annotations

import importlib
import importlib.metadata
import json
import platform
import socket
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import (
    Any,
    BinaryIO,
    Literal,
    Protocol,
)

from .bundle import (
    digest_json,
    load_profile,
    resource_manifest_digest,
    selection_digest,
)

ValueKind = Literal["scalar", "string_set"]
Segmentation = Literal["input", "analyzer"]
Backend = Literal["cpu", "cuda", "rocm"]
PipelineLoader = Callable[[str, str, str, str], "LoadedPipeline"]

PORTABLE_KEYS = ("text", "lemma", "norm", "upos", "xpos")
DEPENDENCY_KEYS = ("dep.head", "dep.rel", "dep.enhanced")

# UD v2 feature names accepted by the adapter. This is the base-name inventory
# in UniversalDependencies/docs `_u-feat` at 26c65669, plus `Variant`, which is
# retained in language feature inventories. Hyphenated relationship suffixes
# in the documentation are represented by spaCy as layers (for example,
# `Gender[psor]`) and therefore share their base entry here. Names outside this
# pinned registry remain visible under the provider namespace.
UD_FEATURE_NAMES = frozenset(
    {
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
    }
)


@dataclass(frozen=True)
class ProjectionAttribute:
    key: str
    kind: ValueKind


@dataclass(frozen=True)
class LoadedPipeline:
    pipeline: Any
    runtime: str
    language: str
    boundary_policy: str
    schema: tuple[ProjectionAttribute, ...]
    component_closure: tuple[dict[str, str], ...]
    output_config_digest: str


class MorphLike(Protocol):
    def to_dict(self) -> Mapping[str, Any]: ...


class TokenLike(Protocol):
    i: int
    text: str
    idx: int
    lemma_: str
    norm_: str
    pos_: str
    tag_: str
    morph: MorphLike
    is_space: bool
    dep_: str
    head: TokenLike


class SpanLike(Protocol):
    start_char: int
    end_char: int

    def __iter__(self) -> Iterable[TokenLike]: ...


class DocLike(Protocol):
    text: str
    sents: Iterable[SpanLike]

    def __iter__(self) -> Iterable[TokenLike]: ...


def morph_attribute_key(name: str) -> str:
    """Maps one spaCy morphology field into the canonical V8 namespace."""

    base, layer = _split_layer(name)
    if base in UD_FEATURE_NAMES:
        return f"feat.{base}{f'[{layer}]' if layer is not None else ''}"
    if layer is not None or not name or not name.isascii() or not name.isalnum():
        raise ValueError(
            f"spaCy morphology name {name!r} has no canonical provider key"
        )
    return f"spacy.morph.{name.lower()}"


def analyze_document(
    doc: DocLike,
    projection: Sequence[ProjectionAttribute],
    segmentation: Segmentation,
) -> list[dict[str, Any]]:
    """Projects one already-created spaCy document into protocol sentences."""

    return list(iter_analyzed_sentences(doc, projection, segmentation))


def iter_analyzed_sentences(
    doc: DocLike,
    projection: Sequence[ProjectionAttribute],
    segmentation: Segmentation,
    resident_limit: int | None = None,
) -> Iterable[dict[str, Any]]:
    """Yields mapped sentences without retaining a document-sized result list."""

    _validate_projection(projection)
    offsets = _utf8_boundaries(doc.text)
    if segmentation == "input":
        units: Iterable[tuple[int, int, Iterable[TokenLike]]] = [
            (0, len(doc.text), iter(doc))
        ]
    elif segmentation == "analyzer":
        units = (
            (sentence.start_char, sentence.end_char, iter(sentence))
            for sentence in doc.sents
        )
    else:
        raise ValueError(f"unsupported segmentation mode {segmentation!r}")

    for start_char, end_char, tokens in units:
        if not 0 <= start_char <= end_char <= len(doc.text):
            raise ValueError("spaCy sentence range lies outside the admitted text")
        surface_ranges = []
        values: list[Any] = []
        word_count = 0
        resident_bytes = 128
        prior_end = offsets[start_char]
        dependency_requested = any(
            attribute.key.startswith("dep.") for attribute in projection
        )
        first_token_index: int | None = None
        for token in tokens:
            if token.is_space:
                continue
            if dependency_requested and first_token_index is None:
                first_token_index = token.i
            token_end = token.idx + len(token.text)
            if not start_char <= token.idx <= token_end <= end_char:
                raise ValueError("spaCy token range lies outside its sentence")
            byte_range = [offsets[token.idx], offsets[token_end]]
            if byte_range[0] < prior_end:
                raise ValueError("spaCy token ranges overlap or decrease")
            prior_end = byte_range[1]
            projected = _project_token(token, projection, first_token_index)
            resident_bytes += 32 + sum(_json_value_size(value) for value in projected)
            if resident_limit is not None and resident_bytes > resident_limit:
                raise ValueError(
                    "adapter response sentence exceeds its residency bound"
                )
            surface_ranges.append(byte_range)
            values.extend(projected)
            word_count += 1
        yield {
            "source_range": [offsets[start_char], offsets[end_char]],
            "surface_ranges": surface_ranges,
            "alignment": {"kind": "identity"},
            "word_count": word_count,
            "values": values,
        }


def serve(
    stdin: BinaryIO,
    stdout: BinaryIO,
    *,
    model_root: str,
    backend: Backend,
    batch_size: int,
    lock_digest: str,
    load_pipeline: PipelineLoader | None = None,
) -> None:
    """Runs one strict, long-lived morphology protocol process."""

    if batch_size <= 0:
        raise ValueError("batch size must be positive")
    _validate_digest(lock_digest, "lock digest")
    first = _read_record(stdin, 1 << 20)
    if first is None:
        raise ValueError("missing run_header")
    header, header_bytes = first
    _decode_header(header, header_bytes)
    profile = header["profile"]
    segmentation = header["segmentation"]
    if profile not in {"tokenize", "morphology"}:
        raise ValueError(f"unsupported extraction profile {profile!r}")
    if segmentation not in {"input", "analyzer"}:
        raise ValueError(f"unsupported segmentation mode {segmentation!r}")
    limits = _decode_limits(header.get("protocol_limits"))
    projection = _decode_projection(header.get("projection"))
    if len(projection) > limits["max_schema_keys"] or any(
        len(attribute.key.encode("utf-8")) > limits["max_schema_key_bytes"]
        for attribute in projection
    ):
        raise ValueError("projection exceeds its key count or byte bound")
    if profile == "tokenize" and projection != [ProjectionAttribute("text", "scalar")]:
        raise ValueError("tokenize profile supports only the scalar text projection")
    if limits["max_record_bytes"] > 1 << 30:
        raise ValueError("max_record_bytes exceeds the adapter safety ceiling")

    if load_pipeline is None:
        actual_resource_digest = resource_manifest_digest(Path(model_root), Path.cwd())
        if actual_resource_digest != header["resource_manifest_digest"]:
            raise ValueError(
                "spaCy model root does not match the declared resource manifest"
            )
    loader = load_pipeline or load_spacy_pipeline
    loaded = loader(model_root, backend, profile, segmentation)
    if loaded.language != header["language_policy"]:
        raise ValueError("loaded spaCy language differs from language_policy")
    if loaded.boundary_policy != header["boundary_policy"]:
        raise ValueError("loaded spaCy boundary producer differs from boundary_policy")
    expected_closure = [
        {
            "name": "adapter-command",
            "digest": _component_digest(header, "adapter-command"),
        },
        *loaded.component_closure,
    ]
    expected_closure.sort(key=lambda component: component["name"])
    if header["component_closure"] != expected_closure:
        raise ValueError("loaded spaCy component closure differs from run_header")
    if header["output_config_digest"] != loaded.output_config_digest:
        raise ValueError("loaded spaCy output configuration differs from run_header")
    produced_schema = list(loaded.schema)
    if len(produced_schema) > limits["max_schema_keys"] or any(
        len(attribute.key.encode("utf-8")) > limits["max_schema_key_bytes"]
        for attribute in produced_schema
    ):
        raise ValueError("produced spaCy schema exceeds its count or key byte bound")
    produced_by_key = {attribute.key: attribute for attribute in produced_schema}
    if any(produced_by_key.get(attribute.key) != attribute for attribute in projection):
        raise ValueError(
            "requested projection is unavailable from the spaCy model profile"
        )
    pipeline = loaded.pipeline
    ready = {
        "record": "run_ready",
        "run_id": header["run_id"],
        "family": header["family"],
        "profile": profile,
        "segmentation": segmentation,
        "normalization": header["normalization"],
        "language_policy": header["language_policy"],
        "boundary_policy": header["boundary_policy"],
        "implementation_digest": header["implementation_digest"],
        "resource_manifest_digest": header["resource_manifest_digest"],
        "projection_policy_digest": header["projection_policy_digest"],
        "alignment_policy_digest": header["alignment_policy_digest"],
        "component_closure": header["component_closure"],
        "output_config_digest": loaded.output_config_digest,
        "schema": [
            {"key": attribute.key, "kind": attribute.kind}
            for attribute in produced_schema
        ],
        "returned_projection": header["projection"],
        "capabilities": [profile],
        "execution": {
            "backend": backend,
            "runtime": loaded.runtime,
            "lock_digest": lock_digest,
            "workers": 1,
            "batch_size": batch_size,
        },
    }
    _emit_record(stdout, ready, limits["max_record_bytes"])

    next_batch_id = 0
    last_input_id: int | None = None
    while True:
        begin_record = _read_record(stdin, limits["max_record_bytes"])
        if begin_record is None:
            return
        begin, request_bytes = begin_record
        batch_id, document_count = _decode_batch_begin(begin)
        if batch_id != next_batch_id:
            raise ValueError("batch identities must be contiguous")
        if not 0 < document_count <= limits["max_documents_per_batch"]:
            raise ValueError("document count exceeds the protocol bound")

        documents = []
        for ordinal in range(document_count):
            document_record = _read_required_record(
                stdin, limits["max_record_bytes"], "document"
            )
            document, encoded_bytes = document_record
            request_bytes += encoded_bytes
            if request_bytes > limits["max_request_batch_bytes"]:
                raise ValueError("request batch exceeds max_request_batch_bytes")
            _decode_document(document)
            if _required_int(document, "batch_id") != batch_id:
                raise ValueError("document batch identity mismatch")
            if _required_int(document, "document_ordinal") != ordinal:
                raise ValueError("document ordinals must be contiguous")
            input_id = _required_int(document, "input_id")
            if last_input_id is not None and input_id <= last_input_id:
                raise ValueError("input identities must be globally increasing")
            last_input_id = input_id
            expected_kind = "sentence" if segmentation == "input" else "segment"
            if document.get("kind") != expected_kind:
                raise ValueError("document kind differs from segmentation mode")
            if not isinstance(document.get("text"), str):
                raise ValueError("document text must be a string")
            documents.append(document)

        end, encoded_bytes = _read_required_record(
            stdin, limits["max_record_bytes"], "batch_end"
        )
        request_bytes += encoded_bytes
        if request_bytes > limits["max_request_batch_bytes"]:
            raise ValueError("request batch exceeds max_request_batch_bytes")
        if _decode_batch_end(end) != batch_id:
            raise ValueError("invalid batch_end record")

        response_bytes = 0
        sentence_counts = []
        texts = [document["text"] for document in documents]
        docs = pipeline.pipe(texts, batch_size=batch_size)
        produced = 0
        for ordinal, (document, doc) in enumerate(zip(documents, docs, strict=True)):
            produced += 1
            if doc.text != document["text"]:
                raise ValueError("spaCy changed the admitted document text")
            sentence_count = 0
            for sentence_ordinal, sentence in enumerate(
                iter_analyzed_sentences(
                    doc,
                    projection,
                    segmentation,
                    min(
                        limits["max_record_bytes"],
                        limits["max_response_batch_bytes"] - response_bytes,
                    ),
                )
            ):
                sentence_count += 1
                record = {
                    "record": "sentence",
                    "batch_id": batch_id,
                    "document_ordinal": ordinal,
                    "input_id": document["input_id"],
                    "sentence_ordinal": sentence_ordinal,
                    **sentence,
                    "warnings": [],
                }
                remaining = limits["max_response_batch_bytes"] - response_bytes
                encoded = _encode_record(
                    record, min(limits["max_record_bytes"], remaining)
                )
                _write_record(stdout, encoded)
                response_bytes += len(encoded)
            sentence_counts.append(sentence_count)
        if produced != document_count:
            raise ValueError("spaCy returned the wrong document count")
        complete = {
            "record": "batch_complete",
            "batch_id": batch_id,
            "sentence_counts": sentence_counts,
        }
        remaining = limits["max_response_batch_bytes"] - response_bytes
        encoded = _encode_record(complete, min(limits["max_record_bytes"], remaining))
        _write_record(stdout, encoded)
        next_batch_id += 1


def load_spacy_pipeline(
    model_root: str,
    backend: str,
    profile: str,
    segmentation: str,
) -> LoadedPipeline:
    """Loads one caller-selected model with explicit backend requirements."""

    (
        spacy,
        device,
        accelerator_runtime,
        accelerator_driver,
        torch_build,
        cupy_distribution,
    ) = initialize_spacy_backend(backend)
    root = Path(model_root).resolve(strict=True)
    model_profile = load_profile(root)
    selection_key = f"{profile}/{segmentation}"
    selection = model_profile["selections"].get(selection_key)
    if selection is None:
        raise ValueError(f"spaCy model does not provide {selection_key}")
    config = spacy.util.load_config(root / "config.cfg")
    all_components = list(config["nlp"]["pipeline"])
    selected = [component["name"] for component in selection["components"]]
    pipeline = spacy.load(
        root, exclude=[name for name in all_components if name not in selected]
    )
    if pipeline.lang != model_profile["language"] or pipeline.pipe_names != selected:
        raise ValueError(
            "loaded spaCy language or component order differs from profile"
        )
    tokenizer_config = _plain_config(pipeline.config["nlp"].get("tokenizer", {}))
    if digest_json(tokenizer_config) != selection["tokenizer_config_digest"]:
        raise ValueError("loaded spaCy tokenizer configuration differs from profile")
    for expected in selection["components"]:
        actual = _plain_config(pipeline.config["components"][expected["name"]])
        if (
            actual.get("factory") != expected["factory"]
            or digest_json(actual) != expected["config_digest"]
        ):
            raise ValueError(
                "loaded spaCy component configuration differs from profile"
            )
    schema = tuple(
        ProjectionAttribute(attribute["key"], attribute["kind"])
        for attribute in selection["schema"]
    )
    _validate_projection(schema)
    selection_hash = selection_digest(selection)
    runtime = _runtime_description(
        spacy.__version__,
        backend,
        device,
        accelerator_runtime,
        accelerator_driver,
        torch_build,
        cupy_distribution,
    )
    return LoadedPipeline(
        pipeline=pipeline,
        runtime=runtime,
        language=model_profile["language"],
        boundary_policy=selection["boundary_policy"],
        schema=schema,
        component_closure=({"name": "spacy-profile", "digest": selection_hash},),
        output_config_digest=selection_hash,
    )


def initialize_spacy_backend(
    backend: str,
) -> tuple[Any, str | None, str | None, str | None, str | None, str | None]:
    if backend not in {"cpu", "cuda", "rocm"}:
        raise ValueError(f"unsupported backend {backend!r}")
    device = None
    accelerator_runtime = None
    accelerator_driver = None
    torch_build = None
    cupy_distribution = None
    if backend in {"cuda", "rocm"}:
        torch = importlib.import_module("torch")
        if not torch.cuda.is_available():
            raise RuntimeError(f"{backend} backend requested but Torch found no GPU")
        if backend == "rocm" and not getattr(torch.version, "hip", None):
            raise RuntimeError("ROCm backend requires a HIP-enabled Torch build")
        if backend == "cuda" and not getattr(torch.version, "cuda", None):
            raise RuntimeError("CUDA backend requires a CUDA-enabled Torch build")
        cupy = importlib.import_module("cupy")
        cupy_runtime = cupy.cuda.runtime
        if backend == "rocm" and not cupy_runtime.is_hip:
            raise RuntimeError("ROCm backend requires a HIP-enabled CuPy build")
        if backend == "cuda" and cupy_runtime.is_hip:
            raise RuntimeError("CUDA backend requires a CUDA-enabled CuPy build")
        if cupy_runtime.getDeviceCount() < 1:
            raise RuntimeError(f"{backend} backend requested but CuPy found no GPU")
        accelerator_runtime = str(cupy_runtime.runtimeGetVersion())
        accelerator_driver = str(cupy_runtime.driverGetVersion())
        torch_build = str(
            torch.version.hip if backend == "rocm" else torch.version.cuda
        )
        cupy_distribution = "cupy-rocm-7-0" if backend == "rocm" else "cupy-cuda12x"
        device = torch.cuda.get_device_name(0)
    spacy = importlib.import_module("spacy")
    if backend in {"cuda", "rocm"}:
        spacy.require_gpu(0)
    return (
        spacy,
        device,
        accelerator_runtime,
        accelerator_driver,
        torch_build,
        cupy_distribution,
    )


def _split_layer(name: str) -> tuple[str, str | None]:
    if "[" not in name:
        return name, None
    base, suffix = name.split("[", 1)
    if not suffix.endswith("]") or suffix.count("]") != 1:
        raise ValueError(f"malformed layered morphology name {name!r}")
    layer = suffix[:-1]
    if not layer or not layer.isascii() or not layer.isalnum() or not layer.islower():
        raise ValueError(f"malformed layered morphology name {name!r}")
    return base, layer


def _utf8_boundaries(text: str) -> list[int]:
    boundaries = [0]
    total = 0
    for character in text:
        total += len(character.encode("utf-8"))
        boundaries.append(total)
    return boundaries


def _validate_projection(projection: Sequence[ProjectionAttribute]) -> None:
    if not projection:
        raise ValueError("projection must not be empty")
    seen: set[str] = set()
    for attribute in projection:
        if attribute.key in seen:
            raise ValueError(f"duplicate projection key {attribute.key!r}")
        seen.add(attribute.key)
        expected: ValueKind
        if attribute.key in PORTABLE_KEYS or attribute.key in ("dep.head", "dep.rel"):
            expected = "scalar"
        elif attribute.key == "dep.enhanced" or attribute.key.startswith(
            ("feat.", "spacy.morph.")
        ):
            expected = "string_set"
        else:
            raise ValueError(f"unsupported spaCy projection key {attribute.key!r}")
        if attribute.kind != expected:
            raise ValueError(
                f"projection key {attribute.key!r} must have {expected} kind"
            )
        if attribute.key.startswith("feat."):
            base, _ = _split_layer(attribute.key.removeprefix("feat."))
            if base not in UD_FEATURE_NAMES:
                raise ValueError(f"unsupported UD feature key {attribute.key!r}")
        if attribute.key.startswith("spacy.morph."):
            provider_name = attribute.key.removeprefix("spacy.morph.")
            if (
                not provider_name
                or not provider_name.isascii()
                or not provider_name.isalnum()
                or not provider_name.islower()
            ):
                raise ValueError(f"noncanonical spaCy provider key {attribute.key!r}")
    canonical = sorted(
        projection, key=lambda attribute: _projection_order(attribute.key)
    )
    if list(projection) != canonical:
        raise ValueError("projection keys are not in canonical V8 order")


def _projection_order(key: str) -> tuple[int, int, bytes]:
    if key in PORTABLE_KEYS:
        return 0, PORTABLE_KEYS.index(key), b""
    if key.startswith("feat."):
        return 1, 0, key.encode("utf-8")
    if key in DEPENDENCY_KEYS:
        return 2, DEPENDENCY_KEYS.index(key), b""
    return 3, 0, key.encode("utf-8")


def _project_token(
    token: TokenLike,
    projection: Sequence[ProjectionAttribute],
    first_token_index: int | None,
) -> list[Any]:
    morphology = _canonical_morphology(token.morph.to_dict())
    scalar = {
        "text": token.text,
        "lemma": token.lemma_ or None,
        "norm": token.norm_ or None,
        "upos": token.pos_ or None,
        "xpos": token.tag_ or None,
    }
    if any(attribute.key.startswith("dep.") for attribute in projection):
        assert first_token_index is not None
        scalar["dep.head"] = "0" if token.head.i == token.i else str(
            token.head.i - first_token_index + 1
        )
        scalar["dep.rel"] = token.dep_.lower() or None
        morphology["dep.enhanced"] = []
    return [
        scalar[attribute.key]
        if attribute.kind == "scalar"
        else morphology.get(attribute.key, [])
        for attribute in projection
    ]


def _canonical_morphology(values: Mapping[str, Any]) -> dict[str, list[str]]:
    canonical: dict[str, list[str]] = {}
    owners: dict[str, str] = {}
    for source_name, raw_values in values.items():
        key = morph_attribute_key(source_name)
        if key in owners and owners[key] != source_name:
            raise ValueError(
                f"spaCy morphology names {owners[key]!r} and {source_name!r} collide"
            )
        owners[key] = source_name
        if isinstance(raw_values, str):
            members = raw_values.split(",") if raw_values else []
        else:
            members = list(raw_values or [])
        if any(not isinstance(member, str) or not member for member in members):
            raise ValueError(f"morphology values for {source_name!r} must be strings")
        canonical[key] = sorted(set(members), key=lambda member: member.encode("utf-8"))
    return canonical


def _runtime_description(
    spacy_version: str,
    backend: str,
    device: str | None,
    accelerator_runtime: str | None = None,
    accelerator_driver: str | None = None,
    torch_build: str | None = None,
    cupy_distribution: str | None = None,
) -> str:
    parts = [f"python-{platform.python_version()}", f"spacy-{spacy_version}"]
    for distribution in (
        "ginza",
        "ja-ginza",
        "ja-ginza-electra",
        "torch",
        cupy_distribution,
    ):
        if distribution is None:
            continue
        try:
            version = importlib.metadata.version(distribution)
        except importlib.metadata.PackageNotFoundError:
            continue
        parts.append(f"{distribution}-{version}")
    parts.append(f"backend-{backend}")
    parts.append(f"host-{socket.gethostname()}")
    parts.append(f"platform-{platform.platform()}")
    if device is not None:
        parts.append(f"device-{device}")
    if accelerator_runtime is not None:
        parts.append(f"accelerator-runtime-{accelerator_runtime}")
    if accelerator_driver is not None:
        parts.append(f"accelerator-driver-{accelerator_driver}")
    if torch_build is not None:
        parts.append(f"torch-build-{torch_build}")
    return "/".join(parts)


def _component_digest(header: Mapping[str, Any], name: str) -> str:
    for component in header["component_closure"]:
        if component.get("name") == name:
            return component["digest"]
    raise ValueError(f"run_header omits required component {name!r}")


def _plain_config(value: Any) -> Any:
    if isinstance(value, Mapping):
        return {str(key): _plain_config(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_plain_config(item) for item in value]
    return value


def _decode_projection(value: Any) -> list[ProjectionAttribute]:
    if not isinstance(value, list):
        raise ValueError("run_header projection must be an array")
    projection = []
    for item in value:
        if not isinstance(item, dict) or set(item) != {"key", "kind"}:
            raise ValueError("projection entries must contain exactly key and kind")
        key = item["key"]
        kind = item["kind"]
        if not isinstance(key, str) or kind not in {"scalar", "string_set"}:
            raise ValueError("projection entry has an invalid key or kind")
        projection.append(ProjectionAttribute(key, kind))
    _validate_projection(projection)
    return projection


def _decode_header(value: Mapping[str, Any], encoded_bytes: int) -> None:
    if value.get("protocol") != "lingrep.external-morphology/v1":
        raise ValueError(f"unsupported protocol {value.get('protocol')!r}")
    required = {
        "record",
        "protocol",
        "run_id",
        "family",
        "profile",
        "segmentation",
        "normalization",
        "language_policy",
        "boundary_policy",
        "projection",
        "implementation_digest",
        "resource_manifest_digest",
        "projection_policy_digest",
        "alignment_policy_digest",
        "component_closure",
        "output_config_digest",
        "protocol_limits",
    }
    if set(value) != required:
        raise ValueError("run_header has the wrong shape")
    if value["record"] != "run_header":
        raise ValueError("first protocol record must be run_header")
    if value["family"] != "external":
        raise ValueError("spaCy adapter requires the external analyzer family")
    if value["profile"] not in {"tokenize", "morphology"}:
        raise ValueError("unsupported extraction profile")
    if value["segmentation"] not in {"input", "analyzer"}:
        raise ValueError("unsupported segmentation mode")
    if value["normalization"] not in {"none", "nfc"}:
        raise ValueError("spaCy adapter received an unsupported normalization policy")
    for name in ("run_id", "language_policy", "boundary_policy"):
        if not isinstance(value[name], str) or not value[name]:
            raise ValueError(f"run_header {name} must be a nonempty string")
    for name in (
        "implementation_digest",
        "resource_manifest_digest",
        "projection_policy_digest",
        "alignment_policy_digest",
        "output_config_digest",
    ):
        if not isinstance(value[name], str):
            raise ValueError(f"run_header {name} must be a digest string")
        _validate_digest(value[name], name)
    _decode_projection(value["projection"])
    limits = _decode_limits(value["protocol_limits"])
    if encoded_bytes > limits["max_record_bytes"]:
        raise ValueError("run_header exceeds its declared max_record_bytes")
    closure = value["component_closure"]
    if not isinstance(closure, list) or not closure:
        raise ValueError("component_closure must be a nonempty array")
    names = []
    for component in closure:
        if not isinstance(component, dict) or set(component) != {"name", "digest"}:
            raise ValueError("component_closure entry has the wrong shape")
        if not isinstance(component["name"], str) or not component["name"]:
            raise ValueError("component name must be nonempty")
        if not isinstance(component["digest"], str):
            raise ValueError("component digest must be a string")
        _validate_digest(component["digest"], "component digest")
        names.append(component["name"])
    if names != sorted(set(names)):
        raise ValueError("component_closure must be strictly name-ordered")
    if len(closure) > limits["max_schema_keys"] or any(
        len(name.encode("utf-8")) > limits["max_schema_key_bytes"] for name in names
    ):
        raise ValueError("component_closure exceeds its count or name byte bound")
    if any(
        len(value[name].encode("utf-8")) > limits["max_schema_key_bytes"]
        for name in ("run_id", "language_policy", "boundary_policy")
    ):
        raise ValueError("run identity or policy exceeds its byte bound")


def _decode_batch_begin(value: Mapping[str, Any]) -> tuple[int, int]:
    if set(value) != {"record", "batch_id", "document_count"}:
        raise ValueError("batch_begin has the wrong shape")
    if value["record"] != "batch_begin":
        raise ValueError("expected batch_begin at the run boundary")
    return _required_int(value, "batch_id"), _required_int(value, "document_count")


def _decode_document(value: Mapping[str, Any]) -> None:
    required = {
        "record",
        "batch_id",
        "document_ordinal",
        "input_id",
        "kind",
        "text",
        "provenance",
    }
    if set(value) != required or value["record"] != "document":
        raise ValueError("document has the wrong shape")
    for name in ("batch_id", "document_ordinal", "input_id"):
        _required_int(value, name)
    if value["kind"] not in {"sentence", "segment"} or not isinstance(
        value["text"], str
    ):
        raise ValueError("document kind or text has the wrong type")
    provenance = value["provenance"]
    required_provenance = {
        "source_identity",
        "document_identity",
        "paragraph_identity",
        "metadata_digest",
    }
    if not isinstance(provenance, dict) or set(provenance) != required_provenance:
        raise ValueError("document provenance has the wrong shape")
    for name in ("source_identity", "metadata_digest"):
        if not isinstance(provenance[name], str):
            raise ValueError("document provenance digest must be a string")
        _validate_digest(provenance[name], name)
    _required_int(provenance, "document_identity")
    if provenance["paragraph_identity"] is not None:
        _required_int(provenance, "paragraph_identity")


def _decode_batch_end(value: Mapping[str, Any]) -> int:
    if set(value) != {"record", "batch_id"} or value["record"] != "batch_end":
        raise ValueError("batch_end has the wrong shape")
    return _required_int(value, "batch_id")


def _decode_limits(value: Any) -> dict[str, int]:
    required = {
        "max_record_bytes",
        "max_request_batch_bytes",
        "max_response_batch_bytes",
        "max_documents_per_batch",
        "max_schema_keys",
        "max_schema_key_bytes",
        "max_warnings_per_sentence",
        "max_warning_message_bytes",
        "max_stderr_tail_bytes",
    }
    if not isinstance(value, dict) or set(value) != required:
        raise ValueError("run_header protocol_limits has the wrong shape")
    if any(type(value[name]) is not int or value[name] <= 0 for name in required):
        raise ValueError("protocol limits must be positive integers")
    return value


def _validate_digest(value: str, label: str) -> None:
    if len(value) != 64:
        raise ValueError(f"{label} must contain 32 hexadecimal bytes")
    try:
        bytes.fromhex(value)
    except ValueError as error:
        raise ValueError(f"{label} must contain 32 hexadecimal bytes") from error


def _required_int(record: Mapping[str, Any], key: str) -> int:
    value = record.get(key)
    if type(value) is not int or value < 0:
        raise ValueError(f"protocol field {key!r} must be a nonnegative integer")
    return value


def _read_required_record(
    stream: BinaryIO, limit: int, expected: str
) -> tuple[dict[str, Any], int]:
    record = _read_record(stream, limit)
    if record is None:
        raise ValueError(f"premature EOF while reading {expected}")
    return record


def _read_record(stream: BinaryIO, limit: int) -> tuple[dict[str, Any], int] | None:
    encoded = stream.readline(limit + 1)
    if not encoded:
        return None
    if len(encoded) > limit or not encoded.endswith(b"\n"):
        raise ValueError("protocol record exceeds its bound or lacks LF framing")
    if encoded.endswith(b"\r\n"):
        raise ValueError("CRLF protocol records are not admitted")
    try:
        value = json.loads(
            encoded[:-1].decode("utf-8"), object_pairs_hook=_unique_object
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid protocol JSON: {error}") from error
    if not isinstance(value, dict):
        raise ValueError("protocol record must be a JSON object")
    return value, len(encoded)


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _emit_record(stream: BinaryIO, record: Mapping[str, Any], limit: int) -> int:
    encoded = _encode_record(record, limit)
    _write_record(stream, encoded)
    return len(encoded)


def _encode_record(record: Mapping[str, Any], limit: int) -> bytes:
    if limit <= 0:
        raise ValueError("adapter response batch has no remaining byte capacity")
    _validate_json_string_allocations(record, limit)
    encoded = bytearray()
    encoder = json.JSONEncoder(
        ensure_ascii=False, separators=(",", ":"), allow_nan=False
    )
    for chunk in encoder.iterencode(record):
        for start in range(0, len(chunk), 4096):
            part = chunk[start : start + 4096].encode("utf-8")
            if len(encoded) + len(part) + 1 > limit:
                raise ValueError("adapter response exceeds its remaining wire bound")
            encoded.extend(part)
    encoded.append(0x0A)
    return bytes(encoded)


def _write_record(stream: BinaryIO, encoded: bytes) -> None:
    stream.write(encoded)
    stream.flush()


def _json_value_size(value: Any) -> int:
    if value is None:
        return 4
    if isinstance(value, str):
        return _json_string_size(value)
    if isinstance(value, list):
        return 2 + sum(_json_value_size(item) + 1 for item in value)
    if isinstance(value, (int, bool)):
        return 24
    raise ValueError("unsupported protocol JSON value")


def _json_string_size(value: str) -> int:
    size = 2
    for character in value:
        codepoint = ord(character)
        if character in {'"', "\\", "\b", "\f", "\n", "\r", "\t"}:
            size += 2
        elif codepoint < 0x20:
            size += 6
        else:
            size += len(character.encode("utf-8"))
    return size


def _validate_json_string_allocations(value: Any, limit: int) -> None:
    if isinstance(value, str):
        if _json_string_size(value) > limit:
            raise ValueError("adapter response string exceeds its remaining wire bound")
    elif isinstance(value, Mapping):
        for key, item in value.items():
            _validate_json_string_allocations(key, limit)
            _validate_json_string_allocations(item, limit)
    elif isinstance(value, Sequence) and not isinstance(value, (bytes, bytearray)):
        for item in value:
            _validate_json_string_allocations(item, limit)
