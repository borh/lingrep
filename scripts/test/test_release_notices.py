import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_notices", ROOT / "scripts/third-party-notices.py")
NOTICES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NOTICES)


class ReleaseNoticesTests(unittest.TestCase):
    def test_release_notices_include_complete_browser_asset_notices(self):
        metadata = {
            "packages": [{"id": "lingrep", "name": "lingrep", "version": "0.7.0"}],
            "workspace_members": ["lingrep"],
            "resolve": {"nodes": [{"id": "lingrep", "deps": []}]},
        }
        output = io.StringIO()
        with patch.object(NOTICES, "cargo_metadata", return_value=metadata), patch(
            "sys.argv", ["third-party-notices.py"]
        ), contextlib.redirect_stdout(output):
            self.assertEqual(NOTICES.main(), 0)
        browser_notices = (ROOT / "lingrep-web/assets/THIRD-PARTY-NOTICES.md").read_text()
        self.assertIn(browser_notices, output.getvalue())


    def run_notices(self, crates):
        """`main()`'s exit status, stdout and stderr over `crates`, each a
        (name, version, license, source, crate directory) shipped by `lingrep`."""
        metadata = {
            "packages": [{"id": "lingrep", "name": "lingrep", "version": "0.7.0"}]
            + [
                {
                    "id": name,
                    "name": name,
                    "version": version,
                    "license": license_expression,
                    "source": source,
                    "manifest_path": str(directory / "Cargo.toml"),
                }
                for name, version, license_expression, source, directory in crates
            ],
            "workspace_members": ["lingrep"],
            "resolve": {
                "nodes": [
                    {
                        "id": "lingrep",
                        "deps": [
                            {"pkg": crate[0], "dep_kinds": [{"kind": None}]}
                            for crate in crates
                        ],
                    }
                ]
            },
        }
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch.object(NOTICES, "cargo_metadata", return_value=metadata), patch(
            "sys.argv", ["third-party-notices.py"]
        ), contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = NOTICES.main()
        return status, stdout.getvalue(), stderr.getvalue()

    def test_a_crate_whose_license_requires_its_text_fails_without_one(self):
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        for license_expression in ["MIT", "BSD-3-Clause", "Apache-2.0", "MIT OR Apache-2.0"]:
            with self.subTest(license_expression), tempfile.TemporaryDirectory() as directory:
                crate = Path(directory) / "textless-1.0.0"
                crate.mkdir()
                status, _, stderr = self.run_notices(
                    [("textless", "1.0.0", license_expression, registry, crate)]
                )
                self.assertEqual(status, 1)
                self.assertIn("textless 1.0.0", stderr)

    def test_license_texts_are_found_beside_the_crate_in_licenses_and_at_a_git_checkout_root(self):
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        git = "git+https://example.invalid/repo.git#0000000"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            licenses = root / "licenses-dir-1.0.0"
            (licenses / "LICENSES").mkdir(parents=True)
            (licenses / "LICENSES" / "MIT.txt").write_text("licenses-directory text\n")
            checkout = root / "checkout"
            (checkout / ".git").mkdir(parents=True)
            (checkout / "LICENSE").write_text("checkout-root text\n")
            member = checkout / "member"
            member.mkdir()
            status, stdout, stderr = self.run_notices(
                [
                    ("licenses-dir", "1.0.0", "MIT", registry, licenses),
                    ("git-member", "1.0.0", "Apache-2.0", git, member),
                ]
            )
        self.assertEqual(status, 0, stderr)
        self.assertIn("licenses-directory text", stdout)
        self.assertIn("checkout-root text", stdout)

    def test_alloc_stdlib_carries_its_bsd_3_clause_text_from_the_repository(self):
        registry = "registry+https://github.com/rust-lang/crates.io-index"
        with tempfile.TemporaryDirectory() as directory:
            crate = Path(directory) / "alloc-stdlib-0.3.0"
            crate.mkdir()
            status, stdout, stderr = self.run_notices(
                [("alloc-stdlib", "0.3.0", "BSD-3-Clause", registry, crate)]
            )
        self.assertEqual(status, 0, stderr)
        self.assertIn("Copyright (c) 2016 Dropbox, Inc.", stdout)
        self.assertIn("Applies to: alloc-stdlib 0.3.0", stdout)


if __name__ == "__main__":
    unittest.main()
