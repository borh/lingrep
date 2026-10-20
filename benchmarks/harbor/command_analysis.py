#!/usr/bin/env python3
"""Derive deterministic shell-command facts from Harbor ATIF trajectories."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
from collections import Counter
from pathlib import Path
from typing import Any

import tree_sitter_bash
from tree_sitter import Language, Node, Parser

SCHEMA = "lingrep-linguistic-eval.command-analysis/v1"
RUNTIME_SCHEMA = "lingrep-linguistic-eval.tool-invocation/v1"
MAX_RUNTIME_RECORD_BYTES = 1024 * 1024
_BASH = Language(tree_sitter_bash.language())
_PARSER = Parser(_BASH)
INSTRUMENTED_TOOLS = ("mecab", "ling")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def walk(node: Node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def text(source: bytes, node: Node) -> str:
    return source[node.start_byte : node.end_byte].decode(errors="replace")


def literal(node: Node) -> bool:
    dynamic = {
        "command_substitution",
        "expansion",
        "process_substitution",
        "simple_expansion",
    }
    return not any(descendant.type in dynamic for descendant in walk(node))


def executable_category(executable: str | None) -> str:
    if executable is None:
        return "other"
    name = executable.rsplit("/", 1)[-1]
    if name == "ling":
        return "lingrep"
    if name == "mecab":
        return "mecab"
    if name in {"python", "python3"} or (
        name.startswith("python3.") and name.removeprefix("python3.").isdigit()
    ):
        return "python"
    return "other"


def command_record(source: bytes, node: Node) -> dict[str, Any]:
    name = node.child_by_field_name("name")
    executable = text(source, name) if name is not None and literal(name) else None
    arguments = []
    for child in node.named_children:
        if child == name or child.type == "variable_assignment":
            continue
        arguments.append(
            {
                "source": text(source, child),
                "literal": literal(child),
            }
        )
    operation = None
    if executable and executable.rsplit("/", 1)[-1] == "ling" and arguments:
        operation = arguments[0]["source"] if arguments[0]["literal"] else None
    patterns = []
    for index, argument in enumerate(arguments):
        value = argument["source"]
        if value in {"--patterns", "-f"} and index + 1 < len(arguments):
            candidate = arguments[index + 1]
            if candidate["literal"]:
                patterns.append(candidate["source"])
        elif value.startswith("--patterns=") and argument["literal"]:
            patterns.append(value.partition("=")[2])
    return {
        "source_range": [node.start_byte, node.end_byte],
        "executable": executable,
        "category": executable_category(executable),
        "arguments": arguments,
        "lingrep_operation": operation,
        "pattern_catalogs": patterns,
    }


def python_subprocess_tools(source: str) -> list[str]:
    """Return instrumented tools invoked by literal Python subprocess calls."""
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return []
    tools = set()
    subprocess_calls = {"call", "check_call", "check_output", "Popen", "run"}
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call) or not node.args:
            continue
        function = node.func
        if not isinstance(function, ast.Attribute) or function.attr not in subprocess_calls:
            continue
        command = node.args[0]
        executable = None
        if isinstance(command, ast.Constant) and isinstance(command.value, str):
            executable = command.value.strip().split(maxsplit=1)[0]
        elif (
            isinstance(command, (ast.List, ast.Tuple))
            and command.elts
            and isinstance(command.elts[0], ast.Constant)
            and isinstance(command.elts[0].value, str)
        ):
            executable = command.elts[0].value
        if executable is not None:
            name = executable.rsplit("/", 1)[-1]
            if name in INSTRUMENTED_TOOLS:
                tools.add(name)
    return sorted(tools)


def shell_structure(command: str) -> dict[str, Any]:
    source = command.encode()
    tree = _PARSER.parse(source)
    nodes = list(walk(tree.root_node))
    errors = [
        {
            "type": node.type,
            "source_range": [node.start_byte, node.end_byte],
        }
        for node in nodes
        if node.type == "ERROR" or node.is_missing
    ]
    commands = [
        command_record(source, node) for node in nodes if node.type == "command"
    ]
    pattern_writes = []
    python_tool_scripts = []
    for node in nodes:
        if node.type != "redirected_statement":
            continue
        destinations = [
            text(source, child)
            for descendant in walk(node)
            if descendant.type == "file_redirect"
            for child in descendant.named_children
            if child.type == "word" and literal(child)
        ]
        bodies = [
            text(source, descendant)
            for descendant in walk(node)
            if descendant.type == "heredoc_body"
        ]
        for destination in destinations:
            if destination.endswith(".lx"):
                pattern_writes.append(
                    {
                        "path": destination,
                        "source": "".join(bodies) if bodies else None,
                    }
                )
            elif destination.endswith(".py") and bodies:
                script_source = "".join(bodies)
                tools = python_subprocess_tools(script_source)
                if tools:
                    python_tool_scripts.append(
                        {
                            "path": destination,
                            "tools": tools,
                        }
                    )
    categories = []
    for category in ("lingrep", "mecab", "python"):
        if any(command["category"] == category for command in commands):
            categories.append(category)
    return {
        "has_error": tree.root_node.has_error,
        "errors": errors,
        "commands": commands,
        "category": "+".join(categories) if categories else "other",
        "pipelines": sum(node.type == "pipeline" for node in nodes),
        "redirections": sum(
            node.type in {"file_redirect", "heredoc_redirect"} for node in nodes
        ),
        "heredocs": sum(node.type == "heredoc_redirect" for node in nodes),
        "pattern_writes": pattern_writes,
        "python_tool_scripts": python_tool_scripts,
    }


def recording_timings(path: Path, commands: list[str]) -> list[dict[str, Any]]:
    """Align canonical ATIF command strings against asciinema v3 input."""
    missing = [{"status": "unavailable", "seconds": None} for _ in commands]
    if not path.is_file():
        return missing
    lines = path.read_text(errors="replace").splitlines()
    if not lines or json.loads(lines[0]).get("version") != 3:
        return missing

    now = 0.0
    stream = ""
    segments = []
    prompts = []
    for line in lines[1:]:
        delta, kind, content = json.loads(line)[:3]
        now += float(delta)
        if kind == "i":
            start = len(stream)
            stream += content
            segments.append((start, len(stream), now))
        elif kind == "o" and "\u001b[?2004h" in content:
            prompts.append(now)

    cursor = 0
    timings = []
    for command in commands:
        if not command:
            timings.append({"status": "no_input", "seconds": None})
            continue
        start = stream.find(command, cursor)
        if start < 0:
            timings.append({"status": "unaligned", "seconds": None})
            continue
        end = start + len(command)
        cursor = end
        first = next((time for lo, hi, time in segments if lo <= start < hi), None)
        last = next((time for lo, hi, time in segments if lo < end <= hi), None)
        finished = next(
            (time for time in prompts if last is not None and time >= last), None
        )
        seconds = (
            round(max(0.0, finished - first), 6)
            if first is not None and finished is not None
            else None
        )
        timings.append(
            {
                "status": "exact" if seconds is not None else "unaligned",
                "seconds": seconds,
            }
        )
    return timings


def bash_calls(trajectory: dict[str, Any]) -> list[dict[str, Any]]:
    calls = []
    for step in trajectory.get("steps") or []:
        for call in step.get("tool_calls") or []:
            if call.get("function_name") != "bash_command":
                continue
            arguments = call.get("arguments") or {}
            calls.append(
                {
                    "step_id": step.get("step_id"),
                    "tool_call_id": call.get("tool_call_id"),
                    "source": arguments.get("keystrokes") or "",
                }
            )
    return calls


def active_wall_seconds(invocations: list[dict[str, Any]]) -> dict[str, float]:
    by_tool: dict[str, list[tuple[int, int]]] = {}
    for invocation in invocations:
        started = invocation["started_at_unix_ns"]
        finished = invocation["finished_at_unix_ns"]
        if started is None or finished is None or finished < started:
            continue
        by_tool.setdefault(invocation["tool"], []).append((started, finished))
    totals = {}
    for tool, intervals in by_tool.items():
        merged = []
        for start, end in sorted(intervals):
            if merged and start <= merged[-1][1]:
                merged[-1] = (merged[-1][0], max(merged[-1][1], end))
            else:
                merged.append((start, end))
        totals[tool] = round(sum(end - start for start, end in merged) / 1e9, 6)
    return dict(sorted(totals.items()))


def captured_pattern_evidence(
    invocation: Path, patterns: list[dict]
) -> tuple[list[dict], list[dict]]:
    root = invocation.resolve()
    queries = []
    errors = []
    for pattern in patterns:
        capture = pattern.get("capture")
        if not isinstance(capture, str):
            errors.append(
                {
                    "argument": pattern.get("argument"),
                    "resolved": pattern.get("resolved"),
                    "error": pattern.get("capture_error") or "capture unavailable",
                }
            )
            continue
        path = (invocation / capture).resolve()
        if (
            not path.is_relative_to(root)
            or not path.is_file()
            or path.stat().st_size > MAX_RUNTIME_RECORD_BYTES
        ):
            errors.append(
                {
                    "argument": pattern.get("argument"),
                    "resolved": pattern.get("resolved"),
                    "error": "captured source is missing, oversized, or outside invocation",
                }
            )
            continue
        queries.append(
            {
                "argument": pattern.get("argument"),
                "resolved": pattern.get("resolved"),
                "sha256": sha256(path),
                "source": path.read_text(errors="replace"),
            }
        )
    return queries, errors


def runtime_invocations(trial: Path) -> tuple[str, list[dict[str, Any]]]:
    root = trial / "agent" / "tool-invocations"
    if not root.is_dir():
        return "unavailable", []
    invocations = []
    for directory in sorted(path for path in root.iterdir() if path.is_dir()):
        try:
            start_path = directory / "start.json"
            if start_path.stat().st_size > MAX_RUNTIME_RECORD_BYTES:
                raise ValueError("start record exceeds size limit")
            start = json.loads(start_path.read_text())
            if start.get("schema") != RUNTIME_SCHEMA or start.get("phase") != "start":
                raise ValueError("invalid start record")
            if start.get("invocation_id") != directory.name:
                raise ValueError("start identity does not match directory")
            if start.get("tool") not in {"ling", "mecab"}:
                raise ValueError("unknown instrumented tool")
            spawn_path = directory / "spawn.json"
            finish_path = directory / "finish.json"
            for record_path in (spawn_path, finish_path):
                if (
                    record_path.is_file()
                    and record_path.stat().st_size > MAX_RUNTIME_RECORD_BYTES
                ):
                    raise ValueError(f"{record_path.name} exceeds size limit")
            spawn = json.loads(spawn_path.read_text()) if spawn_path.is_file() else {}
            finish = (
                json.loads(finish_path.read_text()) if finish_path.is_file() else {}
            )
            if spawn and (
                spawn.get("schema") != RUNTIME_SCHEMA
                or spawn.get("phase") != "spawn"
                or spawn.get("invocation_id") != directory.name
            ):
                raise ValueError("invalid spawn record")
            if finish and (
                finish.get("schema") != RUNTIME_SCHEMA
                or finish.get("phase") != "finish"
                or finish.get("invocation_id") != directory.name
            ):
                raise ValueError("invalid finish record")
            started = spawn.get("started_at_unix_ns")
            finished = finish.get("finished_at_unix_ns")
            elapsed_seconds = (
                round((finished - started) / 1e9, 6)
                if isinstance(started, int)
                and isinstance(finished, int)
                and finished >= started
                else None
            )
            status = "complete" if elapsed_seconds is not None else "incomplete"
            if finish.get("spawn_error") is not None:
                status = "spawn_error"
            pattern_queries, pattern_capture_errors = captured_pattern_evidence(
                directory, start.get("patterns") or []
            )
            invocations.append(
                {
                    "invocation_id": start.get("invocation_id", directory.name),
                    "tool": start.get("tool"),
                    "argv": start.get("argv") or [],
                    "cwd": start.get("cwd"),
                    "real_executable": start.get("real_executable"),
                    "launcher_pid": start.get("launcher_pid"),
                    "parent_pid": start.get("parent_pid"),
                    "child_pid": spawn.get("child_pid"),
                    "started_at_unix_ns": started,
                    "finished_at_unix_ns": finished,
                    "elapsed_seconds": elapsed_seconds,
                    "exit_code": finish.get("exit_code"),
                    "signal": finish.get("signal"),
                    "status": status,
                    "pattern_queries": pattern_queries,
                    "pattern_capture_errors": pattern_capture_errors,
                }
            )
        except (OSError, ValueError, TypeError, json.JSONDecodeError) as error:
            invocations.append(
                {
                    "invocation_id": directory.name,
                    "tool": None,
                    "status": "malformed",
                    "error": str(error),
                    "started_at_unix_ns": None,
                    "finished_at_unix_ns": None,
                    "elapsed_seconds": None,
                    "pattern_queries": [],
                    "pattern_capture_errors": [],
                }
            )
    return "available", invocations


def summary(
    calls: list[dict[str, Any]],
    *,
    runtime_telemetry: str = "unavailable",
    invocations: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    invocations = invocations or []
    executables: Counter[str] = Counter()
    categories: Counter[str] = Counter()
    category_seconds: Counter[str] = Counter()
    operations: Counter[str] = Counter()
    pattern_uses: Counter[str] = Counter()
    lifecycle: dict[str, Counter[str]] = {}
    shell_commands = 0
    timed_calls = 0
    runtime_counts: Counter[str] = Counter()
    runtime_process_seconds: Counter[str] = Counter()
    direct_pipeline_calls: Counter[str] = Counter()
    python_scripts_by_tool: dict[str, set[str]] = {
        tool: set() for tool in INSTRUMENTED_TOOLS
    }
    python_script_tools: dict[str, set[str]] = {}
    executed_python_paths: set[str] = set()
    for invocation in invocations:
        tool = invocation.get("tool")
        if tool is not None:
            runtime_counts[tool] += 1
        if tool is not None and invocation.get("elapsed_seconds") is not None:
            runtime_process_seconds[tool] += invocation["elapsed_seconds"]
    for call in calls:
        structure = call["shell"]
        if call["source_sha256"] is not None:
            categories[structure["category"]] += 1
        if call["timing"]["seconds"] is not None:
            category_seconds[structure["category"]] += call["timing"]["seconds"]
            timed_calls += 1
        shell_commands += len(structure["commands"])
        direct_tools = {
            command["executable"].rsplit("/", 1)[-1]
            for command in structure["commands"]
            if command["executable"] is not None
            and command["executable"].rsplit("/", 1)[-1] in INSTRUMENTED_TOOLS
        }
        if structure["pipelines"]:
            direct_pipeline_calls.update(direct_tools)
        python_commands = [
            command
            for command in structure["commands"]
            if command["category"] == "python"
        ]
        for script in structure["python_tool_scripts"]:
            python_script_tools.setdefault(script["path"], set()).update(
                script["tools"]
            )
        for command in python_commands:
            executed_python_paths.update(
                argument["source"].strip("'\"")
                for argument in command["arguments"]
                if argument["literal"]
            )
        for write in structure["pattern_writes"]:
            lifecycle.setdefault(write["path"], Counter())["writes"] += 1
        for command in structure["commands"]:
            if command["executable"] is not None:
                executables[command["executable"].rsplit("/", 1)[-1]] += 1
            if command["lingrep_operation"] is not None:
                operations[command["lingrep_operation"]] += 1
            pattern_uses.update(command["pattern_catalogs"])
            for pattern in command["pattern_catalogs"]:
                operation = command["lingrep_operation"] or "unknown"
                lifecycle.setdefault(pattern, Counter())[operation] += 1
    for path in sorted(executed_python_paths & python_script_tools.keys()):
        for tool in python_script_tools[path]:
            python_scripts_by_tool[tool].add(path)
    tool_workflows = {}
    for tool in INSTRUMENTED_TOOLS:
        direct = executables[tool]
        scripts = sorted(python_scripts_by_tool[tool])
        if direct and scripts:
            mode = "mixed"
        elif scripts:
            mode = "python_subprocess"
        elif direct:
            mode = "direct_cli"
        elif runtime_counts[tool]:
            mode = "runtime_only"
        else:
            mode = "not_used"
        tool_workflows[tool] = {
            "mode": mode,
            "direct_shell_commands": direct,
            "direct_shell_pipeline_calls": direct_pipeline_calls[tool],
            "executed_python_scripts": scripts,
            "runtime_invocations": runtime_counts[tool],
        }
    return {
        "bash_tool_calls": len(calls),
        "nonempty_bash_tool_calls": sum(
            call["source_sha256"] is not None for call in calls
        ),
        "shell_commands": shell_commands,
        "parse_error_calls": sum(call["shell"]["has_error"] for call in calls),
        "timed_calls": timed_calls,
        "unaligned_calls": sum(
            call["timing"]["status"] == "unaligned" for call in calls
        ),
        "bash_call_counts_by_shell_category": dict(sorted(categories.items())),
        "bash_roundtrip_seconds_by_shell_category": {
            key: round(value, 6) for key, value in sorted(category_seconds.items())
        },
        "executable_counts": dict(sorted(executables.items())),
        "lingrep_operation_counts": dict(sorted(operations.items())),
        "pattern_catalog_uses": dict(sorted(pattern_uses.items())),
        "pattern_lifecycle": {
            path: dict(sorted(counts.items()))
            for path, counts in sorted(lifecycle.items())
        },
        "runtime_telemetry": runtime_telemetry,
        "runtime_tool_invocation_counts": dict(sorted(runtime_counts.items())),
        "runtime_incomplete_invocations": sum(
            invocation["status"] != "complete" for invocation in invocations
        ),
        "runtime_tool_process_seconds": {
            key: round(value, 6)
            for key, value in sorted(runtime_process_seconds.items())
        },
        "runtime_tool_active_wall_seconds": active_wall_seconds(invocations),
        "tool_workflows": tool_workflows,
    }


def analyze_trial(trial: Path) -> dict[str, Any]:
    runtime_telemetry, invocations = runtime_invocations(trial)
    trajectory_path = trial / "agent" / "trajectory.json"
    if not trajectory_path.is_file():
        return {
            "schema": SCHEMA,
            "trajectory_sha256": None,
            "recording_sha256": None,
            "calls": [],
            "runtime_invocations": invocations,
            "summary": summary(
                [],
                runtime_telemetry=runtime_telemetry,
                invocations=invocations,
            ),
        }
    trajectory = json.loads(trajectory_path.read_text())
    calls = bash_calls(trajectory)
    recording = trial / "agent" / "recording.cast"
    timings = recording_timings(recording, [call["source"] for call in calls])
    for call, timing in zip(calls, timings, strict=True):
        source = call.pop("source")
        call["source_sha256"] = (
            hashlib.sha256(source.encode()).hexdigest() if source else None
        )
        call["timing"] = timing
        call["shell"] = shell_structure(source)
    return {
        "schema": SCHEMA,
        "trajectory_sha256": sha256(trajectory_path),
        "recording_sha256": sha256(recording) if recording.is_file() else None,
        "calls": calls,
        "runtime_invocations": invocations,
        "summary": summary(
            calls,
            runtime_telemetry=runtime_telemetry,
            invocations=invocations,
        ),
    }


def find_trials(path: Path) -> list[Path]:
    resolved = path.resolve()
    if (resolved / "agent" / "trajectory.json").is_file():
        return [resolved]
    return sorted(
        trajectory.parent.parent
        for trajectory in resolved.rglob("agent/trajectory.json")
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", type=Path, help="Harbor trial, job, or experiment")
    args = parser.parse_args()
    trials = find_trials(args.path)
    if not trials:
        raise SystemExit(f"no ATIF trajectories below {args.path}")
    for trial in trials:
        output = trial / "command-analysis.json"
        output.write_text(
            json.dumps(analyze_trial(trial), indent=2, sort_keys=True) + "\n"
        )
        print(output)


if __name__ == "__main__":
    main()
