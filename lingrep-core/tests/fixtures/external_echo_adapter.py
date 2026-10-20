#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import sys
import time


mode = sys.argv[1] if len(sys.argv) > 1 else "echo"
documents = []
header = None
batch_id = None

if mode in {"ignore_term", "ignore_term_silent"}:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

if mode in {"descendant", "ignore_term"}:
    child_code = "import time; time.sleep(60)"
    if mode == "ignore_term":
        child_code = "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"
    descendant = subprocess.Popen([sys.executable, "-c", child_code])
    sys.stderr.write(f"DESCENDANT={descendant.pid}\n")
    sys.stderr.flush()


def emit(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    record = json.loads(line)
    kind = record["record"]
    if kind == "run_header":
        header = record
        emit({
            "record": "run_ready",
            "run_id": header["run_id"],
            "family": header["family"],
            "profile": header["profile"],
            "segmentation": header["segmentation"],
            "normalization": header["normalization"],
            "language_policy": header["language_policy"],
            "boundary_policy": header["boundary_policy"],
            "implementation_digest": header["implementation_digest"],
            "resource_manifest_digest": header["resource_manifest_digest"],
            "projection_policy_digest": header["projection_policy_digest"],
            "alignment_policy_digest": header["alignment_policy_digest"],
            "component_closure": header["component_closure"],
            "output_config_digest": header["output_config_digest"],
            "schema": header["projection"],
            "returned_projection": header["projection"],
            "capabilities": [header["profile"]],
            "execution": {
                "backend": "cpu",
                "runtime": sys.version.split()[0],
                "lock_digest": header["implementation_digest"],
                "workers": 1,
                "batch_size": 1,
            },
        })
        if mode == "early_exit":
            sys.exit(0)
    elif kind == "batch_begin":
        batch_id = record["batch_id"]
        documents = []
    elif kind == "document":
        documents.append(record)
    elif kind == "batch_end":
        if mode == "stderr":
            sys.stderr.write("x" * 200000 + "\n")
            sys.stderr.flush()
        if mode == "delay":
            time.sleep(0.2)
        if mode in {"silent", "ignore_term_silent"}:
            time.sleep(60)
        if mode == "wrong_tag":
            emit({"record": "run_ready"})
            time.sleep(60)
        if mode == "malformed":
            sys.stdout.write("not-json\n")
            sys.stdout.flush()
            continue
        if mode == "nonzero":
            sys.exit(7)
        if mode == "segment_two":
            document = documents[0]
            text = document["text"]
            for sentence_ordinal, (start, end) in enumerate([(0, 4), (4, 8)]):
                surface = text[start:end]
                values = [
                    surface if attribute["key"] == "text" else None
                    for attribute in header["projection"]
                ]
                emit({
                    "record": "sentence",
                    "batch_id": batch_id,
                    "document_ordinal": 0,
                    "input_id": document["input_id"],
                    "sentence_ordinal": sentence_ordinal,
                    "source_range": [start, end],
                    "surface_ranges": [[start, end]],
                    "alignment": {"kind": "identity"},
                    "word_count": 1,
                    "values": values,
                    "warnings": [],
                })
            emit({"record": "batch_complete", "batch_id": batch_id, "sentence_counts": [2]})
            continue
        emitted_documents = documents
        if mode == "decreasing_document_silent":
            emitted_documents = list(reversed(documents))
        for document in emitted_documents:
            text = document["text"]
            values = []
            for attribute in header["projection"]:
                values.append(text if attribute["key"] == "text" else None)
            input_id = document["input_id"] + (1 if mode in {"wrong_order", "wrong_identity_silent"} else 0)
            emit({
                "record": "sentence",
                "batch_id": batch_id,
                "document_ordinal": document["document_ordinal"],
                "input_id": input_id,
                "sentence_ordinal": 0,
                "source_range": [0, len(text.encode("utf-8"))],
                "surface_ranges": [[0, len(text.encode("utf-8"))]],
                "alignment": {"kind": "identity"},
                "word_count": 1,
                "values": values,
                "warnings": [],
            })
            if mode == "wrong_identity_silent":
                time.sleep(60)
            if mode == "decreasing_document_silent" and document["document_ordinal"] == 0:
                time.sleep(60)
            if mode == "overproduce_silent":
                duplicate = {
                    "record": "sentence",
                    "batch_id": batch_id,
                    "document_ordinal": document["document_ordinal"],
                    "input_id": input_id,
                    "sentence_ordinal": 1,
                    "source_range": [0, len(text.encode("utf-8"))],
                    "surface_ranges": [[0, len(text.encode("utf-8"))]],
                    "alignment": {"kind": "identity"},
                    "word_count": 1,
                    "values": values,
                    "warnings": [],
                }
                emit(duplicate)
                time.sleep(60)
        emit({
            "record": "batch_complete",
            "batch_id": batch_id,
            "sentence_counts": [1 for _ in documents],
        })
        if mode == "extra_after_complete":
            emit({"record": "batch_complete", "batch_id": batch_id, "sentence_counts": []})

if mode == "finish_delay":
    time.sleep(60)

sys.exit(0)
