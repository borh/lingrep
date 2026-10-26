import ast
import importlib.metadata
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tomllib
import zipfile
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
SPEC = importlib.util.spec_from_file_location(
    "generate_dataset", ROOT / "generate_dataset.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)

REPORT_SPEC = importlib.util.spec_from_file_location("report", ROOT / "report.py")
REPORT = importlib.util.module_from_spec(REPORT_SPEC)
assert REPORT_SPEC.loader
REPORT_SPEC.loader.exec_module(REPORT)

RUN_SPEC = importlib.util.spec_from_file_location("run_benchmark", ROOT / "run.py")
RUN = importlib.util.module_from_spec(RUN_SPEC)
assert RUN_SPEC.loader
RUN_SPEC.loader.exec_module(RUN)

ORACLE_SPEC = importlib.util.spec_from_file_location(
    "oracle_preflight", ROOT / "oracle_preflight.py"
)
ORACLE = importlib.util.module_from_spec(ORACLE_SPEC)
assert ORACLE_SPEC.loader
ORACLE_SPEC.loader.exec_module(ORACLE)
PAIRS_SPEC = importlib.util.spec_from_file_location("export_pairs", ROOT / "export_pairs.py")
PAIRS = importlib.util.module_from_spec(PAIRS_SPEC)
assert PAIRS_SPEC.loader
PAIRS_SPEC.loader.exec_module(PAIRS)

EXPORT_SPEC = importlib.util.spec_from_file_location(
    "export_viewer", ROOT / "export_viewer.py"
)
EXPORT = importlib.util.module_from_spec(EXPORT_SPEC)
assert EXPORT_SPEC.loader
EXPORT_SPEC.loader.exec_module(EXPORT)

ORACLES = {
    "oracle_query": "target := [text=の].\n",
    "oracle_mecab": (
        "def select(tokens, document):\n"
        "    for index, token in enumerate(tokens):\n"
        "        if token.surface == \"の\":\n"
        "            yield index, index + 1, \"target\"\n"
    ),
}


def test_generation_is_paired_and_keeps_gold_out_of_agent_image(tmp_path):
    catalog = ROOT / "catalogs" / "smoke.json"
    MODULE.generate(tmp_path, catalog, protocol="catalog-authoring")
    cases = json.loads(catalog.read_text())
    assert len(list((tmp_path / "lingrep").iterdir())) == len(cases) == 7
    assert [case["targets"] for case in cases].count([]) == 1
    assert {path.name for path in (tmp_path / "lingrep").iterdir()} == {
        path.name for path in (tmp_path / "mecab").iterdir()
    }
    for case in cases:
        lingrep = tmp_path / "lingrep" / case["id"]
        mecab = tmp_path / "mecab" / case["id"]
        assert (lingrep / "environment" / "input.json").read_bytes() == (
            mecab / "environment" / "input.json"
        ).read_bytes()
        assert (lingrep / "environment" / "TOOL_GUIDE.md").read_bytes() == (
            mecab / "environment" / "TOOL_GUIDE.md"
        ).read_bytes()
        assert (lingrep / "environment" / "TOOL_GUIDE.md").read_bytes() == (
            ROOT / "guides" / "analyzer-guide.md"
        ).read_bytes()
        assert "ENV ANALYZER=ling" in (lingrep / "environment" / "Dockerfile").read_text()
        assert "ENV ANALYZER=mecab" in (mecab / "environment" / "Dockerfile").read_text()
        assert len((lingrep / "environment" / "input.json").read_text().splitlines()) == len(
            case["documents"]
        )
        assert (lingrep / "tests" / "gold.jsonl").read_bytes() == (
            mecab / "tests" / "gold.jsonl"
        ).read_bytes()
        assert "gold" not in (lingrep / "environment" / "Dockerfile").read_text()
        assert (lingrep / "tests" / "visible.jsonl").read_bytes() == (
            lingrep / "environment" / "input.json"
        ).read_bytes()
        assert (lingrep / "tests" / "query-replay.json").is_file() == bool(
            case.get("oracle_query")
        )
        assert not (mecab / "tests" / "query-replay.json").exists()
        assert (lingrep / "tests" / "analysis-replay.json").is_file()
        assert (mecab / "tests" / "analysis-replay.json").is_file()
        assert (
            "FROM lingrep-linguistic-eval:latest"
            in (lingrep / "tests" / "Dockerfile").read_text()
        )
        rows = [
            json.loads(line)
            for line in (lingrep / "tests" / "gold.jsonl").read_text().splitlines()
        ]
        assert bool(rows) == bool(case["targets"])
        assert all(row["end_char"] > row["start_char"] for row in rows)
        metadata = tomllib.loads((lingrep / "task.toml").read_text())["metadata"]
        assert metadata["protocol"] == "catalog-authoring"
        assert metadata["protocol_schema"] == MODULE.PROTOCOL_SCHEMA
        assert metadata["case_id"] == case["id"]
        assert metadata["arm"] == "lingrep"
        assert metadata["capability"] == case["capability"]
        task_config = tomllib.loads((lingrep / "task.toml").read_text())
        assert task_config["agent"]["timeout_sec"] == 3600.0
        assert task_config["environment"]["allow_internet"] is False
        instruction = (lingrep / "instruction.md").read_text()
        assert "The only permitted evidence labels" in instruction
        assert f'"label":"{case["labels"][0]}"' in instruction
        assert (
            "Your submitted catalog is executable output" in instruction
        ) == bool(case.get("oracle_query"))
        assert "Use zero-width lookaround" not in instruction
        solve = (lingrep / "solution" / "solve.sh").read_text()
        assert "result.json" not in instruction and "result.json" not in solve
        assert "analysis.md" not in solve
        for task in (lingrep, mecab):
            assert not (task / "solution" / "gold.jsonl").exists()
            assert "gold" not in (task / "solution" / "solve.sh").read_text()


def test_common_analysis_protocol_gives_both_arms_identical_instructions(tmp_path):
    catalog = ROOT / "catalogs" / "smoke.json"
    MODULE.generate(tmp_path, catalog)

    for case in json.loads(catalog.read_text()):
        assert case["oracle_query"]
        lingrep = tmp_path / "lingrep" / case["id"]
        mecab = tmp_path / "mecab" / case["id"]
        assert (lingrep / "instruction.md").read_bytes() == (mecab / "instruction.md").read_bytes()
        assert "query.lx" not in (lingrep / "instruction.md").read_text()
        for task in (lingrep, mecab):
            assert not (task / "tests" / "query-replay.json").exists()
            metadata = tomllib.loads((task / "task.toml").read_text())["metadata"]
            assert metadata["protocol"] == "common-analysis"
            assert metadata["protocol_schema"] == MODULE.PROTOCOL_SCHEMA
            assert ORACLE.replay_dimensions(task) == (False, False)

    with pytest.raises(ValueError, match="unknown protocol"):
        MODULE.generate(tmp_path / "bad", catalog, protocol="guided")


def test_generation_pins_supplied_base_images_in_every_dockerfile(tmp_path):
    lingrep_image = "sha256:" + "1" * 64
    mecab_image = "sha256:" + "2" * 64

    MODULE.generate(
        tmp_path,
        ROOT / "catalogs" / "smoke.json",
        lingrep_image=lingrep_image,
        mecab_image=mecab_image,
    )

    for task in (tmp_path / "lingrep").iterdir():
        assert (task / "environment" / "Dockerfile").read_text().startswith(
            f"FROM {lingrep_image}\n"
        )
        assert (task / "tests" / "Dockerfile").read_text().startswith(
            f"FROM {lingrep_image}\n"
        )
    for task in (tmp_path / "mecab").iterdir():
        assert (task / "environment" / "Dockerfile").read_text().startswith(
            f"FROM {mecab_image}\n"
        )
        assert (task / "tests" / "Dockerfile").read_text().startswith(
            f"FROM {mecab_image}\n"
        )


def test_worked_example_generates_every_capability_with_positive_and_negative_sentences(
    tmp_path,
):
    catalog = ROOT / "catalogs" / "worked-example.json"
    cases = MODULE.load_catalog(catalog)

    assert len(cases) == len(MODULE.CAPABILITIES)
    assert {case["capability"] for case in cases} == MODULE.CAPABILITIES
    for case in cases:
        targeted_ordinals = {target["sentence_ordinal"] for target in case["targets"]}
        assert targeted_ordinals
        assert targeted_ordinals < set(range(len(case["documents"][0]["sentences"])))

    MODULE.generate(tmp_path, catalog)
    assert {path.name for path in (tmp_path / "lingrep").iterdir()} == {
        case["id"] for case in cases
    }
    assert {path.name for path in (tmp_path / "mecab").iterdir()} == {
        case["id"] for case in cases
    }
    query_task = tmp_path / "lingrep" / "worked-sentence-final-query"
    assert (query_task / "tests" / "hidden.jsonl").is_file()
    assert (query_task / "tests" / "hidden-gold.jsonl").is_file()
    mecab_query_task = tmp_path / "mecab" / "worked-sentence-final-query"
    assert (mecab_query_task / "tests" / "hidden.jsonl").is_file()
    assert (mecab_query_task / "tests" / "hidden-gold.jsonl").is_file()
    assert "hidden-001" not in (query_task / "environment" / "input.json").read_text()
    query_case = next(
        case for case in cases if case["id"] == "worked-sentence-final-query"
    )
    assert query_case["oracle_query"]
    assert not (query_task / "tests" / "query-replay.json").exists()
    for task in (query_task, mecab_query_task):
        analysis_replay = json.loads(
            (task / "tests" / "analysis-replay.json").read_text()
        )
        assert analysis_replay == {"has_hidden_driver": True, "replay_timeout_sec": 30}
        assert "run-analysis" in (task / "instruction.md").read_text()
        assert "run-analysis" in (task / "solution" / "solve.sh").read_text()


def test_generation_rejects_a_driver_span_mismatching_surface(tmp_path):
    case = json.loads((ROOT / "catalogs" / "worked-example.json").read_text())[-1]
    case["driver"]["targets"][0]["start_char"] = 0
    catalog = tmp_path / "invalid-driver.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    with pytest.raises(ValueError, match="driver"):
        MODULE.generate(tmp_path / "generated", catalog)


@pytest.mark.parametrize("protocol", sorted(MODULE.PROTOCOLS))
def test_oracle_solutions_run_their_tool_on_the_task_input(tmp_path, protocol):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    catalog = tmp_path / "oracle.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    generated = tmp_path / "generated"
    MODULE.generate(generated, catalog, protocol=protocol)

    lingrep = generated / "lingrep" / case["id"]
    mecab = generated / "mecab" / case["id"]
    assert (lingrep / "solution" / "query.lx").read_text() == case["oracle_query"]
    assert (lingrep / "solution" / "run-analysis").read_bytes() == (
        ROOT / "oracle_lingrep_runner.py"
    ).read_bytes()
    assert (mecab / "solution" / "mecab_reference.py").read_bytes() == (
        ROOT / "mecab_reference.py"
    ).read_bytes()
    mecab_runner = (mecab / "solution" / "run-analysis").read_text()
    assert mecab_runner.startswith("#!/usr/bin/env python3\n")
    assert case["oracle_mecab"] in mecab_runner
    assert mecab_runner.rstrip().endswith("main(select)")
    assert not (lingrep / "environment" / "query.lx").exists()
    assert not (mecab / "solution" / "query.lx").exists()
    assert not (lingrep / "solution" / "mecab_reference.py").exists()
    for task, support in ((lingrep, "query.lx"), (mecab, "mecab_reference.py")):
        solve = (task / "solution" / "solve.sh").read_text().splitlines()
        assert solve == [
            "#!/bin/bash",
            "set -euo pipefail",
            "mkdir -p /logs/artifacts/submission",
            f"cp /solution/run-analysis /solution/{support} /logs/artifacts/submission/",
            (
                "/logs/artifacts/submission/run-analysis /data/input.jsonl"
                " /logs/artifacts/submission/evidence.jsonl"
            ),
            (
                "printf '%s\\n' '{\"schema\": \"lingrep-linguistic-eval.verdict/v1\","
                " \"answerable\": true, \"reason\": \"reference solution\"}'"
                " > /logs/artifacts/submission/verdict.json"
            ),
        ]
        assert json.loads((task / "tests" / "verdict.json").read_text()) == {"answerable": True}
        assert not (task / "solution" / "gold.jsonl").exists()
        assert (task / "solution" / "run-analysis").stat().st_mode & 0o111


@pytest.mark.parametrize(
    ("field", "value", "message"),
    [
        ("oracle_query", None, "missing fields"),
        ("oracle_mecab", None, "missing fields"),
        ("oracle_query", " ", "oracle_query must be"),
        ("oracle_mecab", "def select(tokens)\n", "oracle_mecab must compile"),
        ("oracle_mecab", "SELECT = 1\n", "oracle_mecab must define select"),
        ("oracle_where", " ", "oracle_where must be"),
    ],
)
def test_catalog_requires_a_reference_for_both_arms(tmp_path, field, value, message):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    if value is None:
        del case[field]
    else:
        case[field] = value
    catalog = tmp_path / "catalog.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))
    with pytest.raises((ValueError, SyntaxError), match=message):
        MODULE.load_catalog(catalog)


def test_oracle_preflight_requires_all_replay_dimensions():
    complete = {
        "exact": 1.0,
        "verdict_correct": 1.0,
        "query_valid": 1.0,
        "visible_query_exact": 1.0,
        "hidden_query_exact": 1.0,
        "query_exact": 1.0,
        "script_valid": 1.0,
        "visible_script_exact": 1.0,
        "hidden_script_exact": 1.0,
        "script_exact": 1.0,
    }
    ORACLE.require_oracle_reward("case", complete, has_query=True, has_hidden=True)
    mecab_complete = {
        key: value for key, value in complete.items() if "query" not in key
    }
    ORACLE.require_oracle_reward(
        "case", mecab_complete, has_query=False, has_hidden=True
    )

    with pytest.raises(RuntimeError, match="hidden query exact"):
        ORACLE.require_oracle_reward(
            "case",
            {**complete, "hidden_query_exact": 0.0},
            has_query=True,
            has_hidden=True,
        )


def test_preflight_reads_replayed_dimensions_from_the_generated_task(tmp_path):
    catalog = ROOT / "catalogs" / "smoke.json"
    case = json.loads(catalog.read_text())[0]

    plain = tmp_path / "plain"
    MODULE.generate(plain, catalog)
    scored = tmp_path / "scored"
    MODULE.generate(scored, catalog, protocol="catalog-authoring")

    for arm in ("lingrep", "mecab"):
        plain_task = plain / arm / case["id"]
        assert ORACLE.replay_dimensions(plain_task) == (False, False)
        assert not (plain_task / "tests" / "query-replay.json").exists()
        assert "query.lx" not in (plain_task / "instruction.md").read_text()
    assert (plain / "lingrep" / case["id"] / "solution" / "query.lx").is_file()
    assert ORACLE.replay_dimensions(scored / "lingrep" / case["id"]) == (True, False)
    assert ORACLE.replay_dimensions(scored / "mecab" / case["id"]) == (False, False)


def test_generation_rejects_a_surface_mismatching_explicit_character_span(
    tmp_path,
):
    catalog = tmp_path / "invalid.json"
    catalog.write_text(
        json.dumps(
            [
                {
                    "id": "repeated-surface",
                    "title": "Repeated surface",
                    "capability": "segmentation_alignment",
                    "phenomenon": "Find the second の.",
                    "labels": ["target"],
                    "documents": [{"eval_document_id": "doc-001", "sentences": ["のとの"]}],
                    **ORACLES,
                    "targets": [
                        {
                            "eval_document_id": "doc-001", "sentence_ordinal": 0,
                            "start_char": 0,
                            "end_char": 1,
                            "surface": "と",
                            "label": "target",
                        }
                    ],
                }
            ]
        )
    )

    with pytest.raises(ValueError, match="does not select surface"):
        MODULE.generate(tmp_path / "generated", catalog)


def test_generation_rejects_non_nfc_sentences(tmp_path):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    case["documents"][0]["sentences"][0] = "か\u3099"
    case["targets"] = []
    catalog = tmp_path / "non-nfc.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    with pytest.raises(ValueError, match="NFC-normalized"):
        MODULE.generate(tmp_path / "generated", catalog)


def test_generation_records_an_explicit_agent_timeout(tmp_path):
    generated = tmp_path / "generated"
    MODULE.generate(
        generated,
        ROOT / "catalogs" / "smoke.json",
        agent_timeout_sec=7200,
    )
    task = tomllib.loads(
        (generated / "lingrep" / "direct-noun-morphology" / "task.toml").read_text()
    )
    assert task["agent"]["timeout_sec"] == 7200.0


def test_generation_rejects_targets_outside_declared_label_vocabulary(tmp_path):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    case["labels"] = ["different_label"]
    catalog = tmp_path / "invalid-label.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    with pytest.raises(ValueError, match="is not declared"):
        MODULE.generate(tmp_path / "generated", catalog)


def test_explicit_spans_select_repeated_surface_and_allow_negative_case(tmp_path):
    catalog = tmp_path / "boundaries.json"
    catalog.write_text(
        json.dumps(
            [
                {
                    "id": "repeated-surface",
                    "title": "Repeated surface",
                    "capability": "segmentation_alignment",
                    "phenomenon": "Find the second の.",
                    "labels": ["target"],
                    "documents": [{"eval_document_id": "doc-001", "sentences": ["のとの"]}],
                    **ORACLES,
                    "targets": [
                        {
                            "eval_document_id": "doc-001", "sentence_ordinal": 0,
                            "start_char": 2,
                            "end_char": 3,
                            "surface": "の",
                            "label": "target",
                        }
                    ],
                },
                {
                    "id": "negative-case",
                    "title": "No matches",
                    "capability": "negative_evidence",
                    "phenomenon": "Emit no matches.",
                    "labels": ["target"],
                    "documents": [{"eval_document_id": "doc-001", "sentences": ["対象なし。"]}],
                    **ORACLES,
                    "targets": [],
                },
            ]
        )
    )

    generated = tmp_path / "generated"
    MODULE.generate(generated, catalog)
    repeated = (
        generated / "lingrep" / "repeated-surface" / "tests" / "gold.jsonl"
    ).read_text()
    negative = (generated / "lingrep" / "negative-case" / "tests" / "gold.jsonl").read_text()
    assert json.loads(repeated)["start_char"] == 2
    assert negative == ""


def test_experiment_manifest_requires_explicit_matching_resume(tmp_path):
    directory = tmp_path / "pilot-001"
    manifest = {
        "schema": "lingrep-linguistic-eval.experiment/v2",
        "created_at": "first",
        "repetitions": 3,
        "agent_timeout_sec": 3600,
    }
    RUN.write_experiment_manifest(directory, manifest, resume=False)

    with pytest.raises(FileExistsError, match="pass --resume"):
        RUN.write_experiment_manifest(directory, manifest, resume=False)

    resumed = {**manifest, "created_at": "second"}
    RUN.write_experiment_manifest(directory, resumed, resume=True)
    with pytest.raises(ValueError, match="does not match"):
        RUN.write_experiment_manifest(
            directory, {**resumed, "agent_timeout_sec": 7200}, resume=True
        )


@pytest.mark.parametrize(
    "before,after", [({}, {"image": "/archive"}), ({"image": "/archive"}, {})]
)
def test_resume_uses_image_ids_instead_of_archive_paths(tmp_path, before, after):
    manifest = {"image_ids": {"lingrep": "sha256:first"}, "image_outputs": before}
    RUN.write_experiment_manifest(tmp_path, manifest, resume=False)
    RUN.write_experiment_manifest(
        tmp_path, {**manifest, "image_outputs": after}, resume=True
    )
    with pytest.raises(ValueError, match="does not match"):
        RUN.write_experiment_manifest(
            tmp_path,
            {**manifest, "image_ids": {"lingrep": "sha256:second"}},
            resume=True,
        )


@pytest.mark.parametrize(
    "field",
    [
        "models",
        "cases",
        "protocol",
        "repetitions",
        "agent_timeout_sec",
        "controller_seed",
        "catalog_sha256",
        "image_ids",
    ],
)
def test_resume_rejects_changed_experiment_inputs(tmp_path, field):
    manifest = {field: "original"}
    RUN.write_experiment_manifest(tmp_path, manifest, resume=False)
    with pytest.raises(ValueError, match="does not match"):
        RUN.write_experiment_manifest(tmp_path, {field: "changed"}, resume=True)


def test_experiment_snapshots_private_catalog_and_review_evidence(tmp_path, monkeypatch):
    catalog = tmp_path / "private-catalog.json"
    catalog.write_text("[]\n")
    review = tmp_path / "review"
    review.mkdir()
    (review / "adjudication.json").write_text('{"approved":true}\n')
    experiment = tmp_path / "experiment"

    monkeypatch.setattr(RUN, "validate_review", lambda *args: {})
    recorded = RUN.snapshot_experiment_inputs(experiment, catalog, review)

    assert recorded["review_status"] == "reviewed"
    assert (experiment / "inputs" / "catalog.json").read_bytes() == catalog.read_bytes()
    assert (experiment / "inputs" / "review" / "adjudication.json").read_bytes() == (
        review / "adjudication.json"
    ).read_bytes()
    assert recorded["catalog_snapshot"] == "inputs/catalog.json"
    assert recorded["review_evidence_sha256"] == {
        "adjudication.json": RUN.sha256(review / "adjudication.json")
    }

    catalog.write_text("[1]\n")
    with pytest.raises(ValueError, match="catalog snapshot does not match"):
        RUN.snapshot_experiment_inputs(experiment, catalog, review)

    catalog.write_text("[]\n")
    (review / "comparison.json").write_text("{}\n")
    with pytest.raises(ValueError, match="review snapshot file set does not match"):
        RUN.snapshot_experiment_inputs(experiment, catalog, review)


def test_default_model_is_the_thinking_variant():
    assert RUN.DEFAULT_MODELS == ["qwen3.8-27b-fp8:think"]


def test_discovery_guide_is_tool_neutral():
    guide = (ROOT / "guides" / "analyzer-guide.md").read_text()
    assert "$ANALYZER" in guide
    assert "ling" not in guide
    assert "mecab" not in guide
    assert "lingrep patterns" not in guide
    assert "lookaround" not in guide


def test_experiment_rejects_uncommitted_benchmark_state(monkeypatch):
    monkeypatch.setattr(RUN, "output", lambda _command: " M benchmarks/harbor/run.py")
    with pytest.raises(RuntimeError, match="dirty worktree"):
        RUN.benchmark_commit()


def test_report_emits_each_trial_and_keeps_correctness_separate_from_failure(tmp_path):
    started = datetime(2026, 8, 22, tzinfo=UTC)
    experiment = tmp_path / "pilot-001"
    experiment.mkdir()
    (experiment / "experiment.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.experiment/v2",
                "experiment_id": "pilot-001",
                "models": ["qwen:think"],
                "repetitions": 2,
                "tasks": {
                    "lingrep/lingrep-direct-noun-morphology": {
                        "arm": "lingrep",
                        "case_id": "direct-noun-morphology",
                        "capability": "feature_lookup",
                    }
                },
            }
        )
    )
    trial = {
        "id": "11111111-1111-1111-1111-111111111111",
        "task_name": "lingrep/lingrep-direct-noun-morphology",
        "trial_name": "direct-noun-morphology__abc1234",
        "task_checksum": "checksum",
        "started_at": started.isoformat(),
        "finished_at": (started + timedelta(seconds=901)).isoformat(),
        "environment_setup": {
            "started_at": (started - timedelta(seconds=30)).isoformat(),
            "finished_at": (started - timedelta(seconds=5)).isoformat(),
        },
        "agent_setup": {
            "started_at": (started - timedelta(seconds=5)).isoformat(),
            "finished_at": started.isoformat(),
        },
        "agent_execution": {
            "started_at": started.isoformat(),
            "finished_at": (started + timedelta(seconds=900)).isoformat(),
        },
        "verifier": {
            "started_at": (started + timedelta(seconds=900)).isoformat(),
            "finished_at": (started + timedelta(seconds=915)).isoformat(),
        },
        "agent_info": {
            "name": "terminus-2",
            "model_info": {"name": "qwen-normalized"},
        },
        "agent_result": {
            "n_input_tokens": 10,
            "n_cache_tokens": 3,
            "n_output_tokens": 2,
            "metadata": {"api_request_times_msec": [1000, 2500]},
        },
        "verifier_result": {
            "rewards": {
                "evidence_f1": 1.0,
                "exact": 1.0,
                "format": 1.0,
                "script_valid": 1.0,
                "visible_script_f1": 1.0,
                "visible_script_exact": 1.0,
                "script_exact": 1.0,
            }
        },
        "exception_info": {
            "exception_type": "AgentTimeoutError",
            "exception_message": "timed out",
        },
    }
    first = experiment / "qwen" / "job" / "trial-a"
    second = experiment / "qwen" / "job" / "trial-b"
    first.mkdir(parents=True)
    second.mkdir(parents=True)
    (experiment / "qwen" / "job" / "config.json").write_text(
        json.dumps({"job_name": "job", "n_attempts": 2})
    )
    trial_config = {"agent": {"model_name": "openai/qwen:think"}}
    (first / "config.json").write_text(
        json.dumps({**trial_config, "trial_name": "direct-noun-morphology__abc1234"})
    )
    (first / "result.json").write_text(json.dumps(trial))
    (first / "agent").mkdir()
    lingrep_command = "ling tokenize /data/input.jsonl | python3 /tmp/convert.py\n"
    mecab_command = "mecab /tmp/sentence.txt\n"
    (first / "agent" / "trajectory.json").write_text(
        json.dumps(
            {
                "schema_version": "ATIF-v1.7",
                "steps": [
                    {
                        "step_id": 1,
                        "source": "user",
                        "timestamp": started.isoformat(),
                    },
                    {
                        "step_id": 2,
                        "source": "agent",
                        "timestamp": (started + timedelta(seconds=40)).isoformat(),
                        "tool_calls": [
                            {
                                "tool_call_id": "lingrep",
                                "function_name": "bash_command",
                                "arguments": {"keystrokes": lingrep_command},
                            },
                            {
                                "tool_call_id": "mecab",
                                "function_name": "bash_command",
                                "arguments": {"keystrokes": mecab_command},
                            },
                        ],
                    }
                ],
            }
        )
    )
    (first / "agent" / "recording.cast").write_text(
        "\n".join(
            [
                json.dumps({"version": 3}),
                json.dumps(
                    [
                        1.0,
                        "i",
                        lingrep_command,
                    ]
                ),
                json.dumps([2.0, "o", "done\n\u001b[?2004h"]),
                json.dumps([5.0, "i", mecab_command]),
                json.dumps([3.0, "o", "EOS\n\u001b[?2004h"]),
            ]
        )
        + "\n"
    )
    runtime = first / "agent" / "tool-invocations" / "lingrep-runtime"
    runtime.mkdir(parents=True)
    runtime_schema = "lingrep-linguistic-eval.tool-invocation/v1"
    (runtime / "start.json").write_text(
        json.dumps(
            {
                "schema": runtime_schema,
                "phase": "start",
                "invocation_id": "lingrep-runtime",
                "tool": "ling",
                "argv": ["ling", "tokenize", "/data/input.jsonl"],
                "cwd": "/app",
                "real_executable": "/nix/store/example/bin/ling",
                "launcher_pid": 10,
                "parent_pid": 9,
                "patterns": [],
            }
        )
    )
    (runtime / "spawn.json").write_text(
        json.dumps(
            {
                "schema": runtime_schema,
                "phase": "spawn",
                "invocation_id": "lingrep-runtime",
                "started_at_unix_ns": 10_000_000_000,
                "child_pid": 11,
            }
        )
    )
    (runtime / "finish.json").write_text(
        json.dumps(
            {
                "schema": runtime_schema,
                "phase": "finish",
                "invocation_id": "lingrep-runtime",
                "finished_at_unix_ns": 12_000_000_000,
                "exit_code": 0,
                "signal": None,
                "spawn_error": None,
            }
        )
    )
    submission = first / "artifacts" / "logs" / "artifacts" / "submission"
    submission.mkdir(parents=True)
    query_source = "target := [upos=NOUN].\n"
    (submission / "query.lx").write_text(query_source)
    runner_source = "#!/bin/bash\nling matches \"$1\" > \"$2\"\n"
    (submission / "run-analysis").write_text(runner_source)
    trial["id"] = "22222222-2222-2222-2222-222222222222"
    trial["trial_name"] = "direct-noun-morphology__def5678"
    trial["started_at"] = (started + timedelta(seconds=1000)).isoformat()
    trial["finished_at"] = (started + timedelta(seconds=1010)).isoformat()
    trial["agent_execution"] = None
    trial["exception_info"] = None
    (second / "config.json").write_text(
        json.dumps({**trial_config, "trial_name": "direct-noun-morphology__def5678"})
    )
    (second / "result.json").write_text(json.dumps(trial))

    rows = REPORT.summarize_experiment(experiment)
    assert len(rows) == 2
    row = rows[0]
    assert row["exact"] == 1.0
    assert row["operational_success"] is False
    assert row["trial_elapsed_seconds"] == 901
    assert row["agent_elapsed_seconds"] == 900
    assert row["arm"] == "lingrep"
    assert row["capability"] == "feature_lookup"
    assert row["model"] == "qwen:think"
    assert row["job_name"] == "job"
    assert row["reported_model"] == "qwen-normalized"
    assert row["input_tokens"] == 10
    assert row["cached_input_tokens"] == 3
    assert row["output_tokens"] == 2
    assert row["model_api_requests"] == 2
    assert row["model_api_seconds"] == 3.5
    assert row["model_api_request_seconds_max"] == 2.5
    assert row["phase_seconds"] == {
        "environment_setup": 25.0,
        "agent_setup": 5.0,
        "agent_execution": 900.0,
        "verifier": 15.0,
    }
    assert row["longest_step_gap"] == {"seconds": 40.0, "before_step_id": 2}
    assert row["command_analysis"] == {
        "bash_tool_calls": 2,
        "nonempty_bash_tool_calls": 2,
        "shell_commands": 3,
        "parse_error_calls": 0,
        "timed_calls": 2,
        "unaligned_calls": 0,
        "bash_call_counts_by_shell_category": {"mecab": 1, "lingrep+python": 1},
        "bash_roundtrip_seconds_by_shell_category": {
            "mecab": 3.0,
            "lingrep+python": 2.0,
        },
        "executable_counts": {"mecab": 1, "python3": 1, "ling": 1},
        "lingrep_operation_counts": {"tokenize": 1},
        "pattern_catalog_uses": {},
        "pattern_lifecycle": {},
        "runtime_telemetry": "available",
        "runtime_tool_invocation_counts": {"ling": 1},
        "runtime_incomplete_invocations": 0,
        "runtime_tool_process_seconds": {"ling": 2.0},
        "runtime_tool_active_wall_seconds": {"ling": 2.0},
        "tool_workflows": {
            "mecab": {
                "direct_shell_commands": 1,
                "direct_shell_pipeline_calls": 0,
                "executed_python_scripts": [],
                "mode": "direct_cli",
                "runtime_invocations": 0,
            },
            "ling": {
                "direct_shell_commands": 1,
                "direct_shell_pipeline_calls": 1,
                "executed_python_scripts": [],
                "mode": "direct_cli",
                "runtime_invocations": 1,
            },
        },
    }
    assert row["runtime_invocations"][0]["status"] == "complete"
    assert row["runtime_invocations"][0]["elapsed_seconds"] == 2.0
    assert row["pattern_queries"] == [
        {
            "path": "query.lx",
            "sha256": "807f082f521acf56ec277ef1f6ab022c5979418ed4588fe784e587cf4146cc19",
            "source": query_source,
        }
    ]
    assert row["analysis_runner"] == {
        "path": "run-analysis",
        "sha256": "dadf1375301505108ddc30fa652442a59684086429adceaff3d1fa830eec5d87",
        "size_bytes": len(runner_source.encode()),
        "source": runner_source,
    }
    assert row["query_valid"] is None
    assert row["visible_query_f1"] is None
    assert row["hidden_query_f1"] is None
    assert row["query_exact"] is None
    assert row["script_valid"] == 1.0
    assert row["visible_script_f1"] == 1.0
    assert row["hidden_script_f1"] is None
    assert row["script_exact"] == 1.0
    assert row["verifier_outcomes"] is None
    assert row["agent_other_seconds"] == 891.5
    assert [row["attempt"] for row in rows] == [1, 2]

    assert rows[0]["artifact_complexity"] == {
        "run_analysis": {
            "bytes": len(runner_source.encode()),
            "non_blank_lines": 2,
            "language": "shell",
        },
        "query": {
            "bytes": len(query_source.encode()),
            "non_blank_lines": 1,
            "definitions": 1,
            "attribute_keys": ["upos"],
        },
    }
    assert rows[1]["artifact_complexity"] == {"run_analysis": None, "query": None}

    (experiment / "inputs").mkdir()
    reference_case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    reference_case["id"] = "direct-noun-morphology"
    (experiment / "inputs" / "catalog.json").write_text(
        json.dumps([reference_case], ensure_ascii=False)
    )
    reference_runner = (ROOT / "oracle_lingrep_runner.py").read_text()
    summaries = REPORT.summarize_groups(
        rows, json.loads((experiment / "experiment.json").read_text()), experiment
    )
    assert summaries == [
        {
            "schema": "lingrep-linguistic-eval.group-report/v1",
            "experiment_id": "pilot-001",
            "model": "qwen:think",
            "arm": "lingrep",
            "trials": 2,
            "planned_trials": 2,
            "matrix_complete": True,
            "operational_successes": 1,
            "infrastructure_failures": 0,
            "exact_trials": 2.0,
            "verdict_correct_trials": None,
            "mean_evidence_f1": 1.0,
            "query_replay_trials": 0,
            "query_valid_trials": 0,
            "query_exact_trials": 0,
            "mean_visible_query_f1": None,
            "mean_hidden_query_f1": None,
            "hidden_query_replay_trials": 0,
            "hidden_query_scored_trials": 0,
            "mean_hidden_query_f1_when_scored": None,
            "script_replay_trials": 2,
            "script_valid_trials": 2,
            "script_exact_trials": 2,
            "mean_visible_script_f1": 1.0,
            "mean_hidden_script_f1": None,
            "hidden_script_replay_trials": 0,
            "hidden_script_scored_trials": 0,
            "mean_hidden_script_f1_when_scored": None,
            "verifier_outcome_trials": 0,
            "dimension_outcomes": {},
            "median_trial_elapsed_seconds": 455.5,
            "input_tokens_total": 20,
            "input_tokens_median": 10.0,
            "cached_input_tokens_total": 6,
            "output_tokens_total": 4,
            "output_tokens_median": 2.0,
            "trials_with_pattern_queries": 1,
            "pattern_query_artifacts": 1,
            "analysis_runner_artifacts": 1,
            "artifact_complexity_median": {
                "run_analysis": {
                    "bytes": len(runner_source.encode()),
                    "non_blank_lines": 2,
                    "ast_nodes": None,
                    "branches": None,
                },
                "query": {
                    "bytes": len(query_source.encode()),
                    "non_blank_lines": 1,
                    "definitions": 1,
                },
            },
            "reference_artifact_complexity_median": {
                "run_analysis": {
                    "bytes": len(reference_runner.encode()),
                    "non_blank_lines": REPORT.non_blank_lines(reference_runner),
                    "ast_nodes": len(list(ast.walk(ast.parse(reference_runner)))),
                    "branches": 6,
                },
                "query": {
                    "bytes": len(reference_case["oracle_query"].encode()),
                    "non_blank_lines": 1,
                    "definitions": 1,
                },
            },
            "model_api_requests": 4,
            "model_api_seconds": 7.0,
            "agent_elapsed_seconds": 900.0,
            "agent_other_seconds": 891.5,
            "model_api_request_seconds_max": 2.5,
            "step_gap_seconds_max": 40.0,
            "verifier_seconds": 30.0,
            "bash_tool_calls": 2,
            "shell_commands": 3,
            "shell_parse_error_calls": 0,
            "timed_shell_calls": 2,
            "unaligned_shell_calls": 0,
            "bash_call_counts_by_shell_category": {
                "mecab": 1,
                "lingrep+python": 1,
            },
            "bash_roundtrip_seconds_by_shell_category": {
                "mecab": 3.0,
                "lingrep+python": 2.0,
            },
            "shell_executable_counts": {
                "mecab": 1,
                "python3": 1,
                "ling": 1,
            },
            "lingrep_operation_counts": {"tokenize": 1},
            "runtime_telemetry_trials": 1,
            "runtime_incomplete_invocations": 0,
            "runtime_tool_invocation_counts": {"ling": 1},
            "runtime_tool_process_seconds": {"ling": 2.0},
            "runtime_tool_active_wall_seconds": {"ling": 2.0},
        }
    ]


def test_artifact_complexity_measures_python_shell_and_query_sources():
    python_runner = (
        "#!/usr/bin/env python3\n"
        "import sys\n\n"
        "for line in open(sys.argv[1]):\n"
        "    if line.strip():\n"
        "        print(line)\n"
    )
    measures = REPORT.runner_complexity(python_runner, len(python_runner.encode()))
    assert measures["language"] == "python"
    assert measures["non_blank_lines"] == 5
    assert measures["branches"] == 2
    assert measures["ast_nodes"] == len(list(ast.walk(ast.parse(python_runner))))

    shell_runner = "#!/bin/bash\nset -euo pipefail\n\nmecab < \"$1\" | python3 select.py > \"$2\"\n"
    assert REPORT.runner_complexity(shell_runner, 7) == {
        "bytes": 7,
        "non_blank_lines": 3,
        "language": "shell",
    }
    assert REPORT.runner_complexity(None, 2_000_000) == {
        "bytes": 2_000_000,
        "non_blank_lines": None,
        "language": "other",
    }

    query = (
        "# ヲ格 collocation\n"
        "set CONTENT := {動詞, 助動詞}.\n"
        "fragment ARU := [text in {ある, 有る}].\n"
        "wo :=\n"
        "  (?<c> [unidic.pos1 in {名詞, 代名詞}] [unidic.pos1=接尾辞 & unidic.pos2=名詞的]*)\n"
        "  [text=を] [unidic.pos3!=サ変可能 & lemma~^為]{0,5} (@ARU | [unidic.pos1 not in @CONTENT]).\n"
        'lexicon passive by [text] := {"承認された"}.\n'
    )
    assert REPORT.query_complexity(query) == {
        "bytes": len(query.encode()),
        "non_blank_lines": 7,
        "definitions": 4,
        "attribute_keys": ["lemma", "text", "unidic.pos1", "unidic.pos2", "unidic.pos3"],
    }


def test_viewer_export_co_locates_trials_without_packaging_gold(tmp_path):
    task = tmp_path / "generated" / "lingrep" / "case"
    (task / "environment").mkdir(parents=True)
    (task / "tests").mkdir()
    (task / "solution").mkdir()
    (task / "instruction.md").write_text("# Japanese task\n")
    (task / "task.toml").write_text('[metadata]\narm = "lingrep"\n')
    (task / "environment" / "input.json").write_text('{"sentences":[]}\n')
    (task / "tests" / "gold.jsonl").write_text("secret gold\n")
    (task / "solution" / "solve.sh").write_text("secret solution\n")

    experiment = tmp_path / "experiment"
    experiment.mkdir()
    (experiment / "experiment.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.experiment/v2",
                "experiment_id": "probe",
                "repetitions": 1,
                "tasks": {
                    "lingrep/lingrep-case": {
                        "arm": "lingrep",
                        "case_id": "case",
                        "capability": "feature_lookup",
                    }
                },
            }
        )
    )
    trial = experiment / "model" / "job" / "case__attempt"
    (trial / "agent").mkdir(parents=True)
    (trial / "verifier").mkdir()
    (trial / "artifacts" / "logs" / "artifacts" / "submission").mkdir(parents=True)
    (experiment / "model" / "job" / "config.json").write_text(
        json.dumps({"job_name": "job", "n_attempts": 1})
    )
    (trial / "config.json").write_text(
        json.dumps({"task": {"path": str(task)}, "trial_name": "case__attempt"})
    )
    (trial / "agent" / "trajectory.json").write_text(
        json.dumps({"schema_version": "ATIF-v1.7", "steps": []})
    )
    (trial / "verifier" / "test-stdout.txt").write_text("passed\n")
    (trial / "artifacts" / "logs" / "artifacts" / "submission" / "query.lx").write_text(
        "target := [upos=NOUN].\n"
    )
    (trial / "analysis.json").write_text('{"summary":"reviewed"}\n')
    (trial / "result.json").write_text(
        json.dumps(
            {
                "trial_name": "case__attempt",
                "task_name": "lingrep/lingrep-case",
                "verifier_result": {"rewards": {"exact": 1.0}},
            }
        )
    )

    output = tmp_path / "viewer.zip"
    assert EXPORT.export_experiment(experiment, output) == 1
    with zipfile.ZipFile(output) as archive:
        names = set(archive.namelist())
        assert "lingrep-case/instruction.md" in names
        assert "lingrep-case/environment/input.json" in names
        assert "lingrep-case/jobs/case__attempt/agent/trajectory.json" in names
        assert "lingrep-case/jobs/case__attempt/result.json" in names
        assert "lingrep-case/jobs/case__attempt/verifier/test-stdout.txt" in names
        assert "lingrep-case/jobs/case__attempt/analysis.json" in names
        assert "lingrep-case/jobs/case__attempt/command-analysis.json" in names
        assert "lingrep-case/jobs/case__attempt/artifacts/logs/artifacts/submission/query.lx" in names
        assert not any("gold" in name or "/solution/" in name for name in names)


def test_viewer_export_rejects_non_atif_trials(tmp_path):
    trajectory = tmp_path / "trajectory.json"
    trajectory.write_text(json.dumps({"messages": []}))
    with pytest.raises(ValueError, match="requires an ATIF-v1.x trajectory"):
        EXPORT.validate_trajectory(trajectory)


@pytest.mark.parametrize("mecab_source", [None, {"url": "other", "hash": "different"}])
def test_dictionary_sources_reject_missing_or_mismatched_metadata(monkeypatch, mecab_source):
    source = {"url": "pinned UniDic source", "hash": "pinned source hash"}

    def inspect(command, **kwargs):
        bound_source = source if command[3] == "lingrep-image" else mecab_source
        return json.dumps({"org.lingrep.unidic-source": json.dumps(bound_source)})

    monkeypatch.setattr(RUN, "output", inspect)
    with pytest.raises(ValueError, match="dictionary source.*lingrep.*mecab"):
        RUN.dictionary_sources({"lingrep": "lingrep-image", "mecab": "mecab-image"})


def test_dictionary_sources_are_read_from_both_pinned_images(monkeypatch):
    source = {"url": "pinned UniDic source", "hash": "pinned source hash"}
    commands = []

    def inspect(command, **kwargs):
        commands.append(command)
        return json.dumps({"org.lingrep.unidic-source": json.dumps(source)})

    monkeypatch.setattr(RUN, "output", inspect)
    sources = RUN.dictionary_sources({"lingrep": "sha256:first", "mecab": "sha256:second"})
    assert sources == {"lingrep": source, "mecab": source}
    assert [command[3] for command in commands] == ["sha256:first", "sha256:second"]


def test_run_rejects_dictionary_mismatch_before_any_trial_command(monkeypatch, tmp_path):
    catalog = tmp_path / "catalog.json"
    catalog.write_text("[]")
    monkeypatch.setattr(RUN, "ROOT", tmp_path)
    monkeypatch.setattr(RUN, "benchmark_commit", lambda: "committed")
    monkeypatch.setattr(sys, "argv", [
        "run.py", "--api-base", "http://unused", "--no-build", "--catalog", str(catalog),
    ])
    monkeypatch.setenv("PATH", "/bin")
    monkeypatch.setenv("OPENAI_API_KEY", "test")

    def inspect(command, **kwargs):
        if command[:3] == ["uv", "run", "--frozen"]:
            return json.dumps({"version": "stubbed", "commit": "0" * 40})
        if command[-1] == "{{.Id}}":
            return "sha256:" + command[3].split("-")[0]
        source = {"url": command[3], "hash": "different source for each arm"}
        return json.dumps({"org.lingrep.unidic-source": json.dumps(source)})

    def unexpected_command(*args, **kwargs):
        pytest.fail("dataset generation, oracle replay, and trials must not start")

    monkeypatch.setattr(RUN, "output", inspect)
    monkeypatch.setattr(RUN, "run", unexpected_command)
    with pytest.raises(ValueError, match="dictionary source mismatch"):
        RUN.main()


@pytest.mark.parametrize("unavailable,invalid", [(False, True), (True, False)])
def test_invalid_oracle_stops_before_docker(
    tmp_path, monkeypatch, unavailable, invalid
):
    MODULE.generate(
        tmp_path / "generated", ROOT / "catalogs" / "smoke.json", protocol="catalog-authoring"
    )
    commands = []

    def execute(command, **kwargs):
        commands.append(command)
        assert command[0] == "ling", "Docker ran before catalog validation"
        if invalid:
            raise ORACLE.subprocess.CalledProcessError(
                2, command, stderr="invalid catalog"
            )
        return ORACLE.subprocess.CompletedProcess(
            command,
            0,
            stdout=json.dumps(
                {
                    "data": {
                        "binding": {
                            "unavailable": [{"attribute": "missing"}]
                            if unavailable
                            else []
                        }
                    }
                }
            ),
        )

    monkeypatch.setattr(ORACLE.subprocess, "run", execute)
    with pytest.raises(RuntimeError, match="oracle catalog"):
        ORACLE.preflight(tmp_path / "generated")
    assert commands


def test_report_and_export_do_not_dereference_submission_symlinks(tmp_path):
    marker = tmp_path / "host-marker.lx"
    marker.write_text("host := [upos=NOUN].\n")
    trial = tmp_path / "trial"
    submission = trial / "artifacts" / "logs" / "artifacts" / "submission"
    submission.mkdir(parents=True)
    (submission / "query.lx").symlink_to(marker)
    (submission / "run-analysis").symlink_to(marker)
    (submission / "own.lx").write_text("own := [upos=VERB].\n")

    queries = REPORT.pattern_query_artifacts(trial)
    assert [query["path"] for query in queries] == ["own.lx"]
    assert REPORT.analysis_runner_artifact(trial) is None

    bundle = tmp_path / "bundle.zip"
    with zipfile.ZipFile(bundle, "x") as archive:
        EXPORT.add_tree(archive, trial / "artifacts", "artifacts")
        EXPORT.add_file(archive, submission / "query.lx", "query.lx")
    with zipfile.ZipFile(bundle) as archive:
        assert archive.namelist() == ["artifacts/logs/artifacts/submission/own.lx"]


def test_submission_is_read_from_harbor_collected_artifacts(tmp_path):
    collected = tmp_path / "collected" / "artifacts" / "logs" / "artifacts" / "submission"
    collected.mkdir(parents=True)
    (collected / "query.lx").write_text("own := [upos=VERB].\n")
    (collected / "run-analysis").write_text("#!/bin/sh\n")
    assert [query["path"] for query in REPORT.pattern_query_artifacts(tmp_path / "collected")] == [
        "query.lx"
    ]
    assert REPORT.analysis_runner_artifact(tmp_path / "collected")["path"] == "run-analysis"
    assert REPORT.pattern_query_artifacts(tmp_path / "none") == []
    assert REPORT.analysis_runner_artifact(tmp_path / "none") is None


def test_verifier_budget_covers_every_declared_replay(tmp_path):
    MODULE.generate(
        tmp_path, ROOT / "catalogs" / "worked-example.json", protocol="catalog-authoring"
    )
    replay = MODULE.REPLAY_TIMEOUT_SEC
    overhead = MODULE.VERIFIER_OVERHEAD_SEC
    budgets = {}
    for arm in ("lingrep", "mecab"):
        for task in sorted((tmp_path / arm).iterdir()):
            config = tomllib.loads((task / "task.toml").read_text())
            budgets[(arm, task.name)] = config["verifier"]["timeout_sec"]
            script = json.loads((task / "tests" / "analysis-replay.json").read_text())
            assert script["replay_timeout_sec"] == replay
            query_config = task / "tests" / "query-replay.json"
            if query_config.exists():
                assert json.loads(query_config.read_text())["replay_timeout_sec"] == replay
    assert budgets[("lingrep", "worked-sentence-final-query")] == overhead + 4 * replay
    assert budgets[("mecab", "worked-sentence-final-query")] == overhead + 2 * replay
    assert budgets[("lingrep", "worked-adverb-feature-lookup")] == overhead + 2 * replay
    assert budgets[("mecab", "worked-adverb-feature-lookup")] == overhead + replay

    task = tmp_path / "lingrep" / "worked-sentence-final-query"
    assert ORACLE.replay_dimensions(task) == (True, True)
    toml = task / "task.toml"
    toml.write_text(toml.read_text().replace("timeout_sec = 150.0", "timeout_sec = 60.0"))
    with pytest.raises(RuntimeError, match="verifier timeout"):
        ORACLE.replay_dimensions(task)


def write_trial(experiment, model, job, name, result, *, outcomes=None):
    trial = experiment / model / job / name
    trial.mkdir(parents=True)
    job_config = experiment / model / job / "config.json"
    if not job_config.exists():
        job_config.write_text(json.dumps({"job_name": job, "n_attempts": 1}))
    (trial / "config.json").write_text(
        json.dumps({"agent": {"model_name": f"openai/{model}"}, "trial_name": name})
    )
    (trial / "result.json").write_text(json.dumps({"trial_name": name, **result}))
    if outcomes is not None:
        (trial / "verifier").mkdir()
        (trial / "verifier" / "outcome.json").write_text(
            json.dumps(
                {
                    "schema": "lingrep-linguistic-eval.verifier-outcome/v1",
                    "dimensions": outcomes,
                }
            )
        )
    return trial


def dimension_outcomes(**named):
    dimensions = {
        name: {"applicable": False, "outcome": "not_applicable"}
        for name in ("evidence", "query_visible", "query_hidden", "script_visible", "script_hidden")
    }
    for name, outcome in named.items():
        dimensions[name] = {"applicable": True, "outcome": outcome}
    return dimensions


def test_hidden_mean_keeps_a_failed_visible_replay_in_its_denominator(tmp_path):
    experiment = tmp_path / "pilot"
    experiment.mkdir()
    task_name = "lingrep/mecab-worked-sentence-final-query"
    (experiment / "experiment.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.experiment/v2",
                "experiment_id": "pilot",
                "models": ["qwen"],
                "repetitions": 2,
                "tasks": {
                    task_name: {
                        "arm": "mecab",
                        "case_id": "worked-sentence-final-query",
                        "capability": "query_authoring",
                    }
                },
            }
        )
    )
    failed = {
        "task_name": task_name,
        "started_at": "2026-08-22T00:00:00",
        "verifier_result": {
            "rewards": {
                "evidence_f1": 1.0,
                "exact": 1.0,
                "format": 1.0,
                "precision": 1.0,
                "recall": 1.0,
                "script_valid": 0.0,
                "visible_script_f1": 0.0,
                "visible_script_exact": 0.0,
                "hidden_script_f1": 0.0,
                "hidden_script_exact": 0.0,
                "script_exact": 0.0,
            }
        },
    }
    perfect = {
        **failed,
        "started_at": "2026-08-22T01:00:00",
        "verifier_result": {
            "rewards": {
                **failed["verifier_result"]["rewards"],
                "script_valid": 1.0,
                "visible_script_f1": 1.0,
                "visible_script_exact": 1.0,
                "hidden_script_f1": 1.0,
                "hidden_script_exact": 1.0,
                "script_exact": 1.0,
            }
        },
    }
    write_trial(
        experiment,
        "qwen",
        "worked-sentence-final-query-mecab",
        "worked-sentence-final-query__a",
        failed,
        outcomes=dimension_outcomes(
            evidence="scored",
            script_visible="invalid_submission",
            script_hidden="not_reached",
        ),
    )
    write_trial(
        experiment,
        "qwen",
        "worked-sentence-final-query-mecab",
        "worked-sentence-final-query__b",
        perfect,
        outcomes=dimension_outcomes(
            evidence="scored", script_visible="scored", script_hidden="scored"
        ),
    )

    rows = REPORT.summarize_experiment(experiment)
    assert [row["verifier_outcomes"]["script_hidden"]["outcome"] for row in rows] == [
        "not_reached",
        "scored",
    ]
    summary = REPORT.summarize_groups(
        rows, json.loads((experiment / "experiment.json").read_text())
    )[0]
    assert summary["hidden_script_replay_trials"] == 2
    assert summary["hidden_script_scored_trials"] == 1
    assert summary["mean_hidden_script_f1"] == 0.5
    assert summary["mean_hidden_script_f1_when_scored"] == 1.0
    assert summary["verifier_outcome_trials"] == 2
    assert summary["dimension_outcomes"]["script_visible"] == {
        "invalid_submission": 1,
        "scored": 1,
    }
    assert summary["dimension_outcomes"]["script_hidden"] == {
        "not_reached": 1,
        "scored": 1,
    }


def write_manifest(experiment, tasks, *, models=("qwen",), repetitions=1):
    experiment.mkdir(parents=True, exist_ok=True)
    (experiment / "experiment.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.experiment/v2",
                "repetitions": repetitions,
                "experiment_id": experiment.name,
                "models": list(models),
                "tasks": tasks,
            }
        )
    )


PAIRED_TASKS = {
    "lingrep/lingrep-case": {"arm": "lingrep", "case_id": "case", "capability": "feature_lookup"},
    "lingrep/mecab-case": {"arm": "mecab", "case_id": "case", "capability": "feature_lookup"},
}


def harbor_result(task_name, started="2026-08-22T00:00:00"):
    return {
        "task_name": task_name,
        "started_at": started,
        "finished_at": started,
        "verifier_result": {"rewards": {"evidence_f1": 1.0, "exact": 1.0, "format": 1.0}},
    }


def test_trials_are_discovered_through_the_job_layout_not_submission_artifacts(tmp_path):
    experiment = tmp_path / "probe"
    write_manifest(experiment, PAIRED_TASKS)
    trial = write_trial(
        experiment, "qwen", "case-lingrep", "case__a", harbor_result("lingrep/lingrep-case")
    )
    (experiment / "qwen" / "case-lingrep" / "result.json").write_text(
        json.dumps({"id": "job", "n_total_trials": 1, "stats": {}})
    )
    submission = trial / "artifacts" / "logs" / "artifacts" / "submission"
    submission.mkdir(parents=True)
    (submission / "result.json").write_text("{not json")
    (experiment / "inputs").mkdir()
    (experiment / "inputs" / "result.json").write_text("{not json either")

    rows = REPORT.summarize_experiment(experiment)
    assert [(row["job_name"], row["trial_name"]) for row in rows] == [
        ("case-lingrep", "case__a")
    ]


def test_verifier_infrastructure_failure_excludes_the_trial_everywhere(tmp_path):
    experiment = tmp_path / "pilot"
    write_manifest(experiment, PAIRED_TASKS, repetitions=1)
    zeroed = {
        "evidence_f1": 0.0,
        "exact": 0.0,
        "format": 0.0,
        "script_valid": 0.0,
        "visible_script_f1": 0.0,
        "visible_script_exact": 0.0,
        "hidden_script_f1": 0.0,
        "hidden_script_exact": 0.0,
        "script_exact": 0.0,
    }
    write_trial(
        experiment,
        "qwen",
        "case-lingrep",
        "case__a",
        {**harbor_result("lingrep/lingrep-case"), "verifier_result": {"rewards": zeroed}},
        outcomes=dimension_outcomes(
            evidence="infrastructure_failure",
            script_visible="infrastructure_failure",
            script_hidden="infrastructure_failure",
        ),
    )
    write_trial(
        experiment,
        "qwen",
        "case-mecab",
        "case__b",
        {
            **harbor_result("lingrep/mecab-case", "2026-08-22T01:00:00"),
            "verifier_result": {
                "rewards": {**zeroed, "visible_script_f1": 1.0, "hidden_script_f1": 1.0}
            },
        },
        outcomes=dimension_outcomes(
            evidence="scored", script_visible="scored", script_hidden="scored"
        ),
    )
    manifest = json.loads((experiment / "experiment.json").read_text())

    write_trial(
        experiment,
        "qwen",
        "case-lingrep",
        "case__c",
        {
            **harbor_result("lingrep/lingrep-case", "2026-08-22T02:00:00"),
            "verifier_result": {"rewards": {**zeroed, "hidden_script_f1": 1.0}},
        },
        outcomes=dimension_outcomes(
            evidence="infrastructure_failure", script_visible="scored", script_hidden="scored"
        ),
    )

    rows = REPORT.summarize_experiment(experiment)
    assert [row["infrastructure_failure"] for row in rows] == [True, False, True]
    summaries = {summary["arm"]: summary for summary in REPORT.summarize_groups(rows, manifest)}
    assert summaries["lingrep"]["infrastructure_failures"] == 2
    assert summaries["lingrep"]["mean_hidden_script_f1"] is None
    assert summaries["lingrep"]["hidden_script_scored_trials"] == 0
    assert summaries["lingrep"]["mean_hidden_script_f1_when_scored"] is None
    assert summaries["lingrep"]["matrix_complete"] is False
    assert summaries["mecab"]["mean_hidden_script_f1"] == 1.0
    assert summaries["mecab"]["mean_hidden_script_f1_when_scored"] == 1.0
    paired = REPORT.paired_comparison(rows, manifest, resamples=10)
    assert paired and all(comparison["case_differences"] == {} for comparison in paired)
    assert all(
        comparison["excluded_trials"] == {"lingrep": 2, "mecab": 0} for comparison in paired
    )
    record = REPORT.reconcile_experiment(experiment)
    cells = {cell["task_name"]: cell for cell in record["cells"]}
    assert cells["lingrep/lingrep-case"]["infrastructure_failures"] == 2
    assert cells["lingrep/lingrep-case"]["status"] == "partial"
    assert cells["lingrep/mecab-case"]["status"] == "complete"
    assert record["complete"] is False


def test_reconciliation_reports_missing_surplus_and_incomplete_cells(tmp_path):
    experiment = tmp_path / "pilot"
    write_manifest(experiment, PAIRED_TASKS, repetitions=2)

    empty = REPORT.reconcile_experiment(experiment)
    assert empty["schema"] == "lingrep-linguistic-eval.experiment-reconciliation/v2"
    assert (empty["planned_trials"], empty["observed_trials"], empty["complete"]) == (
        4,
        0,
        False,
    )
    assert {cell["status"] for cell in empty["cells"]} == {"missing"}

    for suffix, started in (("a", "T00"), ("b", "T01"), ("c", "T02")):
        write_trial(
            experiment,
            "qwen",
            "case-lingrep",
            f"case__{suffix}",
            harbor_result("lingrep/lingrep-case", f"2026-08-22{started}:00:00"),
        )
    stalled = experiment / "qwen" / "case-mecab" / "case__d"
    stalled.mkdir(parents=True)
    (experiment / "qwen" / "case-mecab" / "config.json").write_text(
        json.dumps({"job_name": "case-mecab", "n_attempts": 2})
    )
    (stalled / "config.json").write_text(json.dumps({"trial_name": "case__d"}))

    record = REPORT.reconcile_experiment(experiment)
    cells = {(cell["model"], cell["task_name"]): cell for cell in record["cells"]}
    assert cells[("qwen", "lingrep/lingrep-case")]["status"] == "surplus"
    assert cells[("qwen", "lingrep/lingrep-case")]["trial_names"] == [
        "case__a",
        "case__b",
        "case__c",
    ]
    assert cells[("qwen", "lingrep/mecab-case")]["status"] == "missing"
    assert record["incomplete_trials"] == [
        {"job_name": "case-mecab", "trial_name": "case__d", "error": "no result.json"}
    ]
    assert record["complete"] is False

    rows = REPORT.summarize_experiment(experiment)
    assert [row["attempt"] for row in rows] == [1, 2, 3]
    summaries = {
        summary["arm"]: summary
        for summary in REPORT.summarize_groups(rows, json.loads((experiment / "experiment.json").read_text()))
    }
    assert summaries["lingrep"]["planned_trials"] == 2
    assert summaries["lingrep"]["trials"] == 3
    assert summaries["lingrep"]["matrix_complete"] is False
    assert "mecab" not in summaries


def test_report_rejects_a_manifest_of_another_schema(tmp_path):
    experiment = tmp_path / "old"
    write_manifest(experiment, PAIRED_TASKS)
    manifest = json.loads((experiment / "experiment.json").read_text())
    manifest["schema"] = "lingrep-linguistic-eval.experiment/v1"
    (experiment / "experiment.json").write_text(json.dumps(manifest))
    with pytest.raises(ValueError, match="unsupported experiment manifest schema"):
        REPORT.reconcile_experiment(experiment)


def harbor_jobs(commands):
    jobs = []
    for command in commands:
        if command[:4] != ["uv", "run", "--frozen", "harbor"]:
            continue
        task = Path(command[command.index("-p") + 1])
        jobs.append((task.parent.name, task.name, command[command.index("--job-name") + 1]))
        assert command[command.index("--n-attempts") + 1] == "1"
        assert command[command.index("--env") + 1] == "podman"
    return jobs


def test_repetitions_run_as_alternating_per_case_arm_pairs(monkeypatch, tmp_path):
    commands = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e1", "--repetitions", "3"]
    )
    RUN.main()
    manifest = json.loads((tmp_path / "jobs" / "e1" / "experiment.json").read_text())
    assert manifest["repetitions"] == 3 and "attempts" not in manifest

    jobs = harbor_jobs(commands)
    cases = [case["id"] for case in json.loads((tmp_path / "catalogs" / "smoke.json").read_text())]
    assert len(jobs) == 3 * 2 * len(cases)
    for index, case in enumerate(cases):
        block = jobs[index * 6 : (index + 1) * 6]
        assert {job[1] for job in block} == {case}
        arms = [job[0] for job in block]
        first, second = arms[0], arms[1]
        assert {first, second} == {"lingrep", "mecab"}
        assert arms == [first, second, second, first, first, second]
        assert [job[2] for job in block] == [
            f"{case}-{arm}-r{repetition}"
            for repetition, pair in enumerate(([first, second], [second, first], [first, second]), 1)
            for arm in pair
        ]

    repeated = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e2", "--repetitions", "3"]
    )
    RUN.main()
    assert harbor_jobs(repeated) == jobs


def controller_environment(monkeypatch, tmp_path, *, argv):
    (tmp_path / "catalogs").mkdir(exist_ok=True)
    (tmp_path / "guides").mkdir(exist_ok=True)
    (tmp_path / "verifier").mkdir(exist_ok=True)
    for name in (
        "catalogs/smoke.json",
        "guides/analyzer-guide.md",
        "verifier/verify.py",
        "oracle_lingrep_runner.py",
        "mecab_reference.py",
    ):
        (tmp_path / name).write_bytes((ROOT / name).read_bytes())
    monkeypatch.setattr(RUN, "ROOT", tmp_path)
    monkeypatch.setattr(MODULE, "ROOT", tmp_path)
    monkeypatch.setattr(RUN, "benchmark_commit", lambda: "committed")
    monkeypatch.setenv("PATH", "/bin")
    monkeypatch.setenv("OPENAI_API_KEY", "test")
    source = {"url": "pinned", "hash": "pinned"}

    def inspect(command, **kwargs):
        if command == ["podman", "--version"]:
            return "podman version stubbed"
        if command[:3] == ["uv", "run", "--frozen"]:
            return json.dumps({"version": "stubbed", "commit": "0" * 40})
        if command[-1] == "{{.Id}}":
            return "sha256:" + command[3].split("-")[0].ljust(64, "0")
        return json.dumps({"org.lingrep.unidic-source": json.dumps(source)})

    commands = []
    monkeypatch.setattr(RUN, "output", inspect)
    monkeypatch.setattr(RUN, "run", lambda command, **kwargs: commands.append(command))
    monkeypatch.setattr(RUN, "preflight", lambda generated: commands.append(["preflight", str(generated)]))
    monkeypatch.setattr(
        sys,
        "argv",
        ["run.py", "--api-base", "http://unused", "--no-build", "--catalog", str(tmp_path / "catalogs" / "smoke.json"), *argv],
    )
    return commands


def tree_bytes(root):
    return {
        str(path.relative_to(root)): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def test_rejected_resume_leaves_generated_inputs_and_manifest_byte_identical(
    monkeypatch, tmp_path
):
    commands = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e1", "--agent-timeout-sec", "3600"]
    )
    RUN.main()
    generated = tmp_path / ".generated" / "e1"
    manifest_path = tmp_path / "jobs" / "e1" / "experiment.json"
    before_tree = tree_bytes(generated)
    before_manifest = manifest_path.read_bytes()
    assert before_tree and json.loads(before_manifest)["agent_timeout_sec"] == 3600
    assert commands[0][0] == "preflight"
    assert sum(command[:4] == ["uv", "run", "--frozen", "harbor"] for command in commands) == 14
    commands.clear()

    controller_environment(
        monkeypatch,
        tmp_path,
        argv=["--experiment", "e1", "--agent-timeout-sec", "3601", "--resume"],
    )
    with pytest.raises(ValueError, match="does not match"):
        RUN.main()
    assert tree_bytes(generated) == before_tree
    assert manifest_path.read_bytes() == before_manifest
    assert [path.name for path in (tmp_path / ".generated").iterdir()] == ["e1"]
    assert commands == []


def test_manifest_records_the_harbor_revision_of_the_launching_environment(
    monkeypatch, tmp_path
):
    lock = tomllib.loads((ROOT / "uv.lock").read_text())
    source = next(package for package in lock["package"] if package["name"] == "harbor")
    locked_commit = source["source"]["git"].rsplit("#", 1)[1]
    installed = RUN.installed_harbor()
    assert installed == {
        "version": importlib.metadata.version("harbor"),
        "commit": locked_commit,
    }

    commands = controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1"])
    RUN.main()
    manifest = json.loads((tmp_path / "jobs" / "e1" / "experiment.json").read_text())
    assert (manifest["harbor_version"], manifest["harbor_commit"]) == (
        "stubbed",
        "0" * 40,
    )
    host = manifest["host"]
    assert set(host) == {
        "cpu_model",
        "logical_cpus",
        "memory_bytes",
        "kernel",
        "machine",
        "container_runtime",
    }
    assert host["logical_cpus"] == os.cpu_count() and host["memory_bytes"] > 0
    assert host["container_runtime"] == "podman version stubbed"
    launches = [command for command in commands if "harbor" in command]
    assert launches
    assert all(command[:4] == ["uv", "run", "--frozen", "harbor"] for command in launches)


def test_controller_records_the_declared_protocol(monkeypatch, tmp_path):
    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "common"])
    RUN.main()
    manifest = json.loads((tmp_path / "jobs" / "common" / "experiment.json").read_text())
    assert manifest["schema"] == "lingrep-linguistic-eval.experiment/v2"
    assert (manifest["protocol"], manifest["protocol_schema"]) == (
        "common-analysis",
        MODULE.PROTOCOL_SCHEMA,
    )
    task = tmp_path / ".generated" / "common" / "lingrep" / "direct-noun-morphology"
    assert not (task / "tests" / "query-replay.json").exists()

    controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "authoring", "--protocol", "catalog-authoring"]
    )
    RUN.main()
    manifest = json.loads((tmp_path / "jobs" / "authoring" / "experiment.json").read_text())
    assert manifest["protocol"] == "catalog-authoring"
    task = tmp_path / ".generated" / "authoring" / "lingrep" / "direct-noun-morphology"
    assert (task / "tests" / "query-replay.json").is_file()
    assert tomllib.loads((task / "task.toml").read_text())["metadata"]["protocol"] == "catalog-authoring"


def test_selected_cases_and_arms_are_the_only_tasks_generated_and_launched(
    monkeypatch, tmp_path
):
    commands = controller_environment(
        monkeypatch,
        tmp_path,
        argv=[
            "--experiment", "probe",
            "--case", "direct-noun-morphology",
            "--arm", "lingrep",
            "--repetitions", "2",
        ],
    )
    RUN.main()
    generated = tmp_path / ".generated" / "probe"
    assert [path.relative_to(generated).as_posix() for path in sorted(generated.glob("*/*"))] == [
        "lingrep/direct-noun-morphology"
    ]
    assert sum(command[:4] == ["uv", "run", "--frozen", "harbor"] for command in commands) == 2

    commands = controller_environment(
        monkeypatch,
        tmp_path,
        argv=["--experiment", "probe", "--case", "direct-noun-morphology", "--arm", "lingrep", "--repetitions", "2", "--resume"],
    )
    RUN.main()
    assert sum(command[:4] == ["uv", "run", "--frozen", "harbor"] for command in commands) == 2


def test_matching_resume_reuses_verified_generated_inputs(monkeypatch, tmp_path):
    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1"])
    RUN.main()
    generated = tmp_path / ".generated" / "e1"
    before = tree_bytes(generated)

    commands = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e1", "--resume"]
    )
    RUN.main()
    assert tree_bytes(generated) == before
    assert [path.name for path in (tmp_path / ".generated").iterdir()] == ["e1"]
    assert sum(command[:4] == ["uv", "run", "--frozen", "harbor"] for command in commands) == 14

    (generated / "lingrep" / "direct-noun-morphology" / "task.toml").write_text("tampered\n")
    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1", "--resume"])
    with pytest.raises(ValueError, match="generated inputs differ"):
        RUN.main()
    assert (generated / "lingrep" / "direct-noun-morphology" / "task.toml").read_text() == "tampered\n"


def test_invalid_selection_is_rejected_before_generation(monkeypatch, tmp_path):
    commands = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e1", "--case", "no-such-case"]
    )
    monkeypatch.setattr(RUN, "generate", lambda *args, **kwargs: pytest.fail("generated"))
    with pytest.raises(ValueError, match="unknown cases"):
        RUN.main()
    assert commands == []
    assert not (tmp_path / ".generated").exists()


def test_generation_reads_the_stored_catalog_snapshot(monkeypatch, tmp_path):
    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1"])
    catalogs = []
    real_generate = RUN.generate

    def record(destination, catalog, **kwargs):
        catalogs.append(catalog)
        real_generate(destination, catalog, **kwargs)

    monkeypatch.setattr(RUN, "generate", record)
    RUN.main()
    assert catalogs == [tmp_path / "jobs" / "e1" / "inputs" / "catalog.json"]


def test_a_second_controller_cannot_hold_the_same_experiment(monkeypatch, tmp_path):
    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1"])
    held = RUN.hold_experiment(tmp_path / "jobs" / "e1")
    with pytest.raises(RuntimeError, match="another controller"):
        RUN.main()
    held.close()
    RUN.main()


def stub_replays(monkeypatch):
    """Returns the lists that record podman run commands and built contexts."""
    monkeypatch.setattr(
        ORACLE.subprocess,
        "run",
        lambda command, **kwargs: ORACLE.subprocess.CompletedProcess(
            command, 0, stdout=json.dumps({"data": {"binding": {"unavailable": []}}})
        ),
    )
    docker = []
    built = []

    def build(context):
        built.append(Path(context))
        return f"sha256:{len(built):064x}"

    monkeypatch.setattr(ORACLE, "build", build)

    def execute(command):
        docker.append(command)
        if command[-1] == "/tests/test.sh":
            logs = Path(command[command.index("-v") + 1].split(":")[0])
            (logs / "verifier").mkdir(parents=True)
            (logs / "verifier" / "reward.json").write_text(
                json.dumps(
                    {
                        "exact": 1.0,
                        "verdict_correct": 1.0,
                        "query_valid": 1.0,
                        "visible_query_exact": 1.0,
                        "query_exact": 1.0,
                        "script_valid": 1.0,
                        "visible_script_exact": 1.0,
                        "script_exact": 1.0,
                    }
                )
            )

    monkeypatch.setattr(ORACLE, "run", execute)
    return docker, built


def test_preflight_replays_every_task_against_its_pinned_image(tmp_path, monkeypatch):
    MODULE.generate(
        tmp_path / "generated",
        ROOT / "catalogs" / "smoke.json",
        lingrep_image="sha256:" + "1" * 64,
        mecab_image="sha256:" + "2" * 64,
    )
    docker, built = stub_replays(monkeypatch)
    ORACLE.preflight(tmp_path / "generated")

    solves = [command for command in docker if command[-1] == "/solution/solve.sh"]
    verifies = [command for command in docker if command[-1] == "/tests/test.sh"]
    assert len(solves) == len(verifies) == 14
    assert set(built) == {
        task / child
        for arm in ("lingrep", "mecab")
        for task in (tmp_path / "generated" / arm).iterdir()
        for child in ("environment", "tests")
    }
    assert sorted(command[-2] for command in solves + verifies) == [
        f"sha256:{index:064x}" for index in range(1, 29)
    ]
    assert not any(command[:2] == ["podman", "build"] for command in docker)


def test_preflight_replays_only_the_generated_tasks(tmp_path, monkeypatch):
    generated = tmp_path / "generated"
    MODULE.generate(
        generated,
        ROOT / "catalogs" / "smoke.json",
        cases=["direct-noun-morphology"],
        arms=["lingrep"],
    )
    docker, built = stub_replays(monkeypatch)
    ORACLE.preflight(generated)

    task = generated / "lingrep" / "direct-noun-morphology"
    assert [command[-1] for command in docker] == ["/solution/solve.sh", "/tests/test.sh"]
    assert built == [task / "environment", task / "tests"]


def test_scoped_case_ships_its_where_clause_only_under_common_analysis(tmp_path):
    case = json.loads((ROOT / "catalogs" / "soranoha.json").read_text())[0]
    assert case["oracle_where"] == '@document.ndc = "NDC 913"'
    catalog = tmp_path / "scoped.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    with pytest.raises(ValueError, match="oracle_where.*common-analysis"):
        MODULE.generate(tmp_path / "authoring", catalog, protocol="catalog-authoring")
    assert not (tmp_path / "authoring").exists()

    generated = tmp_path / "generated"
    MODULE.generate(generated, catalog, protocol="common-analysis")
    lingrep = generated / "lingrep" / case["id"]
    mecab = generated / "mecab" / case["id"]
    assert (lingrep / "solution" / "scope.where").read_text() == '@document.ndc = "NDC 913"\n'
    assert not (mecab / "solution" / "scope.where").exists()
    assert (
        "cp /solution/run-analysis /solution/query.lx /solution/scope.where"
        " /logs/artifacts/submission/"
    ) in (lingrep / "solution" / "solve.sh").read_text()
    assert "scope.where" not in (mecab / "solution" / "solve.sh").read_text()


def test_lingrep_reference_runner_passes_the_sibling_scope_clause(tmp_path):
    solution = tmp_path / "solution"
    solution.mkdir()
    shutil.copy2(ROOT / "oracle_lingrep_runner.py", solution / "run-analysis")
    (solution / "query.lx").write_text("first_person := [unidic.pos1=代名詞].\n")
    (solution / "scope.where").write_text('@document.ndc = "NDC 913"\n')
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    argv_log = tmp_path / "ling-argv.json"
    ling = fake_bin / "ling"
    ling.write_text(
        "#!/usr/bin/env python3\nimport json, sys\n"
        f"open({str(argv_log)!r}, 'w').write(json.dumps(sys.argv[1:]))\n"
    )
    ling.chmod(0o755)
    corpus = tmp_path / "input.jsonl"
    corpus.write_text("")
    evidence = tmp_path / "evidence.jsonl"

    subprocess.run(
        [sys.executable, str(solution / "run-analysis"), str(corpus), str(evidence)],
        check=True,
        env={**os.environ, "PATH": f"{fake_bin}:{os.environ['PATH']}"},
    )
    argv = json.loads(argv_log.read_text())
    assert argv[:4] == ["--json", "matches", "--patterns", str(solution / "query.lx")]
    assert argv[4:6] == ["--where", '@document.ndc = "NDC 913"']
    assert evidence.read_text() == ""

    (solution / "scope.where").unlink()
    subprocess.run(
        [sys.executable, str(solution / "run-analysis"), str(corpus), str(evidence)],
        check=True,
        env={**os.environ, "PATH": f"{fake_bin}:{os.environ['PATH']}"},
    )
    assert "--where" not in json.loads(argv_log.read_text())


def test_generate_rejects_unknown_cases_and_arms(tmp_path):
    with pytest.raises(ValueError, match="unknown cases: \\['no-such-case'\\]"):
        MODULE.generate(tmp_path / "g", ROOT / "catalogs" / "smoke.json", cases=["no-such-case"])
    with pytest.raises(ValueError, match="unknown arms: \\['sudachi'\\]"):
        MODULE.generate(tmp_path / "g", ROOT / "catalogs" / "smoke.json", arms=["sudachi"])
    assert not (tmp_path / "g").exists()


def test_review_evidence_is_validated_against_the_snapshot_before_generation(
    monkeypatch, tmp_path
):
    review = tmp_path / "private-review"
    review.mkdir()
    (review / "packet.json").write_text("{}")
    commands = controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e1", "--review-evidence", str(review)]
    )
    monkeypatch.setattr(RUN, "generate", lambda *args, **kwargs: pytest.fail("generated"))
    with pytest.raises(ValueError, match="invalid review packet"):
        RUN.main()
    assert commands == []

    validated = []
    monkeypatch.setattr(RUN, "generate", MODULE.generate)
    monkeypatch.setattr(RUN, "validate_review", lambda *args: validated.append(args) or {})
    controller_environment(
        monkeypatch, tmp_path, argv=["--experiment", "e2", "--review-evidence", str(review)]
    )
    RUN.main()
    assert validated == [
        (tmp_path / "jobs" / "e2" / "inputs" / "review", tmp_path / "jobs" / "e2" / "inputs" / "catalog.json")
    ]
    manifest = json.loads((tmp_path / "jobs" / "e2" / "experiment.json").read_text())
    assert manifest["review_status"] == "reviewed"

    controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e3"])
    RUN.main()
    manifest = json.loads((tmp_path / "jobs" / "e3" / "experiment.json").read_text())
    assert manifest["review_status"] == "unreviewed"


def test_revision_tasks_start_from_the_base_reference_artifact(tmp_path):
    catalog = ROOT / "catalogs" / "soranoha.json"
    case = next(case for case in json.loads(catalog.read_text()) if "revision" in case)
    MODULE.generate(tmp_path, catalog, cases=[case["id"]])

    revision_id = f"{case['id']}.revision"
    for arm, support in (("lingrep", "query.lx"), ("mecab", "mecab_reference.py")):
        base = tmp_path / arm / case["id"]
        revision = tmp_path / arm / revision_id
        prior = revision / "environment" / "prior"
        assert sorted(path.name for path in prior.iterdir()) == sorted(["run-analysis", support])
        for name in ("run-analysis", support):
            assert (prior / name).read_bytes() == (base / "solution" / name).read_bytes()
        assert os.access(prior / "run-analysis", os.X_OK)
        assert "COPY prior/ /data/prior/\n" in (revision / "environment" / "Dockerfile").read_text()
        assert "prior" not in (base / "environment" / "Dockerfile").read_text()
        assert (revision / "environment" / "input.json").read_bytes() == (
            base / "environment" / "input.json"
        ).read_bytes()
        gold = [json.loads(line) for line in (revision / "tests" / "gold.jsonl").read_text().splitlines()]
        assert gold == MODULE.evidence_rows(case["revision"]["targets"])
        assert gold != MODULE.evidence_rows(case["targets"])
        task = tomllib.loads((revision / "task.toml").read_text())
        assert task["task"]["name"] == f"lingrep/{arm}-{revision_id}"
        assert task["metadata"]["case_id"] == revision_id
        assert task["metadata"]["base_case_id"] == case["id"]
        assert task["metadata"]["stage"] == "revision"
        base_metadata = tomllib.loads((base / "task.toml").read_text())["metadata"]
        assert (base_metadata["stage"], base_metadata["base_case_id"]) == ("base", case["id"])
        instruction = (revision / "instruction.md").read_text()
        assert case["phenomenon"] in instruction
        assert case["revision"]["instruction"] in instruction
        assert "/data/prior/" in instruction
        assert "/data/prior/" not in (base / "instruction.md").read_text()
    assert (tmp_path / "lingrep" / revision_id / "instruction.md").read_bytes() == (
        tmp_path / "mecab" / revision_id / "instruction.md"
    ).read_bytes()
    assert (tmp_path / "lingrep" / revision_id / "solution" / "query.lx").read_text() == (
        case["revision"]["oracle_query"]
    )
    assert case["revision"]["oracle_mecab"] in (
        tmp_path / "mecab" / revision_id / "solution" / "run-analysis"
    ).read_text()


def revision_of(case: dict, **overrides) -> dict:
    revision = {
        "instruction": "Count only the first match of each sentence.",
        "targets": case["targets"][:1],
        "oracle_query": case["oracle_query"],
        "oracle_mecab": case["oracle_mecab"],
    }
    revision.update(overrides)
    return revision


@pytest.mark.parametrize(
    ("overrides", "message"),
    [
        ({"oracle_mecab": None}, r"missing \['oracle_mecab'\]"),
        ({"labels": ["x"]}, r"unknown \['labels'\]"),
        ({"instruction": " "}, "revision instruction must be"),
        ({"driver_targets": []}, "driver_targets must be present exactly when"),
        ({"targets": "all"}, "revision targets must be a list"),
    ],
)
def test_catalog_rejects_malformed_revisions(tmp_path, overrides, message):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    assert "driver" not in case
    case["revision"] = revision_of(case, **overrides)
    if case["revision"].get("oracle_mecab") is None:
        del case["revision"]["oracle_mecab"]
    catalog = tmp_path / "catalog.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))
    with pytest.raises((ValueError, TypeError), match=message):
        MODULE.load_catalog(catalog)


def test_revision_of_a_driver_case_scores_its_own_hidden_targets(tmp_path):
    case = next(
        case
        for case in json.loads((ROOT / "catalogs" / "worked-example.json").read_text())
        if "driver" in case
    )
    case["revision"] = revision_of(case)
    catalog = tmp_path / "catalog.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))
    with pytest.raises(ValueError, match="driver_targets must be present exactly when"):
        MODULE.load_catalog(catalog)

    case["revision"]["driver_targets"] = []
    catalog.write_text(json.dumps([case], ensure_ascii=False))
    MODULE.generate(tmp_path / "generated", catalog)
    revision = tmp_path / "generated" / "mecab" / f"{case['id']}.revision"
    assert (revision / "tests" / "hidden-gold.jsonl").read_text() == ""
    assert (revision / "tests" / "hidden.jsonl").read_bytes() == (
        tmp_path / "generated" / "mecab" / case["id"] / "tests" / "hidden.jsonl"
    ).read_bytes()
    assert json.loads((revision / "tests" / "analysis-replay.json").read_text())[
        "has_hidden_driver"
    ]


def test_experiment_runs_revision_tasks_and_records_who_revises(monkeypatch, tmp_path):
    commands = controller_environment(monkeypatch, tmp_path, argv=["--experiment", "e1"])
    catalog = tmp_path / "catalogs" / "smoke.json"
    cases = json.loads(catalog.read_text())
    cases[0]["revision"] = revision_of(cases[0])
    catalog.write_text(json.dumps(cases, ensure_ascii=False))
    RUN.main()

    manifest = json.loads((tmp_path / "jobs" / "e1" / "experiment.json").read_text())
    revision_id = f"{cases[0]['id']}.revision"
    assert manifest["revisions"] == {
        "performed_by": "agent",
        "start_state": "reference artifact of the base case",
        "cases": [cases[0]["id"]],
    }
    assert manifest["tasks"][f"lingrep/mecab-{revision_id}"] == {
        "arm": "mecab",
        "case_id": revision_id,
        "base_case_id": cases[0]["id"],
        "stage": "revision",
        "capability": cases[0]["capability"],
    }
    assert manifest["tasks"][f"lingrep/lingrep-{cases[0]['id']}"]["stage"] == "base"
    jobs = harbor_jobs(commands)
    assert len(jobs) == 2 * (len(cases) + 1)
    assert {job[1] for job in jobs if job[2].startswith(revision_id)} == {revision_id}
    assert (tmp_path / ".generated" / "e1" / "lingrep" / revision_id / "environment" / "prior").is_dir()


def revision_experiment(tmp_path):
    """Base and revision trials for one case in both arms; the revision trials carry
    a submission, and every trial records a generated task with gold."""
    experiment = tmp_path / "pilot"
    experiment.mkdir()
    case = {
        "id": "case",
        "oracle_query": "target := [upos=NOUN].\n",
        "oracle_mecab": "def select(tokens, document):\n    return []\n",
    }
    (experiment / "inputs").mkdir()
    (experiment / "inputs" / "catalog.json").write_text(json.dumps([case]))
    tasks = {}
    for arm in ("lingrep", "mecab"):
        for stage, case_id in (("base", "case"), ("revision", "case.revision")):
            tasks[f"lingrep/{arm}-{case_id}"] = {
                "arm": arm,
                "case_id": case_id,
                "base_case_id": "case",
                "stage": stage,
                "capability": "feature_lookup",
            }
    (experiment / "experiment.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.experiment/v2",
                "experiment_id": "pilot",
                "models": ["m"],
                "repetitions": 1,
                "tasks": tasks,
            }
        )
    )
    started = datetime(2026, 10, 25, tzinfo=UTC)
    (experiment / "m" / "job").mkdir(parents=True)
    (experiment / "m" / "job" / "config.json").write_text(json.dumps({"job_name": "job", "n_attempts": 1}))
    for task_name, task in tasks.items():
        trial = experiment / "m" / "job" / task_name.replace("/", "-")
        (trial / "agent").mkdir(parents=True)
        (trial / "config.json").write_text(
            json.dumps({"agent": {"model_name": "openai/m"}, "trial_name": trial.name})
        )
        (trial / "agent" / "trajectory.json").write_text(
            json.dumps({"schema_version": "ATIF-v1.7", "steps": []})
        )
        (trial / "result.json").write_text(
            json.dumps(
                {
                    "id": f"id-{trial.name}",
                    "trial_name": trial.name,
                    "task_name": task_name,
                    "agent_execution": {
                        "started_at": started.isoformat(),
                        "finished_at": (started + timedelta(seconds=120)).isoformat(),
                    },
                    "agent_result": {"metadata": {"api_request_times_msec": [500, 1500]}},
                    "verifier_result": {
                        "rewards": {
                            "visible_script_f1": 0.5 if task["stage"] == "base" else 1.0,
                            "script_exact": 0.0 if task["stage"] == "base" else 1.0,
                        }
                    },
                }
            )
        )
        generated = tmp_path / "generated" / task["arm"] / task["case_id"]
        (generated / "tests").mkdir(parents=True)
        (generated / "instruction.md").write_text(f"# {task['case_id']} title\n\nBody.\n")
        gold = {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": "doc-001",
            "sentence_ordinal": 0,
            "start_char": 0,
            "end_char": 1,
            "label": "target",
        }
        (generated / "tests" / "gold.jsonl").write_text(json.dumps(gold) + "\n")
        (trial / "config.json").write_text(
            json.dumps(
                {
                    "agent": {"model_name": "openai/m"},
                    "trial_name": trial.name,
                    "task": {"path": str(generated)},
                }
            )
        )
        if task["stage"] == "revision":
            submission = trial / "artifacts" / "logs" / "artifacts" / "submission"
            submission.mkdir(parents=True)
            (submission / "evidence.jsonl").write_text(
                json.dumps({**gold, "end_char": 2}) + "\n"
            )
            if task["arm"] == "lingrep":
                (submission / "run-analysis").write_text(
                    (ROOT / "oracle_lingrep_runner.py").read_text() + "# revised\n"
                )
                (submission / "query.lx").write_text("target := [upos=VERB].\n")
            else:
                (submission / "run-analysis").write_text("#!/bin/bash\necho revised\n")
    return experiment, case


def test_revision_report_pairs_each_revision_with_its_base_trial(tmp_path):
    experiment, case = revision_experiment(tmp_path)
    rows = REPORT.summarize_experiment(experiment)
    assert {(row["case_id"], row["base_case_id"], row["stage"]) for row in rows} == {
        ("case", "case", "base"),
        ("case.revision", "case", "revision"),
    }
    report = REPORT.revision_report(rows, json.loads((experiment / "experiment.json").read_text()), experiment)
    by_arm = {row["arm"]: row for row in report}
    assert set(by_arm) == {"lingrep", "mecab"}
    lingrep = by_arm["lingrep"]
    assert lingrep["schema"] == REPORT.REVISION_SCHEMA
    assert lingrep["case_id"] == "case" and lingrep["attempt"] == 1
    assert lingrep["base_trial_id"] == "id-lingrep-lingrep-case"
    assert lingrep["revision_trial_id"] == "id-lingrep-lingrep-case.revision"
    assert lingrep["revision_agent_elapsed_seconds"] == 120.0
    assert lingrep["revision_model_api_requests"] == 2
    assert lingrep["revision_model_api_seconds"] == 2.0
    assert (lingrep["base_visible_script_f1"], lingrep["revision_visible_script_f1"]) == (0.5, 1.0)
    assert (lingrep["base_script_exact"], lingrep["revision_script_exact"]) == (0.0, 1.0)
    assert lingrep["run_analysis_changed_lines"] == 1
    assert lingrep["query_changed_lines"] == 2
    mecab = by_arm["mecab"]
    assert mecab["query_changed_lines"] is None
    reference = MODULE.mecab_reference_runner(case["oracle_mecab"])
    assert mecab["run_analysis_changed_lines"] == REPORT.changed_lines(
        reference, "#!/bin/bash\necho revised\n"
    )
    assert REPORT.changed_lines("a\nb\n", "a\nc\nd\n") == 3


def test_unanswerable_case_has_no_targets_and_a_false_reference_verdict(tmp_path):
    catalog = ROOT / "catalogs" / "soranoha.json"
    case = next(case for case in json.loads(catalog.read_text()) if case.get("answerable") is False)
    assert case["targets"] == []
    MODULE.generate(tmp_path, catalog, cases=[case["id"]])
    for arm in ("lingrep", "mecab"):
        task = tmp_path / arm / case["id"]
        assert json.loads((task / "tests" / "verdict.json").read_text()) == {"answerable": False}
        assert (task / "tests" / "gold.jsonl").read_text() == ""
        solve = (task / "solution" / "solve.sh").read_text()
        assert '"answerable": false' in solve and "verdict.json" in solve
        instruction = (task / "instruction.md").read_text()
        assert "`verdict.json`" in instruction and MODULE.VERDICT_SCHEMA in instruction

    case["targets"] = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]["targets"]
    case["documents"] = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]["documents"]
    case["labels"] = [case["targets"][0]["label"]]
    bad = tmp_path / "bad.json"
    bad.write_text(json.dumps([case], ensure_ascii=False))
    with pytest.raises(ValueError, match="unanswerable case has no targets"):
        MODULE.load_catalog(bad)
    case["answerable"] = "no"
    bad.write_text(json.dumps([case], ensure_ascii=False))
    with pytest.raises(TypeError, match="answerable must be a boolean"):
        MODULE.load_catalog(bad)


def test_pair_pages_render_each_case_offline_from_report_rows(tmp_path):
    experiment, _ = revision_experiment(tmp_path)
    pages = PAIRS.export_pairs(experiment, tmp_path / "pages")
    assert [page.name for page in pages] == ["case.md"]
    assert (tmp_path / "pages" / "index.md").read_text() == "# pilot\n\n- [case](case.md)\n"
    page = (tmp_path / "pages" / "case.md").read_text()
    assert page.startswith("# case title\n")
    assert "## Instruction\n" in page and "Body." in page
    assert "## Revised condition\n" in page and "# case.revision title" in page
    assert "| lingrep | base | 1 | 0.5 | – | 0 | – | 120 | 2 | 2 | 0 | – |" in page
    assert "| lingrep | revision | 1 | 1 | – | 1 | – | 120 | 2 | 2 | 0 | 0/1/1 |" in page
    assert "### lingrep, revision, attempt 1" in page
    assert "+# revised" in page
    assert "-target := [upos=NOUN]." in page and "+target := [upos=VERB]." in page
    assert '- missed: `["doc-001", 0, 0, 1, "target"]`' in page
    assert '- extra: `["doc-001", 0, 0, 2, "target"]`' in page
    assert "### mecab, revision, attempt 1" in page
    assert "+echo revised" in page
    assert "`query.lx`" not in page.split("### mecab, base, attempt 1")[1].split("###")[0]
