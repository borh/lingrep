#!/usr/bin/env python3
"""Generate paired Harbor tasks from one linguistic case catalog."""

from __future__ import annotations

import argparse
import ast
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
PROTOCOL_SCHEMA = "lingrep-linguistic-eval.protocol/v2"
VERDICT_SCHEMA = "lingrep-linguistic-eval.verdict/v1"
COMMON_ANALYSIS = "common-analysis"
CATALOG_AUTHORING = "catalog-authoring"
PROTOCOLS = (COMMON_ANALYSIS, CATALOG_AUTHORING)
DEFAULT_AGENT_TIMEOUT_SEC = 3600
REPLAY_TIMEOUT_SEC = 30
VERIFIER_OVERHEAD_SEC = 30


def verifier_timeout_sec(*, has_query: bool, has_hidden: bool) -> int:
    replays = (1 + has_hidden) * (1 + has_query)
    return VERIFIER_OVERHEAD_SEC + replays * REPLAY_TIMEOUT_SEC


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
            "documents",
            "targets",
            "oracle_query",
            "oracle_mecab",
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
        validate_documents(case["id"], case["documents"])
        if not isinstance(case.get("answerable", True), bool):
            raise TypeError(f"answerable must be a boolean for {case['id']}")
        if not case.get("answerable", True) and (
            case["targets"] or (case.get("driver") or {}).get("targets")
        ):
            raise ValueError(f"an unanswerable case has no targets: {case['id']}")
        if not isinstance(case["targets"], list):
            raise TypeError(f"targets must be a list for {case['id']}")
        validate_targets(case["id"], case["labels"], case["documents"], case["targets"])
        driver = case.get("driver")
        if driver is not None:
            if set(driver) != {"documents", "targets"}:
                raise ValueError(
                    f"driver fields must be exactly documents and targets for {case['id']}"
                )
            validate_documents(f"{case['id']} driver", driver["documents"])
            if not isinstance(driver["targets"], list):
                raise TypeError(f"driver targets must be a list for {case['id']}")
            validate_targets(
                f"{case['id']} driver",
                case["labels"],
                driver["documents"],
                driver["targets"],
            )
        if case.get("source") is not None:
            validate_source(case["id"], case["source"], case["documents"])
        validate_oracles(case["id"], case)
        if "revision" in case:
            validate_revision(case)
    return cases


def validate_oracles(case_id: str, holder: dict) -> None:
    if not isinstance(holder["oracle_query"], str) or not holder["oracle_query"].strip():
        raise ValueError(f"oracle_query must be a non-empty string for {case_id}")
    validate_mecab_reference(case_id, holder["oracle_mecab"])
    if "oracle_where" in holder and (
        not isinstance(holder["oracle_where"], str) or not holder["oracle_where"].strip()
    ):
        raise ValueError(f"oracle_where must be a non-empty string for {case_id}")


REVISION_FIELDS = {
    "instruction",
    "targets",
    "driver_targets",
    "oracle_query",
    "oracle_mecab",
    "oracle_where",
}


def validate_revision(case: dict) -> None:
    """A `revision` restates the condition over the case's own documents and labels."""
    revision = case["revision"]
    case_id = f"{case['id']} revision"
    if not isinstance(revision, dict):
        raise TypeError(f"revision must be an object for {case['id']}")
    unknown = sorted(set(revision) - REVISION_FIELDS)
    missing = sorted({"instruction", "targets", "oracle_query", "oracle_mecab"} - set(revision))
    if unknown or missing:
        raise ValueError(
            f"revision fields for {case['id']}: unknown {unknown}, missing {missing}"
        )
    if not isinstance(revision["instruction"], str) or not revision["instruction"].strip():
        raise ValueError(f"revision instruction must be a non-empty string for {case['id']}")
    if not isinstance(revision["targets"], list):
        raise TypeError(f"revision targets must be a list for {case['id']}")
    validate_targets(case_id, case["labels"], case["documents"], revision["targets"])
    driver = case.get("driver")
    if (driver is not None) != ("driver_targets" in revision):
        raise ValueError(
            f"revision driver_targets must be present exactly when the case has a driver"
            f" for {case['id']}"
        )
    if driver is not None:
        if not isinstance(revision["driver_targets"], list):
            raise TypeError(f"revision driver targets must be a list for {case['id']}")
        validate_targets(
            f"{case_id} driver", case["labels"], driver["documents"], revision["driver_targets"]
        )
    validate_oracles(case_id, revision)


def task_specs(cases: list[dict]) -> list[dict]:
    """One task per case plus, for a case with a `revision`, a `<id>.revision` task
    whose documents are the case's and whose start state is the base reference."""
    specs = []
    for case in cases:
        specs.append({**case, "stage": "base", "base_case_id": case["id"]})
        revision = case.get("revision")
        if revision is None:
            continue
        spec = {
            key: value
            for key, value in case.items()
            if key not in {"oracle_where", "revision", "driver"}
        }
        spec.update(
            id=f"{case['id']}.revision",
            stage="revision",
            base_case_id=case["id"],
            revision_instruction=revision["instruction"],
            targets=revision["targets"],
            oracle_query=revision["oracle_query"],
            oracle_mecab=revision["oracle_mecab"],
            prior=case,
        )
        if "oracle_where" in revision:
            spec["oracle_where"] = revision["oracle_where"]
        if case.get("driver") is not None:
            spec["driver"] = {
                "documents": case["driver"]["documents"],
                "targets": revision["driver_targets"],
            }
        specs.append(spec)
    return specs


def write_reference_solution(directory: Path, arm: str, case: dict) -> list[str]:
    """The arm's reference `run-analysis` and its support files; returns their names."""
    runner = directory / "run-analysis"
    if arm == "lingrep":
        supports = ["query.lx"]
        (directory / "query.lx").write_text(case["oracle_query"])
        if "oracle_where" in case:
            supports.append("scope.where")
            (directory / "scope.where").write_text(case["oracle_where"].strip() + "\n")
        shutil.copy2(ROOT / "oracle_lingrep_runner.py", runner)
    else:
        supports = ["mecab_reference.py"]
        shutil.copy2(ROOT / supports[0], directory / supports[0])
        runner.write_text(mecab_reference_runner(case["oracle_mecab"]))
    runner.chmod(0o755)
    return supports


def validate_mecab_reference(case_id: str, source: object) -> None:
    """`source` is Python that defines `select(tokens, document)`; `mecab_reference.main` calls it."""
    if not isinstance(source, str):
        raise TypeError(f"oracle_mecab must be a string for {case_id}")
    try:
        module = ast.parse(source)
    except SyntaxError as error:
        raise ValueError(f"oracle_mecab must compile for {case_id}: {error}") from error
    if not any(
        isinstance(node, ast.FunctionDef) and node.name == "select" for node in module.body
    ):
        raise ValueError(f"oracle_mecab must define select for {case_id}")


def validate_documents(case_id: str, documents: object) -> None:
    if not isinstance(documents, list) or not documents:
        raise TypeError(f"documents must be a non-empty list for {case_id}")
    seen: set[str] = set()
    for document in documents:
        if not isinstance(document, dict) or not {"eval_document_id", "sentences"} <= set(
            document
        ) <= {"eval_document_id", "metadata", "sentences"}:
            raise ValueError(
                f"document fields must be eval_document_id, sentences and optional metadata for {case_id}"
            )
        document_id = document["eval_document_id"]
        if not isinstance(document_id, str) or not document_id or document_id in seen:
            raise ValueError(f"document ids must be unique non-empty strings for {case_id}")
        seen.add(document_id)
        metadata = document.get("metadata", {})
        if not isinstance(metadata, dict) or not all(
            isinstance(key, str) and isinstance(value, (str, int, float, bool))
            for key, value in metadata.items()
        ):
            raise TypeError(f"document metadata must map strings to scalars for {case_id}")
        sentences = document["sentences"]
        if not isinstance(sentences, list) or not all(
            isinstance(value, str) for value in sentences
        ):
            raise TypeError(f"sentences must be strings for {case_id}")
        validate_normalization(case_id, sentences)


def validate_source(case_id: str, source: object, documents: list[dict]) -> None:
    """A Soranoha `source` names the archive the documents were derived from;
    `soranoha_source.verify_catalog` re-derives them."""
    if not isinstance(source, dict) or set(source) != {
        "kind",
        "url",
        "sha256",
        "release",
        "works",
    }:
        raise ValueError(
            f"source fields must be kind, url, sha256, release and works for {case_id}"
        )
    if source["kind"] != "soranoha":
        raise ValueError(f"unknown source kind {source['kind']!r} for {case_id}")
    if not all(isinstance(source[key], str) and source[key] for key in ("url", "sha256", "release")):
        raise TypeError(f"source url, sha256 and release must be non-empty strings for {case_id}")
    works = source["works"]
    if not isinstance(works, list) or not all(
        isinstance(work, dict)
        and set(work) == {"identifier", "first_sentence", "sentence_count"}
        and isinstance(work["identifier"], str)
        and isinstance(work["first_sentence"], int)
        and isinstance(work["sentence_count"], int)
        and work["first_sentence"] >= 0
        and work["sentence_count"] > 0
        for work in works
    ):
        raise ValueError(f"source works must be identifier, first_sentence, sentence_count for {case_id}")
    if [work["identifier"] for work in works] != [
        document["eval_document_id"] for document in documents
    ]:
        raise ValueError(f"source works must list the document ids in order for {case_id}")


def validate_normalization(case_id: str, sentences: list[str]) -> None:
    if any(
        unicodedata.normalize("NFC", sentence) != sentence for sentence in sentences
    ):
        raise ValueError(f"sentences must be NFC-normalized for {case_id}")


def validate_targets(
    case_id: str, labels: list[str], documents: list[dict], targets: list[dict]
) -> None:
    by_id = {document["eval_document_id"]: document["sentences"] for document in documents}
    for target in targets:
        target_fields = {
            "eval_document_id",
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
        sentences = by_id.get(target["eval_document_id"])
        if sentences is None:
            raise ValueError(
                f"target document {target['eval_document_id']!r} is not a case document for {case_id}"
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


def document_lines(documents: list[dict]) -> str:
    return "".join(json.dumps(document, ensure_ascii=False) + "\n" for document in documents)


def evidence_rows(targets: list[dict]) -> list[dict]:
    rows = [
        {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": target["eval_document_id"],
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


def mecab_reference_runner(select_source: str) -> str:
    return (
        "#!/usr/bin/env python3\n"
        "from mecab_reference import auxiliary_run, main, predicate_start, sentence_final\n\n\n"
        f"{select_source.rstrip()}\n\n\n"
        "main(select)\n"
    )


def select_cases(cases: list[dict], case_ids: list[str] | None) -> list[dict]:
    """Catalog cases named by `case_ids` in catalog order; `None` selects all."""
    if case_ids is None:
        return cases
    unknown = sorted(set(case_ids) - {case["id"] for case in cases})
    if unknown:
        raise ValueError(f"unknown cases: {unknown}")
    return [case for case in cases if case["id"] in case_ids]


def generate(
    destination: Path,
    catalog: Path = DEFAULT_CATALOG,
    *,
    agent_timeout_sec: int = DEFAULT_AGENT_TIMEOUT_SEC,
    lingrep_image: str = ARMS["lingrep"][0],
    mecab_image: str = ARMS["mecab"][0],
    protocol: str = COMMON_ANALYSIS,
    cases: list[str] | None = None,
    arms: list[str] | None = None,
) -> None:
    """Under `common-analysis` both arms receive identical instructions and only the
    executable replay is scored; `catalog-authoring` adds the `query.lx` dimension to
    lingrep tasks. `cases` and `arms` select by id; `None` selects all."""
    if agent_timeout_sec < 1:
        raise ValueError("agent timeout must be at least one second")
    if protocol not in PROTOCOLS:
        raise ValueError(f"unknown protocol {protocol!r}; expected one of {PROTOCOLS}")
    specs = task_specs(select_cases(load_catalog(catalog), cases))
    scoped = [spec["id"] for spec in specs if "oracle_where" in spec]
    if scoped and protocol == CATALOG_AUTHORING:
        raise ValueError(
            f"cases {scoped} scope documents with oracle_where, which the"
            " catalog-authoring replay of query.lx alone cannot apply;"
            " generate them under common-analysis"
        )
    unknown_arms = sorted(set(arms or ()) - set(ARMS))
    if unknown_arms:
        raise ValueError(f"unknown arms: {unknown_arms}")
    if destination.exists():
        shutil.rmtree(destination)
    images = {"lingrep": lingrep_image, "mecab": mecab_image}
    for arm, (_, analyzer) in ARMS.items():
        if arms is not None and arm not in arms:
            continue
        image = images[arm]
        for case in specs:
            task = destination / arm / case["id"]
            for child in ("environment", "tests", "solution"):
                (task / child).mkdir(parents=True, exist_ok=True)

            (task / "environment" / "input.json").write_text(
                document_lines(case["documents"])
            )
            shutil.copy2(
                ROOT / "guides" / DISCOVERY_GUIDE,
                task / "environment" / "TOOL_GUIDE.md",
            )
            prior = ""
            if case["stage"] == "revision":
                (task / "environment" / "prior").mkdir()
                write_reference_solution(task / "environment" / "prior", arm, case["prior"])
                prior = "COPY prior/ /data/prior/\n"
            (task / "environment" / "Dockerfile").write_text(
                f"FROM {image}\nCOPY input.json /data/input.jsonl\n{prior}"
                f"ENV ANALYZER={analyzer}\n"
                "COPY TOOL_GUIDE.md /app/TOOL_GUIDE.md\nWORKDIR /app\n"
            )

            write_jsonl(task / "tests" / "gold.jsonl", evidence_rows(case["targets"]))
            answerable = case.get("answerable", True)
            (task / "tests" / "verdict.json").write_text(
                json.dumps({"answerable": answerable}) + "\n"
            )
            scores_query = arm == "lingrep" and protocol == CATALOG_AUTHORING
            shutil.copy2(ROOT / "verifier" / "verify.py", task / "tests" / "verify.py")
            shutil.copy2(
                task / "environment" / "input.json",
                task / "tests" / "visible.jsonl",
            )
            driver = case.get("driver")
            (task / "tests" / "analysis-replay.json").write_text(
                json.dumps(
                    {
                        "has_hidden_driver": driver is not None,
                        "replay_timeout_sec": REPLAY_TIMEOUT_SEC,
                    }
                )
                + "\n"
            )
            if scores_query:
                replay_config = {
                    "labels": case["labels"],
                    "has_hidden_driver": driver is not None,
                    "replay_timeout_sec": REPLAY_TIMEOUT_SEC,
                }
                (task / "tests" / "query-replay.json").write_text(
                    json.dumps(replay_config, ensure_ascii=False) + "\n"
                )
            if driver is not None:
                (task / "tests" / "hidden.jsonl").write_text(
                    document_lines(driver["documents"])
                )
                write_jsonl(
                    task / "tests" / "hidden-gold.jsonl", evidence_rows(driver["targets"])
                )
            (task / "tests" / "test.sh").write_text(
                "#!/bin/bash\nexec python3 /tests/verify.py\n"
            )
            (task / "tests" / "Dockerfile").write_text(
                f"FROM {image}\nCOPY . /tests/\n"
            )
            supports = write_reference_solution(task / "solution", arm, case)
            copied = " ".join(f"/solution/{support}" for support in supports)
            verdict = json.dumps(
                {
                    "schema": VERDICT_SCHEMA,
                    "answerable": answerable,
                    "reason": "reference solution",
                }
            )
            (task / "solution" / "solve.sh").write_text(
                "#!/bin/bash\nset -euo pipefail\nmkdir -p /logs/artifacts/submission\n"
                f"cp /solution/run-analysis {copied} /logs/artifacts/submission/\n"
                "/logs/artifacts/submission/run-analysis /data/input.jsonl"
                " /logs/artifacts/submission/evidence.jsonl\n"
                f"printf '%s\\n' '{verdict}' > /logs/artifacts/submission/verdict.json\n"
            )
            query_contract = ""
            if scores_query:
                query_contract = """
Your submitted catalog is executable output, not supporting prose. Save exactly one
catalog as `/logs/artifacts/submission/query.lx`. Its published definition names must
be exactly the permitted evidence labels, and its published matches must have the
submitted spans and labels without Python filtering or relabeling. A definition's
published name is its own name unless it declares `output_name`. The verifier replays
this file on the visible input and may replay it on hidden cases.
"""
            if case["stage"] == "revision":
                opening = f"""# {case["title"]} (revised condition)

A previous analysis of `/data/input.jsonl` answered this phenomenon:

{case["phenomenon"]}

Its working submission is in `/data/prior/`: `run-analysis` and the support files it
reads, built with the installed analysis tool. The condition has changed:

{case["revision_instruction"]}

Revise the analysis so that it satisfies the changed condition on the same input.
Start from `/data/prior/` or replace it; only the revised submission is scored.
"""
            else:
                opening = f"""# {case["title"]}

Analyze `/data/input.jsonl` for this phenomenon:

{case["phenomenon"]}
"""
            instruction = f"""{opening}
Each line of the input is one document: `eval_document_id`, `sentences`, and an
optional `metadata` object (for example work identifier, title, author, NDC class,
first publication). The phenomenon may restrict which documents count.

The only permitted evidence labels for this task are: {", ".join(case["labels"])}.
Copy these labels exactly.

Read `/app/TOOL_GUIDE.md`. Use the installed analysis tool and inspect its output; do not answer from intuition alone.

Write these files under `/logs/artifacts/submission/`:

- `evidence.jsonl`: one strict `lingrep-linguistic-eval.evidence/v1` object per match, ordered by document, sentence, character span, and label.
- `run-analysis`: an executable entrypoint that accepts `INPUT_JSONL OUTPUT_JSONL`
  and reproduces the evidence from that input. Put any support files beside it and
  use paths relative to the entrypoint directory. The verifier reruns it unchanged
  in a networkless Bubblewrap sandbox on the visible input and may use hidden input.
- `verdict.json`: `{{"schema":"{VERDICT_SCHEMA}","answerable":true,"reason":"..."}}`.
  Set `answerable` to `false` when this material and the installed analysis cannot
  decide the phenomenon as stated; then emit no evidence rows and say in `reason`
  what is missing. A phenomenon that is decidable but absent is answerable with no
  matches.

Before finishing, execute `/logs/artifacts/submission/run-analysis
/data/input.jsonl /logs/artifacts/submission/evidence.jsonl` yourself and inspect
the resulting evidence. Do not make the entrypoint depend on files outside its
submission directory, the two arguments, or the installed analyzer runtime.

Every evidence row must have exactly these six fields. This structural example uses
one label permitted by this task; change all values to describe the actual match:

```json
{{"schema":"lingrep-linguistic-eval.evidence/v1","eval_document_id":"doc-001","sentence_ordinal":0,"start_char":0,"end_char":1,"label":"{case["labels"][0]}"}}
```

Do not add token text, sentence text, features, nested spans, or alternate field names. Spans are zero-based, half-open Unicode code-point offsets within the original NFC-normalized sentence. Emit no extra evidence rows. Create all three files even when there are no matches. You may
also write `analysis.md` beside them to explain the decision; it is not scored.
{query_contract}
"""
            (task / "instruction.md").write_text(instruction)
            (task / "task.toml").write_text(f"""schema_version = "1.4"

[task]
name = "lingrep/{arm}-{case["id"]}"
description = "Paired Japanese linguistic-analysis correctness task"

[metadata]
category = "Linguistics"
tags = ["japanese", "morphology", "paired-evaluation", "{arm}"]
arm = "{arm}"
protocol = "{protocol}"
protocol_schema = "{PROTOCOL_SCHEMA}"
case_id = "{case["id"]}"
base_case_id = "{case["base_case_id"]}"
stage = "{case["stage"]}"
capability = "{case["capability"]}"

[verifier]
environment_mode = "separate"
timeout_sec = {float(verifier_timeout_sec(has_query=scores_query, has_hidden=driver is not None))}

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
    parser.add_argument("--protocol", choices=PROTOCOLS, default=COMMON_ANALYSIS)
    args = parser.parse_args()
    generate(
        args.output.resolve(),
        args.catalog.resolve(),
        agent_timeout_sec=args.agent_timeout_sec,
        lingrep_image=args.lingrep_image,
        mecab_image=args.mecab_image,
        protocol=args.protocol,
    )
