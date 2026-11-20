import json
import os
import subprocess
import sys
import unittest
from tempfile import TemporaryDirectory
from unittest.mock import patch

from lingrep import Tokenizer


class TokenizerCliTest(unittest.TestCase):
    def test_invalid_worker_counts_report_a_usage_error(self):
        for workers in ("-1", "0", str(2**100)):
            with self.subTest(workers=workers):
                result = subprocess.run(
                    [sys.executable, "-m", "lingrep", "tokenize", "--workers", workers, "猫"],
                    text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("--workers", result.stderr)
                self.assertNotIn("Traceback", result.stderr)

    def test_stdin_preserves_sentence_order_and_empty_lines(self):
        result = subprocess.run(
            [sys.executable, "-m", "lingrep", "tokenize", "--workers", "2"],
            input="猫が眠る。\n\n犬。", text=True, capture_output=True, check=True,
        )
        self.assertEqual([json.loads(line) for line in result.stdout.splitlines()],
                         [["猫", "が", "眠る", "。"], [], ["犬", "。"]])

    def test_missing_default_uses_download_base_but_explicit_paths_do_not(self):
        with TemporaryDirectory() as directory, patch.dict(os.environ, {
            "XDG_DATA_HOME": directory, "LINGREP_DICTIONARY_PATH": "",
            "LING_DOWNLOAD_BASE": "http://127.0.0.1:0",
        }):
            with self.assertRaisesRegex(OSError, "cannot download dictionary"):
                Tokenizer()
            with self.assertRaises(OSError) as failure:
                Tokenizer(os.path.join(directory, "missing.dic"))
            self.assertNotIn("cannot download dictionary", str(failure.exception))
