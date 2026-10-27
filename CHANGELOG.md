# Changelog

All notable changes to lingrep are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Downloadable Python wheels and checksums for spaCy and Stanza setup,
  including the shared protocol package.

### Changed

- Adapter packages use the CLI release version. Preparing a bundle with a later
  release's wheels changes its analyzer identity and the identities of corpora
  prepared with it, even when the adapter code is unchanged. Upgrading `ling` alone
  does not require setup or rebuilding existing corpora, provided their format
  remains supported.

## [0.5.0] - 2026-10-27

### Added

- TEI P5 input through `--input-format tei`, and Soranoha TEI archives through
  `--input-format soranoha-tei`. Soranoha TEI preserves the paired plaintext
  edition's principal text and sentence boundaries. Query inline readings,
  elements, rendering labels and source references through `tei.ruby`,
  `tei.element`, `tei.rend` and `tei.source`; prepared corpora retain them.
- Reusable external-analyzer configurations and verified spaCy and Stanza
  bundles, with setup entry points in the adapter wheels. Nix also packages
  a sealed English spaCy CPU model and analyzer configuration.
- Web corpus scoping, compound-chain and overlap inspection, document counts,
  section comparisons, KWIC sorting, and bounded paragraph/document reading.
  Scope and filters apply to comparisons, exports and shared links.

### Changed

- Long sentences use singleton batches automatically within the documented
  resource limits. Sudachi processes long input in chunks and restores the
  original sentence offsets. Soranoha plaintext record limits apply per line,
  with a separate bound on each inflated work.
- Common CLI help and terminal reports show routine options and readable
  summaries; `--help-all` retains advanced controls. Resource validation applies
  to the selected analyzer.
- NFC normalization uses Unicode 18 tables, and identities and manifests record
  the table version. NFC prepared corpora with a missing or different
  normalization-table version must be rebuilt.
- Markdown front matter uses a parser that rejects duplicate keys. The Markdown
  reader identity changes; rebuild from source to use the revised reader.
- Prepared-corpus builds collect postings while writing tokens, preserving
  artifact bytes and identities while avoiding a second reconstruction pass.

### Fixed

- `tokenize --limit` caps direct, stdin and prepared sentence output, including
  the bounded human-output default, and reports prefix completion accurately.
- Dependency-pattern exclusions check captured negative edges against final
  capture assignments. Inline and named set operands produce the same
  `has-any` results regardless of authored operand order.
- Human concordances preserve source spacing, and terminal progress is enabled
  automatically when stderr is a terminal.
- Dictionary installations and Nix packages include upstream license notices.
- Dependency updates address RustSec findings and remove unmaintained packages.
  All locked Rust graphs use patched `zstd 0.14.3` and `zstd-safe 8.2.0`, removing
  dependencies affected by memory-disclosure and soundness advisories. Runtime
  analyzer revisions stay pinned; the source dictionary converter uses the same
  public Vibrato 0.7.18 revision as the runtime, with dictionary digests unchanged.
- Dictionary identity documentation describes memoization of both `.dic` and
  `.zst` files and its metadata-key freshness limit.

### Removed

- The dedicated sentence-final command; use `matches` or `summarize` with the
  sentence-final pattern catalog.
- Raw-map matcher compatibility APIs and public mutable NFA construction;
  compile definitions through `FsmBuilder` and use `CompiledCatalog`.
- Execution and reuse of artifacts with a nonzero recorded truncation count;
  rebuild them from source.

## [0.4.0] - 2026-10-21

### Added

- `ling dictionary install [NAME]` downloads one of the seven UniDic 202512
  dictionaries from the `unidic-202512` release, checks its BLAKE3 digest, and
  installs it under `$XDG_DATA_HOME/lingrep/dictionaries`. When neither
  `--dict-path` nor `LINGREP_DICTIONARY_PATH` names a dictionary, `ling` uses
  the installed `cwj`. The `vibrato-dict-*` Nix packages download the same
  files instead of converting UniDic, which required about 10 GB of memory;
  `vibrato-dictionaries-from-source` still converts them. See
  [DICTIONARIES.md](DICTIONARIES.md), which also states each dictionary's
  license.
- `ling corpus variants` lists each unreadable artifact with its size and
  marks it prunable when it belongs to the target's source and was written in
  an earlier format than this build reads. `--prune` deletes those artifacts,
  printing each path and the bytes freed, and nothing else. The
  `lingrep.variants/v1` records gain the optional `bytes`, `prunable`,
  `pruned` and `freed_bytes` fields. See
  [Discovering realizations](docs/cli-usage-guide.md#discovering-realizations).

### Changed

- A prepared corpus whose manifest has a field this `ling` does not read
  fails validation with `unknown_manifest_field`, naming the field, the `ling` version
  that wrote it, and this version, instead of a generic JSON error.
- `install.sh` follows the XDG Base Directory Specification: `ling` goes to
  `$XDG_BIN_HOME` when set, and the manual pages and Bash, fish and zsh
  completions from the archive are installed under `$XDG_DATA_HOME`
  (default `~/.local/share`). See [Install ling](docs/installation.md).
- `ling` and `ling -h` describe what each command produces and end with
  example invocations.
- Sudachi analysis uses sudachi.rs 0.7.0, which reads only the
  `SudachiBinaryDic` dictionary container. Use SudachiDict v20261015 or later
  from its GitHub releases page; earlier releases and the 20261015 archive on
  the CloudFront mirror are rejected with an error naming the container. The
  Nix packages pin SudachiDict v20261015. Sudachi analysis identity changes,
  so Sudachi corpora prepared with 0.3.x must be rebuilt. On an 8.5 MB
  scientific-text corpus, 130 of 103,007 sentences tokenize differently, all
  at out-of-vocabulary katakana boundaries, from sudachi.rs's lattice and
  character-category fixes (#323, #325, #326).
- CoNLL-U corpora prepared with 0.3.x keep their analysis identity and are
  read without a rebuild. Sudachi corpora must still be rebuilt, as above.
- `--analyzer sudachi` requires only `--sudachi-dict`. `--sudachi-resources`
  defaults to the resource files built into `ling`, `--sudachi-split` to `c`,
  and `--sudachi-edition` to the edition in a `system_<edition>.dic` file
  name; for any other file name, `--sudachi-edition` is required.

## [0.3.2] - 2026-10-20

### Added

- A `ling` archive for Intel Macs (`x86_64-apple-darwin`).
- `install.sh`, published with each release: installs that release's
  `ling` for the host platform into `~/.local/bin` after verifying its SHA-256
  checksum, and adds the directory to `PATH`. See the
  [release notes](docs/release-notes/v0.3.2.md).

## [0.3.1] - 2026-10-20

### Fixed

- The macOS archive no longer requires Homebrew `xz`: liblzma is linked
  statically. See the [release notes](docs/release-notes/v0.3.1.md).

## [0.3.0] - 2026-10-20

First public release. See the [release notes](docs/release-notes/v0.3.0.md).
