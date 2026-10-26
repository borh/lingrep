from __future__ import annotations

import json
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any, BinaryIO, Literal

ValueKind = Literal["scalar", "string_set"]


@dataclass(frozen=True)
class ProjectionAttribute:
    key: str
    kind: ValueKind


def decode_header(
    value: Mapping[str, Any],
    encoded_bytes: int,
    *,
    family: str,
    provider: str,
    projection_decoder: Callable[[Any], list[ProjectionAttribute]],
) -> None:
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
    if value["family"] != family:
        raise ValueError(f"{provider} adapter requires the {family} analyzer family")
    if value["profile"] not in {"tokenize", "morphology"}:
        raise ValueError("unsupported extraction profile")
    if value["segmentation"] not in {"input", "analyzer"}:
        raise ValueError("unsupported segmentation mode")
    if value["normalization"] not in {"none", "nfc"}:
        raise ValueError(
            f"{provider} adapter received an unsupported normalization policy"
        )
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
    projection_decoder(value["projection"])
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


def _component_digest(header: Mapping[str, Any], name: str) -> str:
    for component in header["component_closure"]:
        if component.get("name") == name:
            return component["digest"]
    raise ValueError(f"run_header omits required component {name!r}")


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
