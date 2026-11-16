import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


@unittest.skipUnless(
    shutil.which("tsc"), "Run inside nix develop .#default for TypeScript checks"
)
class AssetBuildTest(unittest.TestCase):
    def test_generated_assets_are_checked_against_typescript_sources(self):
        root = Path(__file__).resolve().parent
        with tempfile.TemporaryDirectory() as directory:
            copy = Path(directory)
            shutil.copytree(root / "src", copy / "src")
            for path in root.iterdir():
                if path.suffix in (".js", ".mjs") or path.name in (
                    "tsconfig.json",
                    "build.py",
                ):
                    shutil.copy2(path, copy / path.name)
            command = [sys.executable, str(copy / "build.py"), "--check"]
            subprocess.run(command, check=True, capture_output=True)
            asset = copy / "app.js"
            asset.write_bytes(asset.read_bytes() + b"\n")
            stale = subprocess.run(command, check=False, capture_output=True, text=True)
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("app.js", stale.stderr)
            self.assertIn("stale", stale.stderr)


if __name__ == "__main__":
    unittest.main()
