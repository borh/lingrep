import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "wo_predicate_export.py"
SPEC = importlib.util.spec_from_file_location("wo_predicate_export", SCRIPT)
EXPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPORT)


class ExportTests(unittest.TestCase):
    def test_both_exports_replace_tabs_with_single_spaces(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def blob(data):
                digest = hashlib.sha256(data).hexdigest()
                path = EXPORT.blob_path(root / "kura/objects", digest)
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                return digest

            text = "猫\tがいる。空\u3000\t白。"
            run = root / "run.json"
            run.write_text(
                json.dumps(
                    {
                        "catalog_csv_hash": blob(b"work_id,title\n"),
                        "works": {"1_test": {"plaintext": blob(text.encode())}},
                    }
                )
            )
            output = root / "out"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--kernel-root",
                    str(root),
                    "--run",
                    str(run),
                    "--out-dir",
                    str(output),
                ],
                check=True,
                capture_output=True,
            )
            sentences = json.loads((output / "aozora.jsonl").read_text())["sentences"]
            self.assertEqual(sentences, ["猫 がいる。", "空\u3000 白。"])
            lines = [
                line
                for line in (output / "aozora-sentences.txt").read_text().splitlines()
                if not line.startswith("#")
            ]
            self.assertEqual(lines, sentences)
            self.assertEqual(sum(map(len, sentences)), len(text))
            manifest = json.loads((output / "export-manifest.json").read_text())
            self.assertIn("tabs to spaces", manifest["sentence_rule"])
