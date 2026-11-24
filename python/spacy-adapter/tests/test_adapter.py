from __future__ import annotations

import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass, replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from lingrep_adapter_protocol.resources import adapter_semantics_digest

from lingrep_spacy_adapter.adapter import (
    ADAPTER_SEMANTICS_REVISION,
    LoadedPipeline,
    ProjectionAttribute,
    _decode_batch_begin,
    _decode_batch_end,
    _decode_document,
    _encode_record,
    analyze_document,
    default_spacy_backend,
    initialize_spacy_backend,
    iter_analyzed_sentences,
    load_spacy_pipeline,
    morph_attribute_key,
    serve,
)
from lingrep_spacy_adapter.bundle import (
    BUNSETU_KEY,
    HUB_REVISIONS,
    MANIFEST_HASH_CHUNK_BYTES,
    prepare_model_bundle,
    resource_manifest_digest,
    save_model,
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
    def test_sentence_mapping_rejects_invalid_morphology_members(self) -> None:
        for members in ([7], [""], ["猫", None]):
            with self.subTest(members=members):
                token = FakeToken("猫", 0, morph=FakeMorph({"Reading": members}))
                doc = FakeDoc("猫", [token], [FakeSpan((token,), 0, 1)])
                with self.assertRaisesRegex(ValueError, "must be strings"):
                    analyze_document(doc, projection(), "input")

    def test_ud_features_split_comma_joined_values_and_provider_fields_keep_them(
        self,
    ) -> None:
        token = FakeToken(
            "，", 0, morph=FakeMorph({"Number": "Sing,Plur", "Reading": ","})
        )
        doc = FakeDoc("，", [token], [FakeSpan((token,), 0, 1)])
        values = analyze_document(doc, projection(), "input")[0]["values"]
        self.assertEqual(values[5:], [[], ["Plur", "Sing"], [","]])

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

            def run(implementation_digest: str) -> subprocess.CompletedProcess:
                records = [
                    {**header, "implementation_digest": implementation_digest},
                    {"record": "batch_begin", "batch_id": 0, "document_count": 1},
                    document,
                    {"record": "batch_end", "batch_id": 0},
                ]
                encoded = b"".join(
                    json.dumps(
                        record, ensure_ascii=False, separators=(",", ":")
                    ).encode()
                    + b"\n"
                    for record in records
                )
                return subprocess.run(
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

            rejected = run(zero)
            result = run(adapter_semantics_digest(ADAPTER_SEMANTICS_REVISION))
        self.assertEqual(rejected.returncode, 1)
        self.assertIn(b"does not name adapter semantics", rejected.stderr)
        self.assertEqual(rejected.stdout, b"")
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

    def test_mps_initializes_torch_then_spacy_without_cupy_or_fallback(self) -> None:
        imported = []
        required = []
        modules = {
            "torch": SimpleNamespace(
                backends=SimpleNamespace(mps=SimpleNamespace(is_available=lambda: True))
            ),
            "spacy": SimpleNamespace(
                require_gpu=lambda device: required.append(device)
            ),
        }

        def import_module(name: str):
            imported.append(name)
            return modules[name]

        with patch(
            "lingrep_spacy_adapter.adapter.importlib.import_module",
            side_effect=import_module,
        ):
            backend = initialize_spacy_backend("mps")
        self.assertEqual(imported, ["torch", "spacy"])
        self.assertEqual(required, [0])
        self.assertEqual(backend[1:], (None, None, None, None, None))

    def test_mps_rejects_a_torch_without_mps_before_importing_spacy(self) -> None:
        def missing_torch(name: str):
            raise ImportError(name)

        without_mps = SimpleNamespace(
            backends=SimpleNamespace(mps=SimpleNamespace(is_available=lambda: False))
        )
        for label, torch in (
            ("mps unavailable", lambda name: without_mps),
            ("torch not installed", missing_torch),
        ):
            imported = []

            def import_module(name: str, torch=torch, imported=imported):
                imported.append(name)
                return torch(name)

            with (
                self.subTest(label),
                patch(
                    "lingrep_spacy_adapter.adapter.importlib.import_module",
                    side_effect=import_module,
                ),
                self.assertRaisesRegex(RuntimeError, "available MPS device"),
            ):
                initialize_spacy_backend("mps")
            self.assertEqual(imported, ["torch"])

    def test_setup_default_is_mps_only_on_apple_silicon_with_mps_available(
        self,
    ) -> None:
        for system, machine, available, expected in (
            ("Darwin", "arm64", True, "mps"),
            ("Darwin", "arm64", False, "cpu"),
            ("Darwin", "x86_64", True, "cpu"),
            ("Linux", "aarch64", True, "cpu"),
            ("Linux", "x86_64", True, "cpu"),
        ):
            with self.subTest(system=system, machine=machine, available=available):
                self.assertEqual(
                    default_spacy_backend(system, machine, lambda a=available: a),
                    expected,
                )

    def test_adapter_command_line_accepts_the_mps_backend(self) -> None:
        from lingrep_spacy_adapter.__main__ import parser

        args = parser().parse_args(["--model-root", "model", "--backend", "mps"])
        self.assertEqual(args.backend, "mps")


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

    def test_hub_transformers_are_refused_unless_pinned_and_downloadable(
        self,
    ) -> None:
        import spacy

        pinned = next(iter(HUB_REVISIONS))
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            spacy.blank("en").to_disk(source)
            config = spacy.util.load_config(source / "config.cfg")
            config["nlp"]["pipeline"] = ["transformer"]
            for repository, offline, message in (
                ("someone/unpinned", False, "pins no revision"),
                (pinned, True, "without --offline"),
            ):
                config["components"]["transformer"] = {
                    "factory": "transformer_custom",
                    "model": {"name": repository},
                }
                config.to_disk(source / "config.cfg")
                destination = Path(directory) / "model"
                with (
                    self.subTest(repository=repository, offline=offline),
                    self.assertRaisesRegex(ValueError, message),
                ):
                    save_model(str(source), destination, offline=offline)
                self.assertFalse(destination.exists())
            with self.assertRaisesRegex(ValueError, "Hugging Face Hub"):
                prepare_model_bundle(source, probe_text="Cats run.")

    @unittest.skipUnless(
        importlib.util.find_spec("ginza_transformers"),
        "requires the electra or bert-large uv extra",
    )
    def test_hub_transformer_is_saved_from_its_pinned_download_alone(self) -> None:
        import spacy
        import srsly
        from transformers import BertConfig, BertModel, BertTokenizerFast

        repository = "lingrep-test/tiny"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tiny = root / "tiny"
            tiny.mkdir()
            vocab = tiny / "vocab.txt"
            vocab.write_text(
                "\n".join(["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]", "cats", "run"])
            )
            BertTokenizerFast(str(vocab)).save_pretrained(tiny)
            BertModel(
                BertConfig(
                    vocab_size=7,
                    hidden_size=8,
                    num_hidden_layers=1,
                    num_attention_heads=1,
                    intermediate_size=8,
                    max_position_embeddings=32,
                )
            ).save_pretrained(tiny)
            source = root / "source"
            pipeline = spacy.blank("en")
            pipeline.add_pipe("transformer", config={"model": {"name": str(tiny)}})
            pipeline.initialize()
            pipeline.to_disk(source)
            config = spacy.util.load_config(source / "config.cfg")
            config["components"]["transformer"]["factory"] = "transformer_custom"
            config["components"]["transformer"]["model"]["name"] = repository
            config.to_disk(source / "config.cfg")
            model = srsly.msgpack_loads((source / "transformer" / "model").read_bytes())
            model["shims"] = [
                [
                    srsly.msgpack_dumps(
                        {
                            "config": {"_name_or_path": repository},
                            "_init_tokenizer_config": {"use_fast": True},
                            "_init_transformer_config": {},
                        }
                    )
                    for _ in shims
                ]
                for shims in model["shims"]
            ]
            (source / "transformer" / "model").write_bytes(srsly.msgpack_dumps(model))

            requests = []

            def refuse(session, method, url, *args, **kwargs):
                requests.append(url)
                raise ConnectionError(f"Hugging Face Hub request: {url}")

            for complete in (True, False):

                def download(repo_id, *, revision, local_dir, complete=complete):
                    self.assertEqual((repo_id, revision), (repository, "0" * 40))
                    if complete:
                        shutil.copytree(tiny, local_dir)

                destination = root / f"model-{complete}"
                with (
                    self.subTest(complete=complete),
                    patch.dict(HUB_REVISIONS, {repository: "0" * 40}),
                    patch("huggingface_hub.snapshot_download", download),
                    patch("requests.sessions.Session.request", refuse),
                ):
                    if complete:
                        save_model(str(source), destination)
                        saved = spacy.load(destination)
                        self.assertTrue(saved("cats run")._.trf_data.tensors)
                    else:
                        with self.assertRaises((OSError, ValueError)):
                            save_model(str(source), destination)
                    self.assertEqual(requests, [])

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


GOLDEN_FILE = Path(__file__).with_name("golden") / "ginza-output.json"
GOLDEN_MODEL = "ja_ginza"
GOLDEN_RUNTIME_DISTRIBUTIONS = (
    "ginza",
    "ja-ginza",
    "spacy",
    "sudachipy",
    "sudachidict-core",
    "thinc",
)
GOLDEN_TEXTS = (
    "彼女は坂を一つ上った。",
    "猫が走った。犬は庭で寝ている。",
    "えたいの知れない不吉な塊が私の心を始終圧えつけていた。",
    "開いている窓を一つ一つ見ていると、僕はそのことを憶い出す。",
    "2026年11月17日、東京駅でＡＩの本を3冊買いました！　高かった？",
    "「行こうか」と彼は言ったが、誰も答えなかった。",
)


def _golden_sentences(model: Path, selection: str) -> list[dict]:
    """Serve GOLDEN_TEXTS through the adapter protocol and return its
    sentence records."""

    profile, segmentation = selection.split("/")
    loaded = load_spacy_pipeline(str(model), "cpu", profile, segmentation)
    zero = "00" * 32
    header = {
        "record": "run_header",
        "protocol": "lingrep.external-morphology/v1",
        "run_id": "ginza-golden",
        "family": "external",
        "profile": profile,
        "segmentation": segmentation,
        "normalization": "none",
        "language_policy": loaded.language,
        "boundary_policy": loaded.boundary_policy,
        "projection": [
            {"key": attribute.key, "kind": attribute.kind}
            for attribute in loaded.schema
        ],
        "implementation_digest": adapter_semantics_digest(ADAPTER_SEMANTICS_REVISION),
        "resource_manifest_digest": zero,
        "projection_policy_digest": zero,
        "alignment_policy_digest": zero,
        "component_closure": sorted(
            [{"name": "adapter-command", "digest": zero}, *loaded.component_closure],
            key=lambda component: component["name"],
        ),
        "output_config_digest": loaded.output_config_digest,
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
    kind = "sentence" if segmentation == "input" else "segment"
    records = [
        header,
        {"record": "batch_begin", "batch_id": 0, "document_count": len(GOLDEN_TEXTS)},
        *(
            {
                "record": "document",
                "batch_id": 0,
                "document_ordinal": ordinal,
                "input_id": ordinal,
                "kind": kind,
                "text": text,
                "provenance": {
                    "source_identity": zero,
                    "document_identity": 1,
                    "paragraph_identity": None,
                    "metadata_digest": zero,
                },
            }
            for ordinal, text in enumerate(GOLDEN_TEXTS)
        ),
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
        model_root=str(model),
        backend="cpu",
        batch_size=32,
        lock_digest=zero,
        load_pipeline=lambda *_: loaded,
    )
    return [
        record
        for record in map(json.loads, stdout.getvalue().splitlines())
        if record["record"] == "sentence"
    ]


def _golden_json(value: dict) -> str:
    """Render one sentence record per line so that output changes diff by
    sentence."""

    def dump(item: object) -> str:
        return json.dumps(item, ensure_ascii=False, separators=(",", ":"))

    lines = ["{"]
    for key in ("revision", "model", "runtime", "texts"):
        lines.append(f" {dump(key)}: {dump(value[key])},")
    lines.append(' "selections": {')
    selections = list(value["selections"].items())
    for index, (key, sentences) in enumerate(selections):
        lines.append(f"  {dump(key)}: [")
        lines.extend(
            f"   {dump(sentence)}{',' if position + 1 < len(sentences) else ''}"
            for position, sentence in enumerate(sentences)
        )
        lines.append("  ]" + ("," if index + 1 < len(selections) else ""))
    lines.extend([" }", "}"])
    return "\n".join(lines) + "\n"


@unittest.skipUnless(
    os.environ.get("LINGREP_SPACY_GINZA_MODEL"),
    "requires LINGREP_SPACY_GINZA_MODEL and the ginza uv extra",
)
class GiNzaModelTests(unittest.TestCase):
    def test_default_profiles_match_ginza_cli_conllu(self) -> None:
        lines = [
            "彼女は坂を一つ上った。",
            "猫が走った。犬は庭で寝ている。",
            "えたいの知れない不吉な塊が私の心を始終圧えつけていた。",
            "開いている窓を一つ一つ見ていると、僕はそのことを憶い出す。",
        ]
        name = os.environ["LINGREP_SPACY_GINZA_MODEL"]
        process = subprocess.run(
            ["ginza", "-g", "-1", "-m", name, "-f", "conllu"],
            input="".join(line + "\n" for line in lines),
            text=True,
            capture_output=True,
            check=True,
        )
        keys = ["text", "lemma", "upos", "xpos", "dep.head", "dep.rel", BUNSETU_KEY]
        cli: list[list[list[str | None]]] = [[]]
        for line in process.stdout.splitlines():
            if not line:
                if cli[-1]:
                    cli.append([])
                continue
            columns = line.split("\t")
            if line.startswith("#") or not columns[0].isdigit():
                continue
            misc = dict(
                entry.split("=", 1) for entry in columns[9].split("|") if "=" in entry
            )
            cli[-1].append(
                [
                    columns[1],
                    None if columns[2] == "_" else columns[2],
                    None if columns[3] == "_" else columns[3],
                    None if columns[4] == "_" else columns[4],
                    columns[6],
                    columns[7].lower(),
                    misc["BunsetuBILabel"],
                ]
            )
        cli = [sentence for sentence in cli if sentence]

        def rows(loaded, segmentation):
            projection = [
                ProjectionAttribute(key, "scalar")
                for key in keys
                if ProjectionAttribute(key, "scalar") in loaded.schema
            ]
            self.assertEqual([attribute.key for attribute in projection], keys)
            sentences = []
            for doc in loaded.pipeline.pipe(lines, batch_size=1):
                for sentence in analyze_document(doc, projection, segmentation):
                    values = sentence["values"]
                    sentences.append(
                        [
                            values[offset : offset + len(keys)]
                            for offset in range(0, len(values), len(keys))
                        ]
                    )
            return sentences

        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            save_model(name, model)
            prepare_model_bundle(model, probe_text="猫が走る。")
            analyzer = rows(
                load_spacy_pipeline(str(model), "cpu", "morphology", "analyzer"),
                "analyzer",
            )
            unit = rows(
                load_spacy_pipeline(str(model), "cpu", "morphology", "input"), "input"
            )
        self.assertEqual(analyzer, cli)
        self.assertEqual({"B", "I"}, {row[-1] for sentence in cli for row in sentence})
        self.assertEqual(unit[0], cli[0])
        self.assertEqual(unit[2:], cli[3:])
        second = [
            row[:4]
            + [str(int(row[4]) + len(cli[1])) if row[4] != "0" else "0"]
            + row[5:]
            for row in cli[2]
        ]
        self.assertEqual(unit[1], cli[1] + second)

    def test_real_ginza_model_skipped_spaces_preserve_word_ids_and_bunsetsu(
        self,
    ) -> None:
        import spacy

        doc = spacy.load(os.environ["LINGREP_SPACY_GINZA_MODEL"])("猫  が　走る。")
        keys = ["text", "dep.head", BUNSETU_KEY]
        rows = analyze_document(
            doc, [ProjectionAttribute(key, "scalar") for key in keys], "input"
        )[0]["values"]
        self.assertEqual(
            rows, ["猫", "3", "B", "が", "1", "I", "走る", "0", "B", "。", "3", "I"]
        )

    def test_real_ginza_model_produces_source_aligned_portable_morphology(self) -> None:
        """The saved model analyzes offline, from its own files, like the original."""

        import spacy

        text = "坂を一つ上った。"
        name = os.environ["LINGREP_SPACY_GINZA_MODEL"]
        projection = [
            ProjectionAttribute("text", "scalar"),
            ProjectionAttribute("lemma", "scalar"),
            ProjectionAttribute("upos", "scalar"),
            ProjectionAttribute("xpos", "scalar"),
        ]
        expected = analyze_document(spacy.load(name)(text), projection, "input")[0]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = root / "model"
            save_model(name, model)
            prepare_model_bundle(
                model,
                probe_text="猫が走る。",
                morphology_keys=(
                    "spacy.morph.inflection",
                    "spacy.morph.reading",
                ),
            )
            empty_hub = root / "hub"
            empty_hub.mkdir()
            script = (
                "import json, sys\n"
                "from lingrep_spacy_adapter.adapter import ProjectionAttribute, "
                "analyze_document, load_spacy_pipeline\n"
                "loaded = load_spacy_pipeline(sys.argv[1], 'cpu', 'morphology', "
                "'input')\n"
                "doc = next(loaded.pipeline.pipe([sys.argv[2]], batch_size=1))\n"
                "projection = [ProjectionAttribute(k, 'scalar') "
                "for k in ('text', 'lemma', 'upos', 'xpos')]\n"
                "print(json.dumps(analyze_document(doc, projection, 'input')[0]))\n"
            )
            result = subprocess.run(
                [sys.executable, "-c", script, str(model), text],
                capture_output=True,
                check=False,
                cwd=empty_hub,
                env={
                    **os.environ,
                    "HF_HUB_OFFLINE": "1",
                    "HF_HOME": str(empty_hub),
                    "HF_HUB_CACHE": str(empty_hub),
                    "HF_MODULES_CACHE": str(empty_hub),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(list(empty_hub.iterdir()), [])
        self.assertEqual(json.loads(result.stdout), expected)
        self.assertEqual("".join(expected["values"][0::4]), text)

    @unittest.skipUnless(
        os.environ.get("LINGREP_SPACY_GINZA_MODEL") == GOLDEN_MODEL,
        f"the golden output is recorded for {GOLDEN_MODEL}",
    )
    def test_real_ginza_model_output_matches_its_semantics_revision_golden(
        self,
    ) -> None:
        """Changing the adapter's output requires a new semantics revision.

        Set LINGREP_SPACY_WRITE_GOLDEN=1 to record the golden file after the
        revision or the runtime changes."""

        import importlib.metadata

        runtime = {
            name: importlib.metadata.version(name)
            for name in GOLDEN_RUNTIME_DISTRIBUTIONS
        }
        with tempfile.TemporaryDirectory() as directory:
            model = Path(directory) / "model"
            save_model(GOLDEN_MODEL, model)
            profile = prepare_model_bundle(model, probe_text="猫が走る。")
            selections = {
                key: _golden_sentences(model, key)
                for key in sorted(profile["selections"])
            }
        actual = {
            "revision": ADAPTER_SEMANTICS_REVISION,
            "model": GOLDEN_MODEL,
            "runtime": runtime,
            "texts": list(GOLDEN_TEXTS),
            "selections": selections,
        }
        recorded = (
            json.loads(GOLDEN_FILE.read_text()) if GOLDEN_FILE.is_file() else None
        )
        same_inputs = recorded is not None and all(
            recorded[key] == actual[key]
            for key in ("revision", "model", "runtime", "texts")
        )
        if os.environ.get("LINGREP_SPACY_WRITE_GOLDEN") == "1":
            self.assertFalse(
                same_inputs and recorded != actual,
                "adapter output changed under the same semantics revision and "
                "runtime; replace ADAPTER_SEMANTICS_REVISION and its Rust mirror",
            )
            GOLDEN_FILE.parent.mkdir(exist_ok=True)
            GOLDEN_FILE.write_text(_golden_json(actual))
            return
        self.assertIsNotNone(recorded, f"{GOLDEN_FILE} is missing")
        self.assertTrue(
            same_inputs,
            "the semantics revision or runtime differs from the golden file; "
            "record it with LINGREP_SPACY_WRITE_GOLDEN=1",
        )
        self.assertEqual(
            recorded["selections"],
            actual["selections"],
            "adapter output changed under the same semantics revision and "
            "runtime; replace ADAPTER_SEMANTICS_REVISION and its Rust mirror, "
            "then record the golden file",
        )

    def _serve_sealed_model(
        self, backends: tuple[str, ...], texts: tuple[str, ...]
    ) -> dict[str, tuple[list[dict], dict]]:
        """Seals LINGREP_SPACY_GINZA_MODEL and serves `texts` once per backend.

        Returns each backend's sentence records and its run_ready record.
        """

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
        documents = [
            {
                "record": "document",
                "batch_id": 0,
                "document_ordinal": ordinal,
                "input_id": 10 + ordinal,
                "kind": "sentence",
                "text": text,
                "provenance": {
                    "source_identity": zero,
                    "document_identity": 1,
                    "paragraph_identity": None,
                    "metadata_digest": zero,
                },
            }
            for ordinal, text in enumerate(texts)
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            model = root / "model"
            save_model(os.environ["LINGREP_SPACY_GINZA_MODEL"], model)
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
            header["implementation_digest"] = adapter_semantics_digest(
                ADAPTER_SEMANTICS_REVISION
            )
            encoded = b"".join(
                json.dumps(record, ensure_ascii=False, separators=(",", ":")).encode()
                + b"\n"
                for record in [
                    header,
                    {
                        "record": "batch_begin",
                        "batch_id": 0,
                        "document_count": len(documents),
                    },
                    *documents,
                    {"record": "batch_end", "batch_id": 0},
                ]
            )

            def result_for(backend: str) -> tuple[list[dict], dict]:
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
                    [record for record in records if record["record"] == "sentence"],
                    next(
                        record for record in records if record["record"] == "run_ready"
                    ),
                )

            return {backend: result_for(backend) for backend in backends}

    @unittest.skipUnless(
        os.environ.get("LINGREP_SPACY_RUN_ROCM") == "1",
        "requires LINGREP_SPACY_RUN_ROCM=1 and the rocm uv extra",
    )
    def test_cpu_and_rocm_processes_emit_identical_ginza_sentences(self) -> None:
        results = self._serve_sealed_model(("cpu", "rocm"), ("猫が走る。",))
        cpu_sentences, cpu_ready = results["cpu"]
        rocm_sentences, rocm_ready = results["rocm"]
        self.assertEqual(len(cpu_sentences), 1)
        self.assertEqual(cpu_sentences, rocm_sentences)
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
        os.environ.get("LINGREP_SPACY_RUN_MPS") == "1",
        "requires LINGREP_SPACY_RUN_MPS=1 on Apple Silicon with the cpu uv extra",
    )
    def test_mps_process_analyzes_ginza_sentences(self) -> None:
        texts = ("猫が走る。", "彼女は坂を一つ上った。", "犬は庭で寝ている。")
        sentences, ready = self._serve_sealed_model(("mps",), texts)["mps"]
        self.assertEqual(ready["execution"]["backend"], "mps")
        self.assertIn("backend-mps", ready["execution"]["runtime"].split("/"))
        self.assertEqual(
            [sentence["document_ordinal"] for sentence in sentences],
            list(range(len(texts))),
        )
        for text, sentence in zip(texts, sentences, strict=True):
            self.assertEqual(sentence["source_range"], [0, len(text.encode())])
            self.assertGreater(sentence["word_count"], 1)


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
        self.assertNotIn(BUNSETU_KEY, keys)
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

    def test_packaged_model_splits_sentences_like_its_full_pipeline(self):
        import spacy

        root = os.environ["LINGREP_SPACY_ENGLISH_MODEL"]
        text = (
            "I was speaking to the man who came yesterday. They listened "
            "carefully and then they left the room. It rained."
        )
        loaded = load_spacy_pipeline(root, "cpu", "tokenize", "analyzer")
        self.assertEqual(
            [sentence.text for sentence in loaded.pipeline(text).sents],
            [sentence.text for sentence in spacy.load(root)(text).sents],
        )
