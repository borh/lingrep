#!/usr/bin/env python3

import re
import shlex
import subprocess
import sys


SUMMARY = re.compile(rb"^test result: ok\. (\d+) passed;")
# CSI sequences (ECMA-48) and the ESC ( B charset reset that libtest's
# terminfo colors emit around "ok".
ESCAPE = re.compile(rb"\x1b(?:\[[0-?]*[ -/]*[@-~]|[()][0-~])")


def passed_count(line):
    summary = SUMMARY.match(ESCAPE.sub(b"", line))
    return int(summary.group(1)) if summary else 0


def main():
    command = ["cargo", "test", *sys.argv[1:]]
    passed = 0
    pending = b""
    with subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    ) as process:
        for chunk in iter(lambda: process.stdout.read1(8192), b""):
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            *lines, pending = (pending + chunk).split(b"\n")
            passed += sum(passed_count(line) for line in lines)
        passed += passed_count(pending)
        status = process.wait()
    if status:
        return status if status > 0 else 128 - status
    if passed == 0:
        print(f"no tests executed: {shlex.join(command)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
