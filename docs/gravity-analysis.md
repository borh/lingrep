# Gravity analysis as an external program

Gravity is a corpus-level calculation, not a lingrep patterns matcher primitive.
A lingrep patterns catalog selects the tokens to analyze; an external program
computes adjacent-pair frequencies and gravity scores. The repository includes
[`examples/gravity.py`](../examples/gravity.py) as that integration example.

Emit the selected tokens as matches. For each sentence, the script takes the union
of all complete match spans and preserves token order:

```bash
ling --json matches \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  --pattern '[upos=NOUN].' \
  --context 0 CORPUS \
  | python3 examples/gravity.py
```

Use the inline pattern `[].` to select every token. Filtering belongs in the
lingrep patterns catalog rather than in script flags.

The output is JSONL with one
`{"left":"…","right":"…","count":N,"score":G}` record per observed
adjacent pair, in lexical pair order. Ranking, thresholding, chain formation,
and presentation are left to the calling analysis workflow.

The script accepts only a complete `ling` run with full coverage and EOF
termination, and rejects a failed, cancelled, or limited upstream query.

## Scope and resource boundary

The script holds distinct terms and distinct pairs in Python memory, so it
suits moderate corpora; a large corpus requires a disk-backed implementation
outside this repository.
