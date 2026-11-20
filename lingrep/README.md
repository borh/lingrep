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
ling table
ling contrast
ling tokenize
ling coverage
ling explore
ling serve
ling corpus build|inspect|inventory|variants|disk|prune|render
ling neighbors
ling dictionary install
ling analyzer prepare
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
- Omitting a corpus root is an error; pass `-` explicitly for stdin.
- Queries using `--corpus` with a prepared `.lingcorpus` artifact reject
  `--dict-path`, because the artifact already contains analyzed tokens.

Use `ling contracts` to inspect stable machine-output contracts and
`ling describe COMMAND` for arguments, examples, and contracts owned by a
command.
