# N-gram analysis with lingrep patterns

The
[`patterns/ngrams.lx`](../patterns/ngrams.lx) catalog emits overlapping token
windows from one through five tokens, and `ling summarize` counts their
ordered keys.

```bash
ling summarize \
  --patterns patterns/ngrams.lx \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  --tsv \
  corpus.jsonl
```

Use `--corpus artifact.lingcorpus` in place of the input path and dictionary for
a prepared-corpus run. The output's `pattern` column identifies the order:
`unigram`, `bigram`, `trigram`, `fourgram`, or `fivegram`.

Each key records `text` and `upos` separately for every token position. These
separate fields preserve token boundaries even when a value contains punctuation
or whitespace. Key publication is all-or-nothing: if either attribute is absent
on any token, that match contributes to `records_unkeyed` instead of a partial
tuple. Use `ling coverage --patterns patterns/ngrams.lx ...` to check
attribute coverage for a corpus or analyzer.

To count a linguistic selection, write a smaller catalog. For example, a noun
followed by a verb:

```pattern
key: [$noun.text, $noun.upos, $verb.text, $verb.upos]
pattern noun_verb := (?<noun> [upos=NOUN]) (?<verb> [upos=VERB]).
```

`--top` limits ranked summary output. Minimum-frequency filtering, relative
frequency, matrix transposition, and custom separators are presentation steps;
apply them to the JSONL or TSV output.
