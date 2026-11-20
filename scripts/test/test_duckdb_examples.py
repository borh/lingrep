"""Exercise the DuckDB examples with evidence from the CLI."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LING = os.environ.get("LING", str(ROOT / "target/debug/ling"))
SQL = ROOT / "examples/dependency-complexity.sql"


class DependencyComplexity(unittest.TestCase):
    def test_sentence_profiles(self):
        corpus = (ROOT / "lingrep/tests/fixtures/psycholinguistics.conllu").read_text()
        corpus += "\n1\t猫\t猫\tNOUN\t_\t_\t0\troot\t_\tBunsetuBILabel=B\n\n"
        # A separate root cannot make a disconnected cycle reachable.
        corpus += (
            "1\ta\ta\tNOUN\t_\t_\t0\troot\t_\t_\n"
            "2\tb\tb\tNOUN\t_\t_\t3\tdep\t_\t_\n"
            "3\tc\tc\tNOUN\t_\t_\t2\tdep\t_\t_\n"
        )
        with tempfile.TemporaryDirectory() as directory:
            events = Path(directory) / "sentences.jsonl"
            with events.open("w") as output:
                subprocess.run(
                    [
                        LING,
                        "--json",
                        "matches",
                        "--evidence",
                        "--pattern",
                        "^ []+ $.",
                        "--input-format",
                        "conllu",
                        "-",
                    ],
                    input=corpus,
                    text=True,
                    stdout=output,
                    check=True,
                )
            command = [
                "duckdb",
                "-batch",
                "-bail",
                "-json",
                "-cmd",
                f"SET VARIABLE matches = '{events}'",
            ]

            def run():
                return subprocess.run(
                    command,
                    check=False,
                    input=SQL.read_text(),
                    text=True,
                    capture_output=True,
                )

            result = run()
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = {(r["sentence"], r["unit"]): r for r in json.loads(result.stdout)}

            native = subprocess.run(
                [
                    LING,
                    "--json",
                    "matches",
                    "--patterns",
                    str(ROOT / "examples/occurrence-measures.lx"),
                    "--input-format",
                    "conllu",
                    "-",
                ],
                input=corpus,
                capture_output=True,
                text=True,
                check=True,
            )
            profiles = {}
            for event in map(json.loads, native.stdout.splitlines()):
                record = event["data"]
                if event["type"] == "match" and record["pattern"].endswith("_profile"):
                    key = (
                        record["corpus_sentence_ordinal"],
                        record["pattern"].removesuffix("_profile"),
                    )
                    self.assertNotIn(key, profiles)
                    profiles[key] = {
                        b["name"]: (b["value"], b["status"])
                        for b in record["graph_bindings"]
                    }
            self.assertEqual(profiles.keys(), rows.keys())
            for key, row in rows.items():
                self.assertEqual(
                    profiles[key],
                    {
                        metric: (
                            row[metric],
                            "empty"
                            if row["status"] == "ok" and row[metric] is None
                            else row["status"],
                        )
                        for metric in (
                            "mean_distance",
                            "mean_depth",
                            "height",
                            "peak_open_links",
                        )
                    },
                )

            def measures(sentence, unit):
                row = rows[sentence, unit]
                return tuple(
                    row[k]
                    for k in (
                        "status",
                        "mean_distance",
                        "mean_depth",
                        "height",
                        "peak_open_links",
                    )
                )

            self.assertEqual(measures(0, "word"), ("ok", 2, 1.2, 2, 3))
            self.assertEqual(measures(0, "bunsetsu"), ("ok", 1.5, 2 / 3, 1, 2))
            for unit in ("word", "bunsetsu"):
                self.assertEqual(measures(6, unit), ("ok", None, 0, 0, 0))
            self.assertEqual(
                measures(4, "bunsetsu"), ("unavailable", None, None, None, None)
            )
            self.assertEqual(
                measures(7, "word"), ("invalid_tree", None, None, None, None)
            )
            records = [json.loads(line) for line in events.read_text().splitlines()]
            first = next(
                record["data"] for record in records if record["type"] == "match"
            )
            spans = first["evidence"]["spans"]
            for span in spans:
                if span["governor"] is not None and span["governor"] >= 1:
                    span["governor"] += 1
            spans.insert(1, {"kind": "entity", "start": 0, "end": 1})

            def save(records):
                events.write_text("".join(json.dumps(r) + "\n" for r in records))

            save(records)
            with_entity = run()
            self.assertEqual(with_entity.returncode, 0, with_entity.stderr)
            self.assertEqual(json.loads(with_entity.stdout), json.loads(result.stdout))
            spans[0]["governor"] = None
            save(records)
            incomplete = run()
            self.assertEqual(incomplete.returncode, 0, incomplete.stderr)
            first_word, first_bunsetsu = json.loads(incomplete.stdout)[:2]
            self.assertEqual(first_word, rows[0, "word"])
            self.assertEqual(first_bunsetsu["status"], "invalid_tree")
            self.assertIsNone(first_bunsetsu["peak_open_links"])
            save(records[:-1])
            self.assertNotEqual(run().returncode, 0)


class Psycholinguistics(unittest.TestCase):
    def test_documented_run_reports_the_neighbors_of_a_two_kanji_word(self):
        with tempfile.TemporaryDirectory() as directory:
            events = Path(directory) / "reading.jsonl"
            with events.open("w") as output:
                subprocess.run(
                    [
                        LING,
                        "--json",
                        "matches",
                        "--evidence",
                        "--input-format",
                        "conllu",
                        "--patterns",
                        str(ROOT / "patterns/psycholinguistics.lx"),
                        str(ROOT / "lingrep/tests/fixtures/psycholinguistics.conllu"),
                    ],
                    stdout=output,
                    check=True,
                )
            result = subprocess.run(
                [
                    "duckdb",
                    "-batch",
                    "-bail",
                    "-json",
                    "-cmd",
                    f"SET VARIABLE matches = '{events}'",
                ],
                input=(ROOT / "examples/psycholinguistics.sql").read_text(),
                text=True,
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn(
                '{"surface":"学校","frequency":1,"same_first_neighbors":1,'
                '"same_last_neighbors":1,"neighborhood_size":2}',
                result.stdout,
            )


if __name__ == "__main__":
    unittest.main()
