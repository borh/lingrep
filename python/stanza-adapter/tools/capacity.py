#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

import blake3

from lingrep_stanza_adapter.bundle import load_profile, resource_manifest_digest

ZERO = "00" * 32
LIMITS = {
    "max_record_bytes": 1_048_576,
    "max_request_batch_bytes": 67_108_864,
    "max_response_batch_bytes": 67_108_864,
    "max_documents_per_batch": 2048,
    "max_schema_keys": 255,
    "max_schema_key_bytes": 256,
    "max_warnings_per_sentence": 64,
    "max_warning_message_bytes": 16_384,
    "max_stderr_tail_bytes": 65_536,
}


def encode(value: Any) -> bytes:
    return (
        json.dumps(
            value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        ).encode()
        + b"\n"
    )


def peak_rss(pid: int, stop: threading.Event, result: list[int]) -> None:
    peak = 0
    status = Path(f"/proc/{pid}/status")
    while not stop.wait(0.005):
        try:
            for line in status.read_text(encoding="ascii").splitlines():
                if line.startswith("VmRSS:"):
                    peak = max(peak, int(line.split()[1]))
                    break
        except FileNotFoundError:
            break
    result.append(peak)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--adapter", type=Path, required=True)
    parser.add_argument("--resource-root", type=Path, required=True)
    parser.add_argument("--profile", choices=("tokenize", "morphology"), required=True)
    parser.add_argument("--segmentation", choices=("input", "analyzer"), required=True)
    parser.add_argument("--documents", type=int, default=128)
    args = parser.parse_args()
    if not 0 < args.documents <= LIMITS["max_documents_per_batch"]:
        parser.error("--documents must fit the protocol batch bound")

    model = args.resource_root.resolve(strict=True)
    manifest_root = model.parent
    model_relative = Path(model.name)
    profile = load_profile(model)
    selection_name = f"{args.profile}/{args.segmentation}"
    selection = profile["selections"][selection_name]
    selection_digest = profile["selection_digests"][selection_name]
    lock = args.adapter.resolve().parents[1] / "uv.lock"
    text = (
        "Je mange du pain. Il en mange aussi."
        if args.segmentation == "analyzer"
        else "Je mange du pain."
    )
    header = {
        "record": "run_header",
        "protocol": "lingrep.external-morphology/v1",
        "run_id": "stanza-capacity-v1",
        "family": "stanza",
        "profile": args.profile,
        "segmentation": args.segmentation,
        "normalization": "nfc",
        "language_policy": "fr",
        "boundary_policy": selection["boundary_policy"],
        "projection": selection["schema"],
        "implementation_digest": ZERO,
        "resource_manifest_digest": resource_manifest_digest(
            model_relative, manifest_root
        ),
        "projection_policy_digest": ZERO,
        "alignment_policy_digest": ZERO,
        "component_closure": [
            {"name": "adapter-command", "digest": ZERO},
            {"name": "stanza-profile", "digest": selection_digest},
        ],
        "output_config_digest": selection_digest,
        "protocol_limits": LIMITS,
    }
    command = [
        sys.executable,
        str(args.adapter.resolve()),
        "--resource-root",
        model.name,
        "--language",
        "fr",
        "--backend",
        "cpu",
        "--batch-size",
        "1",
    ]
    launched = time.perf_counter()
    process = subprocess.Popen(
        command,
        cwd=manifest_root,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdin is not None and process.stdout is not None
    stop = threading.Event()
    rss: list[int] = []
    sampler = threading.Thread(target=peak_rss, args=(process.pid, stop, rss))
    sampler.start()
    header_bytes = encode(header)
    process.stdin.write(header_bytes)
    process.stdin.flush()
    ready_line = process.stdout.readline()
    if not ready_line:
        raise RuntimeError(process.stderr.read().decode(errors="replace"))
    ready = json.loads(ready_line)
    startup_seconds = time.perf_counter() - launched

    records = [
        {"record": "batch_begin", "batch_id": 0, "document_count": args.documents}
    ]
    for ordinal in range(args.documents):
        records.append(
            {
                "record": "document",
                "batch_id": 0,
                "document_ordinal": ordinal,
                "input_id": ordinal + 1,
                "kind": "sentence" if args.segmentation == "input" else "segment",
                "text": text,
                "provenance": {
                    "source_identity": ZERO,
                    "document_identity": ordinal + 1,
                    "paragraph_identity": None,
                    "metadata_digest": ZERO,
                },
            }
        )
    records.append({"record": "batch_end", "batch_id": 0})
    request = b"".join(encode(record) for record in records)
    analysis_started = time.perf_counter()
    process.stdin.write(request)
    process.stdin.flush()
    output_hash = blake3.blake3()
    response_bytes = 0
    tokens = 0
    sentences = 0
    first_result_seconds = None
    while True:
        line = process.stdout.readline()
        if not line:
            raise RuntimeError(process.stderr.read().decode(errors="replace"))
        if first_result_seconds is None:
            first_result_seconds = time.perf_counter() - analysis_started
        response_bytes += len(line)
        output_hash.update(line)
        record = json.loads(line)
        if record["record"] == "sentence":
            sentences += 1
            tokens += record["word_count"]
        elif record["record"] == "batch_complete":
            break
    analysis_seconds = time.perf_counter() - analysis_started
    process.stdin.close()
    return_code = process.wait()
    stop.set()
    sampler.join()
    stderr = process.stderr.read().decode(errors="replace")
    if return_code != 0:
        raise RuntimeError(stderr)
    model_bytes = sum(
        path.stat().st_size for path in model.rglob("*") if path.is_file()
    )
    result = {
        "profile": args.profile,
        "segmentation": args.segmentation,
        "documents": args.documents,
        "sentences": sentences,
        "tokens": tokens,
        "startup_seconds": startup_seconds,
        "analysis_seconds": analysis_seconds,
        "first_result_seconds": first_result_seconds,
        "tokens_per_second": tokens / analysis_seconds,
        "request_bytes_per_token": len(request) / tokens,
        "response_bytes_per_token": response_bytes / tokens,
        "peak_child_rss_kib": rss[0],
        "model_bytes": model_bytes,
        "output_blake3": output_hash.hexdigest(),
        "runtime": ready["execution"]["runtime"],
        "lock_blake3": ready["execution"]["lock_digest"],
        "selection_digest": selection_digest,
        "resource_manifest_digest": header["resource_manifest_digest"],
        "adapter_lock_bytes": lock.stat().st_size,
        "host": os.uname().nodename,
    }
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
