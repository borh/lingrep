# ling

`ling` is lingrep's packaged command-line interface. It validates lingrep patterns
catalogs, searches and summarizes Japanese corpora, emits morphological token
records, prepares reusable corpus artifacts, and provides the terminal
explorer.

Start with the repository [README](../README.md) for installation and a first
query. The complete workflow reference is the
[CLI usage guide](../docs/cli-usage-guide.md).

## Command groups

Corpus work:

```text
ling matches
ling summarize
ling tokenize
ling coverage
ling explore
ling corpus build|inspect|inventory|variants
```

Catalog and interface discovery:

```text
ling check
ling doctor
ling describe
ling contracts
ling completions
ling manpages
```

Run `ling <command> --help` for current arguments and output modes.
`ling check --help` and `ling-check(1)` contain the complete lingrep patterns
language reference.

## Defaults

- Direct files and directories are analyzed without reading or writing a
  prepared artifact. Add `--cache` to build and reuse one.
- Human-readable output is the default. Use `--json` or a command's supported
  `--tsv` mode for versioned machine output.
- Omitted roots are errors; pass `-` explicitly for stdin.
- `--corpus` with a prepared `.lingcorpus` artifact rejects `--dict-path`,
  because the artifact already contains analyzed tokens.

Use `ling contracts` to inspect stable machine-output contracts and
`ling describe COMMAND` for arguments, examples, and contracts owned by a
command.
