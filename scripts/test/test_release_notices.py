import contextlib
import importlib.util
import io
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
