# Changelog

All notable changes to lingrep are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `summarize --by-section` counts each pattern's matches and matched
  sentences under every section label of a prepared corpus in one scan. A new
  query/v2 `section` record gives the label, or `null` for unlabelled
  sentences, its admitted sentence and token counts, and per-pattern
  `matches` and `sentences_matched`. It requires `--json` and `--corpus` or
  `--cache`.
- `summarize --by-metadata @document.KEY` counts the same per-pattern
  `matches` and `sentences_matched` under every value of one document
  metadata key in one scan, together with each value's admitted documents,
  sentences and tokens. It publishes them as query/v2 `metadata_value`
  records. A list-valued key is counted once per listed value, and documents
  with no value are counted in a record whose `value` is `null`. It requires
  `--json` and `--corpus` or `--cache`.
- `summarize --write-table PATH` writes each pattern's group counts per
  document of a prepared corpus to a count table, and `ling table PATH`
  reads it to rank each pattern's groups under an `@document` `--where`,
  with each group's count and number of admitted documents, without
  scanning the corpus. Before the rows, a `pattern` record per pattern gives
  its occurrences and admitted documents over all of its groups. A table is rejected when the corpus analysis, the
  catalog or the matcher revision differs from the one it was written for.

### Changed

- The query/v2 `complete` record's `counters.tokens_matched` is renamed
  `counters.tokens_scanned`, with no alias, so a reader of `tokens_matched`
  must change. It counts the tokens of the sentences the matcher
  ran, not of those counted in `sentences_proven_absent`. Admitted tokens are the
  sum of `tokens_admitted` over the `document` records of
  `summarize --by-document`.
- Opening a prepared corpus's provenance no longer decodes every sentence's
  stored analysis. Every `--corpus` run that reads provenance starts sooner:
  `--where`, `--source-metadata`, lookaround divisions, `--by-section`,
  `--by-metadata`, and `ling corpus inspect`. A stored analysis that does not
  decode, or a sentence whose input identity is only partly stored, is now
  reported when that sentence's analysis is read, as by lingrep-web's reading
  view, and by `ling corpus inspect --verify`, instead of when provenance is
  opened. lingrep-web therefore starts serving such an artifact, and the
  request that reads the damaged sentence fails.

### Fixed

- The query/v2 `complete` record's `counters.documents_admitted` counts the
  documents with at least one admitted sentence, for `matches` and
  `summarize` over direct input and `--corpus`. It counted every document
  whatever the `--where` clause, including a document with no admitted
  sentence: an empty one, or one whose sentences an `@section` predicate
  excluded. It now equals the number of `summarize --by-document` `document`
  records. The document-grain count of a `--where` clause remains
  `scope.documents.admitted`.
- `ling coverage` reports in the `scope` record's `admitted_documents` the
  documents with at least one admitted sentence, the same count as
  `summarize`'s `counters.documents_admitted` for the same `--where` clause.
  Over `--corpus` it reported every document in the artifact whatever the
  clause; over direct input it reported 0.
- The query/v2 `complete` record's `counters.sentences_matched` counts the
  sentences with at least one match, once per sentence however many patterns
  match it, for `matches` and `summarize`. It counted every sentence the
  matcher ran, including sentences with no match, so it disagreed with the
  `sentences_matched` of the `section` and `metadata_value` records and
  differed between a `--corpus` run that skipped sentences through the
  posting index and the same query without the skip. The number of sentences
  scanned remains `sentences_admitted` minus `sentences_proven_absent`. The
  progress line of `matches` and `summarize`, "matched N sentences", reports
  the same count.

## [0.7.0] - 2026-11-05

### Added

- A lookaround written with `within=document`, as in
  `[text=x] followed_by($ ^ [upos=NOUN], within=document)`, may match tokens of
  the neighbouring sentences of the same document, and a lookaround written with
  `within=paragraph`, `within=section` or another division kind may match
  tokens of the neighbouring sentences of the same innermost division of that
  kind. Its body must have a maximum width. The neighbours end at a sentence
  removed by a scope clause, sampling, or an oversized-sentence skip.
  `ling matches` and `ling summarize` report a division kind that the corpus
  does not observe as `warning[division_unavailable]`, except after a
  direct-input `ling matches` run that `--limit` stopped; the browser and
  `ling explore` do not.
- `skip: @T` makes a pattern match as if the tokens satisfying the test `T`
  were absent, so `[text=for] [text=example]` matches `for “ example`. Elements,
  lookarounds, `^` and `$` step over skipped tokens, and a match or capture
  neither starts nor ends on one. A `within=` lookaround in such a pattern reads
  at most as many tokens of the neighbouring sentences as its body is wide,
  skipped tokens included. A `defaults` block may supply `skip:` for every
  pattern in a file.
- Either endpoint of a basic `where` edge can be a graph-node binder, so
  `(?<d> []) where (?<h> []) -[*]-> $d` matches every basic dependency edge once,
  whatever the distance or order between head and dependent, and
  `key: [$h.lemma, $d.lemma]` counts edges by both ends.
- A `where` clause accepts order constraints, `$a < $b < $c`, over one-token
  captures and graph-node binders. A pattern that binds a dependency subtree
  from one surface capture through binders chained by `$name` references, and
  orders all its nodes, matches each occurrence of a subtree with that shape and
  token order once.
- The `run` record of `ling --json corpus build` and `corpus inspect` names the
  artifact in `artifact_path`. Without `--output`, `corpus build ROOT` reuses or
  builds the artifact that `--cache` queries read, so a caller can resolve ROOT
  once and use `--corpus PATH` afterwards.
- A catalog file can declare, per attribute, the label set its literals belong
  to: `labels: { upos: "ud-v2", dep.rel: ["clearnlp", "ud-v2"], xpos: "penn" }`,
  or an explicit `{…}` set. A literal outside its declared set is a compile
  error. When the corpus has values outside a declared set, a `matches` or
  `summarize` run ends with `warning[label_outside_declared_set]` and token
  counts. Named sets are `ud-v2`, `spacy`, `clearnlp` and `penn`. `labels` is a
  reserved word, like `analyzer`.
- `summarize --by-document` counts each group per admitted document. Each
  query/v2 `summary` record carries a `document` ordinal, and a new `document`
  record gives the source name and admitted sentence and token counts of every
  document with an admitted sentence, with `@document.KEY` fields selected by `--source-metadata`. It
  requires `--json`.
- `check --diagram --diagram-format svg --output-dir DIR` writes one SVG file
  per selected pattern and fragment into a new directory `DIR` and prints each
  definition name with its file path. Files are named by source-order position
  and a bounded, filesystem-safe form of the name. `ling` creates `DIR` only
  after every selected definition renders; an existing `DIR` is rejected and left
  unchanged.
- `ling serve` runs the `lingrep-web` browser interface and takes the same
  arguments, so a `ling` installed from the release archives or `install.sh`
  serves the browser interface without Nix.
  It is part of the default `web` feature; `--no-default-features` builds
  `ling` without it.

### Changed

- On direct input, a sentence that a scope clause excludes still counts toward
  the `document_sentence_ordinal` of the sentences after it, so a scoped direct
  run reports the same in-document ordinals as the same query over a prepared
  corpus.
- Release binaries are stripped of symbols.
- The graph-node binders of basic `where` edges bind distinct words: two
  binders of one clause, or a binder and a surface capture that a basic edge
  names, never bind the same word.
- `skip` is a reserved word. A `skip:` entry must name a test or be `none`, so
  `ling` rejects a catalog that uses `skip` as a descriptive metadata key, as in
  `skip: true`, with a syntax error, and a metadata schema cannot declare
  `skip`.

### Fixed

- `ling serve` and `lingrep-web` report an occupied port with exit code 2
  instead of panicking.
- Retained lookaround context, including empty sentences, counts toward the
  tokenized byte limit. A required window that exceeds the limit ends execution
  with an error.
- Scoped lookarounds in anchor exclusion fragments fail compilation with
  `ScopedLookaroundUnsupported`, including through fragment references.
- `ling --json check --diagram` exits with `unsupported_output` instead of
  writing a text diagram to stdout and exiting 0.

## [0.6.0] - 2026-11-04

### Added

- `ling check --diagram` renders Unicode and ASCII railroad diagrams and
  standalone SVG without a dictionary or corpus. Select definitions with
  `--name`, expand references with `--expand`, and set layout columns with
  `--width`. Complete labels and constraints remain visible in legends and
  numbered subdiagrams when they cannot fit inline.
- A `schema` block can declare `map` for a key whose value is a map, such as
  `mining: map required`. A schema line with an unknown type name fails with
  an error that names the type and lists the valid ones. In 0.5.1 a `map`
  declaration and an unknown type name both failed with a generic syntax error.

### Changed

- `--progress auto`, the default, omits routine corpus announcements and the
  published-attribute list. Streaming query execution, JSON, TSV and redirected
  stdout show no automatic progress. Warnings are unaffected.
- `--trace-policy 0` prints per-statement policy totals instead of an empty
  `policy trace (0 records)`. A positive `--trace-policy N` prints the same
  totals before its trace. `--progress always` no longer prints policy totals.
  A failed write of the requested report fails the run.
- A human-format query that prepares an artifact with `--cache` prints
  `prepared corpus: PATH` to stderr in every `--progress` mode.
- Deferred human output can show a transient status line under `--progress
  auto` after one second on capable terminals, including during `--cache`
  preparation. `always` shows status immediately: it redraws for deferred human
  output on capable terminals and otherwise prints ordinary lines, at most one
  intermediate update every five seconds plus the latest unprinted final count.
  A failed progress write does not fail the run.
- `ling corpus inventory --progress always` prints the number of sources
  listed so far. In 0.5.1 the option was accepted and ignored.
- `ling corpus build`, and a query that prepares an artifact with `--cache`,
  report progress for the identity pass over the source and for the build
  pass. In 0.5.1 the option was accepted and ignored.
- `ling doctor --corpus` no longer checks the default dictionary, so a prepared
  corpus passes on a machine without one; `--dict-path` still adds a dictionary
  check. Without `--corpus`, including with `--patterns` alone, the default
  dictionary is checked as before. `ready` covers only the selected checks.
- `ling doctor` human output counts the checks that passed or failed, marks
  catalog warnings and unavailable binding attributes as `warning` rows, and
  reports the dictionary as readable instead of loaded. A catalog whose
  `analyzer:` declaration does not admit the checked dictionary produces
  `warning[dictionary_binding_mismatch]` on stderr, as a diagnostic record
  under `--json`.

### Removed

- The Harbor evaluation harness (`benchmarks/harbor`), its task images
  (`nix/linguistic-eval`), the `ci.sh` harness lanes and the
  `linguistic-eval-*` flake outputs moved to the `lingrep-harbor` repository,
  which consumes this flake's `lingrep` and `vibrato-dict-cwj`.

### Fixed

- `ling coverage --where` prints `warning[unused_section_labels]` for a section
  label the corpus does not record or that the read-time section policy
  excludes, as `matches` and `summarize` do. In 0.5.1 it printed no warning.
- A metadata map key that is not an identifier, such as `markdown-it-py`,
  `a.b` or `x+y`, fails compilation with a syntax error at the key. In 0.5.1 it
  was published cut at the first such character (`{"markdown": …}`) with no
  diagnostic.
- With GiNZA, the spaCy adapter publishes `,` as the `spacy.morph.reading` of a
  comma token. In 0.5.1 any text containing a comma failed with `morphology
  values for 'Reading' must be strings`. A `spacy.morph.*` value is one member;
  only `feat.*` values are split on commas.
- Web diagrams share the CLI recognition model: intersection means all operands
  match the same span, and unordered matching shows its distinct operand and gap
  rules. Compound diagrams retain component order and every dependency clause.
  Embedded diagrams follow the application theme; standalone SVG includes its
  own styles.
- Under `--json`, the `attribute_unavailable`, `dictionary_binding_mismatch`
  and `featurization_policy_mismatch` warnings are `lingrep.diagnostic/v1`
  records on stderr, like other warnings, instead of plain text lines.
- A human-format query whose stderr warning cannot be written exits 1. In
  0.5.1 it exited 0, although the write failure aborted the run before its
  results were printed.
- `ling doctor` reports a dictionary failure with one `Dictionary error:`
  prefix instead of two.
- `ling doctor` reuses cached dictionary identities while checking that the
  dictionary can still be opened, avoiding repeated hashing on cache hits.

## [0.5.1] - 2026-10-27

### Added

- Downloadable Python wheels and checksums for spaCy and Stanza setup,
  including the shared protocol package.
- `ling analyzer prepare` for GiNZA and Stanza CPU analyzers. The chosen output
  directory contains Python, dependencies, model and configuration, so clearing
  download caches does not break queries.

### Changed

- Managed analyzer preparation uses hash-pinned dependency exports from the
  release locks and restricts the PyTorch index to PyTorch packages. Analyzer
  identity is independent of the output directory.
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
