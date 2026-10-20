from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path

from .adapter import serve
from .bundle import prepare_model_bundle


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        prog="lingrep-spacy-adapter",
        description="Run the bounded spaCy morphology protocol adapter",
    )
    result.add_argument(
        "--model-root", required=True, help="sealed spaCy model directory"
    )
    result.add_argument("--backend", required=True, choices=("cpu", "cuda", "rocm"))
    result.add_argument("--batch-size", type=int, default=32)
    result.add_argument("--prepare", action="store_true")
    result.add_argument("--probe-text")
    result.add_argument("--morphology-key", action="append", default=[])
    result.add_argument("--boundary-component")
    return result


def main(argv: Sequence[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        import blake3

        if args.prepare:
            if not args.probe_text:
                raise ValueError("--prepare requires --probe-text")
            prepare_model_bundle(
                Path(args.model_root),
                probe_text=args.probe_text,
                morphology_keys=args.morphology_key,
                boundary_component=args.boundary_component,
            )
            return 0
        lock = Path(__file__).resolve().parents[2] / "uv.lock"
        lock_digest = blake3.blake3(lock.read_bytes()).hexdigest()
        serve(
            sys.stdin.buffer,
            sys.stdout.buffer,
            model_root=args.model_root,
            backend=args.backend,
            batch_size=args.batch_size,
            lock_digest=lock_digest,
        )
    except Exception as error:  # The process boundary owns terminal diagnostics.
        print(f"lingrep-spacy-adapter: {error}", file=sys.stderr, flush=True)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
