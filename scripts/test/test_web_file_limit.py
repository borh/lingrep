import json
import os
import resource
import subprocess
import sys
import unittest
from pathlib import Path

RUNNER = Path(__file__).resolve().parents[1] / "run-cargo-test.py"
READ_LIMIT = "import json, resource; print(json.dumps(resource.getrlimit(resource.RLIMIT_NOFILE)))"
READ_BOTH = f"""
import subprocess, sys
{READ_LIMIT}
sys.stdout.flush()
subprocess.run([sys.executable, "-c", {READ_LIMIT!r}], check=True)
"""


class WebFileLimit(unittest.TestCase):
    def limits(self, package, inherited):
        result = subprocess.run(
            [sys.executable, str(RUNNER), sys.executable, "-c", READ_BOTH],
            env={**os.environ, "CARGO_PKG_NAME": package},
            preexec_fn=lambda: resource.setrlimit(resource.RLIMIT_NOFILE, inherited),
            capture_output=True,
            text=True,
            check=True,
        )
        return [tuple(json.loads(line)) for line in result.stdout.splitlines()]

    def test_web_harness_and_child_inherit_the_cap(self):
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        expected = tuple(
            256 if limit == resource.RLIM_INFINITY else min(limit, 256)
            for limit in (soft, hard)
        )
        self.assertEqual(self.limits("lingrep-web", (soft, hard)), [expected, expected])

    def test_lower_limits_are_preserved_and_other_packages_keep_their_limits(self):
        soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        lower = tuple(
            64 if limit == resource.RLIM_INFINITY else min(limit, 64)
            for limit in (soft, hard)
        )
        self.assertEqual(self.limits("lingrep-web", lower), [lower, lower])
        capped_hard = 256 if hard == resource.RLIM_INFINITY else min(hard, 256)
        mixed = (lower[0], capped_hard)
        self.assertEqual(self.limits("lingrep-web", (lower[0], hard)), [mixed, mixed])
        self.assertEqual(
            self.limits("lingrep-core", (soft, hard)), [(soft, hard), (soft, hard)]
        )
