import importlib.util
import json
import sys
import zipfile
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest
import tomllib

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

EXPORT_SPEC = importlib.util.spec_from_file_location(
    "export_viewer", ROOT / "export_viewer.py"
)
EXPORT = importlib.util.module_from_spec(EXPORT_SPEC)
assert EXPORT_SPEC.loader
EXPORT_SPEC.loader.exec_module(EXPORT)


def test_generation_is_paired_and_keeps_gold_out_of_agent_image(tmp_path):
    catalog = ROOT / "catalogs" / "smoke.json"
    MODULE.generate(tmp_path, catalog)
    cases = json.loads(catalog.read_text())
    assert len(list((tmp_path / "lingrep").iterdir())) == len(cases) == 6
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
        assert len((lingrep / "environment" / "input.json").read_text().splitlines()) == 1
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
        assert rows
        assert all(row["end_char"] > row["start_char"] for row in rows)
        metadata = tomllib.loads((lingrep / "task.toml").read_text())["metadata"]
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
        assert targeted_ordinals < set(range(len(case["sentences"])))

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
    assert (query_task / "tests" / "query-replay.json").exists() == bool(
        query_case.get("oracle_query")
    )
    for task in (query_task, mecab_query_task):
        analysis_replay = json.loads(
            (task / "tests" / "analysis-replay.json").read_text()
        )
        assert analysis_replay == {"has_hidden_driver": True}
        assert "run-analysis" in (task / "instruction.md").read_text()
        assert "run-analysis" in (task / "solution" / "solve.sh").read_text()


def test_generation_rejects_a_driver_span_that_disagrees_with_surface(tmp_path):
    case = json.loads((ROOT / "catalogs" / "worked-example.json").read_text())[-1]
    case["driver"]["targets"][0]["start_char"] = 0
    catalog = tmp_path / "invalid-driver.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    with pytest.raises(ValueError, match="driver"):
        MODULE.generate(tmp_path / "generated", catalog)


def test_generation_packages_lingrep_oracle_query_only_in_solution(tmp_path):
    case = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    case["oracle_query"] = "common_noun := [unidic.pos1=名詞 & unidic.pos2=普通名詞].\n"
    catalog = tmp_path / "oracle.json"
    catalog.write_text(json.dumps([case], ensure_ascii=False))

    generated = tmp_path / "generated"
    MODULE.generate(generated, catalog)

    lingrep = generated / "lingrep" / case["id"]
    mecab = generated / "mecab" / case["id"]
    assert (lingrep / "solution" / "query.lx").read_text() == case["oracle_query"]
    assert "query.lx" in (lingrep / "solution" / "solve.sh").read_text()
    assert not (lingrep / "environment" / "query.lx").exists()
    assert not (mecab / "solution" / "query.lx").exists()


def test_oracle_preflight_requires_all_replay_dimensions():
    complete = {
        "exact": 1.0,
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
    authored = json.loads((ROOT / "catalogs" / "smoke.json").read_text())[0]
    assert authored.get("oracle_query")
    case = {key: value for key, value in authored.items() if key != "oracle_query"}
    without_query = tmp_path / "without-query.json"
    without_query.write_text(json.dumps([case], ensure_ascii=False))
    with_query = tmp_path / "with-query.json"
    with_query.write_text(
        json.dumps(
            [{**case, "oracle_query": "common_noun := [unidic.pos1=名詞].\n"}],
            ensure_ascii=False,
        )
    )

    plain = tmp_path / "plain"
    MODULE.generate(plain, without_query)
    scored = tmp_path / "scored"
    MODULE.generate(scored, with_query)

    for arm in ("lingrep", "mecab"):
        plain_task = plain / arm / case["id"]
        assert ORACLE.replay_dimensions(plain_task) == (False, False)
        assert not (plain_task / "solution" / "query.lx").exists()
        assert not (plain_task / "tests" / "query-replay.json").exists()
        assert "query.lx" not in (plain_task / "instruction.md").read_text()
    assert ORACLE.replay_dimensions(scored / "lingrep" / case["id"]) == (True, False)
    assert ORACLE.replay_dimensions(scored / "mecab" / case["id"]) == (False, False)


def test_preflight_refuses_a_catalog_whose_halves_disagree(tmp_path):
    case = {
        key: value
        for key, value in json.loads(
            (ROOT / "catalogs" / "smoke.json").read_text()
        )[0].items()
        if key != "oracle_query"
    }
    catalog = tmp_path / "query.json"
    catalog.write_text(
        json.dumps(
            [{**case, "oracle_query": "common_noun := [unidic.pos1=名詞].\n"}],
            ensure_ascii=False,
        )
    )
    MODULE.generate(tmp_path / "generated", catalog)
    task = tmp_path / "generated" / "lingrep" / case["id"]

    (task / "solution" / "query.lx").unlink()
    with pytest.raises(RuntimeError, match="packages no oracle query.lx"):
        ORACLE.replay_dimensions(task)

    (task / "tests" / "query-replay.json").unlink()
    (task / "solution" / "query.lx").write_text("common_noun := [].\n")
    with pytest.raises(RuntimeError, match="never replayed"):
        ORACLE.replay_dimensions(task)


def test_generation_rejects_a_surface_that_disagrees_with_explicit_character_span(
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
                    "sentences": ["のとの"],
                    "targets": [
                        {
                            "sentence_ordinal": 0,
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
    case["sentences"][0] = "か\u3099"
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
                    "sentences": ["のとの"],
                    "targets": [
                        {
                            "sentence_ordinal": 0,
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
                    "sentences": ["対象なし。"],
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
        "schema": "lingrep-linguistic-eval.experiment/v1",
        "created_at": "first",
        "attempts": 3,
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


def test_experiment_snapshots_private_catalog_and_review_evidence(tmp_path):
    catalog = tmp_path / "private-catalog.json"
    catalog.write_text("[]\n")
    review = tmp_path / "review"
    review.mkdir()
    (review / "adjudication.json").write_text('{"approved":true}\n')
    experiment = tmp_path / "experiment"

    recorded = RUN.snapshot_experiment_inputs(experiment, catalog, review)

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


def test_experiment_refuses_uncommitted_benchmark_state(monkeypatch):
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
                "schema": "lingrep-linguistic-eval.experiment/v1",
                "experiment_id": "pilot-001",
                "models": ["qwen:think"],
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
        "agent_execution": {
            "started_at": started.isoformat(),
            "finished_at": (started + timedelta(seconds=900)).isoformat(),
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
    trial_config = {"agent": {"model_name": "openai/qwen:think"}}
    (first / "config.json").write_text(json.dumps(trial_config))
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
                        "step_id": 2,
                        "source": "agent",
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
    submission = first / "artifacts" / "submission"
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
    (second / "config.json").write_text(json.dumps(trial_config))
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
    assert row["reported_model"] == "qwen-normalized"
    assert row["input_tokens"] == 10
    assert row["cached_input_tokens"] == 3
    assert row["output_tokens"] == 2
    assert row["model_api_requests"] == 2
    assert row["model_api_seconds"] == 3.5
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
    assert row["agent_other_seconds"] == 891.5
    assert [row["attempt"] for row in rows] == [1, 2]

    summaries = REPORT.summarize_groups(rows)
    assert summaries == [
        {
            "schema": "lingrep-linguistic-eval.group-report/v1",
            "experiment_id": "pilot-001",
            "model": "qwen:think",
            "arm": "lingrep",
            "trials": 2,
            "operational_successes": 1,
            "exact_trials": 2.0,
            "mean_evidence_f1": 1.0,
            "query_replay_trials": 0,
            "query_valid_trials": 0,
            "query_exact_trials": 0,
            "mean_visible_query_f1": None,
            "mean_hidden_query_f1": None,
            "script_replay_trials": 2,
            "script_valid_trials": 2,
            "script_exact_trials": 2,
            "mean_visible_script_f1": 1.0,
            "mean_hidden_script_f1": None,
            "median_trial_elapsed_seconds": 455.5,
            "input_tokens_total": 20,
            "input_tokens_median": 10.0,
            "cached_input_tokens_total": 6,
            "output_tokens_total": 4,
            "output_tokens_median": 2.0,
            "trials_with_pattern_queries": 1,
            "pattern_query_artifacts": 1,
            "analysis_runner_artifacts": 1,
            "model_api_requests": 4,
            "model_api_seconds": 7.0,
            "agent_elapsed_seconds": 900.0,
            "agent_other_seconds": 891.5,
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
                "schema": "lingrep-linguistic-eval.experiment/v1",
                "experiment_id": "probe",
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
    (trial / "artifacts" / "submission").mkdir(parents=True)
    (trial / "config.json").write_text(json.dumps({"task": {"path": str(task)}}))
    (trial / "agent" / "trajectory.json").write_text(
        json.dumps({"schema_version": "ATIF-v1.7", "steps": []})
    )
    (trial / "verifier" / "test-stdout.txt").write_text("passed\n")
    (trial / "artifacts" / "submission" / "query.lx").write_text(
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
        assert "lingrep-case/jobs/case__attempt/artifacts/submission/query.lx" in names
        assert not any("gold" in name or "/solution/" in name for name in names)


def test_viewer_export_rejects_non_atif_trials(tmp_path):
    trajectory = tmp_path / "trajectory.json"
    trajectory.write_text(json.dumps({"messages": []}))
    with pytest.raises(ValueError, match="requires an ATIF-v1.x trajectory"):
        EXPORT.validate_trajectory(trajectory)
