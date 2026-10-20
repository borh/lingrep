from __future__ import annotations

import importlib
import importlib.metadata
import json
import platform
import socket
from collections.abc import Callable, Iterable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO, Literal

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


def _component_digest(header: Mapping[str, Any], name: str) -> str:
    for component in header["component_closure"]:
        if component.get("name") == name:
            return component["digest"]
    raise ValueError(f"run_header omits required component {name!r}")


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
    if value["family"] != "stanza":
        raise ValueError("Stanza adapter requires the stanza analyzer family")
    if value["profile"] not in {"tokenize", "morphology"}:
        raise ValueError("unsupported extraction profile")
    if value["segmentation"] not in {"input", "analyzer"}:
        raise ValueError("unsupported segmentation mode")
    if value["normalization"] not in {"none", "nfc"}:
        raise ValueError("Stanza adapter received an unsupported normalization policy")
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
