import importlib.util
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "command_analysis", ROOT / "command_analysis.py"
)
ANALYSIS = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(ANALYSIS)


def write_trial(tmp_path: Path) -> Path:
    trial = tmp_path / "trial"
    agent = trial / "agent"
    agent.mkdir(parents=True)
    write_query = (
        "cat > /logs/artifacts/submission/query.lx << 'EOF'\n"
        "target := [upos=NOUN].\n"
        "EOF\n"
    )
    check_query = "ling check -f /logs/artifacts/submission/query.lx\n"
    run_query = (
        "ling matches --patterns /logs/artifacts/submission/query.lx input.jsonl"
        " | python3 /tmp/project.py\n"
    )
    trajectory = {
        "schema_version": "ATIF-v1.7",
        "steps": [
            {
                "step_id": 2,
                "source": "agent",
                "tool_calls": [
                    {
                        "tool_call_id": "write",
                        "function_name": "bash_command",
                        "arguments": {"keystrokes": write_query},
                    },
                    {
                        "tool_call_id": "check",
                        "function_name": "bash_command",
                        "arguments": {"keystrokes": check_query},
                    },
                    {
                        "tool_call_id": "run",
                        "function_name": "bash_command",
                        "arguments": {"keystrokes": run_query},
                    },
                ],
            }
        ],
    }
    (agent / "trajectory.json").write_text(json.dumps(trajectory))
    split = write_query.index("].") + 1
    (agent / "recording.cast").write_text(
        "\n".join(
            [
                json.dumps({"version": 3}),
                json.dumps([1.0, "i", "clear\r"]),
                json.dumps([1.0, "i", write_query[:split]]),
                json.dumps([0.2, "i", write_query[split:]]),
                json.dumps([0.8, "o", "\u001b[?2004h"]),
                json.dumps([1.0, "i", check_query]),
                json.dumps([0.5, "o", "ok\n\u001b[?2004h"]),
                json.dumps([1.0, "i", run_query]),
                json.dumps([2.0, "o", "rows\n\u001b[?2004h"]),
            ]
        )
        + "\n"
    )
    return trial


def test_atif_commands_are_parsed_and_aligned_to_split_recording_input(tmp_path):
    analysis = ANALYSIS.analyze_trial(write_trial(tmp_path))

    assert analysis["schema"] == "lingrep-linguistic-eval.command-analysis/v1"
    assert [call["timing"] for call in analysis["calls"]] == [
        {"status": "exact", "seconds": 1.0},
        {"status": "exact", "seconds": 0.5},
        {"status": "exact", "seconds": 2.0},
    ]
    assert analysis["calls"][0]["shell"]["pattern_writes"] == [
        {
            "path": "/logs/artifacts/submission/query.lx",
            "source": "target := [upos=NOUN].\n",
        }
    ]
    assert analysis["calls"][2]["shell"]["category"] == "lingrep+python"
    assert analysis["summary"] == {
        "bash_tool_calls": 3,
        "nonempty_bash_tool_calls": 3,
        "shell_commands": 4,
        "parse_error_calls": 0,
        "timed_calls": 3,
        "unaligned_calls": 0,
        "bash_call_counts_by_shell_category": {
            "other": 1,
            "lingrep": 1,
            "lingrep+python": 1,
        },
        "bash_roundtrip_seconds_by_shell_category": {
            "other": 1.0,
            "lingrep": 0.5,
            "lingrep+python": 2.0,
        },
        "executable_counts": {"cat": 1, "python3": 1, "ling": 2},
        "lingrep_operation_counts": {"check": 1, "matches": 1},
        "pattern_catalog_uses": {"/logs/artifacts/submission/query.lx": 2},
        "pattern_lifecycle": {
            "/logs/artifacts/submission/query.lx": {
                "check": 1,
                "matches": 1,
                "writes": 1,
            }
        },
        "runtime_telemetry": "unavailable",
        "runtime_tool_invocation_counts": {},
        "runtime_incomplete_invocations": 0,
        "runtime_tool_process_seconds": {},
        "runtime_tool_active_wall_seconds": {},
        "tool_workflows": {
            "mecab": {
                "direct_shell_commands": 0,
                "direct_shell_pipeline_calls": 0,
                "executed_python_scripts": [],
                "mode": "not_used",
                "runtime_invocations": 0,
            },
            "ling": {
                "direct_shell_commands": 2,
                "direct_shell_pipeline_calls": 1,
                "executed_python_scripts": [],
                "mode": "direct_cli",
                "runtime_invocations": 0,
            },
        },
    }


def test_tool_workflows_distinguish_direct_cli_python_and_mixed_use():
    source = """echo text | mecab
cat <<'PY' > /tmp/analyze.py
import subprocess
subprocess.run(['mecab'], input='text', text=True)
subprocess.check_output(['ling', 'tokenize', '/data/input.jsonl'])
PY
python3 /tmp/analyze.py
"""
    structure = ANALYSIS.shell_structure(source)
    calls = [
        {
            "source_sha256": "present",
            "timing": {"status": "unavailable", "seconds": None},
            "shell": structure,
        }
    ]
    invocations = [
        {"tool": "mecab", "status": "complete", "elapsed_seconds": None,
         "started_at_unix_ns": None, "finished_at_unix_ns": None},
        {"tool": "mecab", "status": "complete", "elapsed_seconds": None,
         "started_at_unix_ns": None, "finished_at_unix_ns": None},
        {"tool": "ling", "status": "complete", "elapsed_seconds": None,
         "started_at_unix_ns": None, "finished_at_unix_ns": None},
    ]

    workflows = ANALYSIS.summary(calls, invocations=invocations)["tool_workflows"]

    assert workflows == {
        "mecab": {
            "direct_shell_commands": 1,
            "direct_shell_pipeline_calls": 1,
            "executed_python_scripts": ["/tmp/analyze.py"],
            "mode": "mixed",
            "runtime_invocations": 2,
        },
        "ling": {
            "direct_shell_commands": 0,
            "direct_shell_pipeline_calls": 0,
            "executed_python_scripts": ["/tmp/analyze.py"],
            "mode": "python_subprocess",
            "runtime_invocations": 1,
        },
    }


def test_malformed_agent_shell_is_reported_without_losing_partial_commands():
    structure = ANALYSIS.shell_structure("echo before; if then\n")

    assert structure["has_error"] is True
    assert structure["errors"]
    assert structure["commands"][0]["executable"] == "echo"


def write_runtime_invocation(
    trial: Path,
    invocation_id: str,
    *,
    tool: str,
    started_at_unix_ns: int,
    finished_at_unix_ns: int | None,
    patterns_source: str | None = None,
    patterns_error: str | None = None,
) -> None:
    invocation = trial / "agent" / "tool-invocations" / invocation_id
    invocation.mkdir(parents=True)
    patterns = []
    if patterns_source is not None:
        patterns_dir = invocation / "patterns"
        patterns_dir.mkdir()
        (patterns_dir / "0.lx").write_text(patterns_source)
        patterns.append(
            {
                "argument": "/tmp/generated.lx",
                "resolved": "/tmp/generated.lx",
                "capture": "patterns/0.lx",
                "capture_error": None,
            }
        )
    if patterns_error is not None:
        patterns.append(
            {
                "argument": "/tmp/missing.lx",
                "resolved": "/tmp/missing.lx",
                "capture": None,
                "capture_error": patterns_error,
            }
        )
    (invocation / "start.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.tool-invocation/v1",
                "phase": "start",
                "invocation_id": invocation_id,
                "tool": tool,
                "argv": [tool, "--from-python"],
                "cwd": "/app",
                "real_executable": f"/nix/store/example/bin/{tool}",
                "launcher_pid": 100,
                "parent_pid": 99,
                "patterns": patterns,
            }
        )
    )
    (invocation / "spawn.json").write_text(
        json.dumps(
            {
                "schema": "lingrep-linguistic-eval.tool-invocation/v1",
                "phase": "spawn",
                "invocation_id": invocation_id,
                "started_at_unix_ns": started_at_unix_ns,
                "child_pid": 101,
            }
        )
    )
    if finished_at_unix_ns is not None:
        (invocation / "finish.json").write_text(
            json.dumps(
                {
                    "schema": "lingrep-linguistic-eval.tool-invocation/v1",
                    "phase": "finish",
                    "invocation_id": invocation_id,
                    "finished_at_unix_ns": finished_at_unix_ns,
                    "exit_code": 0,
                    "signal": None,
                    "spawn_error": None,
                }
            )
        )


def test_runtime_spans_detect_nested_tools_and_keep_overlap_and_incomplete_explicit(
    tmp_path,
):
    trial = write_trial(tmp_path)
    write_runtime_invocation(
        trial,
        "lingrep-one",
        tool="ling",
        started_at_unix_ns=1_000_000_000,
        finished_at_unix_ns=4_000_000_000,
        patterns_source="target := [upos=NOUN].\n",
    )
    write_runtime_invocation(
        trial,
        "lingrep-two",
        tool="ling",
        started_at_unix_ns=3_000_000_000,
        finished_at_unix_ns=5_000_000_000,
        patterns_error="No such file",
    )
    write_runtime_invocation(
        trial,
        "mecab-interrupted",
        tool="mecab",
        started_at_unix_ns=6_000_000_000,
        finished_at_unix_ns=None,
    )

    analysis = ANALYSIS.analyze_trial(trial)

    assert analysis["schema"] == "lingrep-linguistic-eval.command-analysis/v1"
    assert [item["status"] for item in analysis["runtime_invocations"]] == [
        "complete",
        "complete",
        "incomplete",
    ]
    captured = analysis["runtime_invocations"][0]["pattern_queries"]
    assert captured == [
        {
            "argument": "/tmp/generated.lx",
            "resolved": "/tmp/generated.lx",
            "sha256": "807f082f521acf56ec277ef1f6ab022c5979418ed4588fe784e587cf4146cc19",
            "source": "target := [upos=NOUN].\n",
        }
    ]
    assert analysis["runtime_invocations"][1]["pattern_capture_errors"] == [
        {
            "argument": "/tmp/missing.lx",
            "resolved": "/tmp/missing.lx",
            "error": "No such file",
        }
    ]
    assert analysis["summary"]["runtime_telemetry"] == "available"
    assert analysis["summary"]["runtime_tool_invocation_counts"] == {
        "mecab": 1,
        "ling": 2,
    }
    assert analysis["summary"]["runtime_incomplete_invocations"] == 1
    assert analysis["summary"]["runtime_tool_process_seconds"] == {"ling": 5.0}
    assert analysis["summary"]["runtime_tool_active_wall_seconds"] == {"ling": 4.0}
