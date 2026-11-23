# Changelog

All notable changes to lingrep are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `ling check` warns about a token test that no token satisfies
  (`unsatisfiable_test`) and a definition whose body requires one
  (`unsatisfiable_definition`). `ling check --containment` lists the pairs of
  definitions in which one publishes only spans the other also publishes, on
  every token sequence and without a corpus.
- The Sample debugger and the Workbench "Test on sample" drawer verify a
  catalog's `examples` and `counterexamples` when the sample token schema
  supports the catalog. They report invalid entries, unexpected match results,
  and unexpected publication decisions. Select a listed text to add it to
  the sample.
- Sample results name the analyzer and dictionary that tokenized the sample,
  list each referenced attribute that tokenizer does not supply, and report a
  pattern that cannot bind to its token schema. The Workbench drawer also
  reports when the selected corpus was analyzed with a different token schema.
- `ling check --verify-examples` and the Sample debugger explain why a
  definition published no match for an example: the attempt that matched the
  most tokens, the token and pattern element it stopped at, and each attribute
  test of that element with the token's value. A lookaround failure, a `where`
  clause rejection and a match that result policy did not publish are separate
  outcomes. The Sample debugger shows the same explanation, per definition, for
  each sample line without a published match.
  `CompileOutput::explain_definition` is the library entry point.
- The Sample debugger adds a sample sentence to a definition's claims: a
  match offers its sentence for the definition's `examples`, and a sentence the
  definition does not match offers itself for `counterexamples`. The action
  edits the pattern text, creating the key above the definition when it is
  absent, and the sample is checked again. It is not offered for a sentence the
  list already holds, or for a list inherited from `defaults` or a bundle.
- The Sample debugger links a match to the pattern text. Selecting a token of
  a match marks, in the editor, the element that consumed it and each
  reference the element was reached through. Placing the cursor in an element
  or a reference underlines the tokens it consumed in every match.
  `CompileOutput::consumed_tokens` is the library entry point. It reports one
  derivation of the body and nothing for a body with an any-order group,
  `same_as`, a `within=` lookaround or `skip:`.
- The Workbench editor and the Sample debugger show the recognition diagram
  of the pattern or fragment at the cursor, from the text being edited and
  before any corpus run. It is the drawing `ling check --diagram` produces,
  with the same option to expand references. When the text stops compiling,
  the last diagram stays and is marked as out of date.
- `ling check --verify-examples` and the Sample debugger list counterexamples
  that do not test a constraint: the definition does not match them with any
  one attribute test, token element or lookaround removed. They are reported
  separately from failures and do not change the exit status.
  `--propose-counterexamples FILE` reads one sentence per line and lists, per
  definition and constraint, sentences the definition does not match and
  would match without that constraint, with the number of sentences scanned.
- `ling diff A.jsonl B.jsonl` compares two `ling --json matches` runs over
  one corpus and reports, per definition, the occurrences both runs have and
  those only one has, with the first differing sentences. It compares two
  catalog revisions by definition name, or two named definitions.
  `--level span|roles|record` states what identifies an occurrence and is
  repeated in the report. A run stopped at `--limit` is compared over the
  sentences it completed, and the report states the sentences compared; with
  no difference it reports `no difference in N sentences scanned`.
  `ling --json diff` writes `lingrep.diff/v1`.
- The Workbench pattern editor has "Compare with the last search": it runs
  the last searched pattern and the editor's pattern over the selected corpus
  and scope, lists the occurrences only one of them has at the chosen level,
  states whether every sentence or a prefix was compared, and links each
  listed occurrence to the reader.
- A Workbench hit opens its sentence in the Sample debugger with the analysis
  stored in the corpus, so a pattern that reads attributes the sample
  tokenizer does not supply, such as a dependency `where` clause over a
  CoNLL-U corpus, is explained on the tokens it searched. The results state
  that the tokens come from the corpus artifact and name its analyzer. Editing
  the text switches to the sample tokenizer, and the page says so. Enhanced
  dependency relations are not evaluated there.

### Changed

- A definition using `&`, `measure` or `same_as` drops a match that lies
  strictly inside its own longer match from an earlier start, as a definition
  without them always has, so adding one of them to a body no longer changes
  which spans it publishes. On `猫の手`,
  `[text=の] | [text=猫] [text=の] [text=手]` publishes the three-token match;
  the same body with `& []+` or `measure count(match.words[]) as $n` also
  published `の` and now publishes the three-token match only. A definition
  with a `where` clause, a `within_span` or `span_dependency` annotation, or an
  `any_order` group publishes what it did in 0.9.1: its longest match from
  every start, including a relation instance inside a longer match of the same
  definition. A match that ends where an earlier-starting match ends is kept,
  lexicons publish what they did, and different definitions still never drop
  each other's matches before policy. The rule is specified by definition form
  in the
  [pattern reference](docs/pattern-language-reference.md#which-matches-a-definition-publishes).
  The matcher semantics identity is `lingrep-core-matcher/v3`: a count table
  built by an earlier release is not read as current and must be rebuilt.
- `ling check --containment` applies that rule to the definitions it compares,
  so a pair involving a definition with `&` or `measure` can change. A
  definition with a `within_span` or `span_dependency` annotation is listed as
  not compared, as one with a `where` clause is.
- `patterns/sf-experimental.lx` no longer declares `resolve`, so overlapping
  definitions are all counted: a specific form and the general form that
  contains it (`であろう` and `だろう`, `ことも` and `こと`) each keep their
  records, and counts are higher than before. `--overlaps longest` gives one
  record per overlapping set. The unused `priority_order` keys and
  `scripts/upstream_sf_differential.py` are removed. All of its examples now
  pass `ling check --verify-examples`.
- In the web pattern editor, Tab and Shift+Tab move focus to the next and
  previous control instead of indenting, so the editor can be left from the
  keyboard. Ctrl+] and Ctrl+[ indent and outdent.
- A syntax error in the web pattern editor states its line and column, and an
  unprefixed attribute outside `text`, `surface`, `lemma`, `norm`, `upos`,
  `xpos` is named with the portable set.
- The Workbench hides the status of the last search while the editor holds a
  different pattern, and shows a failed search in the error color.
- "Hits kept", "Derive compounds", "Count documents reached" and "Overlapping
  matches" are in a "Run options" disclosure whose summary states the values in
  effect; each option's explanation is visible when it is open.
- Below 64rem the pattern list scrolls in place above the selected pattern's
  results. The concordance toolbar is one row at desktop width, and the open
  inspector leaves the concordance more width.

### Fixed

- The Workbench has higher-contrast placeholder text in both themes, larger
  provenance labels, and the same focus ring on buttons, links, selects,
  checkboxes, and disclosure summaries.

## [0.9.1] - 2026-11-20

See the [0.9.1 release notes](docs/release-notes/v0.9.1.md) for the Python tokenizer command, Stanza bundle options, Unicode alignment fix, and file-limit fixes.

## [0.9.0] - 2026-11-20

### Added

- Python programs can tokenize batches with a shared Vibrato dictionary, compile
  patterns, search corpora, and match spaCy, GiNZA and Stanza documents. Optional
  Polars expressions and Arrow batches expose typed token attributes.
- Patterns can follow span dependencies, bind occurrence measurements and count
  dependents. Saved questions and shared links can pin a word-vector table.

- Every token carries `surface`, the exact source text it covers: the source
  slice for Vibrato, Sudachi and external analyzers, and FORM as written for
  CoNLL-U words and empty nodes. `[surface=ｶﾞﾗｽ]` matches that spelling
  only. `surface` literals are compared exactly, without NFC. A named set
  compared only with `surface` does not report `literal_normalized`.
- `literal_folded` warns when a `text` literal matches as a different folded
  spelling, such as `[text=ＮＨＫ]` matching as `NHK`. `regex_not_folded`
  warns when a regex on `text` contains a character that never occurs in the
  folded key. `text_unfolded` warns when a corpus prepared before 0.9 is
  queried with `text` literals, and names the definitions affected.
- `ling matches` and `ling summarize` report `warning[span_kind_unavailable]`
  after a complete scan that admits at least one sentence if none carries a
  span kind referenced by `--by-span`, `where span(…)`, `within_span`, or
  `span_dependency`. For example, the warning distinguishes missing `bunsetsu`
  annotations over Vibrato from a query with no matches in annotated text.

### Changed

- `text` is the token's match key: its `surface` with variation selectors
  removed, half-width katakana widened, U+FF5E as U+301C, canonical
  composition (NFC) and full-width ASCII as ASCII. In 0.8 `text` was
  UniDic's orthography column for Vibrato and the analyzer's own text for
  other analyzers. `text` literals, inline and named sets compared with
  `text`, lexicon members read `by [text]` and `near` values compared with
  `text` are folded the same way, so `[text=ガラス]` also finds ｶﾞﾗｽ. Regexes
  on `text` are used as written. `lemma`, `norm` and provider attributes are
  compared as written.
- Every analyzer reads a folded view of the source, and its token boundaries
  are mapped back to the source. An ideographic variation sequence such as
  葛 U+E0100 is no longer split into a separate symbol token, and half-width
  ｶﾞﾗｽ is analyzed as ガラス. Vibrato and Sudachi keep a CJK compatibility
  ideograph where the dictionary lists a word containing it, such as 臭氣
  with U+FA5C.
- `--normalization` defaults to `none` for every analyzer, so `surface` and
  `char_span` address the source text exactly. `--normalization nfc` still
  composes the source before analysis.
- Reading-view highlights in `ling serve` and lingrep-web map each matched
  token to its exact source characters, including variation selectors,
  half-width katakana and decomposed kana.
- lingrep-web shows each token's `surface` in KWIC lines, the inspector,
  frequency lines and exports, and labels `text` 照合形 (match form) and
  `surface` 表層形 (exact source text).
- lingrep-web and the KWIC view of `ling explore` display one space wherever
  the source has whitespace between two tokens, as human `ling matches` output
  already did. `I saw NHK news` was displayed as `IsawNHKnews` in the question
  page and the explorer; the workbench and frequency lines guessed spaces from
  the script and displayed `N H K` where the dictionary splits `NHK` into
  letters. Text without whitespace between tokens is displayed as before. A
  KWIC line's left context ends, and its right context begins, with the space
  that separates it from the hit, in exported XLSX cells too. Token rows of
  the question API carry `space_before: true` on a token the source separates
  from the one before it.
- `surface` in JSONL and TSV output, the `summarize` surface and `--by-span`
  keys and the surface keys of count tables join tokens with one space
  wherever their character offsets show a gap, so `NHK news` and `NHKnews`
  are different keys. Count tables are version 4.
- A CoNLL-U sentence whose `# text` comment spaces its forms differently
  from `SpaceAfter` takes its character offsets, and so its `surface`
  spacing and count keys, from the comment. Such a sentence had no offsets
  before.
- `ling summarize` fails with `error[missing_surface_offsets]` when it groups
  a multi-token surface in a sentence without character offsets, such as a
  CoNLL-U sentence whose forms do not occur in its `# text` comment; 0.8.x counted
  such a match under its tokens joined with nothing. The error names the
  file, sentence and token. Correct or delete the `# text` line, or give the
  pattern a `key:`. `ling matches` exports the same match with its tokens
  concatenated.
- Sudachi is loaded without its input-text plugins, so `rewrite.def` and
  `sudachi.json` do not affect analysis; the folded view widens half-width
  katakana and composes decomposed kana instead. The 0.8 documentation said
  `ling` used them.
- Every prepared corpus and count table must be rebuilt. Analysis identities
  include the fold, and the token-schema identity of a schema with `surface`
  includes the revision of the `text` fold. A corpus built by 0.8.x still
  opens, without `surface` and with `text_unfolded` warnings; a count table
  written by 0.8.x fails with "built with a different matcher revision".
  After `ling corpus build`, preview cleanup with `ling corpus prune`. The plan
  includes the superseded 0.8.x corpus of the same source and any count tables
  bound only to it. Add `--execute` to apply the plan.

### Removed

- The library types `lingrep_core::execution::OfflineExternalSource`,
  `LiveExternalSource` and `ExternalAttestation`. They published the
  adapter's own `text` without `surface`, so folded `text` literals did not
  find fold-changed spellings over them, and no `ling` or lingrep-web
  command used them. Run an adapter through `ExternalAnalyzerSession`, which
  publishes `surface` and the folded `text`.

### Fixed

- `ling analyzer prepare stanza` accepts `tokenize`, `pos` and `lemma` without
  `mwt`, so a language for which Stanza has no multi-word token model, such as
  Japanese, can be prepared. Its profile declares the `identity-v1` alignment
  policy in place of `stanza-mwt/v1`. If Stanza loads an `mwt` model omitted
  from `--package`, preparation fails and identifies the missing package.
  Earlier versions failed when running the first query. Analyzers prepared
  with `mwt` keep their identity.
- Long Japanese sentences use 64-bit Vibrato path costs.

## [0.8.2] - 2026-11-18

### Added

- `ling corpus disk DIR` reports the size and status of the prepared
  artifacts, count tables and build staging leftovers in a directory, and with
  `--caches` the decompressed dictionary cache. Given the analysis inputs
  `corpus build` takes, or `--current` for its defaults, it marks the
  artifact `corpus build` would reuse as current and the other readable ones
  as superseded. `--json` writes the `lingrep.disk/v1` stream.
- `ling corpus prune DIR` lists the artifacts of the current source content
  in a directory that are in an older format or differ from the current one
  in analyzer or token schema, and the count tables bound only to them, with
  the bytes they occupy; `--execute` deletes them. `--include-all-superseded`
  includes artifacts from other or unknown source revisions and artifacts
  that differ only in reader or admission policy. When a source has eligible
  entries and lacks a current artifact, pass `--keep ARTIFACT` to designate
  an artifact to retain before executing the plan.

### Changed

- Plain-text, Markdown and Soranoha archive readers have new revisions, so
  `ling corpus build` writes a sibling of an artifact that 0.8.1 built from
  these formats instead of reusing it. Artifacts built by 0.8.1 and their
  reading sidecars keep working, and `ling corpus render` still writes a
  sidecar for a Markdown artifact built by 0.8.1, with two exceptions. If a
  source file starts with a byte-order mark, `render` fails with "re-reading
  FILE does not reproduce the stored sentences". If a file or directory name
  changes under Unicode NFC (see Fixed), `render` fails with "the source tree
  has source identity …, but the artifact records …". Rebuild the artifact
  with `ling corpus build` in either case.

### Fixed

- On Windows through WSL 2, `ling corpus build --output` under `/mnt/c`
  failed with "Permission denied (os error 13)" in 0.8.1, although the 0.8.1
  release notes said it worked there. `ling` held the staged artifact's files
  open while renaming its directory, and Windows does not rename a directory
  with open files. Corpus build, `summarize --write-table`, `table`, `serve`
  and `matches --ending-report` now succeed on `/mnt/c`.
- On macOS 15, `ling corpus build` to an MS-DOS (FAT32) volume failed with
  `invalid provenance.sqlite format`. The provenance database now leaves WAL
  mode on a new connection, after the file has its final size.
- A failure while publishing an artifact or writing its provenance database
  names the path and the operation, such as "cannot rename '<staging>' to
  '<destination>'", and is reported as an I/O error rather than as a damaged
  artifact. A full disk while writing the provenance database is reported
  as a full disk.
- `ling matches --ending-report` on a volume that supports neither no-replace
  rename nor hard links, such as exFAT or FAT on macOS, lost the report after
  the scan. It now creates the report without replacing an existing file. On
  Linux, a no-replace rename that failed with `EOPNOTSUPP` failed the report
  instead of falling back to a hard link.
- `ling analyzer prepare` on a macOS volume without symbolic links reported
  a bare I/O error instead of the symbolic-link check's message.
- `ling corpus build` no longer reuses a prepared artifact whose directory
  name records another analysis identity or trust class than its manifest.
- The bundled SQLite comes from libsqlite3-sys 0.39.1, replacing the
  version that a RustSec advisory flagged.
- Corpus readers strip one leading UTF-8 byte-order mark. In 0.8.1 the mark
  appeared in the first token of plain-text, Markdown and Soranoha documents
  and prevented JSONL and CoNLL-U files from parsing.
- Input that is not UTF-8 fails with the file, line and byte offset of the
  first invalid byte, for corpus sources and for catalogs read by
  `--patterns`, `check` and `lingrep-web --pattern-root`. When the file looks
  like Shift_JIS (CP932), the message gives an `iconv` or `nkf` command that
  converts it. A ZIP archive read as plain text or Markdown fails with advice
  to unzip it, or to read a Soranoha archive with `--input-format soranoha`.
- A pattern syntax error at a full-width character that a Japanese IME types,
  such as `［`, `＝` or the ideographic space, has a note at that character
  naming the ASCII one to type, in `ling` and in lingrep-web's editor and
  question page, with Japanese text in lingrep-web.
- A non-NFC pattern literal, such as `ガ` written as `カ` + U+3099 in text
  copied from a PDF, compiles to its NFC form and matches NFC corpus text.
  Under 0.8.1 it matched nothing. `matches` and `summarize` warn
  `literal_normalized` and `check` reports `NormalizedLiteral`, naming the
  definition and both spellings. A regex that is not NFC is compiled as written
  and warns `regex_not_nfc`. A catalog whose source is not NFC gets a new
  fingerprint, and NFC catalogs keep theirs.
- Source names are in Unicode NFC. A file or directory, a single input file,
  or a Soranoha archive member named in NFD, as macOS names `ガ`, is named by
  its NFC spelling in records and in the source identity of a directory or
  file input. Under 0.8.1 a corpus copied from a Mac had a different identity
  from the same files named in NFC, and a Soranoha member named in NFD had no
  catalog row. Two names equal in NFC fail the run, naming both as written.
- A token-test value in IME or typographic quotes, such as `[text=”猫”]` or
  `[text=「猫」]`, is rejected as `QuotedAttributeValue` like `[text="猫"]`.
  Under 0.8.1 the catalog compiled and its pattern matched nothing.

## [0.8.1] - 2026-11-18

### Added

- `--input-format plain` reads a directory of `.txt` files, one document per
  file, including nested directories. Use it with `ling corpus build` to
  prepare several plain-text works as one corpus. A directory path without
  an extension still defaults to Markdown.
- Question links that use the served catalog identify it by fingerprint. If
  the catalog has changed, the page offers a choice between searching with
  the current catalog and opening the default question.
- Question result lists support arrow keys, Home and End to move between
  document links and examples, with one Tab stop for the list.

### Changed

- spaCy (GiNZA) and Stanza analyzer identity is derived from the adapter's
  semantics revision, the resolved package versions and the model hashes,
  not from the bytes of the adapter files. Under 0.8.1, analyzers prepared
  by either release have new identities, so `ling corpus build` writes a
  sibling of an artifact built under 0.8.0. `ling` rejects a 0.8.1 analyzer
  whose adapter code differs from what its setup recorded, and `ling` 0.8.0
  cannot run an analyzer prepared by 0.8.1.

### Fixed

- Completed question searches remain downloadable when their previews exceed
  display limits. JSONL downloads include every indexed match, including
  examples too large to preview. The page checks that export can succeed
  before offering the link and restores it after a reload.
- Question searches preserve an example opened during the initial search and
  keep the search controls in place while results update. Analysis-tool links
  open the Workbench on the current pattern and corpus with compound
  derivation off.
- Compound derivation stops at explicit limits and reports how to disable
  derivation or search fewer definitions.
- `ling summarize` bounds concurrently open spill files, including for
  token-window catalogs, and fits its shards and merge fan-in to the
  available open-file limit. It reports `warning[open_file_limit]` when it
  narrows the layout. If the minimum layout cannot fit, it fails before
  scanning and reports the required number of additional open files.
- The execution pipeline retries batches that exceed the match-output budget
  in smaller groups. A single sentence that still exceeds the budget reports
  an error with a suggestion to narrow the pattern.
- `ling serve` and `lingrep-web` start without a dictionary when every declared
  corpus is a prepared artifact. Source corpora and Workbench sample analysis
  still require a dictionary; their errors explain how to supply one.
- Corpus, count-table and SVG directory publication use a checked rename when
  no-replace rename is unavailable. This fallback may replace an empty
  destination directory created concurrently. Corpus builds also avoid
  SQLite shared-memory index files.
- Analyzer setup and Hugging Face dataset exports stop before downloading or
  streaming data if their required symbolic-link or hard-link probe fails.
  Errors retain the OS reason and suggest a suitable output directory.
- The spaCy adapter's `electra` and `bert-large` wheel extras install without
  conflicting Transformers constraints. The wheel extras use the constraint
  from spacy-transformers; `ling analyzer prepare ginza` retains its locked
  package versions.
- In Safari, Escape now closes an open example after you click one of its
  tokens. Focus returns to the result line that opened it.
- When the server rejects a Workbench download, the message is in Japanese
  if the interface is in Japanese.

## [0.8.0] - 2026-11-17

### Added

- The spaCy adapter publishes GiNZA's bunsetu (文節) segmentation as
  `spacy.ginza.bunsetu` when the selected profile runs `bunsetu_recognizer`.
  Each bunsetu starts with a token labeled `B`; subsequent tokens are labeled
  `I`. These values match `BunsetuBILabel` in `ginza -f conllu` output.
  Other spaCy pipelines are unchanged.
- `ling table --select PATTERN --key FIELD=VALUE` filters rows by exact values
  in a compound key. A `subset` record reports the selected rows' total
  occurrences, and each row includes its share of that total. The Frequencies
  view provides the same exact-match filters for each key field.
- `ling serve` and `lingrep-web` load word-vector files with `--vectors FILE`.
  For catalogs using `near`, the Workbench displays a notice if the catalog
  has no `vectors:` declaration or declares a different file digest.
  The Definition tab lists the values matched by each `near` test. The Similar
  Words page ranks values by similarity to a word.
  The `--vectors` option also applies when `ling matches` or `ling summarize`
  uses `--serve` or `--link`.
- The Workbench accepts a `--where` clause in its search scope and shows
  equivalent `ling summarize` and `ling matches` commands.
- `ling matches --link NAME=URL` and `ling summarize --link NAME=URL` print
  a Workbench link without reading the corpus. The link includes the catalog,
  scope clause and overlap setting; `NAME` identifies the server's corpus.
- `ling matches --serve` and `ling summarize --serve` start the Workbench on
  a free loopback port and open the query in `BROWSER` or the default browser.
  `--serve=HOST:PORT` sets the address. `ling serve --port 0` prints the port
  assigned by the operating system.
- `matches`, `summarize` and the Workbench accept overlap modes `catalog`,
  `keep` and `longest`. `keep` skips `resolve` statements; other policy statements
  still apply. `longest` adds longest-match resolution when the catalog declares
  no `resolve`. The selected mode also applies to Workbench links, exports,
  inspection and live summaries. Frequencies and Compare use the catalog's policy.
- The Frequencies and Compare pages display the corpus being analyzed. The
  Sample Debugger explains that analysis is limited to the sample text.
- Cluster analysis accepts a minimum occurrence count for patterns in both
  trees. This filter does not apply to section distributions or rate contrasts.
- `ling analyzer prepare` accepts `--download-base` and `LING_DOWNLOAD_BASE`
  for adapter wheels, alongside the installer and dictionary mirror support.
  CLI archives and wheels use `v<version>/`; dictionaries use `unidic-202512/`.
  The installer and adapter downloads require HTTPS. Dictionary downloads are
  verified against the digests built into `ling`.

### Changed

- The Workbench key picker can change an individual field in a compound key
  between `text`, `lemma` and `norm`. This includes fields from `where` graph
  nodes that bind only words, such as `$v` in
  `key: [$n.lemma, $p.lemma, $v.lemma]`. Other fields and the rest of the source
  remain unchanged. `check --json` reports one `capture_key_edits` descriptor
  per eligible field, including its position in `field`.
- Prepared artifacts use generation 3 and provenance schema 5. Rebuild
  generation 2 artifacts with `ling corpus build`.
- The spaCy adapter installs GiNZA 5.3.1 with `ja_ginza` 5.3.0 and spaCy
  3.8.18; its `electra` extra installs `ja_ginza_electra` 5.3.1, and a new
  `bert-large` extra installs `ja_ginza_bert_large` 5.3.0. `ja_ginza` 5.3.0 has
  the same weights and configuration as 5.2.0.

### Fixed

- `ling analyzer prepare ginza` produces the same analysis identity on macOS
  and Linux. Identity metadata in the hashed model directory excludes the
  interpreter's build string and path, and the record for `thinc-apple-ops`,
  which is installed only on arm64 macOS.
  Existing bundle identities are unchanged; newly prepared bundles have new
  identities. Stanza identities still differ between macOS and Linux because
  their locked torch versions are `2.13.0` and `2.13.0+cpu`, respectively.
- The Workbench reports unavailable token attributes and catalog-binding
  failures with their reasons.
- spaCy setup profiles run all pipeline components through the last component
  that produces a selected output, including earlier components whose output
  is required but not declared as a dependency. Both GiNZA morphology profiles
  run the complete pipeline and produce dependencies, UPOS and
  `spacy.ginza.bunsetu` matching `ginza -f conllu`. The `morphology/input`
  profile previously ran only the tokenizer; `morphology/analyzer` ran the
  parser without `tok2vec` or `transformer`. The incomplete pipeline caused
  incorrect dependencies and UPOS in `ja_ginza`, and
  `unregistered extension trf_data` errors in `ja_ginza_electra` and
  `ja_ginza_bert_large`. Sentence splitting also works in `en_core_web_sm`'s
  `tokenize/analyzer` profile, which previously omitted `tok2vec`; its
  morphology profiles are unchanged. Set up existing spaCy analyzers again:
  the corrected profiles change analyzer identity.
- `lingrep-spacy-setup` downloads transformer weights for `ja_ginza_electra`
  and `ja_ginza_bert_large` once, from a pinned Hugging Face Hub commit, and
  stores them in the analyzer directory. Earlier analyzers fetched unpinned
  weights at every startup, so output could change without changes to the
  analyzer files. Analysis runs with Hub access disabled. The Electra tokenizer
  loads from the locked `sudachitra` package without executing code from the
  model repository. Set up existing transformer analyzers again in a new directory.
- Searches cancel after 30 seconds without an open progress stream. A
  replacement scan waits for the superseded scan to stop and displays only
  its own progress. Failed searches no longer also report a lost connection.
- On SIGTERM or SIGINT, the server stops during count-table binding, waits
  up to 3 seconds for scans, and allows corpus builds up to 10 seconds to
  remove staging directories and lock files. Interrupted live summaries
  remove their child process's temporary files.
- Corpus, count-table and reading-sidecar builds reclaim staging directories
  from terminated builds of the same artifact on the same host. Directories
  without lock files require manual cleanup; see the deployment guide.
- If corruption is detected in an artifact beside a declared source after a
  successful open, the server rebuilds it at most once per corpus per process. Error
  reports from a replaced artifact do not invalidate the replacement. Declared
  artifacts and artifacts with errors unrelated to corruption remain on disk.
- Unsupported manifest, graph and normalization versions and SQLite
  environment failures are distinguished from corruption.
- Corpus validation on opening continues after a client disconnects, and its
  results are cached.
- Results downloads are disabled during a newer query and restored if it fails.
  Rejected downloads report the reason and recovery step. Exports skip the same
  oversized sentences as the displayed run and report the count in the About sheet.
- Frequency and comparison endpoints return query errors as 400 responses;
  internal errors return 500 without exposing server paths. Document spools
  remain readable after their paths are removed by temporary-file cleanup.
- Shared links preserve changed compound and document-reach options. Editor
  drafts survive reloads without replacing a followed link's pattern.
- Empty patterns are rejected before submission. Patterns edited during
  submission remain marked as edited until resubmission. Draft changes are
  stored once.
- Escape closes only the open modal. Condition controls support keyboard use
  and retain focus, filter text and caret position across redraws.
- Frequencies and Compare display loading errors without discarding invalid
  clause edits, cancel superseded requests and restore paging controls after cancellation.
  Value searches report both listed and total counts.
- The Sample Debugger runs once on arrival, reports rejected runs and updates
  after edits when the pattern editor cannot load.
- If a query is re-added before its deletion is undone, the query library
  reports the conflict. Undo remains available.
- Japanese translations cover dynamic results, editor diagnostics, accessible
  labels and server errors, and update when the language changes. Compiler and
  binder diagnostics remain in English. Counts retain thousands separators in
  both locales, and scope and clause terminology is consistent within each locale.

## [0.7.0] - 2026-11-10

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
  document of a prepared corpus, and the sentences each group occurs in, to
  a count table, and `ling table PATH`
  reads it to rank each pattern's groups under an `@document` `--where`,
  with each group's count and number of admitted documents, without
  scanning the corpus. Before the rows, a `pattern` record per pattern gives
  its occurrences and admitted documents over all of its groups. A table is rejected when the corpus analysis, the
  catalog or the matcher revision differs from the one it was written for.
- `ling table --entry PATTERN=KEY --by-metadata @document.NAME` counts one
  entry of a count table under every value of a document metadata key. It
  publishes `lingrep.table/v1` `metadata_value` records that have the values,
  order and admitted documents, sentences and tokens of `summarize
  --by-metadata`, and hold the entry's `count` and `works` in place of the
  per-pattern counts. `KEY` is the `group` of the entry's JSON `row` record,
  or its key as human rows print it. It requires `--json`.
- `ling contrast --table PATH --where A --versus B` ranks each pattern's
  entries of a count table by G², UCREL's two-cell log-likelihood, between
  the documents two `@document` clauses admit, with each scope's count,
  documents and rate per million tokens, the minimum expected count, an
  unsmoothed log2 ratio and the direction. Every entry is ranked before
  `--top` applies. A `scopes` record reports the documents and tokens the
  scopes share and whether they are disjoint, overlapping or identical, and
  each row reports its count in the shared documents. A scope without tokens
  gives `null` statistics. The output contract is `lingrep.contrast/v1`.
- `ling serve --count-table PATH` (repeatable) adds a Frequencies view
  to lingrep-web. The view presents controls for the catalog, pattern,
  document metadata conditions, sort order and substring filter, and reads
  counts from tables without scanning the corpus. Each
  condition shows the share of occurrences it keeps, and the page shows a
  corresponding `ling table --where` command. A table binds to a
  served corpus and a catalog by the identities it records; a table that does
  not bind stops startup with an error naming it.
- lingrep-web adds a Compare page beside the Frequencies view, served from the
  same `--count-table` bindings. It states two document scopes with the
  Frequencies view's condition controls, or as two clauses, ranks one
  pattern's entries by G² as `ling contrast` does, shows each scope's rate,
  documents and the shared documents, and shows the `ling contrast` command
  that reproduces the page. The URL stores both scopes.
- Selecting a row of the Frequencies view opens that entry: a chart per
  document metadata key of the entry's occurrences and documents under each
  value, counted under the other conditions as `ling table --entry
  --by-metadata` counts them, and the entry's lines, which are the records of
  `ling matches --where` for that entry, 50 per request. Resting the pointer
  on a bar, or focusing it, previews its value as the key's condition without
  moving the page, and clicking or tapping it sets the condition. The address
  carries the entry, so a copied link reopens it.
- `lingrep_corpus::PreparedSentenceSource::with_ordinals` reads only listed
  sentence ordinals and decodes each block they share once.
- The NixOS module's `services.lingrep-web.countTables` option takes a list of
  count tables and passes each to lingrep-web as `--count-table`, mounted
  read-only, so a NixOS deployment can serve the Frequencies view.
- `ling serve --metadata-labels PATH` (repeatable) reads display labels for
  document metadata keys, by locale, and the Frequencies view names its
  conditions by them in English and Japanese. The address, the clause and the
  shown command keep the keys. `lingrep-web/labels/soranoha-catalog.json`
  labels the Soranoha catalog columns, and the `lingrep` and `lingrep-web`
  packages install it under `share/lingrep-web/labels/`. A label file that
  cannot be read or is invalid stops startup with an error naming it.
- The NixOS module's `services.lingrep-web.metadataLabels` option takes a list
  of label files and passes each to lingrep-web as `--metadata-labels`,
  mounted read-only.
- `ling corpus render ARTIFACT --source-root DIR --output PATH` renders each
  document of a Markdown artifact to HTML in a reading sidecar, with a map
  from each stored sentence to its rendered text. It rejects a source tree or
  `--md-*` options whose identity differs from the artifact's. The
  `lingrep_corpus::ReadingSidecar` API opens a sidecar only when its analysis
  identity, source identity and renderer revision equal the caller's, and
  returns pages of blocks bounded by HTML bytes and an occurrence's highlight
  ranges.
- `ling serve --reading-sidecar PATH` (repeatable) serves a reading sidecar
  for a corpus declared by its `.lingcorpus` path, and the reader's Document
  scope shows that corpus's documents as rendered Markdown, with headings
  and paragraphs, around the occurrence, and marks the occurrence. The
  reader keeps its scope controls and document metadata, and marks the
  sentences a workbench search scope excludes. A sidecar binds to a served
  corpus by the identities it records; a sidecar that does not bind stops
  startup with an error naming it and, for a corpus mismatch, both analysis
  identities. `ReadingSidecar::read_key` reads the identities a sidecar
  records, `ReadingSidecar::verify_digest` checks only its records digest,
  and `ReadingSidecar::page_at` reads a page and an occurrence's anchor
  together. The NixOS module's `services.lingrep-web.readingSidecars` option
  passes each listed sidecar as `--reading-sidecar`, mounted read-only.
- Selecting a line of a Frequencies entry opens the occurrence in its
  document. Closing the reader, or the browser's Back, returns focus to the
  line at the same scroll position. The address holds the open reader as
  `read` and `scope`, in the Frequencies view and in the workbench, so a
  link or a reload reopens it. Reading requests from the Frequencies view
  do not wait for workbench scans or exports.
- The workbench capture-key picker edits single-token `text`, `lemma` and
  `norm` projections using compiler-provided source ranges and runs a fresh
  CLI summary over the selected corpus scope. Saved count tables are unchanged.
- Word-constrained graph-node keys can project any scalar token attribute,
  including `norm`; missing values make the match unkeyable.
- `ling --json check` adds `capture_key_edits` to its `lingrep.check/v1`
  record. Each entry identifies a local, single-field key that projects `text`,
  `lemma` or `norm` from a single-token surface capture, along with its
  definition, capture, source file, available choices and the attribute's
  UTF-8 byte range in the source.
- The query/v2 `run` record from `matches` and `summarize` adds
  `matcher_semantics_identity`. Reusing a saved result requires matching
  matcher semantics, catalog and analysis identities.

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
- The workbench reader marks a hit in the colours of the KWIC keyword.
- The query/v2 `complete` record's `counters.tokens_matched` is renamed
  `counters.tokens_scanned`, with no alias, so a reader of `tokens_matched`
  must change. It counts the tokens of the sentences the matcher
  ran, not of those counted in `sentences_proven_absent`. Admitted tokens are the
  sum of `tokens_admitted` over the `document` records of
  `summarize --by-document`.
- Opening a prepared corpus's provenance no longer decodes every sentence's
  stored analysis. This removes eager analysis decoding from provenance reads for `--where`,
  `--source-metadata`, lookaround divisions, `--by-section`, `--by-metadata`,
  and `ling corpus inspect`. A stored analysis that does not
  decode, or a sentence whose input identity is only partly stored, is now
  reported when that sentence's analysis is read, as by lingrep-web's reading
  view, and by `ling corpus inspect --verify`, instead of when provenance is
  opened. lingrep-web therefore starts serving such an artifact, and the
  request that reads the damaged sentence fails.
- Markdown analysis excludes marked LaTeX math, strips nested list markers,
  and reads included pipe-table rows as cell text. Significance asterisks before
  p-value comparisons are preserved only with conversion-artifact recovery.
- With `--input-format soranoha` or `soranoha-tei`, `@document.ndc` contains
  distinct NDC class codes in catalog order, without the `NDC ` prefix:
  `NDC 712 914` becomes `["712", "914"]`. Replace
  `@document.ndc = "NDC 913"` with `@document.ndc contains "913"`.
  Codes retain leading zeros and `K` prefixes, so `913` and `K913` are distinct.
  The `K` prefix identifies content for children and young adults.
  A catalog field without `NDC` followed by codes produces an error that
  identifies the row. Both reader identities change, so rebuilding a Soranoha
  artifact produces a new analysis identity. Existing artifacts retain their
  identities and string-valued `ndc` metadata.

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
- Under `--base-path /prefix`, lingrep-web serves its start page at
  `/prefix/` and redirects `/prefix` there. `/prefix/` answered 404, so the
  deployment guide's Caddy redirect and every page's link back to the start
  page led to a 404, and the page at `/prefix` loaded its assets from
  outside the mount.
- `ling serve` and `lingrep-web` stop on SIGTERM or SIGINT: they stop
  accepting connections, let in-flight requests finish for up to 10 seconds,
  and exit 0; a second signal ends that wait at once. `ling serve` ignored
  both signals, so `systemctl stop` or a
  container runtime waited for its kill timeout, and `lingrep-web` was killed
  by the signal mid-request.
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
- Malformed reading-sidecar segment ranges return corruption errors before
  highlight projection instead of overflowing.
- The browser scope test waits for its section tab to be clickable.
- Release archives and Nix packages include browser asset notices, and
  `install.sh` retains the archive's licenses and notices.

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
