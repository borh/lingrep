from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path

from .adapter import serve
from .bundle import prepare_model_bundle


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        prog="lingrep-stanza-adapter",
        description="Run the bounded Stanza morphology protocol adapter",
    )
    result.add_argument("--resource-root", required=True)
    result.add_argument("--language", required=True)
    result.add_argument("--backend", required=True, choices=("cpu", "cuda", "rocm"))
    result.add_argument("--batch-size", type=int, default=1)
    result.add_argument("--prepare", action="store_true")
    result.add_argument("--probe-text")
    result.add_argument("--package", action="append", default=[])
    result.add_argument("--feature-key", action="append", default=[])
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if args.prepare:
            if not args.probe_text:
                raise ValueError("--prepare requires --probe-text")
            packages = {}
            for value in args.package:
                name, separator, package = value.partition("=")
                if not separator or not name or not package or name in packages:
                    raise ValueError(
                        "--package must be a unique PROCESSOR=PACKAGE pair"
                    )
                packages[name] = package
            prepare_model_bundle(
                Path(args.resource_root),
                language=args.language,
                probe_text=args.probe_text,
                packages=packages,
                layered_feature_keys=args.feature_key,
            )
            return 0
        import blake3

        lock = Path(__file__).resolve().parents[2] / "uv.lock"
        serve(
            sys.stdin.buffer,
            sys.stdout.buffer,
            resource_root=args.resource_root,
            language=args.language,
            backend=args.backend,
            batch_size=args.batch_size,
            lock_digest=blake3.blake3(lock.read_bytes()).hexdigest(),
        )
    except Exception as error:
        print(f"lingrep-stanza-adapter: {error}", file=sys.stderr, flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
