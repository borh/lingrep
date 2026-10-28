# Progress reporting

`ling` writes progress to stderr, separate from stdout. With the default
`--progress auto`, deferred human output, such as a `summarize` table, can
show a status line at the next count update after one second. Both streams
must be terminals, and `TERM` must be set to a value other than `dumb`.
Streaming query execution, JSON, TSV and redirected stdout show no progress
under `auto`. Artifact preparation with `--cache` can show status before
streaming results begin.

```bash
# Progress on stderr; stdout bytes are unchanged.
ling matches --json --patterns patterns/sfe.lx \
  --progress always corpus/ >matches.jsonl

ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output corpus.lingcorpus --progress always corpus/
```

`--progress always` shows the first available status immediately. For deferred
human output on capable terminals, it redraws in place. Otherwise it prints
ordinary lines, at most one intermediate update every five seconds plus the
latest final count if it has not already been printed. `never` disables
progress; warnings and errors remain visible in every mode. A failed status
erasure is ignored, and a failed progress write does not fail the run. Select
JSON diagnostics with `--json`.

Progress counts are not a substitute for the final `complete` protocol event.
