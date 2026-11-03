import importlib.util
import json
from pathlib import Path
import tempfile
import shutil
from unittest import mock
import unittest

SPEC = importlib.util.spec_from_file_location(
    "pressure", Path(__file__).parents[1] / "bench-corpus-pressure.py"
)
pressure = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pressure)


class PressureTests(unittest.TestCase):
    def test_validate_controls_rejects_mismatched_limits(self):
        with self.assertRaisesRegex(ValueError, "memory.max"):
            pressure.validate_controls({"memory.max": "max", "memory.swap.max": "0"}, 2048)
        with self.assertRaisesRegex(ValueError, "swap"):
            pressure.validate_controls({"memory.max": "2048", "memory.swap.max": "max"}, 2048)

    def test_throttling_limit_invalidates_unconstrained_control(self):
        with self.assertRaisesRegex(ValueError, "memory.high"):
            pressure.validate_controls({"memory.max": "max", "memory.swap.max": "0", "memory.high": "2048"}, "max")

    def test_ancestor_limit_invalidates_unconstrained_control(self):
        with self.assertRaisesRegex(ValueError, "ancestor"):
            pressure.validate_ancestors(["max", "4096"], "max")
        pressure.validate_ancestors(["max", "4096"], 2048)

    def test_failed_build_or_oom_does_not_qualify(self):
        row = {"exit": 0, "memory_events": {"oom": 0, "oom_kill": 0}, "swap_peak": 0}
        pressure.validate_completion(row)
        for replacement in ({"exit": 1}, {"memory_events": {"oom": 1}}, {"swap_peak": 1}):
            with self.assertRaises(ValueError):
                pressure.validate_completion({**row, **replacement})

    def test_counters_raise_when_the_accounting_file_is_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                pressure.counters(Path(directory), "memory.events")

    def test_snapshot_ignores_only_build_timestamp(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = {"build_timestamp_unix_seconds": 1, "identity": "a"}
            (root / "manifest.json").write_text(json.dumps(manifest))
            (root / "tokens").write_bytes(b"payload")
            first = pressure.artifact_snapshot(root)
            manifest["build_timestamp_unix_seconds"] = 2
            (root / "manifest.json").write_text(json.dumps(manifest))
            self.assertEqual(first, pressure.artifact_snapshot(root))
            (root / "tokens").write_bytes(b"changed")
            self.assertNotEqual(first, pressure.artifact_snapshot(root))

    def test_failed_private_copy_is_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(FileNotFoundError):
                with pressure.private_run(root, [(root / "missing", "input")]):
                    self.fail("missing input accepted")
            self.assertEqual(list(root.iterdir()), [])

    def test_private_run_is_removed_after_a_build_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.write_bytes(b"input")
            with mock.patch.object(pressure.subprocess, "run", side_effect=lambda argv, **_: shutil.copyfile(argv[-2], argv[-1])):
                with self.assertRaisesRegex(RuntimeError, "build failed"):
                    with pressure.private_run(root, [(source, "input")]) as run:
                        self.assertNotEqual(source.stat().st_ino, (run / "input").stat().st_ino)
                        self.assertEqual(source.read_bytes(), (run / "input").read_bytes())
                        raise RuntimeError("build failed")
            self.assertEqual(list(root.iterdir()), [source])

    def test_pressure_verdict_requires_interval_and_all_runs(self):
        estimate = {"ratio": 1.02, "low": 1.0, "high": 1.09, "blocks": 3}
        self.assertEqual(pressure.verdict(estimate, 1.10, 12, 12), "qualified")
        self.assertEqual(pressure.verdict({**estimate, "high": 1.11}, 1.10, 12, 12), "inconclusive")
        self.assertEqual(pressure.verdict({**estimate, "ratio": 1.12, "low": 1.11, "high": 1.13}, 1.10, 12, 12), "regressed")
        with self.assertRaises(ValueError):
            pressure.verdict(estimate, 1.10, 8, 12)

    def test_unverified_service_stop_preserves_its_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(pressure.ServiceStopError):
                with pressure.private_run(root, []):
                    raise pressure.ServiceStopError("stop failed")
            self.assertEqual(len(list(root.iterdir())), 1)
