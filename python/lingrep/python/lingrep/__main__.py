import argparse
import json
import sys

from . import Tokenizer


def main() -> int:
    parser = argparse.ArgumentParser(prog="lingrep", description="Tokenize Japanese with Vibrato and UniDic.")
    commands = parser.add_subparsers(dest="command", required=True)
    tokenize = commands.add_parser("tokenize", help="Print one JSON array of token surfaces per sentence.")
    tokenize.add_argument("text", nargs="?", help="A sentence. Omit to read one sentence per line from stdin.")
    tokenize.add_argument("-d", "--dict-path", help="Path to a Vibrato dictionary.")
    tokenize.add_argument("--workers", type=int, default=1, help="Number of tokenizer workers.")
    args = parser.parse_args()
    if not 1 <= args.workers <= sys.maxsize:
        tokenize.error(f"--workers must be between 1 and {sys.maxsize}")
    try:
        tokenizer = Tokenizer(dict_path=args.dict_path, workers=args.workers)
        sentences = (line.removesuffix("\n") for line in sys.stdin) if args.text is None else [args.text]
        for surfaces in tokenizer.pipe(sentences):
            print(json.dumps(surfaces, ensure_ascii=False))
    except (OSError, ValueError) as error:
        parser.exit(1, f"lingrep: {error}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
