#!/usr/bin/env python3
"""Render one Markdown page per case with both arms' trials side by side."""

from __future__ import annotations

import argparse
import difflib
import json
from collections import defaultdict
from pathlib import Path

from experiment import find_experiment
from export_viewer import task_source
from report import reference_artifacts, submission_dir, summarize_experiment

EVIDENCE_KEY = ("eval_document_id", "sentence_ordinal", "start_char", "end_char", "label")
SCORES = ("visible_script_f1", "hidden_script_f1", "script_exact", "verdict_correct")
EFFORT = ("agent_elapsed_seconds", "model_api_seconds", "model_api_requests")


def evidence_rows(path: Path) -> set[tuple] | None:
    if path.is_symlink() or not path.is_file():
        return None
    try:
        return {
            tuple(row[key] for key in EVIDENCE_KEY)
            for row in map(json.loads, path.read_text().splitlines())
        }
    except (ValueError, KeyError, TypeError):
        return None


def agreement(trial_root: Path, gold: Path) -> dict | None:
    """Submitted evidence rows against the task's visible gold."""
    submitted = evidence_rows(submission_dir(trial_root) / "evidence.jsonl")
    expected = evidence_rows(gold)
    if submitted is None or expected is None:
        return None
    return {
        "matched": len(submitted & expected),
        "missed": sorted(expected - submitted),
        "extra": sorted(submitted - expected),
    }


def fenced(text: str | None, language: str = "") -> str:
    if text is None:
        return "_not submitted_\n"
    fence = "````" if "```" in text else "```"
    return f"{fence}{language}\n{text.rstrip()}\n{fence}\n"


def unified(before: str, after: str, name: str) -> str:
    lines = list(
        difflib.unified_diff(
            before.splitlines(), after.splitlines(), f"reference/{name}", f"submitted/{name}", lineterm=""
        )
    )
    return fenced("\n".join(lines) if lines else "(identical to the reference)", "diff")


def number(value) -> str:
    if value is None:
        return "–"
    return f"{value:.3g}" if isinstance(value, float) else str(value)


def trial_table(rows: list[dict]) -> str:
    header = ("arm", "stage", "attempt", *SCORES, *EFFORT, "bash calls", "evidence matched/missed/extra")
    lines = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
    for row in rows:
        agree = row["_agreement"]
        evidence = (
            f"{agree['matched']}/{len(agree['missed'])}/{len(agree['extra'])}" if agree else "–"
        )
        values = (
            row["arm"],
            row["stage"],
            str(row["attempt"]),
            *(number(row[score]) for score in SCORES),
            *(number(row[effort]) for effort in EFFORT),
            str(row["command_analysis"]["bash_tool_calls"]),
            evidence,
        )
        lines.append("| " + " | ".join(values) + " |")
    return "\n".join(lines) + "\n"


def query_source(row: dict) -> str | None:
    query = next((q for q in row["pattern_queries"] if q["path"] == "query.lx"), None)
    return query["source"] if query else None


def trial_section(row: dict, prior: dict[str, str] | None) -> str:
    runner = row["analysis_runner"]
    parts = [f"### {row['arm']}, {row['stage']}, attempt {row['attempt']}\n"]
    if row["stage"] == "revision" and prior:
        if runner and runner["source"] is not None:
            parts.append("Change to `run-analysis` from the reference artifact:\n")
            parts.append(unified(prior["run-analysis"], runner["source"], "run-analysis"))
        if "query.lx" in prior and query_source(row) is not None:
            parts.append("Change to `query.lx` from the reference artifact:\n")
            parts.append(unified(prior["query.lx"], query_source(row), "query.lx"))
    else:
        parts.append("`run-analysis`:\n")
        parts.append(fenced(runner["source"] if runner else None))
        if row["arm"] == "lingrep":
            parts.append("`query.lx`:\n")
            parts.append(fenced(query_source(row)))
    agree = row["_agreement"]
    if agree and (agree["missed"] or agree["extra"]):
        parts.append("Evidence disagreements with gold (document, sentence, start, end, label):\n")
        for kind in ("missed", "extra"):
            for key in agree[kind]:
                parts.append(f"- {kind}: `{json.dumps(key, ensure_ascii=False)}`")
        parts.append("")
    return "\n".join(parts) + "\n"


def task_instruction(root: Path, row: dict) -> str:
    return (task_source(root / Path(row["result_path"]).parent) / "instruction.md").read_text()


def render_case(case_id: str, rows: list[dict], manifest: dict, root: Path) -> str:
    rows = sorted(rows, key=lambda row: (row["stage"] != "base", row["arm"], row["attempt"]))
    base = next(row for row in rows if row["stage"] == "base")
    instruction = task_instruction(root, base)
    title = instruction.splitlines()[0].lstrip("# ").strip()
    models = ", ".join(sorted({row["model"] for row in rows}))
    summary = (
        f"Case `{case_id}`, experiment `{manifest['experiment_id']}`, "
        f"protocol `{manifest.get('protocol', 'unknown')}`, model `{models}`.\n"
    )
    parts = [
        f"# {title}\n",
        summary,
        "## Instruction\n",
        fenced(instruction, "markdown"),
        "## Trials\n",
        trial_table(rows),
    ]
    revision_rows = [row for row in rows if row["stage"] == "revision"]
    if revision_rows:
        parts.append("## Revised condition\n")
        parts.append(fenced(task_instruction(root, revision_rows[0]), "markdown"))
    parts.append("## Submissions\n")
    for row in rows:
        prior = reference_artifacts(root, row["arm"], case_id) if row["stage"] == "revision" else None
        parts.append(trial_section(row, prior))
    return "\n".join(parts)


def export_pairs(experiment: Path, destination: Path) -> list[Path]:
    root, manifest = find_experiment(experiment)
    rows = summarize_experiment(experiment)
    by_case: defaultdict[str, list[dict]] = defaultdict(list)
    for row in rows:
        trial_root = root / Path(row["result_path"]).parent
        gold = task_source(trial_root) / "tests" / "gold.jsonl"
        row["_agreement"] = agreement(trial_root, gold)
        by_case[row["base_case_id"]].append(row)
    destination.mkdir(parents=True, exist_ok=True)
    pages = []
    for case_id, case_rows in sorted(by_case.items()):
        if not any(row["stage"] == "base" for row in case_rows):
            continue
        page = destination / f"{case_id}.md"
        page.write_text(render_case(case_id, case_rows, manifest, root))
        pages.append(page)
    index = destination / "index.md"
    index.write_text(
        f"# {manifest['experiment_id']}\n\n"
        + "".join(f"- [{page.stem}]({page.name})\n" for page in pages)
    )
    return pages


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("experiment", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    pages = export_pairs(args.experiment, args.output)
    print(f"rendered {len(pages)} case pages under {args.output}")


if __name__ == "__main__":
    main()
