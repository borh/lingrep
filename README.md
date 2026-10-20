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

### Install script

```bash
curl --proto '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
sh install.sh
```

The script installs the release it was published with. It picks the archive
for the computer, checks the archive's SHA-256 checksum, and copies `ling` to
`~/.local/bin`. If that directory is not on `PATH`, it adds one marked line to
the shell's startup file. It needs no administrator rights, and running it again
updates `ling`. On Windows, run it in WSL. You can read `install.sh` before
running it; [scripts/install.sh](scripts/install.sh) is the source.

| Variable | Effect |
|---|---|
| `LING_VERSION=0.3.2` | Install another release |
| `LING_INSTALL_DIR=/path/to/bin` | Install somewhere other than `~/.local/bin` |
| `LING_NO_MODIFY_PATH=1` | Leave shell startup files unchanged |

### Download a release

Each [release](https://github.com/borh/lingrep/releases) has `ling` archives
for Linux (x86-64 and arm64) and macOS (Apple silicon and Intel). An archive
unpacks to one directory with the binary, shell completions, manual pages, the
license, and third-party notices. The Linux builds require glibc 2.35 or newer.

```bash
version=0.3.1
target=aarch64-apple-darwin  # or x86_64-apple-darwin, x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
base=https://github.com/borh/lingrep/releases/download/v$version
curl -LO "$base/ling-$version-$target.tar.gz"
curl -LO "$base/ling-$version-$target.tar.gz.sha256"
shasum -a 256 -c "ling-$version-$target.tar.gz.sha256"
tar xzf "ling-$version-$target.tar.gz"
export PATH="$PWD/ling-$version-$target/bin:$PATH"
ling --help
```

The archives include no dictionary. To analyze Japanese, download Sudachi's
dictionary ([below](#japanese-with-sudachi)) or build UniDic with Nix.
CoNLL-U input and `ling check` require no dictionary.

### Try it with Nix

```bash
nix run github:borh/lingrep#ling -- --help
```

## Quick start

### Japanese with Sudachi

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

printf '猫が走る。\n犬が眠る。\n' > sample.txt

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

### Japanese with UniDic

UniDic dictionaries are built with Nix. The conversion needs about 10 GB of
memory:

```bash
export LINGREP_DICTIONARY_PATH="$(nix build github:borh/lingrep#vibrato-dict-cwj \
  --no-link --print-out-paths)/share/vibrato/unidic-cwj-202512.dic"

ling matches --pattern '[upos=VERB].' --context 2 sample.txt
```

[DICTIONARIES.md](DICTIONARIES.md) lists the other UniDic variants.

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
