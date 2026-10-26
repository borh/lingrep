from __future__ import annotations

import json
import sys
import tempfile
import types
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch

from lingrep_stanza_adapter.bundle import (
    PROFILE_FILE,
    load_profile,
    prepare_model_bundle,
    resource_manifest_digest,
    selection_digest,
)


class FakeWord:
    def __init__(
        self,
        text: str,
        *,
        xpos: str | None = "NN",
        feats: str | None = "Number=Sing",
    ) -> None:
        self.text = text
        self.lemma = text.lower()
        self.upos = "NOUN"
        self.xpos = xpos
        self.feats = feats


class FakeToken:
    def __init__(self, text: str, start: int, end: int) -> None:
        self.text = text
        self.start_char = start
        self.end_char = end
        self.words = [FakeWord(text)]


class FakePipeline:
    calls: list[dict[str, object]] = []

    def __init__(self, **kwargs: object) -> None:
        self.calls.append(kwargs)

    def __call__(self, text: str) -> object:
        token = FakeToken(text, 0, len(text))
        sentence = types.SimpleNamespace(tokens=[token])
        return types.SimpleNamespace(sentences=[sentence])


def fake_stanza() -> types.ModuleType:
    module = types.ModuleType("stanza")
    module.Pipeline = FakePipeline
    module.__version__ = "1.14.0"
    return module


def write_resources(root: Path) -> None:
    (root / "resources.json").write_text("{}\n", encoding="utf-8")
    model = root / "fr" / "tokenize" / "gsd.pt"
    model.parent.mkdir(parents=True)
    model.write_bytes(b"model")


class BundleTests(unittest.TestCase):
    def setUp(self) -> None:
        FakePipeline.calls.clear()

    def test_profile_rejects_duplicate_keys_at_each_depth(self) -> None:
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

    def test_profile_binds_language_processors_boundary_schema_and_policy(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_resources(root)
            with patch.dict(sys.modules, {"stanza": fake_stanza()}):
                profile = prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={
                        "tokenize": "gsd",
                        "mwt": "gsd",
                        "pos": "gsd_nocharlm",
                        "lemma": "gsd_nocharlm",
                    },
                )

            selection = profile["selections"]["morphology/analyzer"]
            self.assertEqual(
                profile["selections"]["tokenize/input"]["processors"],
                ["tokenize", "mwt"],
            )
            self.assertEqual(
                selection["processors"], ["tokenize", "mwt", "pos", "lemma"]
            )
            self.assertEqual(selection["boundary_policy"], "stanza:tokenize:v1")
            self.assertEqual(selection["alignment_policy"], "stanza-mwt/v1")
            self.assertEqual(selection["schema"][0], {"key": "text", "kind": "scalar"})
            self.assertIn(
                {"key": "feat.Number", "kind": "string_set"},
                selection["schema"],
            )
            self.assertTrue(
                all(call["download_method"] is None for call in FakePipeline.calls)
            )
            self.assertEqual(load_profile(root), profile)

    def test_profile_has_distinct_input_and_analyzer_selections(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_resources(root)
            with patch.dict(sys.modules, {"stanza": fake_stanza()}):
                profile = prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={"tokenize": "gsd"},
                )

            selections = profile["selections"]
            self.assertEqual(set(selections), {"tokenize/input", "tokenize/analyzer"})
            input_call, analyzer_call = FakePipeline.calls
            self.assertIs(input_call["tokenize_no_ssplit"], True)
            self.assertIs(analyzer_call["tokenize_no_ssplit"], False)
            self.assertNotEqual(
                selections["tokenize/input"]["boundary_policy"],
                selections["tokenize/analyzer"]["boundary_policy"],
            )

    def test_profile_allows_word_local_missing_xpos_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_resources(root)
            original_call = FakePipeline.__call__

            def without_xpos(_pipeline: FakePipeline, text: str) -> object:
                token = FakeToken(text, 0, len(text))
                token.words = [FakeWord(text, xpos=None)]
                return types.SimpleNamespace(
                    sentences=[types.SimpleNamespace(tokens=[token])]
                )

            FakePipeline.__call__ = without_xpos
            try:
                with patch.dict(sys.modules, {"stanza": fake_stanza()}):
                    profile = prepare_model_bundle(
                        root,
                        language="fr",
                        probe_text="Chats",
                        packages={
                            "tokenize": "gsd",
                            "mwt": "gsd",
                            "pos": "gsd_nocharlm",
                            "lemma": "gsd_nocharlm",
                        },
                    )
            finally:
                FakePipeline.__call__ = original_call

            self.assertIn(
                {"key": "xpos", "kind": "scalar"},
                profile["selections"]["morphology/input"]["schema"],
            )

    def test_profile_rejects_missing_resources_unknown_processors_and_downloads(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with (
                patch.dict(sys.modules, {"stanza": fake_stanza()}),
                self.assertRaisesRegex(ValueError, "resources.json"),
            ):
                prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={"tokenize": "gsd"},
                )
            write_resources(root)
            with (
                patch.dict(sys.modules, {"stanza": fake_stanza()}),
                self.assertRaisesRegex(ValueError, "processor"),
            ):
                prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={"tokenize": "gsd", "ner": "wikiner"},
                )
            self.assertTrue(
                all(call.get("download_method") is None for call in FakePipeline.calls)
            )

    def test_resource_manifest_streams_large_files_and_rejects_caches_symlinks(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest_root = Path(directory)
            root = manifest_root / "model"
            root.mkdir()
            write_resources(root)
            large = root / "large.pt"
            large.write_bytes(b"x" * ((1 << 20) + 17))
            first = resource_manifest_digest(Path("model"), manifest_root)
            second = resource_manifest_digest(Path("model"), manifest_root)
            self.assertEqual(first, second)

            cache = root / "__pycache__" / "model.pyc"
            cache.parent.mkdir()
            cache.write_bytes(b"cache")
            with self.assertRaisesRegex(ValueError, "cache"):
                resource_manifest_digest(Path("model"), manifest_root)
            cache.unlink()
            cache.parent.rmdir()

            link = root / "linked.pt"
            link.symlink_to("large.pt")
            with self.assertRaisesRegex(ValueError, "symlink"):
                resource_manifest_digest(Path("model"), manifest_root)

    def test_resource_manifest_rejects_files_changed_during_hashing(self) -> None:
        original_open = Path.open

        @contextmanager
        def changing_open(path, *args, **kwargs):
            with original_open(path, *args, **kwargs) as stream:
                yield stream
            with original_open(path, "ab") as stream:
                stream.write(b"changed")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "model").mkdir()
            (root / "model/weights.bin").write_bytes(b"weights")
            with (
                patch.object(Path, "open", changing_open),
                self.assertRaisesRegex(ValueError, "changed while it was hashed"),
            ):
                resource_manifest_digest(Path("model"), root)

    def test_resource_and_selection_identity_fixture(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "model/nested").mkdir(parents=True)
            (root / "model/weights.bin").write_bytes(b"x" * ((1 << 20) + 17))
            (root / "model/nested/日本語.json").write_bytes(b'{"x":1}\n')
            self.assertEqual(
                resource_manifest_digest(Path("model"), root),
                "86557e8354c78a05fcbdebf75b98245b325f795377cb73af83ac30fae68a5892",
            )
        self.assertEqual(
            selection_digest({"z": ["日本語", 1], "a": True}),
            "1a7fe031c4aa6b8e40a1c6bdea47c10a34a0a3ae5aa241449ec24ef2c0414f51",
        )

    def test_profile_json_and_selection_digest_are_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_resources(root)
            with patch.dict(sys.modules, {"stanza": fake_stanza()}):
                first = prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={"tokenize": "gsd"},
                )
                first_bytes = (root / PROFILE_FILE).read_bytes()
                second = prepare_model_bundle(
                    root,
                    language="fr",
                    probe_text="Chats",
                    packages={"tokenize": "gsd"},
                )
            self.assertEqual(first, second)
            self.assertEqual(first_bytes, (root / PROFILE_FILE).read_bytes())
            loaded = load_profile(root)
            for name, selection in loaded["selections"].items():
                self.assertEqual(
                    loaded["selection_digests"][name], selection_digest(selection)
                )
            self.assertEqual(json.loads(first_bytes), first)


if __name__ == "__main__":
    unittest.main()
