from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from lingrep_adapter_protocol.resources import (
    _manifest_digest,
    adapter_semantics_digest,
    implementation_manifest_digest,
    resource_manifest_digest,
    validate_runtime,
)
from lingrep_adapter_protocol.setup import (
    _packages,
    _runtime_python,
    _runtime_receipt,
    _stage_code,
    _write_runtime,
    setup,
)

from lingrep_spacy_adapter.adapter import ADAPTER_SEMANTICS_REVISION

REPOSITORY = Path(__file__).resolve().parents[3]

LINUX_RECEIPT = {
    "python": "../runtime/bin/python",
    "python_release": "cpython-3.14.0",
    "python_version": "3.14.0 (main, Oct 14 2025, 17:48:02) [Clang 20.1.4 ]",
    "packages": [
        ["SudachiDict-core", "20260723"],
        ["SudachiPy", "0.6.11"],
        ["ginza", "5.3.1"],
        ["ja_ginza", "5.3.0"],
        ["lingrep-adapter-protocol", "0.8.0"],
        ["lingrep-spacy-adapter", "0.8.0"],
        ["spacy", "3.8.18"],
        ["thinc", "8.3.14"],
    ],
}
DARWIN_RECEIPT = {
    **LINUX_RECEIPT,
    "python_version": "3.14.0 (main, Oct 14 2025, 17:47:42) [Clang 20.1.4 ]",
    "packages": sorted([*LINUX_RECEIPT["packages"], ["thinc_apple_ops", "1.0.3"]]),
}


class SetupTests(unittest.TestCase):
    def test_staged_code_contains_both_exact_project_closures(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _stage_code(root, "spacy")
            for path in (
                "spacy-adapter/bin/lingrep-spacy-adapter",
                "spacy-adapter/pyproject.toml",
                "spacy-adapter/uv.lock",
                "spacy-adapter/src/lingrep_spacy_adapter/adapter.py",
                "adapter-protocol/pyproject.toml",
                "adapter-protocol/src/lingrep_adapter_protocol/__init__.py",
                "adapter-protocol/src/lingrep_adapter_protocol/setup.py",
            ):
                self.assertTrue((root / path).is_file(), path)
            self.assertFalse(list(root.rglob("__pycache__")))

    def test_setup_errors_do_not_publish_a_configuration(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "analyzer"
            status = setup(
                "spacy",
                [
                    "--model",
                    "unavailable_lingrep_test_model",
                    "--probe-text",
                    "Cats run.",
                    "--output",
                    str(output),
                    "--offline",
                ],
            )
            self.assertEqual(status, 1)
            self.assertFalse(output.exists())
            self.assertEqual(list(Path(temporary).iterdir()), [])
            status = setup(
                "stanza",
                [
                    "--language",
                    "fr",
                    "--package",
                    "tokenize=gsd",
                    "--probe-text",
                    "Bonjour.",
                    "--output",
                    str(output),
                    "--offline",
                ],
            )
            self.assertEqual(status, 1)
            self.assertFalse(output.exists())
            output.mkdir()
            status = setup(
                "spacy",
                [
                    "--model",
                    "ja_ginza",
                    "--probe-text",
                    "猫。",
                    "--output",
                    str(output),
                ],
            )
            self.assertEqual(status, 1)
            self.assertEqual(list(output.iterdir()), [])

    def test_spacy_setup_uses_the_default_backend_unless_one_is_given(self):
        initialized = []

        def stop_after_selection(backend):
            initialized.append(backend)
            raise RuntimeError("stop after selection")

        for arguments, expected in (([], "mps"), (["--backend", "cpu"], "cpu")):
            initialized.clear()
            with (
                self.subTest(arguments=arguments),
                tempfile.TemporaryDirectory() as temporary,
                patch(
                    "lingrep_spacy_adapter.adapter.default_spacy_backend",
                    return_value="mps",
                ),
                patch(
                    "lingrep_spacy_adapter.adapter.initialize_spacy_backend",
                    side_effect=stop_after_selection,
                ),
            ):
                status = setup(
                    "spacy",
                    [
                        "--model",
                        "ja_ginza",
                        "--probe-text",
                        "猫。",
                        "--output",
                        str(Path(temporary) / "analyzer"),
                        *arguments,
                    ],
                )
                self.assertEqual(status, 1)
                self.assertEqual(initialized, [expected])

    def test_stanza_setup_rejects_the_mps_backend(self):
        with self.assertRaises(SystemExit):
            setup(
                "stanza",
                [
                    "--language",
                    "ja",
                    "--package",
                    "tokenize=gsd",
                    "--probe-text",
                    "猫。",
                    "--output",
                    "unused",
                    "--backend",
                    "mps",
                ],
            )

    def test_package_selection_and_runtime_changes_are_rejected(self):
        for values in (["tokenize"], ["pos="], ["pos=a", "pos=b"]):
            with self.assertRaises(ValueError):
                _packages(values)
        self.assertEqual(_packages(["tokenize=gsd"]), {"tokenize": "gsd"})
        with tempfile.TemporaryDirectory() as temporary:
            model = Path(temporary) / "model"
            validate_runtime(model)
            (Path(temporary) / "lingrep-runtime.json").write_text(
                json.dumps({"python_version": "other", "packages": []})
            )
            with self.assertRaisesRegex(ValueError, "runtime changed"):
                validate_runtime(model)


class RuntimeLocationTests(unittest.TestCase):
    def test_managed_runtime_receipts_do_not_include_the_output_location(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            receipts = []
            for name in ("first", "second"):
                root = base / name
                python = root / "runtime" / "bin" / "python"
                python.parent.mkdir(parents=True)
                python.touch()
                model = root / "bundle" / "model"
                model.mkdir(parents=True)
                with patch("sys.executable", str(python)):
                    receipt = _runtime_receipt(model)
                _write_runtime(model, receipt)
                self.assertEqual(_runtime_python(model).resolve(), python.resolve())
                receipts.append(receipt)
            self.assertEqual(receipts[0], receipts[1])
            (model.parent / "lingrep-runtime.json").write_text(
                json.dumps({"python": str(python)})
            )
            self.assertEqual(_runtime_python(model), python)

    def test_external_runtime_receipt_retains_its_absolute_reference(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            python = root / "external" / "bin" / "python"
            with patch("sys.executable", str(python)):
                receipt = _runtime_receipt(root / "project" / "analyzer" / "model")
            self.assertEqual(receipt["python"], str(python))


def _with_package(receipt: dict, name: str, version: str) -> dict:
    packages = [package for package in receipt["packages"] if package[0] != name]
    return {**receipt, "packages": sorted([*packages, [name, version]])}


class RuntimeIdentityTests(unittest.TestCase):
    def _digest(self, receipt: dict, weights: bytes = b"weights") -> str:
        with tempfile.TemporaryDirectory() as temporary:
            bundle = Path(temporary)
            model = bundle / "model"
            model.mkdir()
            (model / "model.bin").write_bytes(weights)
            _write_runtime(model, receipt)
            return resource_manifest_digest(Path("model"), bundle, provider="spaCy")

    def test_platform_build_details_do_not_change_the_resource_digest(self):
        self.assertEqual(self._digest(LINUX_RECEIPT), self._digest(DARWIN_RECEIPT))

    def test_lingrep_adapter_versions_do_not_change_the_resource_digest(self):
        bumped = _with_package(LINUX_RECEIPT, "lingrep-spacy-adapter", "0.8.1")
        bumped = _with_package(bumped, "lingrep-adapter-protocol", "0.8.1")
        self.assertEqual(self._digest(LINUX_RECEIPT), self._digest(bumped))

    def test_output_affecting_runtime_facts_change_the_resource_digest(self):
        baseline = self._digest(LINUX_RECEIPT)
        for name, receipt in (
            ("spacy", _with_package(LINUX_RECEIPT, "spacy", "3.8.19")),
            ("ginza", _with_package(LINUX_RECEIPT, "ginza", "5.3.2")),
            ("ja_ginza", _with_package(LINUX_RECEIPT, "ja_ginza", "5.4.0")),
            ("SudachiPy", _with_package(LINUX_RECEIPT, "SudachiPy", "0.6.12")),
            (
                "SudachiDict-core",
                _with_package(LINUX_RECEIPT, "SudachiDict-core", "20261015"),
            ),
            ("thinc", _with_package(LINUX_RECEIPT, "thinc", "8.3.15")),
            ("transformers", _with_package(LINUX_RECEIPT, "transformers", "4.57.1")),
            ("python", {**LINUX_RECEIPT, "python_release": "cpython-3.14.1"}),
        ):
            with self.subTest(name):
                self.assertNotEqual(self._digest(receipt), baseline)
        with self.subTest("weights"):
            self.assertNotEqual(self._digest(LINUX_RECEIPT, b"other"), baseline)


class AdapterSemanticsTests(unittest.TestCase):
    def test_revision_digest_matches_the_rust_pin(self):
        # SPACY_SEMANTICS_DIGEST in lingrep-core/tests/external_process_contract.rs
        self.assertEqual(
            adapter_semantics_digest("lingrep.spacy-adapter/v2"),
            "fb03302d08b4d7742dcaacfc221ec7c4a38afd1dbf88a4e7c768d7b4a3febe48",
        )

    @unittest.skipUnless(
        (REPOSITORY / "lingrep-core").is_dir(), "requires the lingrep repository"
    )
    def test_revision_matches_its_rust_mirror(self):
        source = (
            REPOSITORY / "lingrep-core/src/execution/external_process.rs"
        ).read_text()
        self.assertIn(
            "const SPACY_ADAPTER_SEMANTICS_REVISION: &str = "
            f'"{ADAPTER_SEMANTICS_REVISION}";',
            source,
        )

    def test_implementation_digest_uses_the_rust_manifest_framing(self):
        # The python_bundle_fixture("spacy") tree in external_process_contract.rs.
        files = {
            "adapter/bin/lingrep-spacy-adapter": "entrypoint",
            "adapter/pyproject.toml": "project",
            "adapter/uv.lock": "lock",
            "adapter/src/lingrep_spacy_adapter/__init__.py": "package",
            "adapter/src/lingrep_spacy_adapter/__main__.py": "main",
            "adapter/src/lingrep_spacy_adapter/adapter.py": "adapter",
            "adapter/src/lingrep_spacy_adapter/bundle.py": "bundle",
            "adapter-protocol/pyproject.toml": "protocol project",
            "adapter-protocol/src/lingrep_adapter_protocol/__init__.py": "protocol",
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative, text in files.items():
                (root / relative).parent.mkdir(parents=True, exist_ok=True)
                (root / relative).write_text(text)
            digest = _manifest_digest(
                b"lingrep.adapter-implementation/v1",
                root,
                [
                    Path("adapter/bin"),
                    Path("adapter/src"),
                    Path("adapter/uv.lock"),
                    Path("adapter/pyproject.toml"),
                    Path("adapter-protocol/pyproject.toml"),
                    Path("adapter-protocol/src"),
                ],
                "spaCy",
            )
        # SPACY_FIXTURE_IMPLEMENTATION_MANIFEST_DIGEST in the Rust contract test.
        self.assertEqual(
            digest, "d40fd67b4dac95a6e8901c2dbd0410bf3d7860bcd54c61c94fcb10111c96871a"
        )

    def test_staged_code_edits_change_the_implementation_manifest_digest(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _stage_code(root, "spacy")
            before = implementation_manifest_digest(root, "spacy")
            adapter = root / "spacy-adapter/src/lingrep_spacy_adapter/adapter.py"
            adapter.write_text(adapter.read_text() + "# comment\n")
            self.assertNotEqual(implementation_manifest_digest(root, "spacy"), before)
