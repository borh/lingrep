#!/usr/bin/env python3
"""Start a release binary against a dictionary-free fixture and check its HTTP routes."""

import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.request


def smoke(binary):
    with tempfile.TemporaryDirectory(prefix="ling-serve-smoke-") as scratch:
        root = Path(scratch)
        source = root / "fixture.conllu"
        source.write_text("1\tcat\tcat\tNOUN\t_\t_\t0\troot\t_\t_\n\n", encoding="utf-8")
        corpus = root / "fixture.lingcorpus"
        subprocess.run(
            [binary, "corpus", "build", "--input-format", "conllu",
             "--output", str(corpus), str(source)],
            check=True,
            timeout=60,
        )
        dictionary = root / "unused.dic"
        dictionary.touch()
        log = root / "server.stdout"
        errors = root / "server.stderr"
        with log.open("w") as stdout, errors.open("w") as stderr:
            server = subprocess.Popen(
                [binary, "serve", "--host", "127.0.0.1", "--port", "0",
                 "--input", str(corpus), "--dict-path", str(dictionary)],
                stdout=stdout,
                stderr=stderr,
            )
            try:
                deadline = time.monotonic() + 30
                while True:
                    address = re.search(r"listening on (http://\S+)", log.read_text())
                    if address:
                        break
                    if server.poll() is not None:
                        raise RuntimeError(f"server exited before listening ({server.returncode})")
                    if time.monotonic() >= deadline:
                        raise TimeoutError("server did not report its listening address within 30 seconds")
                    time.sleep(0.05)
                base = address.group(1)
                http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                for route in ["/", "/assets/app.js", "/api/frequency/meta"]:
                    with http.open(base + route, timeout=10) as response:
                        body = response.read()
                        if response.status != 200 or not body:
                            raise RuntimeError(f"{route}: expected a nonempty 200 response")
                        if route.endswith("/meta") and not isinstance(json.loads(body)["pairs"], list):
                            raise RuntimeError("frequency metadata has no pairs list")
                server.terminate()
                server.wait(timeout=15)
                if server.returncode != 0:
                    raise RuntimeError(f"server shutdown failed ({server.returncode})")
            except Exception:
                print(log.read_text(), file=sys.stderr)
                print(errors.read_text(), file=sys.stderr)
                raise
            finally:
                if server.poll() is None:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: smoke-release-server.py PATH_TO_LING")
    smoke(str(Path(sys.argv[1]).resolve()))
