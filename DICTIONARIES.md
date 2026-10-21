# UniDic dictionaries

`ling` analyzes Japanese text with seven UniDic 202512 dictionaries, converted
for Vibrato and published on the
[`unidic-202512` release](https://github.com/borh/lingrep/releases/tag/unidic-202512).

## Install

```bash
ling dictionary install            # cwj, the default
ling dictionary install novel      # any name from the table below
```

`ling dictionary install` downloads the dictionary, checks its BLAKE3 digest,
and writes it to `$XDG_DATA_HOME/lingrep/dictionaries/`
(`~/.local/share/lingrep/dictionaries/` when `XDG_DATA_HOME` is unset). `ling`
loads the installed `cwj` when neither `--dict-path` nor
`LINGREP_DICTIONARY_PATH` names a dictionary; pass any other with `--dict-path`.

| Name | File | Download | Installed | License |
| --- | --- | --- | --- | --- |
| `cwj` | `unidic-cwj-202512.dic` | 412 MB | 1.31 GB | BSD-3-Clause, GPL-2.0 or LGPL-2.1 |
| `csj` | `unidic-csj-202512.dic` | 430 MB | 1.33 GB | BSD-3-Clause, GPL-2.0 or LGPL-2.1 |
| `novel` | `unidic-novel-202512.dic` | 1.02 GB | 3.36 GB | CC BY-NC-SA 4.0 |
| `qkana` | `unidic-qkana-202512.dic` | 1.07 GB | 3.32 GB | CC BY-NC-SA 4.0 |
| `kindai-bungo` | `unidic-kindai-bungo-202512.dic` | 785 MB | 2.49 GB | CC BY-NC-SA 4.0 |
| `kinsei-edo` | `unidic-kinsei-edo-202512.dic` | 726 MB | 2.65 GB | CC BY-NC-SA 4.0 |
| `kinsei-bungo` | `unidic-kinsei-bungo-202512.dic` | 779 MB | 2.87 GB | CC BY-NC-SA 4.0 |

## Licenses

UniDic is developed by the National Institute for Japanese Language and
Linguistics (NINJAL). `cwj` and `csj` are available under the BSD-3-Clause,
GPL-2.0 or LGPL-2.1 license, at the user's choice. The other five are licensed
under [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/):
they, and the converted files published here, may be used and redistributed
only for non-commercial purposes, with attribution to NINJAL, and under the
same license.

## Nix packages

`nix build .#vibrato-dict-cwj` (through `.#vibrato-dict-kinsei-bungo`) and the
aggregate `.#vibrato-dictionaries` download the same release files, so their
`share/vibrato/*.dic` outputs are identical to what `ling dictionary install`
writes. `nix develop` exports the CWJ path as `LINGREP_DICTIONARY_PATH`.

## Building from source

`nix build .#vibrato-dictionaries-from-source` converts each UniDic archive
from NINJAL with `mecab-dic-converter`. Converting CWJ requires about
10 GB of memory; larger dictionaries require more.

`nix build .#vibrato-dictionaries.tests.digests` verifies the released files
and the conversion from source against the BLAKE3 digests pinned in `flake.nix`,
the same digests `ling dictionary install` checks. Run this check after updating
`flake.lock`: a dictionary digest is part of the analysis identity of every
corpus prepared with it.

## Compressed dictionaries

`--dict-path` also accepts a `.dic.zst`. The loader expands it into
`~/.cache/vibrato-rkyv/` on first use and maps that copy, so the archive and
the expansion both stay on disk. `dictionary_identity` is a digest over the
decompressed bytes, so a `.dic` and its `.zst` have the same identity and
prepared corpora remain valid with either.
