# lingrep

lingrep searches text for linguistic patterns: token sequences described by
surface form, lemma, part of speech, morphological features, or dependency
relations. Patterns are authored in catalogs and reused across
corpora. The command-line tool, `ling`, tokenizes raw text or reads text that
is already analyzed, then matches patterns, counts matches, and presents
matches in context.

Japanese is the main focus, but `ling` also reads CoNLL-U, so Universal
Dependencies treebanks and other analyzed data can be searched without a
dictionary.

lingrep provides a
[pattern language](docs/pattern-language-reference.md) with
[lookaround](docs/pattern-language-reference.md#lookaround),
[dependency constraints](docs/pattern-language-reference.md#dependency-constraints)
and [lexicons](docs/pattern-language-reference.md#lexicons); Japanese analysis
with [UniDic](https://clrd.ninjal.ac.jp/unidic/) through
[Vibrato](https://github.com/daac-tools/vibrato) or with
[Sudachi](https://github.com/WorksApplications/sudachi.rs); input from plain
text, JSONL, [Markdown](docs/markdown-corpus-dialect.md),
[CoNLL-U](https://universaldependencies.org/format.html)
([UD treebanks](https://universaldependencies.org/)),
[BCCWJ](https://clrd.ninjal.ac.jp/bccwj/en/) distributions and
[Soranoha](https://soranoha.org/) archives;
[frequency summaries](docs/cli-usage-guide.md#what-summarize-groups-by),
[attribute coverage](docs/cli-usage-guide.md#attribute-coverage-ling-coverage)
and [sampling](docs/cli-usage-guide.md#sampling-a-corpus-with---sample);
reusable [prepared corpora](docs/cli-usage-guide.md#prepared-v1-corpora);
[n-gram](docs/ngram-analysis.md), [gravity](docs/gravity-analysis.md),
[collexeme](patterns/collexeme.lx) and
[distinctive collexeme](patterns/alternation.lx) analyses; an interactive
[terminal explorer](docs/tui-guide.md); and a
[web interface](docs/deployment-guide.md).

## Install

### Prebuilt binaries

Each [release](https://github.com/borh/lingrep/releases) has `ling` archives
for Linux (x86-64 and arm64) and macOS (Apple silicon). An archive unpacks to
one directory with the binary, shell completions, manual pages, the license,
and third-party notices. The Linux builds require glibc 2.35 or newer.

```bash
tar xzf ling-0.3.0-aarch64-apple-darwin.tar.gz
./ling-0.3.0-aarch64-apple-darwin/bin/ling --help
```

The binaries include no dictionary. To analyze Japanese, build UniDic with Nix
(below) or download Sudachi's dictionary (see
[Japanese without Nix](#japanese-without-nix)). CoNLL-U input and
`ling check` require no dictionary.

### From source with Nix

```bash
nix build .#lingrep
./result/bin/ling --help
```

`./result` also holds shell completions and manual pages.

## Quick start

### Japanese

From a checkout, build the default UniDic dictionary, then search a text file:

```bash
export LINGREP_DICTIONARY_PATH="$(nix build .#vibrato-dict-cwj \
  --no-link --print-out-paths)/share/vibrato/unidic-cwj-202512.dic"

printf '猫が走る。\n犬が眠る。\n' > sample.txt

nix run .#lingrep -- matches \
  --pattern '[upos=VERB].' \
  --context 2 \
  sample.txt
```

[DICTIONARIES.md](DICTIONARIES.md) lists the other UniDic variants.

### Japanese without Nix

Download a Sudachi dictionary and the analyzer's resource files, then pass
them to `ling`:

```bash
curl -LO https://d2ej7fkh96fzlu.cloudfront.net/sudachidict/sudachi-dictionary-20260723-core.zip
unzip -q sudachi-dictionary-20260723-core.zip
mkdir -p sudachi-resources
for file in char.def unk.def rewrite.def sudachi.json; do
  curl -fsSL -o "sudachi-resources/$file" \
    "https://raw.githubusercontent.com/WorksApplications/sudachi.rs/f4dd8f20a774bd71d34a7d4ffa00d987b8946f9e/resources/$file"
done

ling matches \
  --analyzer sudachi \
  --sudachi-dict sudachi-dictionary-20260723/system_core.dic \
  --sudachi-resources sudachi-resources \
  --sudachi-edition core \
  --sudachi-split c \
  --pattern '[upos=VERB].' \
  sample.txt
```

`--sudachi-edition` must match the archive (`small`, `core`, or `full`). ling
supports SudachiDict releases up to `v20260723`; later releases use an
unsupported container format.

### CoNLL-U

CoNLL-U files already carry tokens and annotations, so matching requires no
dictionary. This query matches a noun followed by a token whose dependency
relation is `case`:

```bash
ling matches --pattern '[upos=NOUN] [dep.rel=case].' ja_gsd-ud-test.conllu
```

## Documentation

- [Pattern language reference](docs/pattern-language-reference.md) — writing
  `.lx` catalogs
- [CLI usage guide](docs/cli-usage-guide.md) — commands, inputs, output
  formats, and analysis workflows
- [TUI guide](docs/tui-guide.md) — interactive exploration in the terminal
- [Deployment guide](docs/deployment-guide.md) — serving `lingrep-web`, including
  its NixOS module
- [N-gram analysis](docs/ngram-analysis.md) and
  [gravity analysis](docs/gravity-analysis.md) — worked corpus analyses
- [Documentation index](docs/README.md) — developer, design, testing, and
  performance references

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

lingrep is licensed under the [MIT License](LICENSE).
