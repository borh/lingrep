"""Runs `verify.sandbox_command` inside each loaded verifier image; selected by
`pytest -m containers` and skipped by the default lane."""

import importlib.util
import json
import shutil
import subprocess
import textwrap
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("generate_dataset", ROOT / "generate_dataset.py")
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MODULE)

pytestmark = pytest.mark.containers

PROBE = textwrap.dedent(
    '''
    import json
    import os
    import socket
    import subprocess
    import sys
    from pathlib import Path

    sys.path.insert(0, "/verifier")
    import verify

    submission = Path("/tmp/submission")
    submission.mkdir()
    runner = submission / "run-analysis"
    runner.write_text(RUNNER)
    runner.chmod(0o755)
    submission.chmod(0o755)
    workspace = Path("/tmp/work")
    workspace.mkdir()
    workspace.chmod(0o777)
    (workspace / "input.jsonl").write_text("")

    outside = {
        "tests_gold": Path("/tests/gold.jsonl").is_file(),
        "host_secret": Path("/host/secret").is_file(),
        "logs": Path("/logs").is_dir(),
        "interfaces": sorted(name for _, name in socket.if_nameindex()),
    }
    completed = subprocess.run(
        verify.sandbox_command(submission, workspace),
        capture_output=True,
        text=True,
        check=False,
    )
    inside = json.loads((workspace / "evidence.jsonl").read_text()) if completed.returncode == 0 else None
    print(json.dumps({"outside": outside, "inside": inside, "stderr": completed.stderr}))
    '''
)

RUNNER = textwrap.dedent(
    '''
    #!/usr/bin/env python3
    import json
    import os
    import socket
    import sys
    from pathlib import Path

    def writable(path):
        try:
            Path(path, "probe").write_text("x")
            return True
        except OSError:
            return False

    report = {
        "tests_gold": Path("/tests/gold.jsonl").exists(),
        "tests_dir": Path("/tests").exists(),
        "host_secret": Path("/host/secret").exists(),
        "logs": Path("/logs").exists(),
        "interfaces": sorted(name for _, name in socket.if_nameindex()),
        "cwd": os.getcwd(),
        "uid": os.getuid(),
        "submission_writable": writable("/submission"),
        "work_writable": writable("/work"),
        "input_readable": Path(sys.argv[1]).is_file(),
    }
    Path(sys.argv[2]).write_text(json.dumps(report))
    '''
).lstrip()


@pytest.mark.parametrize("image", sorted(image for image, _ in MODULE.ARMS.values()))
def test_replay_sandbox_hides_tests_logs_host_and_network(tmp_path, image):
    tests = tmp_path / "tests"
    tests.mkdir()
    (tests / "gold.jsonl").write_text('{"secret": true}\n')
    host = tmp_path / "host"
    host.mkdir()
    (host / "secret").write_text("host\n")
    logs = tmp_path / "logs"
    logs.mkdir()
    probe = tmp_path / "probe"
    probe.mkdir()
    (probe / "probe.py").write_text(f"RUNNER = {RUNNER!r}\n{PROBE}")

    completed = subprocess.run(
        [
            "podman",
            "run",
            "--rm",
            "-v",
            f"{ROOT / 'verifier'}:/verifier:ro",
            "-v",
            f"{tests}:/tests:ro",
            "-v",
            f"{host}:/host:ro",
            "-v",
            f"{logs}:/logs",
            "-v",
            f"{probe}:/probe:ro",
            image,
            "python3",
            "/probe/probe.py",
        ],
        capture_output=True,
        text=True,
        check=True,
    )
    report = json.loads(completed.stdout.splitlines()[-1])

    assert report["outside"]["tests_gold"] and report["outside"]["host_secret"]
    assert report["outside"]["logs"]
    assert report["outside"]["interfaces"] != ["lo"]
    inside = report["inside"]
    assert inside is not None, report["stderr"]
    assert inside["input_readable"] and inside["work_writable"]
    assert inside["cwd"] == "/submission" and inside["uid"] == 65534
    assert not inside["tests_gold"] and not inside["tests_dir"]
    assert not inside["host_secret"] and not inside["logs"]
    assert not inside["submission_writable"]
    assert inside["interfaces"] == ["lo"]


@pytest.mark.parametrize("image", sorted(image for image, _ in MODULE.ARMS.values()))
def test_root_execs_find_the_recording_tools(image):
    """Terminus runs these two checks as root before each trial
    (harbor/agents/terminus_2/tmux_session.py) and installs on failure."""
    subprocess.run(
        ["podman", "run", "--rm", "--user", "root", image, "sh", "-c", "tmux -V && asciinema --version"],
        check=True,
        capture_output=True,
    )


def test_mecab_reference_tokenizes_unknown_words_in_the_image(tmp_path):
    work = tmp_path / "work"
    work.mkdir()
    work.chmod(0o777)
    (work / "input.jsonl").write_text(
        json.dumps(
            {"eval_document_id": "doc-001", "sentences": ["OpenAIが発表した。", "🙂猫だ。"]},
            ensure_ascii=False,
        )
        + "\n"
    )
    runner = work / "run-analysis"
    runner.write_text(
        MODULE.mecab_reference_runner(
            "def select(tokens, document):\n"
            "    for index, token in enumerate(tokens):\n"
            "        if token.pos1 == \"名詞\" and token.pos2 == \"普通名詞\":\n"
            "            yield index, index + 1, \"common_noun\"\n"
        )
    )
    runner.chmod(0o755)
    shutil.copy2(ROOT / "mecab_reference.py", work / "mecab_reference.py")

    subprocess.run(
        [
            "podman",
            "run",
            "--rm",
            "-v",
            f"{work}:/work",
            MODULE.ARMS["mecab"][0],
            "/work/run-analysis",
            "/work/input.jsonl",
            "/work/evidence.jsonl",
        ],
        check=True,
    )

    rows = [json.loads(line) for line in (work / "evidence.jsonl").read_text().splitlines()]
    assert [(row["sentence_ordinal"], row["start_char"], row["end_char"]) for row in rows] == [
        (0, 7, 9),
        (1, 1, 2),
    ]
