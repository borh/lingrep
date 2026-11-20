# lingrep

lingrep is a linguistic `grep`: whereas standard `grep` matches characters and
regular expressions across plain text lines, lingrep searches token sequences
described by surface form, lemma, part of speech, morphological features, or
dependency relations. As with `grep`, you can pass patterns directly on the
command line, explore them interactively, or organize them into catalogs for
reuse across corpora. The command-line tool, `ling`, tokenizes raw text or reads
text that is already analyzed, then matches patterns, counts matches, and
presents matches in context.

Japanese is the main focus, and `ling` also reads CoNLL-U so you can search
Universal Dependencies treebanks and other analyzed data without a parser.

Capabilities:

- [Pattern language](docs/pattern-language-reference.md) with
  [lookaround](docs/pattern-language-reference.md#lookaround),
  [dependency constraints](docs/pattern-language-reference.md#dependency-constraints),
  and [lexicons](docs/pattern-language-reference.md#lexicons)
- Japanese analysis with [UniDic](https://clrd.ninjal.ac.jp/unidic/) through
  [Vibrato](https://github.com/daac-tools/vibrato) or with
  [Sudachi](https://github.com/WorksApplications/sudachi.rs)
- Input from plain text, JSONL, [Markdown](docs/markdown-corpus-dialect.md),
  [CoNLL-U](https://universaldependencies.org/format.html)
  ([UD treebanks](https://universaldependencies.org/)),
  [BCCWJ](https://clrd.ninjal.ac.jp/bccwj/en/) distributions, and
  [Soranoha](https://soranoha.org/) archives
- [Frequency summaries](docs/cli-usage-guide.md#what-summarize-groups-by),
  [attribute coverage](docs/cli-usage-guide.md#attribute-coverage-ling-coverage),
  [sampling](docs/cli-usage-guide.md#sampling-a-corpus-with---sample), and
  reusable [prepared corpora](docs/cli-usage-guide.md#prepared-v1-corpora)
- [N-gram](docs/ngram-analysis.md), [gravity](docs/gravity-analysis.md),
  [collexeme](patterns/collexeme.lx), and
  [distinctive collexeme](patterns/alternation.lx) analyses
- Interactive [terminal explorer](docs/tui-guide.md) and
  [web interface](docs/deployment-guide.md)

> [!NOTE]
> lingrep is experimental software under active development. Pattern syntax and
> command-line interfaces change between releases.

## Install

```bash
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
sh install.sh
```

The script installs `ling` on Linux and macOS (on Windows, in WSL) into
`~/.local/bin`, or `$XDG_BIN_HOME` when set. With Nix, run
`nix run github:borh/lingrep#ling -- --help` instead.
[Install ling](docs/installation.md) lists the installer options, manual
download steps, and [update instructions](docs/installation.md#install-script).

## Quick start

Patterns are written in the lingrep pattern language. Each bracket matches one
token by its attributes, and a pattern ends with `.`: `[upos=VERB].` matches
any verb, and `[upos=NOUN] [text=が].` matches a noun followed by が. A catalog
is a `.lx` file consisting of one or more patterns, reused across corpora.

### CoNLL-U

CoNLL-U files already carry tokens and annotations, so matching requires no
dictionary download. This query over the UD Japanese GSD test set matches a noun
followed by a token whose dependency relation is `case`:

```bash
curl -LO https://raw.githubusercontent.com/UniversalDependencies/UD_Japanese-GSD/master/ja_gsd-ud-test.conllu
ling matches --pattern '[upos=NOUN] [dep.rel=case].' ja_gsd-ud-test.conllu
```

Each match is printed in context: in bold font, or in brackets when color is
disabled:

```text
これに不快 ⟦感を⟧ 示す住民はいまし
に不快感を示す ⟦住民は⟧ いましたが,
```

### Japanese with UniDic

`ling` analyzes Japanese with UniDic by default. Install the dictionary once
(a 412 MB download):

```bash
ling dictionary install
printf '猫が走る。\n犬が眠る。\n' > sample.txt
ling matches --pattern '[upos=VERB].' sample.txt
```

```text
猫が ⟦走る⟧ 。
犬が ⟦眠る⟧ 。
```

To count matches for every pattern in a catalog, save two patterns as
`patterns.lx`:

```text
verb := [upos=VERB].
subject := [upos=NOUN] [text=が].
```

and summarize them:

```bash
ling summarize --patterns patterns.lx sample.txt
```

```text
       1  verb  眠る
       1  verb  走る
       1  subject  犬が
       1  subject  猫が
```

[DICTIONARIES.md](DICTIONARIES.md) lists the other UniDic dictionaries, for
spoken, literary and historical Japanese, and their licenses.

### Japanese with Sudachi

`ling` also supports Sudachi. Download a
[SudachiDict](https://github.com/WorksApplications/SudachiDict/releases)
dictionary (v20261015 or later), then pass `--analyzer sudachi`:

```bash
curl -LO https://github.com/WorksApplications/SudachiDict/releases/download/v20261015/sudachi-dictionary-20261015-core.zip
unzip -q sudachi-dictionary-20261015-core.zip

ling matches --analyzer sudachi \
  --sudachi-dict sudachi-dictionary-20261015/system_core.dic \
  --pattern '[upos=VERB].' sample.txt
```

```text
猫が ⟦走る⟧ 。
犬が ⟦眠る⟧ 。
```

The [CLI usage guide](docs/cli-usage-guide.md#use-native-sudachi) describes the
split modes and the other Sudachi options.

### Python

The Python package includes the Vibrato tokenizer. The `uvx` command requires
a release newer than 0.9.0. Pass a sentence as an argument, or pipe one sentence
per line:

```bash
uvx lingrep tokenize '猫が眠る。'
printf '猫が眠る。\n犬が走る。\n' | uvx lingrep tokenize --workers 4
```

Each output line is a JSON array of token surfaces. The first command prints
`["猫", "が", "眠る", "。"]`. For the Python API, use `uv add lingrep` or
`pip install lingrep`. To try the development version from this checkout, use
`uvx --from ./python/lingrep lingrep tokenize '猫が眠る。'` in the development
shell.

On first use, `Tokenizer()` and the tokenizer command download the default
UniDic CWJ dictionary when it is missing. They verify its BLAKE3 digest and
store the dictionary (about 1.3 GB) in the same data directory as
`ling dictionary install`. Subsequent tokenization works offline. To use an
existing dictionary, pass `dict_path=` in Python, pass `--dict-path` to
`tokenize`, or set `LINGREP_DICTIONARY_PATH`.

Each input to `Tokenizer.pipe` is one sentence of at most 1 MiB of UTF-8.
Results arrive in input order. For `Ling.search` and `Ling.check`, install
the matching [full CLI](#install).

```python
from lingrep import Ling, Query, Tokenizer

tokenizer = Tokenizer(workers=4)
print(tokenizer.tokenize("猫が眠る。"))
for surfaces in tokenizer.pipe(["猫が眠る。", "犬が走る。"]):
    print(surfaces)

subject = Query("subject := (?<noun> [upos=NOUN]) [surface=が].").compile()
verb = Query("[upos=VERB].").compile()
for sentence in tokenizer.analyze_pipe(["猫が眠る。", "犬が走る。"]):
    print([(token.surface, token.lemma, token["unidic.cform"]) for token in sentence])
    for hit in subject.find_matches(sentence):
        noun = hit["captures"][0]["char_span"]
        print(sentence.text[noun["start"]:noun["end"]])
    print(verb.find_matches(sentence))

with Ling().search("[upos=NOUN].", text="猫が眠る。") as results:
    for hit in results:
        print(hit["surface"])
    print(results.completion["coverage"])
```

`tokenize` and `pipe` return surfaces. `analyze(text)` retains one `Sentence`
with source offsets and every attribute in `tokenizer.schema`; pass
`attributes=` to keep fewer. A sentence is a sequence of tokens: read an
attribute as `token.lemma` or `token["unidic.cform"]`, or a whole column with
`sentence.column("lemma")`.

`compile()` and `find_matches(sentence)` run in Python's process and return raw
matches in canonical order. A compiled query matches any sentence of its schema
that carries the attributes it reads, listed in `compiled.attributes`. Spans
have the form of `ling --json` records: `span` is a half-open token range
`{"start": 0, "end": 1}`, and `char_span` is a half-open Python string range,
or `None` for an empty span.

`Ling().search(query, ...)` runs the `ling` executable, which applies catalog
publication policy; pass `Ling(path)` when `ling` is not on PATH. Use
`Query.from_file(path)` for a catalog, `search(path=...)` for a corpus source,
and `search(corpus=...)` for a prepared artifact. `source_metadata=` accepts
the CLI's selectors and `analyzer_config=` accepts a prepared analyzer configuration.
When the stream ends, `results.diagnostics` holds the warnings `ling` reported,
each also raised as a Python warning. A failed run raises `SearchError`, a
`subprocess.CalledProcessError` whose message includes the reported errors.

For a fixed query, `tokenizer.match_pipe(texts, "[upos=NOUN].")` yields
one list of raw matches per input sentence, including empty lists. It compiles
once per stream, selects the attributes that query reads, and uses the tokenizer's
worker pool for both analysis and matching. For repeat searches on retained
sentences, `sentence[i]`, `sentence.column("lemma")` and `sentence.char_spans`
use the same indices as the captures. Record `tokenizer.dictionary_path`,
`tokenizer.dictionary_digest` and `tokenizer.analysis_identity` with a run.

Install the `arrow` extra for token tables. Each record batch includes the
sentence index, token index and sentence-relative character offsets as
unsigned integers. Conversion
materializes the selected columns. Consumers can process one batch at a time.

```python
import polars as pl
from lingrep import token_batches

for batch in token_batches(tokenizer.analyze_pipe(texts), attributes=["surface", "lemma"]):
    frame = pl.from_arrow(batch)
```

Install the `polars` extra to tokenize or search a sentence column inside a
lazy query. Each expression returns one list per row and preserves nulls.

```python
import polars as pl
from lingrep import Query, Tokenizer
from lingrep.polars_expr import matches, tokenize

tokenizer = Tokenizer(workers=4)
nouns = Query("[upos=NOUN].")
sentences = pl.scan_parquet("sentences.parquet")
hits = sentences.with_columns(hits=matches(pl.col("text"), nouns, tokenizer=tokenizer))
tokens = sentences.with_columns(tokens=tokenize(
    pl.col("text"), tokenizer=tokenizer, attributes=["lemma", "upos", "char_start", "char_end"]
))
```

Omit `attributes` for a list of surfaces. Supplying it gives one struct per
token. `char_start` and `char_end` are half-open Python string offsets into
that row's sentence. `matches` accepts a pattern string, a `Query` or a
`CompiledQuery`, compiles once for the expression, and analyzes only the
attributes the query reads. Spans are `{start, end}` structs of `UInt32`.

Each hit includes the pattern, token and character spans, captures and lexical
identity. The `interpretations_json` and `graph_bindings_json` fields contain
JSON arrays so query-specific keys and value types survive across batches.
Graph bindings have the fields of `ling --json` match records in both the
Polars expression and `find_matches`.

With the corresponding lingrep adapter and NLP library installed, use a
document matcher on the output of an existing pipeline. Construct it once
from a pattern string or `Query.from_file(path)`.

```python
from lingrep.spacy_matcher import Matcher

subjects = Matcher(
    "subject := (?<noun> [upos=NOUN]) where (?<verb> [upos=VERB]) -[nsubj]-> $noun.",
    attributes=("text", "upos", "dep.head", "dep.rel"),
)
for doc in nlp.pipe(texts):  # an existing spaCy or GiNZA pipeline
    doc.spans["subjects"] = [hit.span for hit in subjects(doc)]
```

`hit.span` and `hit.captures` are spans of the original spaCy document, labeled
with the pattern and capture names. For Stanza:

```python
from lingrep.stanza_matcher import Matcher

articles = Matcher("article := (?<det> [upos=DET]).")
for hit in articles(doc):  # an existing Stanza Document
    for word in hit.words:
        print(word.text, word.parent.text)
```

Stanza matches and captures retain the original syntactic words, including
components of a multiword token. Both matchers expose the complete lingrep
record as `hit.raw`, and both accept `dep.head` and `dep.rel` in `attributes=`
for `where` clauses over a parsed document. To work with lingrep sentences
directly, use `from_spacy(doc)` or `from_stanza(doc)` and compile with
`Query(...).compile(sentence.schema)`.

## Documentation

- [Pattern language reference](docs/pattern-language-reference.md): writing
  `.lx` catalogs
- [CLI usage guide](docs/cli-usage-guide.md): commands, inputs, output
  formats, and analysis workflows
- [TUI guide](docs/tui-guide.md): interactive exploration in the terminal
- [Deployment guide](docs/deployment-guide.md): serving `lingrep-web`, including
  its NixOS module
- [N-gram analysis](docs/ngram-analysis.md) and
  [gravity analysis](docs/gravity-analysis.md): worked corpus analyses
- [Documentation index](docs/README.md): developer, testing, and performance
  references

`ling check --help` and `ling-check(1)` are the complete reference for the
pattern language.

## Development

```bash
nix develop
./ci.sh --fast
```

The [development environment guide](docs/development-environment.md) covers
toolchains and setup; [CONTRIBUTING.md](CONTRIBUTING.md) describes the change
workflow and verification.

## License

lingrep is licensed under the [MIT License](LICENSE), except files that state
another license, such as the UD Japanese GSD sample in `lingrep/tests/fixtures`
(CC BY-SA 4.0). In release archives, `THIRD-PARTY-NOTICES.md` lists the
licenses of the Rust crates compiled into `ling` and of the NDC9 data it embeds;
[lingrep-web/assets/THIRD-PARTY-NOTICES.md](lingrep-web/assets/THIRD-PARTY-NOTICES.md)
lists the bundled browser libraries.
