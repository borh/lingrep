# lingrep

lingrep searches text for linguistic patterns: token sequences described by
surface form, lemma, part of speech, morphological features, or dependency
relations. Patterns are authored in catalogs and reused across
corpora. The command-line tool, `ling`, tokenizes raw text or reads text that
is already analyzed, then matches patterns, counts matches, and presents
matches in context.

Japanese is the main focus, but `ling` also reads CoNLL-U, so Universal
Dependencies treebanks and other analyzed data can be searched without a
parser.

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

```bash
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -fsSLO https://github.com/borh/lingrep/releases/latest/download/install.sh
sh install.sh
```

The script installs `ling` on Linux and macOS (on Windows, in WSL) into
`~/.local/bin`, or `$XDG_BIN_HOME` when it is set. With Nix, run
`nix run github:borh/lingrep#ling -- --help` instead.
[Install ling](docs/installation.md) lists the installer's options and the
manual download.

## Quick start

Patterns are written in the lingrep pattern language. Each bracket matches one
token by its attributes, and a pattern ends with `.`: `[upos=VERB].` matches
any verb, and `[upos=NOUN] [text=が].` matches a noun followed by が. A catalog
is a `.lx` file of named patterns, reused across corpora.

### CoNLL-U

CoNLL-U files already carry tokens and annotations, so matching requires no
dictionary. This query over the UD Japanese GSD test set matches a noun
followed by a token whose dependency relation is `case`:

```bash
curl -LO https://raw.githubusercontent.com/UniversalDependencies/UD_Japanese-GSD/master/ja_gsd-ud-test.conllu
ling matches --pattern '[upos=NOUN] [dep.rel=case].' ja_gsd-ud-test.conllu
```

Each match is printed in its context, with the match in brackets:

```text
これに不快 ⟦感を⟧ 示す住民はいまし
に不快感を示す ⟦住民は⟧ いましたが,
```

### Japanese with Sudachi

Download a [SudachiDict](https://github.com/WorksApplications/SudachiDict/releases)
dictionary (v20261015 or later), then pass it to `ling`:

```bash
curl -LO https://github.com/WorksApplications/SudachiDict/releases/download/v20261015/sudachi-dictionary-20261015-core.zip
unzip -q sudachi-dictionary-20261015-core.zip
printf '猫が走る。\n犬が眠る。\n' > sample.txt

ling matches --analyzer sudachi \
  --sudachi-dict sudachi-dictionary-20261015/system_core.dic \
  --pattern '[upos=VERB].' sample.txt
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
ling summarize --analyzer sudachi \
  --sudachi-dict sudachi-dictionary-20261015/system_core.dic \
  --patterns patterns.lx sample.txt
```

```text
       1  verb  眠る
       1  verb  走る
       1  subject  犬が
       1  subject  猫が
```

The [CLI usage guide](docs/cli-usage-guide.md#use-native-sudachi) describes the
split modes and the other Sudachi options.

### Japanese with UniDic

`ling` uses UniDic through Vibrato by default. The UniDic dictionaries are built
with Nix, which requires approximately 10 GB of memory; without Nix, use Sudachi.

```bash
export LINGREP_DICTIONARY_PATH="$(nix build github:borh/lingrep#vibrato-dict-cwj \
  --no-link --print-out-paths)/share/vibrato/unidic-cwj-202512.dic"

printf '猫が走る。\n犬が眠る。\n' > sample.txt
ling matches --pattern '[upos=VERB].' --context 2 sample.txt
```

[DICTIONARIES.md](DICTIONARIES.md) lists the other UniDic variants.

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
- [Documentation index](docs/README.md) — developer, testing, and performance
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
(CC BY-SA 4.0). Release archives list the Rust crates `ling` links and the NDC9
data it embeds in `THIRD-PARTY-NOTICES.md`;
[lingrep-web/assets/THIRD-PARTY-NOTICES.md](lingrep-web/assets/THIRD-PARTY-NOTICES.md)
lists the bundled browser libraries.
