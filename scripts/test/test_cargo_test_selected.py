import os
import select
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


RUNNER = Path(__file__).resolve().parents[1] / "cargo-test-selected.py"


class SelectedCargoTests(unittest.TestCase):
    def test_selected_tests_must_execute_and_failures_preserve_the_cargo_status(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "src").mkdir()
            (root / "Cargo.toml").write_text(
                '[package]\nname="selection-fixture"\nversion="0.0.0"\nedition="2024"\n'
            )
            (root / "src/lib.rs").write_text(
                '#[test] fn selected() {}\n'
                '#[test] #[ignore] fn opt_in() {}\n'
                '#[test] fn failing() { panic!("fixture failure"); }\n'
            )
            (root / "tests").mkdir()
            (root / "tests/empty.rs").touch()
            for label, arguments, expected in [
                ("substring", ["select"], 0),
                ("exact", ["--", "selected", "--exact"], 0),
                ("colored summary", ["--", "selected", "--exact", "--color", "always"], 0),
                ("empty sibling target", ["--tests", "selected"], 0),
                ("stale exact filter", ["--", "deleted", "--exact"], 1),
                ("ignored without opt in", ["opt_in"], 1),
                ("ignored with opt in", ["--", "opt_in", "--ignored", "--exact"], 0),
                ("failed test", ["failing"], 101),
                ("listing only", ["--", "--list"], 1),
            ]:
                with self.subTest(label=label):
                    result = subprocess.run(
                        [sys.executable, str(RUNNER), "--lib", "--quiet", *arguments],
                        cwd=root,
                        env=os.environ | {"CARGO_TARGET_DIR": str(root / "target")},
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    if expected == 1:
                        self.assertIn("no tests executed", result.stdout + result.stderr)
                    elif expected == 101:
                        self.assertIn("fixture failure", result.stdout + result.stderr)
                        self.assertNotIn("no tests executed", result.stdout + result.stderr)

    def test_flushed_progress_is_visible_before_the_child_finishes(self):
        with tempfile.TemporaryDirectory() as temporary:
            cargo = Path(temporary) / "cargo"
            cargo.write_text(
                f"#!{sys.executable}\n"
                "import sys\n"
                "print('started', end='', flush=True)\n"
                "sys.stdin.readline()\n"
                "print('\\ntest result: ok. 1 passed; 0 failed; 0 ignored;')\n"
            )
            cargo.chmod(0o755)
            with subprocess.Popen(
                [sys.executable, str(RUNNER), "selected"],
                env=os.environ | {"PATH": temporary + os.pathsep + os.environ["PATH"]},
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            ) as process:
                try:
                    readable, _, _ = select.select([process.stdout], [], [], 5)
                    self.assertTrue(readable, "flushed progress was withheld")
                    self.assertEqual(process.stdout.read(7), b"started")
                finally:
                    output, _ = process.communicate(input=b"\n", timeout=5)
                self.assertEqual(process.returncode, 0, output)


if __name__ == "__main__":
    unittest.main()
