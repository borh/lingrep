# Scripts

Scripts that tokenize text read the dictionary from `LINGREP_DICTIONARY_PATH`,
which `nix develop` exports, so inside the dev shell they need no extra setup:

```bash
scripts/run_test.sh
scripts/measure_sentence_final_profile.sh
```

To run a script against a different dictionary (CSJ, Novel, qkana,
kindai-bungo, kinsei-edo, or kinsei-bungo), override the variable for that run:

```bash
dict_root="$(nix build .#vibrato-dictionaries --no-link --print-out-paths)"
LINGREP_DICTIONARY_PATH="$dict_root/share/vibrato/unidic-csj-202512.dic" \
  scripts/run_test.sh
```

Scripts must not assume a locally built dictionary or a developer-specific Nix
store path.

## Script categories

Each script's header describes its purpose and arguments.

- `check-*` and `audit-*`: repository guards, most of them run by `./ci.sh`.
- `measure_*.sh`, `profile_memory.sh`, `perf_campaign.sh`: measurement
  helpers. Performance decisions are made by the campaign tooling described in
  [`docs/PERFORMANCE_REQUIREMENTS.md`](../docs/PERFORMANCE_REQUIREMENTS.md).
- `test/`, `tests/`, `test_*.sh` and `run_*`: local validation helpers.
- `generate_*`: fixture preparation helpers.
- `package-release.sh` and `third-party-notices.py`: release packaging, run by
  `.github/workflows/release.yml` on a tag. They need a built binary
  (`PROFILE_DIR`, default `target/release`) and no dictionary.

## Hugging Face datasets

`huggingface_dataset.py` is a locked `uv` script that streams one dataset split
as `ling` JSONL or CoNLL-U. JSONL selects one text field and only the
metadata fields you name. CoNLL-U requires the canonical Universal Dependencies
parallel columns and preserves their supplied tokenization and dependency
analysis. Neither mode collects the split in memory.

Inspect configurations, split names, and nested feature types before choosing
those fields. Repositories with several configurations list them first; focus
one to retrieve its schema:

```bash
scripts/huggingface_dataset.py OWNER/DATASET --inspect
scripts/huggingface_dataset.py OWNER/DATASET --inspect --config CONFIG
```

```bash
scripts/huggingface_dataset.py hotchpotch/fineweb-2-edu-japanese \
  --config default --split train --text-field text --limit 1000 |
  ling summarize --input-format jsonl --patterns patterns/sfe.lx -
```

Query an official Universal Dependencies treebank without retokenizing it:

```bash
scripts/huggingface_dataset.py \
  universal-dependencies/universal_dependencies \
  --config ja_gsd --split train --revision 2.18 \
  --output-format conllu |
  ling matches --input-format conllu --patterns patterns.lx -
```

`--text-field` and `--metadata-field` belong to JSONL projection and are
rejected with `--output-format conllu`. The CoNLL-U encoder validates the
required parallel word columns and retains comments, multiword tokens, and
empty nodes when the dataset supplies them.

Piped stdin cannot be cached. To reuse the analysis, first
materialize an ordinary source file, preferably at a pinned Hub revision, then
use the CLI's explicit cache mode:

```bash
scripts/huggingface_dataset.py OWNER/DATASET --config CONFIG --split train \
  --revision COMMIT --text-field text --metadata-field id \
  --output dataset.jsonl
ling summarize --cache --patterns patterns/sfe.lx dataset.jsonl
```

The output path must not already exist. A failed download leaves neither a
published corpus file nor a partial sidecar. Run the script with `--help` for
nested-field selection and other bounds.

On NixOS, run these commands inside `nix develop` (or prefix the script with
`nix develop -c`). The locked PyPI Arrow/NumPy wheels need the C++ runtime path
that the project shell supplies. Conventional Linux, macOS, and Windows Python
installations do not use that Nix-specific wrapper.
