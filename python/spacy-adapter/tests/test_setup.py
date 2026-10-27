from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from lingrep_adapter_protocol.resources import validate_runtime
from lingrep_adapter_protocol.setup import (
    _packages,
    _runtime_python,
    _runtime_receipt,
    _stage_code,
    setup,
)


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

    def test_package_selection_and_runtime_changes_are_rejected(self):
        for values in (["tokenize"], ["pos="], ["pos=a", "pos=b"]):
            with self.assertRaises(ValueError):
                _packages(values)
        self.assertEqual(_packages(["tokenize=gsd"]), {"tokenize": "gsd"})
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            validate_runtime(root)
            (root / "lingrep-runtime.json").write_text(
                json.dumps({"python_version": "other", "packages": []})
            )
            with self.assertRaisesRegex(ValueError, "runtime changed"):
                validate_runtime(root)


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
                (model / "lingrep-runtime.json").write_text(json.dumps(receipt))
                self.assertEqual(_runtime_python(model).resolve(), python.resolve())
                receipts.append(receipt)
            self.assertEqual(receipts[0], receipts[1])
            (model / "lingrep-runtime.json").write_text(
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
