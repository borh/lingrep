from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass, replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from lingrep_spacy_adapter.adapter import (
    LoadedPipeline,
    ProjectionAttribute,
    _decode_batch_begin,
    _decode_batch_end,
    _decode_document,
    _encode_record,
    analyze_document,
    initialize_spacy_backend,
    iter_analyzed_sentences,
    load_spacy_pipeline,
    morph_attribute_key,
    serve,
)
from lingrep_spacy_adapter.bundle import (
    MANIFEST_HASH_CHUNK_BYTES,
    prepare_model_bundle,
    resource_manifest_digest,
    selection_digest,
)

REAL_SPACY = os.environ.get("LINGREP_SPACY_TEST_REAL") == "1" and bool(
    importlib.util.find_spec("spacy")
)


class FakeMorph:
    def __init__(self, values: dict[str, list[str]]) -> None:
        self._values = values

    def to_dict(self) -> dict[str, list[str]]:
        return self._values


@dataclass(frozen=True)
class FakeToken:
    text: str
    idx: int
    lemma_: str = ""
    norm_: str = ""
    pos_: str = ""
    tag_: str = ""
    morph: FakeMorph | None = None

    @property
    def is_space(self) -> bool:
        return self.text.isspace()


@dataclass(frozen=True)
class FakeSpan:
    tokens: tuple[FakeToken, ...]
    start_char: int
    end_char: int

    def __iter__(self):
        return iter(self.tokens)


class FakeDoc:
    def __init__(
        self,
        text: str,
        tokens: list[FakeToken],
        sentences: list[FakeSpan],
    ) -> None:
        self.text = text
        self._tokens = tokens
        self._sentences = sentences

    def __iter__(self):
        return iter(self._tokens)

    @property
    def sents(self):
        return iter(self._sentences)


class FakePipeline:
    def __init__(self, docs: list[FakeDoc], expected_batch_size: int = 8) -> None:
        self._docs = docs
        self._expected_batch_size = expected_batch_size

    def pipe(self, texts: list[str], *, batch_size: int):
        assert batch_size == self._expected_batch_size
        assert texts == [doc.text for doc in self._docs]
        return iter(self._docs)


def projection() -> list[ProjectionAttribute]:
    return [
        ProjectionAttribute("text", "scalar"),
        ProjectionAttribute("lemma", "scalar"),
        ProjectionAttribute("norm", "scalar"),
        ProjectionAttribute("upos", "scalar"),
        ProjectionAttribute("xpos", "scalar"),
        ProjectionAttribute("feat.Gender[psor]", "string_set"),
        ProjectionAttribute("feat.Number", "string_set"),
        ProjectionAttribute("spacy.morph.reading", "string_set"),
    ]


def japanese_doc() -> FakeDoc:
    cat = FakeToken(
        "猫",
        0,
        lemma_="猫",
        norm_="ネコ",
        pos_="NOUN",
        tag_="名詞-普通名詞",
        morph=FakeMorph(
            {
                "Number": ["Sing", "Plur", "Sing"],
                "Gender[psor]": ["Masc"],
                "Reading": ["ネコ"],
            }
        ),
    )
    gap = FakeToken(" ", 1)
    particle = FakeToken("が", 2, lemma_="", pos_="ADP", morph=FakeMorph({}))
    return FakeDoc(
        "猫 が",
        [cat, gap, particle],
        [FakeSpan((cat,), 0, 1), FakeSpan((particle,), 2, 3)],
    )


class AdapterMappingTests(unittest.TestCase):
    def test_morphology_names_use_ud_or_provider_namespaces(self) -> None:
        self.assertEqual(morph_attribute_key("Number"), "feat.Number")
        self.assertEqual(morph_attribute_key("Gender[psor]"), "feat.Gender[psor]")
        for name in (
            "AdpType",
            "AdvType",
            "Echo",
            "NameType",
            "NounType",
            "NumForm",
            "PartType",
            "PrepCase",
            "PunctSide",
            "PunctType",
            "Style",
            "Subcat",
            "Variant",
            "VerbType",
        ):
            with self.subTest(name=name):
                self.assertEqual(morph_attribute_key(name), f"feat.{name}")
        self.assertEqual(morph_attribute_key("Reading"), "spacy.morph.reading")
        with self.assertRaisesRegex(ValueError, "canonical provider key"):
            morph_attribute_key("Provider_Field")

    def test_input_mode_maps_utf8_ranges_values_and_missing_fields(self) -> None:
        sentences = analyze_document(japanese_doc(), projection(), "input")
        expected = json.loads(
            (Path(__file__).parent / "fixtures" / "analysis.json").read_text()
        )["input"]
        self.assertEqual(sentences, expected)

    def test_analyzer_mode_uses_pipeline_sentence_ranges(self) -> None:
        sentences = analyze_document(japanese_doc(), projection(), "analyzer")
        expected = json.loads(
            (Path(__file__).parent / "fixtures" / "analysis.json").read_text()
        )["analyzer"]
        self.assertEqual(sentences, expected)

    def test_astral_and_combining_character_offsets_are_utf8_bytes(self) -> None:
        first = FakeToken("😀", 0, morph=FakeMorph({}))
        second = FakeToken("e\u0301", 1, morph=FakeMorph({}))
        doc = FakeDoc("😀e\u0301", [first, second], [FakeSpan((first, second), 0, 3)])
        sentence = analyze_document(
            doc, [ProjectionAttribute("text", "scalar")], "input"
        )[0]
        self.assertEqual(sentence["surface_ranges"], [[0, 4], [4, 7]])

    def test_sentence_mapping_refuses_values_above_its_residency_bound(self) -> None:
        token = FakeToken("x", 0, lemma_="y" * 1024, morph=FakeMorph({}))
        doc = FakeDoc("x", [token], [FakeSpan((token,), 0, 1)])
        with self.assertRaisesRegex(ValueError, "residency bound"):
            list(
                iter_analyzed_sentences(
                    doc,
                    [
                        ProjectionAttribute("text", "scalar"),
                        ProjectionAttribute("lemma", "scalar"),
                    ],
                    "input",
                    128,
                )
            )

    def test_noncanonical_projection_kinds_are_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "string_set"):
            analyze_document(
                japanese_doc(),
                [ProjectionAttribute("feat.Number", "scalar")],
                "input",
            )
        with self.assertRaisesRegex(ValueError, "scalar"):
            analyze_document(
                japanese_doc(),
                [ProjectionAttribute("text", "string_set")],
                "input",
            )

    @unittest.skipUnless(
        REAL_SPACY, "requires LINGREP_SPACY_TEST_REAL=1 and the spacy extra"
    )
    def test_real_spacy_doc_uses_the_same_mapping_contract(self) -> None:
        import spacy

        nlp = spacy.blank("en")
        doc = nlp.make_doc("Cats run.")
        doc[0].lemma_ = "cat"
        doc[0].pos_ = "NOUN"
        doc[0].tag_ = "NNS"
        doc[0].set_morph({"Number": "Plur", "Reading": "cats"})
        mapped = analyze_document(
            doc,
            [
                ProjectionAttribute("text", "scalar"),
                ProjectionAttribute("lemma", "scalar"),
                ProjectionAttribute("upos", "scalar"),
                ProjectionAttribute("xpos", "scalar"),
                ProjectionAttribute("feat.Number", "string_set"),
                ProjectionAttribute("spacy.morph.reading", "string_set"),
            ],
            "input",
        )
        self.assertEqual(mapped[0]["surface_ranges"], [[0, 4], [5, 8], [8, 9]])
        self.assertEqual(
            mapped[0]["values"][:6],
            ["Cats", "cat", "NOUN", "NNS", ["Plur"], ["cats"]],
        )

    @unittest.skipUnless(
        REAL_SPACY, "requires LINGREP_SPACY_TEST_REAL=1 and the spacy extra"
    )
    def test_loader_keeps_the_models_boundary_component_in_analyzer_mode(self) -> None:
        import spacy

        nlp = spacy.blank("en")
        nlp.add_pipe("sentencizer")
        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            nlp.to_disk(model)
            prepare_model_bundle(model, probe_text="One. Two.")
            loaded = load_spacy_pipeline(str(model), "cpu", "tokenize", "analyzer")
            doc = next(loaded.pipeline.pipe(["One. Two."], batch_size=1))
        self.assertEqual([span.text for span in doc.sents], ["One.", "Two."])
        self.assertIn("/spacy-", loaded.runtime)


class AdapterProtocolTests(unittest.TestCase):
    def test_request_records_are_closed_and_typed(self) -> None:
        with self.assertRaisesRegex(ValueError, "wrong shape"):
            _decode_batch_begin(
                {"record": "batch_begin", "batch_id": 0, "document_count": 1, "x": 1}
            )
        with self.assertRaisesRegex(ValueError, "nonnegative integer"):
            _decode_batch_begin(
                {"record": "batch_begin", "batch_id": True, "document_count": 1}
            )
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 1,
            "kind": "sentence",
            "text": "x",
            "provenance": {
                "source_identity": "00" * 32,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": "00" * 32,
            },
        }
        _decode_document(document)
        document["provenance"]["extra"] = 1
        with self.assertRaisesRegex(ValueError, "provenance has the wrong shape"):
            _decode_document(document)
        with self.assertRaisesRegex(ValueError, "wrong shape"):
            _decode_batch_end({"record": "batch_end", "batch_id": 0, "extra": 1})

    def test_response_encoder_stops_before_record_or_aggregate_growth(self) -> None:
        with self.assertRaisesRegex(ValueError, "wire bound"):
            _encode_record({"record": "sentence", "value": "x" * 4096}, 128)

    @unittest.skipUnless(
        REAL_SPACY, "requires LINGREP_SPACY_TEST_REAL=1 and the spacy extra"
    )
    def test_cli_loads_a_real_saved_pipeline_and_speaks_current_protocol(self) -> None:
        import spacy

        zero = "00" * 32
        limits = {
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
        attributes = [{"key": "text", "kind": "scalar"}]
        header = {
            "record": "run_header",
            "protocol": "lingrep.external-morphology/v1",
            "run_id": "cli-smoke",
            "family": "external",
            "profile": "tokenize",
            "segmentation": "input",
            "normalization": "none",
            "language_policy": "en",
            "boundary_policy": "input-v1",
            "projection": attributes,
            "implementation_digest": zero,
            "resource_manifest_digest": zero,
            "projection_policy_digest": zero,
            "alignment_policy_digest": zero,
            "component_closure": [
                {"name": "adapter-command", "digest": zero},
                {"name": "spacy-profile", "digest": zero},
            ],
            "output_config_digest": zero,
            "protocol_limits": limits,
        }
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 9,
            "kind": "sentence",
            "text": "Cats run.",
            "provenance": {
                "source_identity": zero,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": zero,
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = root / "model"
            spacy.blank("en").to_disk(model)
            profile = prepare_model_bundle(model, probe_text="Cats run.")
            selection = profile["selections"]["tokenize/input"]
            profile_digest = selection_digest(selection)
            header["resource_manifest_digest"] = resource_manifest_digest(
                Path("model"), root
            )
            header["component_closure"][1]["digest"] = profile_digest
            header["output_config_digest"] = profile_digest
            records = [
                header,
                {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                document,
                {"record": "batch_end", "batch_id": 0},
            ]
            encoded = b"".join(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode()
                + b"\n"
                for record in records
            )
            result = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "lingrep_spacy_adapter",
                    "--model-root",
                    "model",
                    "--backend",
                    "cpu",
                    "--batch-size",
                    "1",
                ],
                input=encoded,
                capture_output=True,
                check=False,
                cwd=root,
            )
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        output = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(
            [record["record"] for record in output],
            ["run_ready", "sentence", "batch_complete"],
        )
        self.assertEqual(output[1]["values"], ["Cats", "run", "."])

    def test_model_loads_before_ready_and_emits_canonical_batch(self) -> None:
        attributes = [
            {"key": attribute.key, "kind": attribute.kind} for attribute in projection()
        ]
        zero = "00" * 32
        header = {
            "record": "run_header",
            "protocol": "lingrep.external-morphology/v1",
            "run_id": "run-1",
            "family": "external",
            "profile": "morphology",
            "segmentation": "input",
            "normalization": "none",
            "language_policy": "ja",
            "boundary_policy": "input-v1",
            "projection": attributes,
            "implementation_digest": zero,
            "resource_manifest_digest": zero,
            "projection_policy_digest": zero,
            "alignment_policy_digest": zero,
            "component_closure": [
                {"name": "adapter-command", "digest": zero},
                {"name": "spacy-profile", "digest": zero},
            ],
            "output_config_digest": zero,
            "protocol_limits": {
                "max_record_bytes": 1_048_576,
                "max_request_batch_bytes": 67_108_864,
                "max_response_batch_bytes": 67_108_864,
                "max_documents_per_batch": 2048,
                "max_schema_keys": 255,
                "max_schema_key_bytes": 256,
                "max_warnings_per_sentence": 64,
                "max_warning_message_bytes": 16_384,
                "max_stderr_tail_bytes": 65_536,
            },
        }
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 7,
            "kind": "sentence",
            "text": "猫 が",
            "provenance": {
                "source_identity": zero,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": zero,
            },
        }
        records = [
            header,
            {"record": "batch_begin", "batch_id": 0, "document_count": 1},
            document,
            {"record": "batch_end", "batch_id": 0},
        ]
        stdin = io.BytesIO(
            b"".join(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode()
                + b"\n"
                for record in records
            )
        )
        stdout = io.BytesIO()
        loaded = []

        def loader(model: str, backend: str, profile: str, segmentation: str):
            loaded.append((model, backend, profile, segmentation))
            return LoadedPipeline(
                pipeline=FakePipeline([japanese_doc()]),
                runtime="python-test/spacy-test",
                language="ja",
                boundary_policy="input-v1",
                schema=tuple(projection()),
                component_closure=({"name": "spacy-profile", "digest": zero},),
                output_config_digest=zero,
            )

        serve(
            stdin,
            stdout,
            model_root="fake-model",
            backend="cpu",
            batch_size=8,
            lock_digest="11" * 32,
            load_pipeline=loader,
        )

        output = [json.loads(line) for line in stdout.getvalue().splitlines()]
        self.assertEqual(loaded, [("fake-model", "cpu", "morphology", "input")])
        self.assertEqual(
            [record["record"] for record in output],
            ["run_ready", "sentence", "batch_complete"],
        )
        self.assertEqual(output[0]["schema"], attributes)
        self.assertEqual(output[0]["returned_projection"], attributes)
        self.assertEqual(output[1]["input_id"], 7)
        self.assertEqual(output[1]["values"][6], ["Plur", "Sing"])
        self.assertEqual(output[2]["sentence_counts"], [1])

    def test_ready_declares_complete_schema_and_refuses_unavailable_subset(
        self,
    ) -> None:
        zero = "00" * 32
        limits = {
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
        header = {
            "record": "run_header",
            "protocol": "lingrep.external-morphology/v1",
            "run_id": "schema-subset",
            "family": "external",
            "profile": "morphology",
            "segmentation": "input",
            "normalization": "none",
            "language_policy": "ja",
            "boundary_policy": "input-v1",
            "projection": [{"key": "text", "kind": "scalar"}],
            "implementation_digest": zero,
            "resource_manifest_digest": zero,
            "projection_policy_digest": zero,
            "alignment_policy_digest": zero,
            "component_closure": [
                {"name": "adapter-command", "digest": zero},
                {"name": "spacy-profile", "digest": zero},
            ],
            "output_config_digest": zero,
            "protocol_limits": limits,
        }
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 8,
            "kind": "sentence",
            "text": "猫 が",
            "provenance": {
                "source_identity": zero,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": zero,
            },
        }

        def transcript(requested_header: dict) -> io.BytesIO:
            return io.BytesIO(
                b"".join(
                    json.dumps(
                        record, ensure_ascii=False, separators=(",", ":")
                    ).encode()
                    + b"\n"
                    for record in [
                        requested_header,
                        {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                        document,
                        {"record": "batch_end", "batch_id": 0},
                    ]
                )
            )

        loaded = LoadedPipeline(
            pipeline=FakePipeline([japanese_doc()], expected_batch_size=1),
            runtime="python-test/spacy-test",
            language="ja",
            boundary_policy="input-v1",
            schema=(
                ProjectionAttribute("text", "scalar"),
                ProjectionAttribute("lemma", "scalar"),
            ),
            component_closure=({"name": "spacy-profile", "digest": zero},),
            output_config_digest=zero,
        )
        stdout = io.BytesIO()
        serve(
            transcript(header),
            stdout,
            model_root="fake-model",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded,
        )
        ready = json.loads(stdout.getvalue().splitlines()[0])
        self.assertEqual(
            ready["schema"],
            [
                {"key": "text", "kind": "scalar"},
                {"key": "lemma", "kind": "scalar"},
            ],
        )
        self.assertEqual(ready["returned_projection"], header["projection"])

        unavailable = {**header, "projection": [{"key": "xpos", "kind": "scalar"}]}
        with self.assertRaisesRegex(ValueError, "unavailable"):
            serve(
                transcript(unavailable),
                io.BytesIO(),
                model_root="fake-model",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded,
            )

        for mismatch, message in (
            (replace(loaded, language="en"), "language"),
            (replace(loaded, boundary_policy="other-v1"), "boundary producer"),
        ):
            with (
                self.subTest(message=message),
                self.assertRaisesRegex(ValueError, message),
            ):
                serve(
                    transcript(header),
                    io.BytesIO(),
                    model_root="fake-model",
                    backend="cpu",
                    batch_size=1,
                    lock_digest="11" * 32,
                    load_pipeline=lambda *_, value=mismatch: value,
                )

        aggregate_header = {
            **header,
            "segmentation": "analyzer",
            "boundary_policy": "adapter-v1",
            "protocol_limits": {**limits, "max_response_batch_bytes": 350},
        }
        document["kind"] = "segment"
        analyzer_loaded = replace(loaded, boundary_policy="adapter-v1")
        with self.assertRaisesRegex(ValueError, "wire bound"):
            serve(
                transcript(aggregate_header),
                io.BytesIO(),
                model_root="fake-model",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: analyzer_loaded,
            )
        extra_header = {**header, "unexpected": True}
        with self.assertRaisesRegex(ValueError, "run_header has the wrong shape"):
            serve(
                transcript(extra_header),
                io.BytesIO(),
                model_root="fake-model",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded,
            )
        small_header = {
            **header,
            "protocol_limits": {**limits, "max_record_bytes": 128},
        }
        with self.assertRaisesRegex(ValueError, "declared max_record_bytes"):
            serve(
                transcript(small_header),
                io.BytesIO(),
                model_root="fake-model",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *_: loaded,
            )

    def test_nfc_header_analyzes_the_already_admitted_utf8_buffer(self) -> None:
        zero = "00" * 32
        limits = {
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
        header = {
            "record": "run_header",
            "protocol": "lingrep.external-morphology/v1",
            "run_id": "nfc-admitted-buffer",
            "family": "external",
            "profile": "tokenize",
            "segmentation": "input",
            "normalization": "nfc",
            "language_policy": "fr",
            "boundary_policy": "input-v1",
            "projection": [{"key": "text", "kind": "scalar"}],
            "implementation_digest": zero,
            "resource_manifest_digest": zero,
            "projection_policy_digest": zero,
            "alignment_policy_digest": zero,
            "component_closure": [
                {"name": "adapter-command", "digest": zero},
                {"name": "spacy-profile", "digest": zero},
            ],
            "output_config_digest": zero,
            "protocol_limits": limits,
        }
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 9,
            "kind": "sentence",
            "text": "Café",
            "provenance": {
                "source_identity": zero,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": zero,
            },
        }
        token = FakeToken("Café", 0, morph=FakeMorph({}))
        doc = FakeDoc("Café", [token], [FakeSpan((token,), 0, 4)])
        loaded = LoadedPipeline(
            pipeline=FakePipeline([doc], expected_batch_size=1),
            runtime="python-test/spacy-test",
            language="fr",
            boundary_policy="input-v1",
            schema=(ProjectionAttribute("text", "scalar"),),
            component_closure=({"name": "spacy-profile", "digest": zero},),
            output_config_digest=zero,
        )
        records = [
            header,
            {"record": "batch_begin", "batch_id": 0, "document_count": 1},
            document,
            {"record": "batch_end", "batch_id": 0},
        ]
        stdin = io.BytesIO(
            b"".join(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode()
                + b"\n"
                for record in records
            )
        )
        stdout = io.BytesIO()
        serve(
            stdin,
            stdout,
            model_root="fake-model",
            backend="cpu",
            batch_size=1,
            lock_digest="11" * 32,
            load_pipeline=lambda *_: loaded,
        )
        sentence = json.loads(stdout.getvalue().splitlines()[1])
        self.assertEqual(sentence["values"], ["Café"])
        self.assertEqual(sentence["source_range"], [0, 5])
        self.assertEqual(sentence["surface_ranges"], [[0, 5]])

    def test_unsupported_protocol_is_rejected_before_model_loading(self) -> None:
        stdin = io.BytesIO(
            b'{"record":"run_header","protocol":"lingrep.external-morphology/v0"}\n'
        )
        loaded = []
        with self.assertRaisesRegex(ValueError, "unsupported protocol"):
            serve(
                stdin,
                io.BytesIO(),
                model_root="unused",
                backend="cpu",
                batch_size=1,
                lock_digest="11" * 32,
                load_pipeline=lambda *args: loaded.append(args),
            )
        self.assertEqual(loaded, [])


class BackendTests(unittest.TestCase):
    def test_rocm_initializes_torch_then_cupy_then_spacy_without_fallback(self) -> None:
        imported = []
        required = []
        runtime = SimpleNamespace(
            is_hip=True,
            getDeviceCount=lambda: 1,
            runtimeGetVersion=lambda: 70_200_000,
            driverGetVersion=lambda: 7_020_000,
        )
        modules = {
            "torch": SimpleNamespace(
                cuda=SimpleNamespace(
                    get_device_name=lambda device: f"gfx1100:{device}",
                    is_available=lambda: True,
                ),
                version=SimpleNamespace(hip="7.2", cuda=None),
            ),
            "cupy": SimpleNamespace(cuda=SimpleNamespace(runtime=runtime)),
            "spacy": SimpleNamespace(
                __version__="3.8.11",
                require_gpu=lambda device: required.append(device),
                load=lambda model, exclude: (model, exclude),
            ),
        }

        def import_module(name: str):
            imported.append(name)
            return modules[name]

        with patch(
            "lingrep_spacy_adapter.adapter.importlib.import_module",
            side_effect=import_module,
        ):
            backend = initialize_spacy_backend("rocm")
        self.assertEqual(imported, ["torch", "cupy", "spacy"])
        self.assertEqual(required, [0])
        self.assertEqual(backend[1], "gfx1100:0")
        self.assertEqual(backend[2], "70200000")
        self.assertEqual(backend[3], "7020000")
        self.assertEqual(backend[4], "7.2")
        self.assertEqual(backend[5], "cupy-rocm-7-0")

    def test_rocm_refuses_a_non_hip_torch_before_importing_spacy(self) -> None:
        imported = []
        torch = SimpleNamespace(
            cuda=SimpleNamespace(is_available=lambda: True),
            version=SimpleNamespace(hip=None, cuda="12.6"),
        )

        def import_module(name: str):
            imported.append(name)
            return torch

        with (
            patch(
                "lingrep_spacy_adapter.adapter.importlib.import_module",
                side_effect=import_module,
            ),
            self.assertRaisesRegex(RuntimeError, "HIP-enabled"),
        ):
            initialize_spacy_backend("rocm")
        self.assertEqual(imported, ["torch"])


class ProfileJsonTests(unittest.TestCase):
    def test_profile_rejects_duplicate_keys_at_each_depth(self) -> None:
        from lingrep_spacy_adapter.bundle import PROFILE_FILE, load_profile

        for source in (
            '{"format": "a", "format": "b"}',
            '{"selections": {"x": {"schema": [{"key": "text", "key": "lemma"}]}}}',
        ):
            with (
                self.subTest(source=source),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                (root / PROFILE_FILE).write_text(source, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                    load_profile(root)


class ResourceManifestTests(unittest.TestCase):
    def test_resource_manifest_hashes_large_files_without_read_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = root / "model"
            model.mkdir()
            (model / "weights.bin").write_bytes(b"x" * (MANIFEST_HASH_CHUNK_BYTES + 17))
            with patch.object(
                Path,
                "read_bytes",
                side_effect=AssertionError("resource hashing must stream"),
            ):
                digest = resource_manifest_digest(Path("model"), root)
        self.assertEqual(len(digest), 64)


@unittest.skipUnless(
    REAL_SPACY, "requires LINGREP_SPACY_TEST_REAL=1 and the spacy extra"
)
class ModelBundleTests(unittest.TestCase):
    def test_preparation_rejects_noncanonical_provider_keys_before_loading(
        self,
    ) -> None:
        for keys in (
            ("reading",),
            ("spacy.morph.Reading",),
            ("spacy.morph.reading", "spacy.morph.inflection"),
            ("spacy.morph.reading", "spacy.morph.reading"),
        ):
            with (
                self.subTest(keys=keys),
                self.assertRaisesRegex(ValueError, "morphology keys"),
            ):
                prepare_model_bundle(
                    Path("unused"), probe_text="x", morphology_keys=keys
                )

    def test_preparation_excludes_renamed_unneeded_components(self) -> None:
        import spacy

        nlp = spacy.blank("en")
        nlp.add_pipe("sentencizer", name="boundary")
        nlp.add_pipe("entity_ruler", name="renamed_ner")
        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            nlp.to_disk(model)
            profile = prepare_model_bundle(model, probe_text="One. Two.")
            selection = profile["selections"]["tokenize/analyzer"]
            self.assertEqual(
                [component["name"] for component in selection["components"]],
                ["boundary"],
            )
            loaded = load_spacy_pipeline(str(model), "cpu", "tokenize", "analyzer")
            self.assertEqual(loaded.pipeline.pipe_names, ["boundary"])
            self.assertEqual(loaded.boundary_policy, "spacy:boundary:v1")

    def test_competing_boundaries_require_an_explicit_selection(self) -> None:
        import spacy

        nlp = spacy.blank("en")
        nlp.add_pipe("sentencizer", name="first_boundary")
        nlp.add_pipe("sentencizer", name="second_boundary")
        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            nlp.to_disk(model)
            with self.assertRaisesRegex(ValueError, "multiple boundary producers"):
                prepare_model_bundle(model, probe_text="One. Two.")

    def test_blank_model_does_not_advertise_morphology(self) -> None:
        import spacy

        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            spacy.blank("en").to_disk(model)
            profile = prepare_model_bundle(model, probe_text="Cats run.")
            self.assertNotIn("morphology/input", profile["selections"])
            with self.assertRaisesRegex(
                ValueError, "does not provide morphology/input"
            ):
                load_spacy_pipeline(str(model), "cpu", "morphology", "input")


@unittest.skipUnless(
    os.environ.get("LINGREP_SPACY_GINZA_MODEL"),
    "requires LINGREP_SPACY_GINZA_MODEL and the ginza uv extra",
)
class GiNzaModelTests(unittest.TestCase):
    def test_python_projection_matches_ginza_cli_conllu_dependencies(self) -> None:
        import spacy

        text = "猫が走る。"
        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            pipeline = spacy.load(os.environ["LINGREP_SPACY_GINZA_MODEL"])
            pipeline.to_disk(model)
            doc = pipeline(text)
            projection = [
                ProjectionAttribute("text", "scalar"),
                ProjectionAttribute("lemma", "scalar"),
                ProjectionAttribute("upos", "scalar"),
                ProjectionAttribute("xpos", "scalar"),
                ProjectionAttribute("dep.enhanced", "string_set"),
                ProjectionAttribute("dep.head", "scalar"),
                ProjectionAttribute("dep.rel", "scalar"),
            ]
            direct = analyze_document(doc, projection, "input")[0]
            process = subprocess.run(
                ["ginza", "-b", str(model), "-f", "conllu"],
                input=text + "\n",
                text=True,
                capture_output=True,
                check=True,
            )

        cli_rows = [
            line.split("\t")
            for line in process.stdout.splitlines()
            if line and not line.startswith("#") and line.split("\t", 1)[0].isdigit()
        ]
        cli = [
            [
                columns[1],
                None if columns[2] == "_" else columns[2],
                None if columns[3] == "_" else columns[3],
                None if columns[4] == "_" else columns[4],
                [] if columns[8] == "_" else columns[8].split("|"),
                None if columns[6] == "_" else columns[6],
                None if columns[7] == "_" else columns[7].lower(),
            ]
            for columns in cli_rows
        ]
        width = len(projection)
        projected = [
            direct["values"][offset : offset + width]
            for offset in range(0, len(direct["values"]), width)
        ]
        self.assertEqual(projected, cli)

    def test_real_ginza_model_produces_source_aligned_portable_morphology(self) -> None:
        import spacy

        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            spacy.load(os.environ["LINGREP_SPACY_GINZA_MODEL"]).to_disk(model)
            prepare_model_bundle(
                model,
                probe_text="猫が走る。",
                morphology_keys=(
                    "spacy.morph.inflection",
                    "spacy.morph.reading",
                ),
            )
            loaded = load_spacy_pipeline(str(model), "cpu", "morphology", "input")
            doc = next(loaded.pipeline.pipe(["猫が走る。"], batch_size=1))
            mapped = analyze_document(
                doc,
                [
                    ProjectionAttribute("text", "scalar"),
                    ProjectionAttribute("lemma", "scalar"),
                    ProjectionAttribute("upos", "scalar"),
                    ProjectionAttribute("xpos", "scalar"),
                ],
                "input",
            )[0]
        forms = mapped["values"][0::4]
        self.assertEqual("".join(forms), "猫が走る。")
        self.assertTrue(all(mapped["values"][index] for index in range(1, 4)))

    @unittest.skipUnless(
        os.environ.get("LINGREP_SPACY_RUN_ROCM") == "1",
        "requires LINGREP_SPACY_RUN_ROCM=1 and the rocm uv extra",
    )
    def test_cpu_and_rocm_processes_emit_identical_ginza_sentences(self) -> None:
        zero = "00" * 32
        attributes = [
            {"key": "text", "kind": "scalar"},
            {"key": "lemma", "kind": "scalar"},
            {"key": "upos", "kind": "scalar"},
            {"key": "xpos", "kind": "scalar"},
        ]
        header = {
            "record": "run_header",
            "protocol": "lingrep.external-morphology/v1",
            "run_id": "ginza-backend-parity",
            "family": "external",
            "profile": "morphology",
            "segmentation": "input",
            "normalization": "none",
            "language_policy": "ja",
            "boundary_policy": "input-v1",
            "projection": attributes,
            "implementation_digest": zero,
            "resource_manifest_digest": zero,
            "projection_policy_digest": zero,
            "alignment_policy_digest": zero,
            "component_closure": [
                {"name": "adapter-command", "digest": zero},
                {"name": "spacy-profile", "digest": zero},
            ],
            "output_config_digest": zero,
            "protocol_limits": {
                "max_record_bytes": 1_048_576,
                "max_request_batch_bytes": 67_108_864,
                "max_response_batch_bytes": 67_108_864,
                "max_documents_per_batch": 2048,
                "max_schema_keys": 255,
                "max_schema_key_bytes": 256,
                "max_warnings_per_sentence": 64,
                "max_warning_message_bytes": 16_384,
                "max_stderr_tail_bytes": 65_536,
            },
        }
        document = {
            "record": "document",
            "batch_id": 0,
            "document_ordinal": 0,
            "input_id": 10,
            "kind": "sentence",
            "text": "猫が走る。",
            "provenance": {
                "source_identity": zero,
                "document_identity": 1,
                "paragraph_identity": None,
                "metadata_digest": zero,
            },
        }
        import spacy

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = root / "model"
            spacy.load(os.environ["LINGREP_SPACY_GINZA_MODEL"]).to_disk(model)
            profile = prepare_model_bundle(
                model,
                probe_text="猫が走る。",
                morphology_keys=(
                    "spacy.morph.inflection",
                    "spacy.morph.reading",
                ),
            )
            selection = profile["selections"]["morphology/input"]
            profile_digest = selection_digest(selection)
            header["resource_manifest_digest"] = resource_manifest_digest(
                Path("model"), root
            )
            header["component_closure"][1]["digest"] = profile_digest
            header["output_config_digest"] = profile_digest
            encoded = b"".join(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode()
                + b"\n"
                for record in [
                    header,
                    {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                    document,
                    {"record": "batch_end", "batch_id": 0},
                ]
            )

            def result_for(backend: str) -> tuple[dict, dict]:
                result = subprocess.run(
                    [
                        sys.executable,
                        "-m",
                        "lingrep_spacy_adapter",
                        "--model-root",
                        "model",
                        "--backend",
                        backend,
                        "--batch-size",
                        "1",
                    ],
                    input=encoded,
                    capture_output=True,
                    check=False,
                    cwd=root,
                )
                self.assertEqual(result.returncode, 0, result.stderr.decode())
                records = list(map(json.loads, result.stdout.splitlines()))
                return (
                    next(
                        record for record in records if record["record"] == "sentence"
                    ),
                    next(
                        record for record in records if record["record"] == "run_ready"
                    ),
                )

            cpu_sentence, cpu_ready = result_for("cpu")
            rocm_sentence, rocm_ready = result_for("rocm")
            self.assertEqual(cpu_sentence, rocm_sentence)
            self.assertIn("host-", cpu_ready["execution"]["runtime"])
            self.assertIn("platform-", cpu_ready["execution"]["runtime"])
            for marker in (
                "cupy-rocm-7-0-",
                "accelerator-runtime-",
                "accelerator-driver-",
                "torch-build-",
                "host-",
                "platform-",
            ):
                self.assertIn(marker, rocm_ready["execution"]["runtime"])


@unittest.skipUnless(
    any(
        os.environ.get(f"LINGREP_SPACY_RUN_{backend}") == "1"
        for backend in ("CUDA", "ROCM")
    ),
    "requires LINGREP_SPACY_RUN_CUDA=1 or LINGREP_SPACY_RUN_ROCM=1",
)
class GinzaGpuLauncherTests(unittest.TestCase):
    def test_ginza_gpu_launcher_matches_cpu_conllu(self) -> None:
        source = (
            "猫が走る。犬は走らない。\n"
            "彼女は「明日、東京へ行きます」と言った。\n"
            "２０２６年の価格は1,234円です。\n"
            "\n"
            "すもももももももものうち。\n"
            "吾輩は猫である。名前はまだ無い。\n"
        ).encode()
        model = os.environ.get("LINGREP_SPACY_GINZA_MODEL", "ja_ginza")
        arguments = ["-b", model, "-d", "-c", "print", "-f", "conllu"]
        cpu = subprocess.run(
            [str(Path(sys.executable).with_name("ginza")), "-g", "-1", *arguments],
            input=source,
            capture_output=True,
            check=True,
            timeout=180,
        )
        self.assertIn("\t猫\t".encode(), cpu.stdout)
        launcher = Path(__file__).resolve().parents[1] / "bin" / "ginza-gpu"
        for backend in ("cuda", "rocm"):
            if os.environ.get(f"LINGREP_SPACY_RUN_{backend.upper()}") != "1":
                continue
            with self.subTest(backend=backend):
                gpu = subprocess.run(
                    [sys.executable, str(launcher), backend, "-g", "0", *arguments],
                    input=source,
                    capture_output=True,
                    check=False,
                    timeout=180,
                )
                self.assertEqual(gpu.returncode, 0, gpu.stderr.decode())
                self.assertEqual(cpu.stdout, gpu.stdout)


if __name__ == "__main__":
    unittest.main()


@unittest.skipUnless(
    os.environ.get("LINGREP_SPACY_ENGLISH_MODEL"), "requires the packaged English model"
)
class EnglishModelTests(unittest.TestCase):
    def test_packaged_model_produces_repeatable_morphology_and_dependencies(self):
        root = os.environ["LINGREP_SPACY_ENGLISH_MODEL"]
        loaded = load_spacy_pipeline(root, "cpu", "morphology", "input")
        keys = [attribute.key for attribute in loaded.schema]
        for key in ("text", "lemma", "upos", "feat.Number", "dep.head", "dep.rel"):
            self.assertIn(key, keys)
        text = "I was speaking. They listened."
        results = []
        for _ in range(2):
            doc = next(loaded.pipeline.pipe([text], batch_size=1))
            results.append(analyze_document(doc, loaded.schema, "input"))
        self.assertEqual(results[0], results[1])
        rows = [
            results[0][0]["values"][offset : offset + len(keys)]
            for offset in range(0, len(results[0][0]["values"]), len(keys))
        ]
        self.assertTrue(any(row[keys.index("lemma")] == "speak" for row in rows))
        self.assertTrue(any(row[keys.index("dep.rel")] == "nsubj" for row in rows))
        self.assertTrue(any(row[keys.index("feat.Number")] for row in rows))
