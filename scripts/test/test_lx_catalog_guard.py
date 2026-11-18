import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

# Push hooks inherit the repository's Git environment (githooks(5), DESCRIPTION).
for name in subprocess.run(["git", "rev-parse", "--local-env-vars"], check=True,
                           capture_output=True, text=True).stdout.split():
    os.environ.pop(name, None)


GUARD = pathlib.Path(__file__).resolve().parents[1] / "check-lx-catalogs.sh"


def run_guard(files, delete_after_tracking=()):
    """Run the guard in a repository tracking `files`, then remove `delete_after_tracking`
    from the working tree so that reading them fails."""
    with tempfile.TemporaryDirectory() as temporary:
        root = pathlib.Path(temporary)
        (root / "scripts").mkdir()
        shutil.copyfile(GUARD, root / "scripts" / GUARD.name)
        for path, text in files.items():
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(text)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        for path in delete_after_tracking:
            (root / path).unlink()
        return subprocess.run(
            ["bash", str(root / "scripts" / GUARD.name)], capture_output=True, text=True
        )


class LxCatalogGuard(unittest.TestCase):
    def test_passes_catalogs_without_regex_operands_after_equals(self):
        for label, text in {
            "literal": "noun := [upos=NOUN].\n",
            "regex after tilde": "noun := [upos~/NOUN/].\n",
            "exempt fixture": "x := [upos=/NOUN/]. // legacy-lx-syntax-ok\n",
        }.items():
            with self.subTest(label=label):
                result = run_guard({"patterns/a.lx": text})
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_reports_a_regex_operand_after_equals(self):
        result = run_guard({"patterns/a.lx": "noun := [upos=/NOUN/].\n"})
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("patterns/a.lx:1:", result.stderr)

    def test_a_grep_error_fails_instead_of_passing(self):
        result = run_guard(
            {"patterns/a.lx": "noun := [upos=NOUN].\n", "src/gone.rs": "\n"},
            delete_after_tracking=["src/gone.rs"],
        )
        self.assertGreaterEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("grep failed with exit status", result.stderr)


if __name__ == "__main__":
    unittest.main()
