# Progress reporting

`ling` writes progress to stderr, separate from stdout. The default,
`--progress auto`, enables progress when stderr is a terminal.

```bash
# Progress on stderr; stdout bytes are unchanged.
ling matches --json --patterns patterns/sfe.lx \
  --progress always corpus/ >matches.jsonl

ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output corpus.lingcorpus --progress always corpus/
```

Every corpus traversal and execution command accepts `--progress auto`,
`--progress never`, or `--progress always`. Use `always` to enable progress
when stderr is redirected, or `never` to suppress it. Select JSON diagnostics
independently with `--json`.

Progress counts are not a substitute for the final `complete` protocol event.
