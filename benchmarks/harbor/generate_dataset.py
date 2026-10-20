#!/usr/bin/env python3
"""Generate paired Harbor tasks from one linguistic case catalog."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent
DEFAULT_CATALOG = ROOT / "catalogs" / "smoke.json"
CAPABILITIES = {
    "feature_lookup",
    "segmentation_alignment",
    "sequence_composition",
    "context_disambiguation",
    "negative_evidence",
    "query_authoring",
}
ARMS = {
    "lingrep": ("lingrep-linguistic-eval:latest", "ling"),
    "mecab": ("mecab-linguistic-eval:latest", "mecab"),
}
DISCOVERY_GUIDE = "analyzer-guide.md"
DEFAULT_AGENT_TIMEOUT_SEC = 3600


def validate_catalog(cases: object) -> list[dict]:
    if not isinstance(cases, list):
        raise TypeError("catalog root must be a list")
    case_ids: set[str] = set()
    for case in cases:
        required = {
            "id",
            "title",
            "capability",
            "phenomenon",
            "labels",
            "sentences",
            "targets",
        }
        missing = required - set(case)
        if missing:
            raise ValueError(f"case is missing fields: {sorted(missing)}")
        if not isinstance(case["id"], str) or not re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9._-]*", case["id"]
        ):
            raise ValueError(
                f"case id is not a safe task path component: {case['id']!r}"
            )
        if case["id"] in case_ids:
            raise ValueError(f"duplicate case id: {case['id']}")
        case_ids.add(case["id"])
        if case["capability"] not in CAPABILITIES:
            raise ValueError(
                f"unknown capability for {case['id']}: {case['capability']}"
            )
        labels = case["labels"]
        if (
            not isinstance(labels, list)
            or not labels
            or not all(isinstance(label, str) and label for label in labels)
            or len(labels) != len(set(labels))
        ):
            raise ValueError(
                f"labels must be a non-empty list of unique strings for {case['id']}"
            )
        if not isinstance(case["sentences"], list) or not all(
            isinstance(value, str) for value in case["sentences"]
        ):
            raise TypeError(f"sentences must be strings for {case['id']}")
        validate_normalization(case["id"], case["sentences"])
        if not isinstance(case["targets"], list):
            raise TypeError(f"targets must be a list for {case['id']}")
        validate_targets(case["id"], case["labels"], case["sentences"], case["targets"])
        driver = case.get("driver")
        if driver is not None:
            if set(driver) != {"sentences", "targets"}:
                raise ValueError(
                    f"driver fields must be exactly sentences and targets for {case['id']}"
                )
            if not isinstance(driver["sentences"], list) or not all(
                isinstance(value, str) for value in driver["sentences"]
            ):
                raise TypeError(f"driver sentences must be strings for {case['id']}")
            validate_normalization(f"{case['id']} driver", driver["sentences"])
            if not isinstance(driver["targets"], list):
                raise TypeError(f"driver targets must be a list for {case['id']}")
            validate_targets(
                f"{case['id']} driver",
                case["labels"],
                driver["sentences"],
                driver["targets"],
            )
        oracle_query = case.get("oracle_query")
        if oracle_query is not None and (
            not isinstance(oracle_query, str) or not oracle_query.strip()
        ):
            raise ValueError(
                f"oracle_query must be a non-empty string for {case['id']}"
            )
    return cases


def validate_normalization(case_id: str, sentences: list[str]) -> None:
    if any(
        unicodedata.normalize("NFC", sentence) != sentence for sentence in sentences
    ):
        raise ValueError(f"sentences must be NFC-normalized for {case_id}")


def validate_targets(
    case_id: str, labels: list[str], sentences: list[str], targets: list[dict]
) -> None:
    for target in targets:
        target_fields = {
            "sentence_ordinal",
            "start_char",
            "end_char",
            "surface",
            "label",
        }
        if set(target) != target_fields:
            raise ValueError(
                f"target fields must be exactly {sorted(target_fields)} for {case_id}"
            )
        ordinal = target["sentence_ordinal"]
        start = target["start_char"]
        end = target["end_char"]
        if not all(isinstance(value, int) for value in (ordinal, start, end)):
            raise TypeError(f"target ordinal and spans must be integers for {case_id}")
        if not isinstance(target["surface"], str) or not isinstance(
            target["label"], str
        ):
            raise TypeError(f"target surface and label must be strings for {case_id}")
        if target["label"] not in labels:
            raise ValueError(
                f"target label {target['label']!r} is not declared for {case_id}"
            )
        if ordinal < 0 or ordinal >= len(sentences):
            raise ValueError(f"target sentence ordinal is out of range for {case_id}")
        sentence = sentences[ordinal]
        selected = sentence[start:end]
        if (
            start < 0
            or end <= start
            or end > len(sentence)
            or selected != target["surface"]
        ):
            raise ValueError(
                f"target span [{start}, {end}) does not select surface {target['surface']!r} for {case_id}"
            )


def load_catalog(path: Path) -> list[dict]:
    return validate_catalog(json.loads(path.read_text()))


def evidence_rows(targets: list[dict], eval_document_id: str) -> list[dict]:
    rows = [
        {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": eval_document_id,
            "sentence_ordinal": target["sentence_ordinal"],
            "start_char": target["start_char"],
            "end_char": target["end_char"],
            "label": target["label"],
        }
        for target in targets
    ]
    rows.sort(
        key=lambda row: tuple(
            row[key]
            for key in (
                "eval_document_id",
                "sentence_ordinal",
                "start_char",
                "end_char",
                "label",
            )
        )
    )
    return rows


def write_jsonl(path: Path, values: list[dict]) -> None:
    with path.open("w") as output:
        for value in values:
            output.write(json.dumps(value, ensure_ascii=False) + "\n")


def oracle_evidence_runner(rows: list[dict]) -> str:
    by_document: dict[str, list[dict]] = {}
    for row in rows:
        by_document.setdefault(row["eval_document_id"], []).append(row)
    encoded = json.dumps(by_document, ensure_ascii=False, sort_keys=True)
    return f'''#!/usr/bin/env python3
import json
import sys

EXPECTED = json.loads({encoded!r})
documents = [json.loads(line) for line in open(sys.argv[1])]
with open(sys.argv[2], "w") as output:
    for document in documents:
        for row in EXPECTED.get(document["eval_document_id"], []):
            output.write(json.dumps(row, ensure_ascii=False) + "\\n")
'''


def generate(
    destination: Path,
    catalog: Path = DEFAULT_CATALOG,
    *,
    agent_timeout_sec: int = DEFAULT_AGENT_TIMEOUT_SEC,
    lingrep_image: str = ARMS["lingrep"][0],
    mecab_image: str = ARMS["mecab"][0],
) -> None:
    if agent_timeout_sec < 1:
        raise ValueError("agent timeout must be at least one second")
    cases = load_catalog(catalog)
    if destination.exists():
        shutil.rmtree(destination)
    images = {"lingrep": lingrep_image, "mecab": mecab_image}
    for arm, (_, analyzer) in ARMS.items():
        image = images[arm]
        for case in cases:
            task = destination / arm / case["id"]
            for child in ("environment", "tests", "solution"):
                (task / child).mkdir(parents=True, exist_ok=True)

            document = {"eval_document_id": "doc-001", "sentences": case["sentences"]}
            (task / "environment" / "input.json").write_text(
                json.dumps(document, ensure_ascii=False) + "\n"
            )
            shutil.copy2(
                ROOT / "guides" / DISCOVERY_GUIDE,
                task / "environment" / "TOOL_GUIDE.md",
            )
            (task / "environment" / "Dockerfile").write_text(
                f"FROM {image}\nCOPY input.json /data/input.jsonl\n"
                f"ENV ANALYZER={analyzer}\n"
                "COPY TOOL_GUIDE.md /app/TOOL_GUIDE.md\nWORKDIR /app\n"
            )

            gold = evidence_rows(case["targets"], "doc-001")
            write_jsonl(task / "tests" / "gold.jsonl", gold)
            shutil.copy2(
                task / "tests" / "gold.jsonl", task / "solution" / "gold.jsonl"
            )
            # One case-level fact, three consequences: a catalog that reproduces
            # this case's gold is what proves the query rubric is reachable, so
            # only a case carrying one is replayed as a catalog, asked for one,
            # or required to supply one during oracle preflight.
            oracle_query = case.get("oracle_query") if arm == "lingrep" else None
            if oracle_query:
                (task / "solution" / "query.lx").write_text(oracle_query)
            shutil.copy2(ROOT / "verifier" / "verify.py", task / "tests" / "verify.py")
            shutil.copy2(
                task / "environment" / "input.json",
                task / "tests" / "visible.jsonl",
            )
            driver = case.get("driver")
            (task / "tests" / "analysis-replay.json").write_text(
                json.dumps({"has_hidden_driver": driver is not None}) + "\n"
            )
            if oracle_query:
                replay_config = {
                    "labels": case["labels"],
                    "has_hidden_driver": driver is not None,
                }
                (task / "tests" / "query-replay.json").write_text(
                    json.dumps(replay_config, ensure_ascii=False) + "\n"
                )
            if driver is not None:
                hidden_document = {
                    "eval_document_id": "hidden-001",
                    "sentences": driver["sentences"],
                }
                (task / "tests" / "hidden.jsonl").write_text(
                    json.dumps(hidden_document, ensure_ascii=False) + "\n"
                )
                write_jsonl(
                    task / "tests" / "hidden-gold.jsonl",
                    evidence_rows(driver["targets"], "hidden-001"),
                )
            (task / "tests" / "test.sh").write_text(
                "#!/bin/bash\nexec python3 /tests/verify.py\n"
            )
            (task / "tests" / "Dockerfile").write_text(
                f"FROM {image}\nCOPY . /tests/\n"
            )
            oracle_copy = (
                "cp /solution/query.lx /logs/artifacts/submission/query.lx\n"
                if oracle_query
                else ""
            )
            (task / "solution" / "solve.sh").write_text(
                "#!/bin/bash\nmkdir -p /logs/artifacts/submission\n"
                "cp /solution/gold.jsonl /logs/artifacts/submission/evidence.jsonl\n"
                + oracle_copy
                + "cp /solution/run-analysis /logs/artifacts/submission/run-analysis\n"
                + 'printf \'{"schema":"lingrep-linguistic-eval.result/v1"}\\n\' > /logs/artifacts/submission/result.json\n'
                "printf '# Oracle solution\\n' > /logs/artifacts/submission/analysis.md\n"
            )
            runner = task / "solution" / "run-analysis"
            if oracle_query:
                shutil.copy2(ROOT / "oracle_lingrep_runner.py", runner)
            else:
                oracle_rows = list(gold)
                if driver is not None:
                    oracle_rows.extend(
                        evidence_rows(driver["targets"], "hidden-001")
                    )
                runner.write_text(
                    oracle_evidence_runner(oracle_rows)
                )
            query_contract = ""
            if oracle_query:
                query_contract = """
Your submitted catalog is executable output, not supporting prose. Save exactly one
catalog as `/logs/artifacts/submission/query.lx`. Its published definition names must
be exactly the permitted evidence labels, and its published matches must have the
submitted spans and labels without Python filtering or relabeling. A definition's
published name is its own name unless it declares `output_name`. The verifier replays
this file on the visible input and may replay it on hidden cases.
"""
            instruction = f"""# {case["title"]}

Analyze `/data/input.jsonl` for this phenomenon:

{case["phenomenon"]}

The only permitted evidence labels for this task are: {", ".join(case["labels"])}.
Copy these labels exactly.

Read `/app/TOOL_GUIDE.md`. Use the installed analysis tool and inspect its output; do not answer from intuition alone.

Write these files under `/logs/artifacts/submission/`:

- `evidence.jsonl`: one strict `lingrep-linguistic-eval.evidence/v1` object per match, ordered by document, sentence, character span, and label.
- `result.json`: `{{"schema":"lingrep-linguistic-eval.result/v1"}}` (additional summary fields are allowed).
- `analysis.md`: briefly explain the linguistic decision and cite the tool evidence used.
- `run-analysis`: an executable entrypoint that accepts `INPUT_JSONL OUTPUT_JSONL`
  and reproduces the evidence from that input. Put any support files beside it and
  use paths relative to the entrypoint directory. The verifier reruns it unchanged
  in a networkless Bubblewrap sandbox on the visible input and may use hidden input.

Before finishing, execute `/logs/artifacts/submission/run-analysis
/data/input.jsonl /logs/artifacts/submission/evidence.jsonl` yourself and inspect
the resulting evidence. Do not make the entrypoint depend on files outside its
submission directory, the two arguments, or the installed analyzer runtime.

Every evidence row must have exactly these six fields. This structural example uses
one label permitted by this task; change all values to describe the actual match:

```json
{{"schema":"lingrep-linguistic-eval.evidence/v1","eval_document_id":"doc-001","sentence_ordinal":0,"start_char":0,"end_char":1,"label":"{case["labels"][0]}"}}
```

Do not add token text, sentence text, features, nested spans, or alternate field names. Spans are zero-based, half-open Unicode code-point offsets within the original NFC-normalized sentence. Emit no extra evidence rows. Create all four files even when there are no matches.
{query_contract}
"""
            (task / "instruction.md").write_text(instruction)
            (task / "task.toml").write_text(f"""schema_version = "1.2"

[task]
name = "lingrep/{arm}-{case["id"]}"
description = "Paired Japanese linguistic-analysis correctness task"

[metadata]
category = "Linguistics"
tags = ["japanese", "morphology", "paired-evaluation", "{arm}"]
arm = "{arm}"
case_id = "{case["id"]}"
capability = "{case["capability"]}"

[verifier]
environment_mode = "separate"
timeout_sec = 60.0

[agent]
timeout_sec = {float(agent_timeout_sec)}

[environment]
build_timeout_sec = 900.0
cpus = 2
memory_mb = 4096
storage_mb = 10240
gpus = 0
allow_internet = false
""")
            for executable in (
                task / "tests" / "test.sh",
                task / "solution" / "solve.sh",
                task / "solution" / "run-analysis",
            ):
                executable.chmod(0o755)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=ROOT / ".generated")
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument(
        "--agent-timeout-sec", type=int, default=DEFAULT_AGENT_TIMEOUT_SEC
    )
    parser.add_argument("--lingrep-image", default=ARMS["lingrep"][0])
    parser.add_argument("--mecab-image", default=ARMS["mecab"][0])
    args = parser.parse_args()
    generate(
        args.output.resolve(),
        args.catalog.resolve(),
        agent_timeout_sec=args.agent_timeout_sec,
        lingrep_image=args.lingrep_image,
        mecab_image=args.mecab_image,
    )
