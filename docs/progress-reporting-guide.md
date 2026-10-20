# Progress reporting

`ling` writes progress to stderr, separate from stdout, and only under
`--progress always`.

```bash
# Progress on stderr; stdout bytes are unchanged.
ling matches --json --patterns patterns/sfe.lx \
  --progress always corpus/ >matches.jsonl

ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output corpus.lingcorpus --progress always corpus/
```

Every corpus traversal and execution command accepts `--progress auto`,
`--progress never`, or `--progress always`. The default, `auto`, writes no
progress lines. Progress does not change corpus
identity, canonical ordering, result schemas, or completion counters. JSON
diagnostics can be selected independently with `--json`.

Progress counts are not a substitute for the final `complete` protocol event.

For library callers, `lingrep_core::execution::ProgressSink` receives cumulative
`ExecutionProgress` updates after finalized logical batches.
