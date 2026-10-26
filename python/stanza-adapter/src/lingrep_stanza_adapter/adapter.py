from __future__ import annotations

import importlib
import importlib.metadata
import platform
import socket
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Literal

from lingrep_adapter_protocol import (
    ProjectionAttribute as ProjectionAttribute,
)
from lingrep_adapter_protocol import (
    _component_digest as _component_digest,
)
from lingrep_adapter_protocol import (
    _decode_batch_begin as _decode_batch_begin,
)
from lingrep_adapter_protocol import (
    _decode_batch_end as _decode_batch_end,
)
from lingrep_adapter_protocol import (
    _decode_document as _decode_document,
)
from lingrep_adapter_protocol import (
    _decode_limits as _decode_limits,
)
from lingrep_adapter_protocol import (
    _emit_record as _emit_record,
)
from lingrep_adapter_protocol import (
    _encode_record as _encode_record,
)
from lingrep_adapter_protocol import (
    _json_string_size as _json_string_size,
)
from lingrep_adapter_protocol import (
    _read_record as _read_record,
)
from lingrep_adapter_protocol import (
    _read_required_record as _read_required_record,
)
from lingrep_adapter_protocol import (
    _required_int as _required_int,
)
from lingrep_adapter_protocol import (
    _unique_object as _unique_object,
)
from lingrep_adapter_protocol import (
    _validate_digest as _validate_digest,
)
from lingrep_adapter_protocol import (
    _validate_json_string_allocations as _validate_json_string_allocations,
)
from lingrep_adapter_protocol import (
    _write_record as _write_record,
)
from lingrep_adapter_protocol import (
    decode_header as _decode_protocol_header,
)

from .bundle import (
    BASE_UD_FEATURE_NAMES,
    _is_feature_key,
    load_profile,
    resource_manifest_digest,
    selection_digest,
)

ValueKind = Literal["scalar", "string_set"]
Segmentation = Literal["input", "analyzer"]
Profile = Literal["tokenize", "morphology"]
Backend = Literal["cpu", "cuda", "rocm"]


@dataclass(frozen=True)
class LoadedPipeline:
    pipeline: Any
    runtime: str
    language: str
    boundary_policy: str
    schema: tuple[ProjectionAttribute, ...]
    component_closure: tuple[dict[str, str], ...]
    output_config_digest: str


PipelineLoader = Callable[[str, str, str, str, str], LoadedPipeline]


def analyze_text(
    pipeline: Callable[[str], Any],
    text: str,
    projection: Sequence[ProjectionAttribute],
    profile: Profile,
    segmentation: Segmentation,
    resident_limit: int | None = None,
) -> list[dict[str, Any]]:
    """Invokes Stanza only after provider-specific input admission."""

    if segmentation == "input" and ("\r" in text or "\n" in text):
        raise ValueError("Stanza input segmentation refuses CR or LF")
    document = pipeline(text)
    if getattr(document, "text", text) != text:
        raise ValueError("Stanza changed the admitted document text")
    return list(
        iter_analyzed_sentences(
            document,
            text,
            projection,
            profile,
            segmentation,
            resident_limit=resident_limit,
        )
    )


def iter_analyzed_sentences(
    document: Any,
    admitted_text: str,
    projection: Sequence[ProjectionAttribute],
    profile: Profile,
    segmentation: Segmentation,
    resident_limit: int | None = None,
) -> Iterable[dict[str, Any]]:
    """Projects Stanza surface tokens and morphology words without fake offsets."""

    _validate_projection(projection)
    if profile not in ("tokenize", "morphology"):
        raise ValueError(f"unsupported extraction profile {profile!r}")
    tokenize_profile = profile == "tokenize"
    offsets = _utf8_boundaries(admitted_text)
    sentences = list(document.sentences)
    if segmentation == "input":
        if len(sentences) != 1:
            raise ValueError(
                "Stanza input segmentation must return exactly one sentence"
            )
        units: Iterable[tuple[int, int, Sequence[Any]]] = [
            (0, len(admitted_text), sentences[0].tokens)
        ]
    elif segmentation == "analyzer":
        units = (_sentence_unit(sentence) for sentence in sentences)
    else:
        raise ValueError(f"unsupported segmentation mode {segmentation!r}")

    for start_char, end_char, tokens in units:
        if not 0 <= start_char <= end_char <= len(admitted_text):
            raise ValueError("Stanza sentence range lies outside admitted text")
        surface_ranges: list[list[int]] = []
        word_ranges: list[list[int]] = []
        values: list[Any] = []
        word_count = 0
        prior_end = offsets[start_char]
        expanded = False
        resident_bytes = 128
        for source_token in tokens:
            token_start = _required_index(source_token.start_char, "token range start")
            token_end = _required_index(source_token.end_char, "token range end")
            if not start_char <= token_start < token_end <= end_char:
                raise ValueError("Stanza token range lies outside its sentence")
            byte_range = [offsets[token_start], offsets[token_end]]
            if byte_range[0] < prior_end:
                raise ValueError("Stanza token ranges overlap or decrease")
            prior_end = byte_range[1]
            source = admitted_text[token_start:token_end]
            if source_token.text != source:
                raise ValueError("Stanza surface token differs from its source range")
            words = list(source_token.words)
            if not words:
                raise ValueError("Stanza surface token has no aligned words")
            word_start = word_count
            if tokenize_profile:
                resident_bytes += 32 + _value_size(source)
                if resident_limit is not None and resident_bytes > resident_limit:
                    raise ValueError(
                        "Stanza adapter response sentence exceeds its residency bound"
                    )
                values.append(source)
                word_count += 1
            else:
                for syntax_word in words:
                    projected = _project_word(syntax_word, projection)
                    resident_bytes += 32 + sum(
                        _value_size(value) for value in projected
                    )
                    if resident_limit is not None and resident_bytes > resident_limit:
                        raise ValueError(
                            "Stanza adapter response sentence exceeds "
                            "its residency bound"
                        )
                    values.extend(projected)
                    word_count += 1
            word_ranges.append([word_start, word_count])
            surface_ranges.append(byte_range)
            expanded |= not tokenize_profile and (
                len(words) != 1 or words[0].text != source
            )

        alignment = (
            {"kind": "expanded", "word_ranges": word_ranges}
            if expanded
            else {"kind": "identity"}
        )
        yield {
            "source_range": [offsets[start_char], offsets[end_char]],
            "surface_ranges": surface_ranges,
            "alignment": alignment,
            "word_count": word_count,
            "values": values,
        }


def serve(
    stdin: BinaryIO,
    stdout: BinaryIO,
    *,
    resource_root: str,
    language: str,
    backend: Backend,
    batch_size: int,
    lock_digest: str,
    load_pipeline: PipelineLoader | None = None,
) -> None:
    """Runs one strict, long-lived Stanza morphology protocol process."""

    if batch_size != 1:
        raise ValueError("Stanza provider batch size must be one")
    _validate_digest(lock_digest, "lock digest")
    first = _read_record(stdin, 1 << 20)
    if first is None:
        raise ValueError("missing run_header")
    header, header_bytes = first
    _decode_header(header, header_bytes)
    profile = header["profile"]
    segmentation = header["segmentation"]
    limits = _decode_limits(header["protocol_limits"])
    projection = _decode_projection(header["projection"])
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
        actual_resource_digest = resource_manifest_digest(
            Path(resource_root), Path.cwd()
        )
        if actual_resource_digest != header["resource_manifest_digest"]:
            raise ValueError(
                "Stanza resource root does not match the declared resource manifest"
            )
    loader = load_pipeline or load_stanza_pipeline
    loaded = loader(resource_root, language, backend, profile, segmentation)
    if loaded.language != language or loaded.language != header["language_policy"]:
        raise ValueError("loaded Stanza language differs from language_policy")
    if loaded.boundary_policy != header["boundary_policy"]:
        raise ValueError("loaded Stanza boundary producer differs from boundary_policy")
    expected_closure = [
        {
            "name": "adapter-command",
            "digest": _component_digest(header, "adapter-command"),
        },
        *loaded.component_closure,
    ]
    expected_closure.sort(key=lambda component: component["name"])
    if header["component_closure"] != expected_closure:
        raise ValueError("loaded Stanza component closure differs from run_header")
    if header["output_config_digest"] != loaded.output_config_digest:
        raise ValueError("loaded Stanza output configuration differs from run_header")
    produced_schema = list(loaded.schema)
    if len(produced_schema) > limits["max_schema_keys"] or any(
        len(attribute.key.encode("utf-8")) > limits["max_schema_key_bytes"]
        for attribute in produced_schema
    ):
        raise ValueError("produced Stanza schema exceeds its count or key byte bound")
    produced_by_key = {attribute.key: attribute for attribute in produced_schema}
    if any(produced_by_key.get(attribute.key) != attribute for attribute in projection):
        raise ValueError("requested projection is unavailable from the Stanza profile")

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
            document, encoded_bytes = _read_required_record(
                stdin, limits["max_record_bytes"], "document"
            )
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
            if document["kind"] != expected_kind:
                raise ValueError("document kind differs from segmentation mode")
            if segmentation == "input" and (
                "\r" in document["text"] or "\n" in document["text"]
            ):
                raise ValueError("Stanza input segmentation refuses CR or LF")
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
        for ordinal, document in enumerate(documents):
            text = document["text"]
            analyzed = loaded.pipeline(text)
            if getattr(analyzed, "text", text) != text:
                raise ValueError("Stanza changed the admitted document text")
            sentence_count = 0
            for sentence_ordinal, sentence in enumerate(
                iter_analyzed_sentences(
                    analyzed,
                    text,
                    projection,
                    profile,
                    segmentation,
                    resident_limit=min(
                        limits["max_record_bytes"],
                        limits["max_response_batch_bytes"] - response_bytes,
                    ),
                )
            ):
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
                sentence_count += 1
            sentence_counts.append(sentence_count)
        complete = {
            "record": "batch_complete",
            "batch_id": batch_id,
            "sentence_counts": sentence_counts,
        }
        remaining = limits["max_response_batch_bytes"] - response_bytes
        encoded = _encode_record(complete, min(limits["max_record_bytes"], remaining))
        _write_record(stdout, encoded)
        next_batch_id += 1


def load_stanza_pipeline(
    resource_root: str,
    language: str,
    backend: str,
    profile: str,
    segmentation: str,
) -> LoadedPipeline:
    stanza, torch, device, runtime_version, driver_version = _initialize_backend(
        backend
    )
    root = Path(resource_root).resolve(strict=True)
    model_profile = load_profile(root)
    selection_key = f"{profile}/{segmentation}"
    selection = model_profile["selections"].get(selection_key)
    if selection is None:
        raise ValueError(f"Stanza model does not provide {selection_key}")
    if language != model_profile["language"]:
        raise ValueError("requested Stanza language differs from model profile")
    pipeline = stanza.Pipeline(
        lang=language,
        dir=str(root),
        processors=",".join(selection["processors"]),
        package=selection["packages"],
        download_method=None,
        resources_filepath=str(root / "resources.json"),
        tokenize_no_ssplit=segmentation == "input",
        use_gpu=backend != "cpu",
        verbose=False,
    )
    if list(pipeline.processors) != selection["processors"]:
        raise ValueError("loaded Stanza processor closure differs from profile")
    schema = tuple(
        ProjectionAttribute(attribute["key"], attribute["kind"])
        for attribute in selection["schema"]
    )
    _validate_projection(schema)
    selection_hash = selection_digest(selection)
    return LoadedPipeline(
        pipeline=pipeline,
        runtime=_runtime_description(
            stanza.__version__,
            torch.__version__,
            backend,
            device,
            runtime_version,
            driver_version,
        ),
        language=model_profile["language"],
        boundary_policy=selection["boundary_policy"],
        schema=schema,
        component_closure=({"name": "stanza-profile", "digest": selection_hash},),
        output_config_digest=selection_hash,
    )


def _sentence_unit(sentence: Any) -> tuple[int, int, Sequence[Any]]:
    tokens = list(sentence.tokens)
    if not tokens:
        raise ValueError("Stanza returned an empty analyzer sentence")
    return (
        _required_index(tokens[0].start_char, "sentence range start"),
        _required_index(tokens[-1].end_char, "sentence range end"),
        tokens,
    )


def _project_word(
    syntax_word: Any, projection: Sequence[ProjectionAttribute]
) -> list[Any]:
    form = _optional_string(syntax_word.text, "word form")
    if form is None:
        raise ValueError("Stanza word form must be nonempty")
    scalar = {
        "text": form,
        "lemma": _optional_string(syntax_word.lemma, "lemma"),
        "upos": _optional_string(syntax_word.upos, "UPOS"),
        "xpos": _optional_string(syntax_word.xpos, "XPOS"),
    }
    features = _parse_features(syntax_word.feats)
    return [
        scalar[attribute.key]
        if attribute.kind == "scalar"
        else features.get(attribute.key, [])
        for attribute in projection
    ]


def _parse_features(value: Any) -> dict[str, list[str]]:
    if value is None or value == "_" or value == "":
        return {}
    if not isinstance(value, str):
        raise ValueError("Stanza features must be a string or null")
    result: dict[str, list[str]] = {}
    for field in value.split("|"):
        name, separator, raw_members = field.partition("=")
        key = f"feat.{name}"
        if not separator or not _is_feature_key(key) or key in result:
            if name.split("[", 1)[0] not in BASE_UD_FEATURE_NAMES:
                continue
            raise ValueError("Stanza features are malformed or duplicated")
        members = raw_members.split(",")
        if any(not member for member in members):
            raise ValueError("Stanza feature sets contain an empty value")
        result[key] = sorted(set(members), key=lambda member: member.encode("utf-8"))
    return result


def _validate_projection(projection: Sequence[ProjectionAttribute]) -> None:
    if not projection or not any(attribute.key == "text" for attribute in projection):
        raise ValueError("Stanza projection must include text")
    keys: list[str] = []
    for attribute in projection:
        if attribute.key in {"text", "lemma", "upos", "xpos"}:
            if attribute.kind != "scalar":
                raise ValueError("Stanza scalar projection has the wrong kind")
        elif not _is_feature_key(attribute.key) or attribute.kind != "string_set":
            raise ValueError("Stanza projection contains an unavailable key or kind")
        keys.append(attribute.key)
    if len(keys) != len(set(keys)):
        raise ValueError("Stanza projection contains duplicate keys")


def _utf8_boundaries(text: str) -> list[int]:
    offsets = [0]
    total = 0
    for character in text:
        total += len(character.encode("utf-8"))
        offsets.append(total)
    return offsets


def _required_index(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"Stanza {label} is invalid")
    return value


def _optional_string(value: Any, label: str) -> str | None:
    if value is None or value == "_" or value == "":
        return None
    if not isinstance(value, str):
        raise ValueError(f"Stanza {label} must be a string or null")
    return value


def _value_size(value: Any) -> int:
    if value is None:
        return 4
    if isinstance(value, str):
        return len(value.encode("utf-8")) + 16
    return 24 + sum(len(member.encode("utf-8")) + 16 for member in value)


def _initialize_backend(
    backend: str,
) -> tuple[Any, Any, str | None, str | None, str | None]:
    if backend not in {"cpu", "cuda", "rocm"}:
        raise ValueError(f"unsupported backend {backend!r}")
    torch = importlib.import_module("torch")
    device = None
    runtime_version = None
    driver_version = None
    if backend in {"cuda", "rocm"}:
        if not torch.cuda.is_available():
            raise RuntimeError(f"{backend} backend requested but Torch found no GPU")
        if backend == "rocm" and not getattr(torch.version, "hip", None):
            raise RuntimeError("ROCm backend requires a HIP-enabled Torch build")
        if backend == "cuda" and not getattr(torch.version, "cuda", None):
            raise RuntimeError("CUDA backend requires a CUDA-enabled Torch build")
        runtime_version = str(
            torch.version.hip if backend == "rocm" else torch.version.cuda
        )
        driver_getter = getattr(torch._C, "_cuda_getDriverVersion", None)
        if driver_getter is None:
            raise RuntimeError("accelerator backend cannot report its driver version")
        driver_version = str(driver_getter())
        device = torch.cuda.get_device_name(0)
    stanza = importlib.import_module("stanza")
    return stanza, torch, device, runtime_version, driver_version


def _runtime_description(
    stanza_version: str,
    torch_version: str,
    backend: str,
    device: str | None,
    accelerator_runtime: str | None,
    accelerator_driver: str | None,
) -> str:
    parts = [
        f"python-{platform.python_version()}",
        f"stanza-{stanza_version}",
        f"torch-{torch_version}",
        f"backend-{backend}",
        f"host-{socket.gethostname()}",
        f"platform-{platform.platform()}",
    ]
    if device is not None:
        parts.append(f"device-{device}")
    if accelerator_runtime is not None:
        parts.append(f"accelerator-runtime-{accelerator_runtime}")
    if accelerator_driver is not None:
        parts.append(f"accelerator-driver-{accelerator_driver}")
    return "/".join(parts)


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
    _decode_protocol_header(
        value,
        encoded_bytes,
        family="stanza",
        provider="Stanza",
        projection_decoder=_decode_projection,
    )
