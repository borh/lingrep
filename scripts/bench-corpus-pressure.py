#!/usr/bin/env python3
import argparse
from contextlib import contextmanager
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import resource
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid


class ServiceStopError(RuntimeError):
    pass


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def counters(root, name):
    return {key: int(value) for key, value in
            (line.split() for line in (root / name).read_text().splitlines())}


def validate_controls(controls, limit):
    if controls["memory.max"].strip() != str(limit):
        raise ValueError("memory.max differs from the plan")
    if controls["memory.swap.max"].strip() != "0":
        raise ValueError("swap is not disabled")
    if controls.get("memory.high", "max").strip() != "max":
        raise ValueError("memory.high would throttle the control")


def validate_ancestors(limits, limit):
    if any(value != "max" and (limit == "max" or int(value) < int(limit)) for value in limits):
        raise ValueError("ancestor memory limit invalidates the comparison")


def validate_completion(row):
    if row["exit"] != 0 or row["swap_peak"] != 0 or any(
        value for key, value in row["memory_events"].items() if key.startswith("oom")
    ):
        raise ValueError("build failed, swapped or triggered OOM")


def artifact_snapshot(root):
    manifest = json.loads((root / "manifest.json").read_text())
    del manifest["build_timestamp_unix_seconds"]
    return {"manifest": manifest, "files": {
        str(path.relative_to(root)): digest(path) for path in sorted(root.rglob("*"))
        if path.is_file() and path != root / "manifest.json"
    }}


@contextmanager
def private_run(output, inputs):
    run = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    cleanup = True
    try:
        for source, name in inputs:
            original_inode = source.stat().st_ino
            subprocess.run(["cp", "--reflink=always", str(source), str(run / name)], check=True)
            if original_inode == (run / name).stat().st_ino:
                raise ValueError("input inode was not isolated")
        yield run
    except ServiceStopError:
        cleanup = False
        print(f"Retained {run}: service termination could not be verified", file=sys.stderr)
        raise
    finally:
        if cleanup:
            shutil.rmtree(run)


def verdict(estimate, ceiling, completed, expected):
    if completed != expected or estimate["blocks"] < 2 or estimate["blocks"] * 4 != expected:
        raise ValueError("incomplete fixed-size campaign")
    if estimate["high"] <= ceiling:
        return "qualified"
    if estimate["low"] > ceiling:
        return "regressed"
    return "inconclusive"


def sample_until_exit(process, sample, period):
    """Calls `sample` every `period` seconds until `process` exits.

    Returns the exit code and the monotonic time at which `wait` returned.
    """
    exited = threading.Event()
    result = []

    def wait():
        code = process.wait()
        result.append((code, time.monotonic()))
        exited.set()

    waiter = threading.Thread(target=wait, daemon=True)
    waiter.start()
    while not exited.is_set():
        sample()
        exited.wait(period)
    waiter.join()
    return result[0]


def collect(plan_path, run, receipt, limit):
    plan = json.loads(plan_path.read_text())
    group = Path("/sys/fs/cgroup") / Path("/proc/self/cgroup").read_text().strip().split("::", 1)[1].lstrip("/")
    controls = {name: (group / name).read_text().strip()
                for name in ("memory.max", "memory.swap.max", "memory.high")}
    validate_controls(controls, limit)
    ancestors = {str(parent): {
        name: (parent / name).read_text().strip()
        for name in ("memory.max", "memory.high")
    } for parent in group.parents if (parent / "memory.max").exists()}
    validate_ancestors([value for controls in ancestors.values() for value in controls.values()], limit)
    os.sched_setaffinity(0, plan["affinity"])
    binary = plan["files"]["binary"]["path"]
    argv = [binary, "corpus", "build", "--input-format", "soranoha", "--dict-path",
            str(run / "dictionary.dic"), "--workers", str(plan["workers"]),
            "--max-record-bytes", "16777216", "--progress", "never",
            "--output", str(run / "artifact"), str(run / "source.zip")]
    start = time.monotonic()
    with receipt.with_suffix(".build.log").open("w") as log, receipt.with_suffix(".samples.jsonl").open("w") as samples:
        process = subprocess.Popen(argv, stdout=log, stderr=log, env={**os.environ, "TMPDIR": str(run)})

        def sample():
            row = {"seconds": time.monotonic() - start,
                   "memory_current": int((group / "memory.current").read_text()),
                   "memory_stat": counters(group, "memory.stat")}
            try:
                row["process_kib"] = {
                    key: int(value.split()[0]) for key, value in
                    (line.split(":", 1) for line in Path(f"/proc/{process.pid}/status").read_text().splitlines())
                    if key in {"VmRSS", "RssAnon", "RssFile", "RssShmem"}}
            except (FileNotFoundError, ProcessLookupError):
                pass
            samples.write(json.dumps(row) + "\n")
            samples.flush()
        try:
            code, ended = sample_until_exit(process, sample, 0.25)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    row = {"argv": argv, "exit": code, "wall": ended - start,
           "user": usage.ru_utime, "sys": usage.ru_stime, "rss_kib": usage.ru_maxrss,
           "memory_peak": int((group / "memory.peak").read_text()),
           "memory_events": counters(group, "memory.events"),
           "memory_stat": counters(group, "memory.stat"),
           "memory_pressure": (group / "memory.pressure").read_text(),
           "swap_peak": int((group / "memory.swap.peak").read_text()),
           "controls": controls, "ancestor_limits": ancestors}
    write_json(receipt, row)
    validate_completion(row)


def campaign(args):
    if args.blocks < 3 or args.workers < 1 or args.limit_bytes < 1:
        raise ValueError("at least three blocks, positive workers and a positive limit are required")
    if not math.isfinite(args.wall_ceiling) or args.wall_ceiling < 1:
        raise ValueError("wall ceiling must be finite and at least 1")
    paths = {name: getattr(args, name).resolve(strict=True)
             for name in ("binary", "estimator", "source", "dictionary")}
    paths["harness"] = Path(__file__).resolve()
    output = args.output.resolve()
    filesystem = subprocess.check_output(["findmnt", "-n", "-o", "FSTYPE", "-T", str(output.parent)], text=True).strip()
    if filesystem in {"tmpfs", "ramfs"}:
        raise ValueError("output must be on disk, not tmpfs")
    output.mkdir()
    plan = {"files": {name: {"path": str(path), "sha256": digest(path)} for name, path in paths.items()},
            "blocks": args.blocks, "workers": args.workers, "limit_bytes": args.limit_bytes,
            "wall_ceiling": args.wall_ceiling, "affinity": sorted(os.sched_getaffinity(0)),
            "hostname": platform.node(), "kernel": platform.release(),
            "order": [list("abba" if block % 2 == 0 else "baab") for block in range(args.blocks)],
            "arms": {"a": "unconstrained", "b": "limited"}, "swap_max": 0,
            "interval": "95% Student-t on balanced block log-ratios; one fixed look",
            "cache": "fresh private input and dictionary inodes; no global cache drop"}
    plan_path = output / "plan.json"
    write_json(plan_path, plan)
    (output / "plan.sha256").write_text(digest(plan_path) + "\n")
    reference = None
    rows = []
    blocks = []
    for block, order in enumerate(plan["order"]):
        times = {"a": [], "b": []}
        for position, arm in enumerate(order):
            label = f"{block}-{position}-{arm}"
            print(f"Starting {label}: {plan['arms'][arm]}", flush=True)
            with private_run(output, [(paths["source"], "source.zip"), (paths["dictionary"], "dictionary.dic")]) as run:
                receipt = output / f"{label}.json"
                limit = str(args.limit_bytes) if arm == "b" else "max"
                unit = f"ling-pressure-{uuid.uuid4().hex}"
                command = ["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect",
                           f"--unit={unit}", "-p", f"MemoryMax={'infinity' if limit == 'max' else limit}", "-p", "MemorySwapMax=0",
                           "-p", "OOMPolicy=continue", sys.executable, str(paths["harness"]),
                           "_collect", str(plan_path), str(run), str(receipt), limit]
                try:
                    result = subprocess.run(command)
                except BaseException:
                    stopped = subprocess.run(["systemctl", "--user", "stop", unit])
                    if stopped.returncode != 0:
                        raise ServiceStopError(f"could not stop {unit}")
                    raise
                result.check_returncode()
                row = json.loads(receipt.read_text())
                validate_completion(row)
                snapshot = artifact_snapshot(run / "artifact")
                if reference is not None and snapshot != reference:
                    raise ValueError("artifact bytes or identity differ")
                reference = snapshot
                for name, private in (("source", "source.zip"), ("dictionary", "dictionary.dic")):
                    if digest(run / private) != plan["files"][name]["sha256"]:
                        raise ValueError("private input bytes changed")
                rows.append(row)
                times[arm].append(row["wall"])
                print(f"Completed {label}: {row['wall']:.2f} s, peak charged {row['memory_peak']} bytes", flush=True)
        blocks.append(times["a"] + times["b"])
    for name, path in paths.items():
        if digest(path) != plan["files"][name]["sha256"]:
            raise ValueError(f"{name} changed during the campaign")
    if digest(plan_path) != (output / "plan.sha256").read_text().strip():
        raise ValueError("plan changed during the campaign")
    write_json(output / "artifact.json", reference)
    block_path = output / "blocks.json"
    write_json(block_path, blocks)
    estimate = json.loads(subprocess.check_output([str(paths["estimator"]), "estimate", str(block_path)], text=True))
    status = verdict(estimate, args.wall_ceiling, len(rows), args.blocks * 4)
    write_json(output / "report.json", {"status": status, "wall": estimate,
               "wall_ceiling": args.wall_ceiling, "runs": len(rows), "parity": True})
    print(f"Pressure qualification: {status}; ratio {estimate['ratio']:.4f}, upper bound {estimate['high']:.4f}", flush=True)
    return 0 if status == "qualified" else 1


def interrupt(_signal, _frame):
    raise KeyboardInterrupt


def main():
    signal.signal(signal.SIGTERM, interrupt)
    if len(sys.argv) > 1 and sys.argv[1] == "_collect":
        collect(*(Path(value) for value in sys.argv[2:5]), sys.argv[5])
        return 0
    parser = argparse.ArgumentParser(description="Qualify a Soranoha corpus build under Linux cgroup-v2 memory pressure.")
    for name in ("binary", "estimator", "source", "dictionary", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--blocks", type=int, default=3)
    parser.add_argument("--workers", type=int, default=16)
    parser.add_argument("--limit-bytes", type=int, default=2 * 1024**3)
    parser.add_argument("--wall-ceiling", type=float, default=1.10)
    return campaign(parser.parse_args())


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except (ValueError, OSError, subprocess.CalledProcessError, ServiceStopError) as error:
        sys.exit(f"pressure campaign failed: {error}")
