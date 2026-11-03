#!/usr/bin/env python3
import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys


def probe(argv):
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=5)
        return {"argv": argv, "exit": result.returncode,
                "stdout": result.stdout.strip(), "stderr": result.stderr.strip()}
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"argv": argv, "error": str(error)}


def host():
    system = platform.system()
    observations = {}
    if system == "Darwin":
        commands = {
            "os_build": ["sw_vers"],
            "cpu_memory": ["sysctl", "hw.model", "machdep.cpu.brand_string", "hw.logicalcpu",
                           "hw.physicalcpu", "hw.perflevel0.physicalcpu", "hw.perflevel1.physicalcpu",
                           "hw.memsize", "hw.pagesize"],
            "power": ["pmset", "-g", "custom"],
            "thermal": ["pmset", "-g", "therm"],
            "swap": ["sysctl", "vm.swapusage"],
            "memory": ["vm_stat"],
            "xctrace": ["xctrace", "version"],
        }
        affinity = {"supported": False, "effective": None}
    elif system == "Linux":
        commands = {"cpu_memory": ["lscpu"], "memory": ["free", "-b"],
                    "perf": ["perf", "--version"]}
        affinity = {"supported": True, "effective": sorted(os.sched_getaffinity(0))}
        for pattern in ("devices/system/cpu/cpufreq/policy*/scaling_governor",
                        "devices/system/cpu/cpufreq/policy*/energy_performance_preference",
                        "devices/system/cpu/cpufreq/boost", "devices/system/cpu/intel_pstate/no_turbo",
                        "class/thermal/thermal_zone*/temp"):
            for path in sorted(Path("/sys").glob(pattern)):
                try:
                    observations[str(path)] = {"value": path.read_text().strip()}
                except OSError as error:
                    observations[str(path)] = {"error": str(error)}
    else:
        raise ValueError("native measurement supports Linux and macOS only")
    for name, argv in commands.items():
        observations[name] = probe(argv)
    return {"measurement_backend": "gnu-time",
            "system": system, "release": platform.release(), "version": platform.version(),
            "architecture": platform.machine(), "hostname": platform.node(),
            "logical_cpus": os.cpu_count(), "page_bytes": os.sysconf("SC_PAGE_SIZE"),
            "affinity": affinity, "observations": observations,
            "profilers": {tool: shutil.which(tool) for tool in
                          ("perf", "sample", "footprint", "vmmap", "xctrace")},
            "profiling_access": "probe the target process separately; tool presence does not establish permission",
            "memory_capacity": "not qualified; no enforced memory or swap limit",
            "memory_scope": "process RSS; host memory/swap observations are not process footprint or compressed memory"}


def run_state():
    system = platform.system()
    observations = {}
    if system == "Darwin":
        observations = {"thermal": probe(["pmset", "-g", "therm"]),
                        "swap": probe(["sysctl", "vm.swapusage"])}
    return {"system": system, "observations": observations}


def measure(argv, output, consumer_argv=None, gnu_time=None):
    before = run_state()
    row = {"argv": argv, "before": before, "clock": "GNU time",
           "usage_scope": "direct child and descendants it waited for",
           "memory_capacity": "not qualified"}
    process = None
    reaped = False
    consumer = None
    timing = output.with_suffix(".time.txt")
    timer = gnu_time or os.environ.get("TIME_BIN") or shutil.which("time")
    if not timer:
        raise ValueError("GNU time is required")
    command = [str(timer), "-o", str(timing), "-f", "%e\t%U\t%S\t%M", *argv]
    row["timer"] = str(timer)
    try:
        if consumer_argv:
            consumer = subprocess.Popen(consumer_argv, stdin=subprocess.PIPE, start_new_session=True)
        process = subprocess.Popen(command, stdout=consumer.stdin if consumer else None,
                                   start_new_session=True)
        if consumer:
            consumer.stdin.close()
        _, status, _ = os.wait4(process.pid, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
        reaped = True
        wall, user, system, rss = map(float, timing.read_text().splitlines()[-1].split())
        row.update(exit=process.returncode, wall_seconds=wall, user_seconds=user,
                   system_seconds=system, peak_rss_bytes=rss * 1024,
                   gnu_peak_rss_kib=rss)
        if consumer:
            row["consumer_argv"] = consumer_argv
            row["consumer_exit"] = consumer.wait()
    except BaseException as error:
        row["error"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        if process is not None and not reaped:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, status, _ = os.wait4(process.pid, 0)
            process.returncode = os.waitstatus_to_exitcode(status)
        if consumer is not None:
            if consumer.poll() is None:
                os.killpg(consumer.pid, signal.SIGKILL)
            consumer.wait()
        row["after"] = run_state()
        output.write_text(json.dumps(row, indent=2, sort_keys=True) + "\n")
    code = process.returncode
    if code == 0 and consumer:
        code = consumer.returncode
    return code if code >= 0 else 128 - code


def memory_snapshot(pid, output):
    if pid < 1:
        raise ValueError("PID must be positive")
    output.mkdir()
    row = {"pid": pid, "host": host(), "scope": "sequential snapshots; not atomic or peak measurements",
           "memory_capacity": "not qualified"}
    if platform.system() == "Darwin":
        row["footprint"] = probe(["footprint", "-j", str(output / "footprint.json"),
                                  "-f", "bytes", "--swapped", "-p", str(pid)])
        row["vmmap"] = probe(["vmmap", "-summary", str(pid)])
        row["rss_kib"] = probe(["ps", "-o", "rss=", "-p", str(pid)])
        row["process_compression_and_swap"] = "footprint swapped column combines compressed and swapped memory"
        try:
            data = json.loads((output / "footprint.json").read_text())
            if data["unit"] != "byte" or data["bytes per unit"] != 1:
                raise ValueError("unrecognized footprint units")
            process = next(item for item in data["processes"] if item["pid"] == pid)
            row["physical_footprint_bytes"] = process["footprint"]
            row["footprint_categories_bytes"] = process["categories"]
        except (OSError, ValueError, KeyError, StopIteration) as error:
            row["footprint_error"] = str(error) or "target absent from footprint report"
    elif platform.system() == "Linux":
        for name in ("status", "smaps_rollup"):
            try:
                row[name] = {"scope": "process", "text": Path(f"/proc/{pid}/{name}").read_text()}
            except OSError as error:
                row[name] = {"error": str(error)}
        row["host_meminfo"] = {"scope": "host", "text": Path("/proc/meminfo").read_text()}
        row["physical_footprint"] = "Darwin footprint metric unavailable on Linux"
    (output / "snapshot.json").write_text(json.dumps(row, indent=2, sort_keys=True) + "\n")


def interrupted(signum, frame):
    raise InterruptedError(signum, signal.strsignal(signum))


def main():
    parser = argparse.ArgumentParser(description="Native process timing and host observations; no capacity verdict")
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("host")
    memory = commands.add_parser("memory", help="Collect memory breakdown separately from timing")
    memory.add_argument("pid", type=int)
    memory.add_argument("--output", type=Path, required=True)
    run = commands.add_parser("run")
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--gnu-time", type=Path)
    run.add_argument("--digest-tool", type=Path)
    run.add_argument("--digest-affinity")
    run.add_argument("argv", nargs=argparse.REMAINDER)
    fields = commands.add_parser("fields")
    fields.add_argument("receipt", type=Path)
    args = parser.parse_args()
    if args.command == "host":
        print(json.dumps(host(), indent=2, sort_keys=True))
        return 0
    if args.command == "memory":
        memory_snapshot(args.pid, args.output)
        return 0
    if args.command == "fields":
        row = json.loads(args.receipt.read_text())
        print(row["wall_seconds"], row["user_seconds"], row["system_seconds"],
              row["peak_rss_bytes"] / 1024, sep="\t")
        return 0
    argv = args.argv[1:] if args.argv[:1] == ["--"] else args.argv
    if not argv:
        parser.error("a command is required after --")
    if args.output.exists():
        parser.error("receipt already exists")
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    try:
        consumer = None
        if args.digest_tool:
            consumer = [str(args.digest_tool), "digest-stream"]
            if args.digest_affinity:
                consumer = ["taskset", "-c", args.digest_affinity] + consumer
        elif args.digest_affinity:
            parser.error("--digest-affinity requires --digest-tool")
        return measure(argv, args.output, consumer, args.gnu_time)
    except KeyboardInterrupt:
        return 130
    except InterruptedError as error:
        return 128 + error.errno


if __name__ == "__main__":
    sys.exit(main())
