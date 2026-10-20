# UniDic dictionaries

lingrep's supported UniDic dictionaries are reproducible Nix packages.

## Quick start

Build the aggregate and select CWJ:

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
export LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-cwj-202512.dic"

ling matches \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx \
  corpus.jsonl
```

Individual packages are also available, for example
`nix build .#vibrato-dict-cwj` or `nix build .#vibrato-dict-novel`. The
aggregate contains these seven UniDic 202512 artifacts:

| Package | Artifact |
| --- | --- |
| `vibrato-dict-cwj` | `unidic-cwj-202512.dic` |
| `vibrato-dict-csj` | `unidic-csj-202512.dic` |
| `vibrato-dict-novel` | `unidic-novel-202512.dic` |
| `vibrato-dict-qkana` | `unidic-qkana-202512.dic` |
| `vibrato-dict-kindai-bungo` | `unidic-kindai-bungo-202512.dic` |
| `vibrato-dict-kinsei-edo` | `unidic-kinsei-edo-202512.dic` |
| `vibrato-dict-kinsei-bungo` | `unidic-kinsei-bungo-202512.dic` |

## Uncompressed artifacts

Each artifact ships uncompressed, as the rkyv-serialized `.dic` that `ling`
memory-maps. A compressed dictionary costs more disk, not less: the loader
expands a `.zst` archive into `~/.cache/vibrato-rkyv/` on first use and maps
that copy, so the archive and the expansion both stay on disk. Building all
seven packages takes about 17 GB of store space.

`dictionary_identity` is a digest over the decompressed bytes, so a `.dic` and
its `.zst` archive have the same identity and the same prepared corpora remain
valid. A user-supplied `.zst` loads, expands into `~/.cache/vibrato-rkyv/`, and
decodes frame-parallel when it contains more than one frame.

## Automation and tests

`ling`, the tests, and the scripts take their default dictionary path from
`LINGREP_DICTIONARY_PATH`, which `nix develop` exports. Inside the dev shell the tests need no extra setup:

```bash
cargo test -p lingrep --test suite -- sentence_final_characterization::
scripts/run_test.sh
```

For a program that needs a different dictionary, override `LINGREP_DICTIONARY_PATH`
with the corresponding artifact below `$dict_root/share/vibrato/`.

## Troubleshooting

If the dictionary is unavailable, build it and export its path:

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
export LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-cwj-202512.dic"
```

The selected file must remain available while a command runs; a Nix store
output satisfies that.
