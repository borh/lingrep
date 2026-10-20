from __future__ import annotations

import io
import json
import types
import unittest
from dataclasses import replace
from pathlib import Path

from lingrep_stanza_adapter.adapter import (
    LoadedPipeline,
    ProjectionAttribute,
    _decode_batch_begin,
    _decode_batch_end,
    _decode_document,
    _encode_record,
    _read_record,
    serve,
)

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


def word(text: str, **values: object) -> object:
    return types.SimpleNamespace(
        text=text,
        lemma=values.get("lemma", text),
        upos=values.get("upos", "NOUN"),
        xpos=values.get("xpos", "NOUN"),
        feats=values.get("feats"),
    )


def token(text: str, start: int, end: int, words: list[object] | None = None) -> object:
    return types.SimpleNamespace(
        text=text,
        start_char=start,
        end_char=end,
        words=words if words is not None else [word(text)],
    )


def document(text: str, sentences: list[list[object]]) -> object:
    return types.SimpleNamespace(
        text=text,
        sentences=[types.SimpleNamespace(tokens=tokens) for tokens in sentences],
    )


def mwt_document() -> object:
    value = json.loads(
        (Path(__file__).parent / "fixtures" / "mwt.json").read_text(encoding="utf-8")
    )
    sentences = []
    for sentence in value["sentences"]:
        tokens = []
        for source in sentence["tokens"]:
            words = [word(item.pop("text"), **item) for item in source["words"]]
            tokens.append(
                token(source["text"], source["start_char"], source["end_char"], words)
            )
        sentences.append(tokens)
    return document(value["text"], sentences)


def attributes() -> list[dict[str, str]]:
    return [
        {"key": "text", "kind": "scalar"},
        {"key": "lemma", "kind": "scalar"},
        {"key": "upos", "kind": "scalar"},
        {"key": "xpos", "kind": "scalar"},
        {"key": "feat.Number", "kind": "string_set"},
    ]


def header(
    *,
    profile: str = "morphology",
    segmentation: str = "input",
    projection: list[dict[str, str]] | None = None,
    limits: dict[str, int] | None = None,
) -> dict[str, object]:
    return {
        "record": "run_header",
        "protocol": "lingrep.external-morphology/v1",
        "run_id": "stanza-test",
        "family": "stanza",
        "profile": profile,
        "segmentation": segmentation,
        "normalization": "nfc",
        "language_policy": "fr",
        "boundary_policy": (
            "input-v1" if segmentation == "input" else "stanza:tokenize:v1"
        ),
        "projection": projection if projection is not None else attributes(),
        "implementation_digest": ZERO,
        "resource_manifest_digest": ZERO,
        "projection_policy_digest": ZERO,
        "alignment_policy_digest": ZERO,
        "component_closure": [
            {"name": "adapter-command", "digest": ZERO},
            {"name": "stanza-profile", "digest": ZERO},
        ],
        "output_config_digest": ZERO,
        "protocol_limits": limits if limits is not None else LIMITS,
    }


def document_record(
    text: str = "du pain", *, batch: int = 0, ordinal: int = 0, input_id: int = 7
) -> dict[str, object]:
    return {
        "record": "document",
        "batch_id": batch,
        "document_ordinal": ordinal,
        "input_id": input_id,
        "kind": "sentence",
        "text": text,
        "provenance": {
            "source_identity": ZERO,
            "document_identity": input_id,
            "paragraph_identity": None,
            "metadata_digest": ZERO,
        },
    }


def records_bytes(records: list[dict[str, object]]) -> bytes:
    return b"".join(
        json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        for record in records
    )


def transcript(
    request_header: dict[str, object] | None = None,
    document_value: dict[str, object] | None = None,
) -> io.BytesIO:
    return io.BytesIO(
        records_bytes(
            [
                request_header or header(),
                {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                document_value or document_record(),
                {"record": "batch_end", "batch_id": 0},
            ]
        )
    )


class FakePipeline:
    def __init__(self, results: list[object]) -> None:
        self.results = results
        self.calls: list[str] = []

    def __call__(self, text: str) -> object:
        self.calls.append(text)
        if not self.results:
            raise AssertionError("pipeline received too many documents")
        return self.results.pop(0)


def loaded(pipeline: FakePipeline, *, schema: list[dict[str, str]] | None = None):
    return LoadedPipeline(
        pipeline=pipeline,
        runtime="python-test/stanza-1.14.0/torch-test/backend-cpu",
        language="fr",
        boundary_policy="input-v1",
        schema=tuple(
            ProjectionAttribute(item["key"], item["kind"])
            for item in (schema if schema is not None else attributes())
        ),
        component_closure=({"name": "stanza-profile", "digest": ZERO},),
        output_config_digest=ZERO,
    )


class CountingReader(io.BytesIO):
    def __init__(self, value: bytes, output: io.BytesIO) -> None:
        super().__init__(value)
        self.output = output
        self.reads = 0

    def readline(self, size: int = -1) -> bytes:
        self.reads += 1
        if self.reads == 5 and b'"batch_complete"' not in self.output.getvalue():
            raise AssertionError(
                "adapter read the next batch before completing the first"
            )
        return super().readline(size)


class ProtocolTests(unittest.TestCase):
    def test_provider_batch_size_must_match_single_document_execution(self) -> None:
        with self.assertRaisesRegex(ValueError, "provider batch size must be one"):
            serve(
                io.BytesIO(),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=2,
                lock_digest="11" * 32,
            )

    def test_request_records_are_closed_typed_and_lf_framed(self) -> None:
        with self.assertRaisesRegex(ValueError, "wrong shape"):
            _decode_batch_begin(
                {"record": "batch_begin", "batch_id": 0, "document_count": 1, "x": 1}
            )
        with self.assertRaisesRegex(ValueError, "nonnegative integer"):
            _decode_batch_begin(
                {"record": "batch_begin", "batch_id": True, "document_count": 1}
            )
        value = document_record()
        _decode_document(value)
        value["provenance"]["extra"] = 1
        with self.assertRaisesRegex(ValueError, "provenance has the wrong shape"):
            _decode_document(value)
        with self.assertRaisesRegex(ValueError, "wrong shape"):
            _decode_batch_end({"record": "batch_end", "batch_id": 0, "extra": 1})
        with self.assertRaisesRegex(ValueError, "CRLF"):
            _read_record(io.BytesIO(b'{"record":"batch_end","batch_id":0}\r\n'), 100)
        with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
            _read_record(
                io.BytesIO(b'{"record":"batch_end","batch_id":0,"batch_id":0}\n'), 100
            )

    def test_process_emits_expanded_mwt_and_complete_schema(self) -> None:
        pipeline = FakePipeline([mwt_document()])
        output = io.BytesIO()
        serve(
            transcript(),
            output,
            resource_root="model",
            language="fr",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded(pipeline),
        )
        response = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(
            [record["record"] for record in response],
            ["run_ready", "sentence", "batch_complete"],
        )
        self.assertEqual(response[0]["schema"], attributes())
        self.assertEqual(response[1]["surface_ranges"], [[0, 2], [3, 7]])
        self.assertEqual(
            response[1]["alignment"],
            {"kind": "expanded", "word_ranges": [[0, 2], [2, 3]]},
        )
        self.assertEqual(response[2]["sentence_counts"], [1])

    def test_morphology_form_subset_keeps_expanded_match_words(self) -> None:
        output = io.BytesIO()
        serve(
            transcript(
                header(projection=[{"key": "text", "kind": "scalar"}]),
                document_record("du pain"),
            ),
            output,
            resource_root="model",
            language="fr",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded(FakePipeline([mwt_document()])),
        )
        response = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(response[1]["values"], ["de", "le", "pain"])
        self.assertEqual(response[1]["word_count"], 3)
        self.assertEqual(
            response[1]["alignment"],
            {"kind": "expanded", "word_ranges": [[0, 2], [2, 3]]},
        )

    def test_subset_projection_nfc_and_unavailable_key_contract(self) -> None:
        text = "é"
        pipeline = FakePipeline([document(text, [[token(text, 0, 1)]])])
        request = header(
            profile="tokenize",
            projection=[{"key": "text", "kind": "scalar"}],
        )
        output = io.BytesIO()
        serve(
            transcript(request, document_record(text)),
            output,
            resource_root="model",
            language="fr",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded(
                pipeline, schema=[{"key": "text", "kind": "scalar"}]
            ),
        )
        response = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(pipeline.calls, [text])
        self.assertEqual(response[1]["surface_ranges"], [[0, 2]])

        unavailable = header(
            projection=[
                {"key": "text", "kind": "scalar"},
                {"key": "feat.Number[psor]", "kind": "string_set"},
            ]
        )
        with self.assertRaisesRegex(ValueError, "unavailable"):
            serve(
                transcript(unavailable),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(FakePipeline([mwt_document()])),
            )

    def test_malformed_ordering_newline_and_response_bounds_fail_terminally(
        self,
    ) -> None:
        extra = header()
        extra["unexpected"] = True
        with self.assertRaisesRegex(ValueError, "run_header has the wrong shape"):
            serve(
                transcript(extra),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(FakePipeline([mwt_document()])),
            )

        wrong = document_record(ordinal=1)
        with self.assertRaisesRegex(ValueError, "ordinals"):
            serve(
                transcript(document_value=wrong),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(FakePipeline([mwt_document()])),
            )

        newline_pipeline = FakePipeline([])
        with self.assertRaisesRegex(ValueError, "CR or LF"):
            serve(
                transcript(document_value=document_record("du\npain")),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(newline_pipeline),
            )
        self.assertEqual(newline_pipeline.calls, [])

        with self.assertRaisesRegex(ValueError, "wire bound"):
            _encode_record({"record": "sentence", "value": "x" * 4096}, 128)

        with self.assertRaisesRegex(ValueError, "premature EOF"):
            serve(
                io.BytesIO(
                    records_bytes(
                        [
                            header(),
                            {
                                "record": "batch_begin",
                                "batch_id": 0,
                                "document_count": 1,
                            },
                        ]
                    )
                ),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(FakePipeline([])),
            )

    def test_scalar_set_and_multi_sentence_responses_stop_at_aggregate_bounds(
        self,
    ) -> None:
        huge_word = word("x" * 4096, feats="Number=Plur,Sing")
        huge = document("du", [[token("du", 0, 2, [huge_word])]])
        with self.assertRaisesRegex(ValueError, "bound"):
            serve(
                transcript(
                    header(limits={**LIMITS, "max_record_bytes": 2048}),
                    document_record("du"),
                ),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded(FakePipeline([huge])),
            )

        analyzer_header = header(
            segmentation="analyzer",
            limits={**LIMITS, "max_response_batch_bytes": 400},
        )
        analyzer_document = document_record("du pain")
        analyzer_document["kind"] = "segment"
        two_sentences = document(
            "du pain",
            [[token("du", 0, 2)], [token("pain", 3, 7)]],
        )
        analyzer_loaded = replace(
            loaded(FakePipeline([two_sentences])),
            boundary_policy="stanza:tokenize:v1",
        )
        with self.assertRaisesRegex(ValueError, "wire bound"):
            serve(
                transcript(analyzer_header, analyzer_document),
                io.BytesIO(),
                resource_root="model",
                language="fr",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: analyzer_loaded,
            )

    def test_two_batches_complete_before_the_next_batch_is_read(self) -> None:
        second_document = document_record(batch=1, input_id=8)
        encoded = records_bytes(
            [
                header(),
                {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                document_record(),
                {"record": "batch_end", "batch_id": 0},
                {"record": "batch_begin", "batch_id": 1, "document_count": 1},
                second_document,
                {"record": "batch_end", "batch_id": 1},
            ]
        )
        output = io.BytesIO()
        reader = CountingReader(encoded, output)
        serve(
            reader,
            output,
            resource_root="model",
            language="fr",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded(
                FakePipeline([mwt_document(), mwt_document()])
            ),
        )
        response = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(
            [
                record["batch_id"]
                for record in response
                if record["record"] == "batch_complete"
            ],
            [0, 1],
        )


if __name__ == "__main__":
    unittest.main()
