import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


CHECKER = pathlib.Path(__file__).resolve().parents[1] / "check-doc-test-references.py"


class TestReferenceGuards(unittest.TestCase):
    def test_live_references_match_cargo_targets_and_existing_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            (root / "scripts").mkdir()
            shutil.copyfile(CHECKER, root / "scripts" / CHECKER.name)
            (root / "Cargo.toml").write_text(
                '[workspace]\nmembers = ["lingrep-a", "lingrep-b"]\nresolver = "2"\n'
            )
            for package in ("lingrep-a", "lingrep-b"):
                crate = root / package
                (crate / "src").mkdir(parents=True)
                (crate / "src/lib.rs").touch()
                (crate / "Cargo.toml").write_text(
                    f'[package]\nname = "{package}"\nversion = "0.0.0"\n'
                    'edition = "2024"\n[[test]]\nname = "custom"\npath = "contract.rs"\n'
                )
                (crate / "contract.rs").touch()
            (root / "lingrep-b/tests").mkdir()
            (root / "lingrep-b/tests/only_b.rs").touch()
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            guide = root / "Live guide.md"
            guide.touch()
            historical = root / "CHANGELOG.md"
            historical.write_text("cargo test --test removed\n")
            subprocess.run(["git", "-C", str(root), "add", "."], check=True)

            cases = [
                ("custom Cargo target", "cargo test -p lingrep-a --test custom", True),
                ("workspace target", "cargo test --workspace --test only_b", True),
                ("wrong package", "cargo test -p lingrep-a --test only_b", False),
                ("continued command", "cargo test -p lingrep-a \\\n  --test removed", False),
                ("equals options", "cargo test --package=lingrep-a --test=only_b", False),
                ("equals valid target", "cargo test --package=lingrep-b --test=only_b", True),
                ("harness arguments", "cargo test -p lingrep-a --test custom -- --test=removed", True),
                ("existing path", "lingrep-b/tests/only_b.rs", True),
                ("missing path", "lingrep-b/tests/deleted.rs", False),
                ("historical banner", "Historical record\ncargo test --test removed", True),
            ]
            for label, text, valid in cases:
                with self.subTest(label=label):
                    guide.write_text(text)
                    result = subprocess.run(
                        [sys.executable, str(root / "scripts" / CHECKER.name)],
                        cwd=root,
                        capture_output=True,
                        text=True,
                    )
                    self.assertEqual(result.returncode, 0 if valid else 1, result.stdout + result.stderr)
                    if not valid:
                        self.assertIn("Live guide.md", result.stdout)



if __name__ == "__main__":
    unittest.main()
