# ling CLI Usage Guide

`ling` is the packaged command-line executable.

## Command tree

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
ling check
ling neighbors
ling dictionary install
ling analyzer prepare
ling doctor
ling describe [COMMAND]
ling completions SHELL
ling manpages --output DIRECTORY
ling contracts [CONTRACT]
```

Use `ling <command> --help` for common options and `--help-all` for additional
analyzer settings, resource limits, formatting and diagnostics. `ling check --help`
also includes the pattern-language reference. JSON output retains full identifiers
and protocol details; terminal output reports readable summaries.

## Choose a workflow

| Goal | Start with | Continue with |
| --- | --- | --- |
| Validate lingrep patterns without reading a corpus | `ling check` | `ling check --help` for the language reference |
| Find individual matches | `ling matches` | [Catalogs and matching](#catalogs-and-matching) |
| Count matches or authored terms | `ling summarize` | [What `summarize` groups by](#what-summarize-groups-by) |
| Inspect analyzed tokens | `ling tokenize` | [Tokenization and interactive workflows](#tokenization-and-interactive-workflows) |
| Check attribute availability | `ling coverage` | [Attribute coverage](#attribute-coverage-ling-coverage) |
| Read saved per-document counts under a scope | `ling table` | [Count tables](#saving-per-document-counts-count-tables) |
| Compare two document scopes | `ling contrast` | [Comparing two scopes](#comparing-two-scopes-ling-contrast) |
| Explore interactively | `ling explore` | [TUI guide](tui-guide.md) |
| Browse corpora in a web browser | `ling serve` | [Serving several corpora](#serving-several-corpora-from-lingrep-web) |
| Reuse analyzed input | `--cache` or `ling corpus build` | [Reusing and preparing a corpus](#reusing-and-preparing-a-corpus-from-a-query) |
| Select an analyzer | `--analyzer` | [Choose an analyzer](#choose-an-analyzer) |
| Build a set of similar words | `ling neighbors` | [Sets of similar words](#sets-of-similar-words-ling-neighbors) |
| Install a UniDic dictionary | `ling dictionary install` | [Use Vibrato with UniDic](#use-vibrato-with-unidic) |
| Set up spaCy or Stanza | `ling analyzer prepare` | [Use an external protocol adapter](#use-an-external-protocol-adapter) |
| Preflight a long operation | `--plan` | [Discovery and diagnostics](#discovery-explanation-and-completion) |
| Diagnose inputs | `ling doctor` | [Discovery and diagnostics](#discovery-explanation-and-completion) |
| Consume machine output | `ling contracts` | [TSV contracts](lingrep-tsv-contracts.md) |

## Catalogs and matching

Commands that compile a catalog require exactly one of `--patterns FILE` and
`--pattern TEXT`. The former compiles a complete lingrep patterns document from
a file; the latter compiles one inline expression or document.

`ling check --help` and `ling-check(1)` carry the complete lingrep patterns
language reference.

```bash
ling check --patterns patterns/sentence-final.lx
ling check --verify-examples --dict-path "$LINGREP_DICTIONARY_PATH" --patterns patterns/sentence-final.lx
ling matches --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sentence-final.lx --json --context 2 corpus/
ling summarize --dict-path "$LINGREP_DICTIONARY_PATH" \
  --pattern '[upos=VERB].' --tsv --top 50 corpus/
```

`check` compiles without analysis by default. `--verify-examples` runs
`examples` and `counterexamples` through the selected native Vibrato or Sudachi
configuration, with one sentence per claim (`--segmentation input`). The report
identifies the catalog, analyzer and resource, distinguishes zero checked claims,
and includes malformed, expected/unexpected-match and post-policy failures.
Violated claims exit 1; unavailable attributes and analysis failures cannot pass
counterexamples. Successful example verification confirms consistency with authored
claims, not linguistic correctness or recall. External adapters are rejected by this
verification path.

The Sample debugger of `ling serve` and the Workbench "Test on sample" drawer
use the server's Vibrato dictionary to tokenize each example and counterexample
independently of the sample text. They check these claims when the catalog binds
to the sample token schema and all referenced attributes are available. Otherwise,
the debugger reports the schema error or lists the missing attributes. Select a
failed claim's text to add it to the sample for inspection.

Each `expected_match` failure carries an `explanation` of why the definition
published no match for that example. Its `outcome` is one of:

| `outcome` | Meaning |
|---|---|
| `stopped` | No start position matched the body. `attempt` is the attempt that matched the most tokens: where it started, the token it stopped at, the failing element's location in the catalog source, and each attribute test of that element with the token's value. `stop` distinguishes a failed token element, the end of the sentence, a sentence anchor, and each lookaround kind. |
| `where_rejected` | The body matched the token `spans`, and the `where` clause or a span constraint rejected each. |
| `not_published` | The definition matched the token `spans`, and result policy published none. |
| `unexplained` | `detail` states why no explanation is available. |

The matcher decides whether the definition matched and what policy published.
The `stopped` and `where_rejected` outcomes come from a separate explanation
pass that evaluates the definition body at each start position; `starts_tried`
counts them, and `truncated` reports that the pass reached its step limit. The
pass does not evaluate `same_as`, `within=` lookarounds, or definitions under
`skip:`, and reports those as `unexplained`. Human output prints the same facts
under each failure. The Sample debugger shows the explanation under each failed
example and, for every sample line without a published match, per definition.

The Sample debugger also writes claims. Each match has an action that adds its
sentence to the definition's `examples`, and each definition that a sentence
does not match has one that adds the sentence to its `counterexamples`. The
action inserts into the pattern text, creating the key on the line above the
definition when the definition does not write it, and the sample runs again.
No action is offered when the list already holds the sentence or when the
definition inherits the list from `defaults` or a bundle.

A counterexample passes whenever its definition does not match it, including
when the sentence is unrelated to the definition. Verification therefore also
evaluates each passing counterexample against its definition once per
attribute test, token element and lookaround of the body, with that one
constraint removed. A counterexample that no such relaxation matches is
reported under `vacuous_counterexamples` with the relaxations tried; it does
not change the exit status. `unchecked_counterexamples` lists those whose
definition the explanation pass cannot evaluate.

`--propose-counterexamples FILE` runs the same relaxations over a file with one
sentence per line, analyzed like the claims. `counterexample_candidates` lists
sentences a definition does not match and does match with one constraint
removed, at most three per definition and constraint, each with the constraint
and the matched token span. The report gives the number of sentences scanned
and the definitions that were not evaluated; it describes that file only.

### Pattern diagrams

`check --diagram` displays recognition flow instead of the check report. No
dictionary or corpus is required. By default, it draws every pattern and fragment
in source order; repeat `--name NAME` to select definitions. References stay named
unless `--expand` substitutes them.

```bash
ling check --pattern '[upos=NOUN]? [upos=VERB]+.' --diagram
ling check --patterns patterns/double-wo.lx --diagram --name double_wo_candidate \
  --expand --width 80
ling check --patterns patterns/double-wo.lx --diagram --name double_wo_candidate \
  --diagram-format svg >double-wo.svg
ling check --patterns patterns/double-wo.lx --diagram --diagram-format svg \
  --output-dir double-wo-diagrams
```

`--diagram-format` selects `unicode` (the default), `ascii` connectors, or a
standalone `svg`. Writing SVG to stdout requires exactly one selected
definition.

`--output-dir DIR` writes one SVG file per selected definition instead, and
requires `--diagram-format svg`. `DIR` must not exist: `ling` writes the files
into a hidden staging directory beside `DIR` and renames that directory to `DIR`
only after every selected definition renders. If `DIR` already exists, `ling`
exits with status 1 and leaves it unchanged; a failed or interrupted export
removes its staging directory and creates no `DIR`. On a filesystem without a
no-replace rename (`renameat2` with `RENAME_NOREPLACE` on Linux, `renamex_np` on
macOS), such as a WSL `/mnt/c` drive, NFS, or some FUSE mounts, `ling` checks
that `DIR` is absent and then renames; an empty directory created at `DIR`
between the check and the rename is replaced.

Each exported file is named `ORDINAL-STEM.svg`: `ORDINAL` is the 1-based
source-order position, padded to the width of the largest ordinal, and `STEM` is
at most 64 bytes of the definition name, with each character other than a
letter, digit, or `_` replaced by `_`. `ling` prints each definition name and
its file path to stdout, separated by a tab, in source order.

`--width COLUMNS` accepts 32–240 columns; the default is the terminal width
within that range, or 80 when piped or writing `--output-dir` files. Wide
sequences continue on connected rows. Numbered labels and subdiagrams retain
text that cannot fit inline; dependency constraints and publication annotations
appear separately from recognition flow.

Diagram mode cannot be combined with `--json`, `--compare-with`,
`--verify-examples` or `--schema`. Compiler warnings go to stderr. `ling` exits 0
for a closed text-output pipe; an incomplete SVG write exits 1.

### Matching and summaries

`matches` emits canonical per-match evidence. A published token carries only the
attributes that the run projects: the catalog's own, plus `text`, plus those
that its policy projects. Add `--publish-attribute lemma` (repeatable) to
include more attributes, or `--publish-all-attributes` for every attribute in
the corpus's token schema. Both options require `--json`. Unavailable extra keys
produce an `attribute_unavailable` warning and are omitted. The `run` record
lists the published attributes in `published_attributes`; individual tokens omit
values they do not carry.
`--context N` projects at most `N` tokens to each side and never changes the
matched span. `summarize` performs exact external aggregation before publishing
rows. Use `--limit N` to publish only the first N matches.

For exact full-scope counts with bounded examples, use
`ling --json matches --retain 100 --retain-seed 7 ...`. That command scans to
EOF, then emits at most 100 published matches in corpus order with their
original sequence IDs. Completion declares the Algorithm R/SplitMix64 selection,
seed, eligible and retained counts; `records_emitted` counts retained records.
Retention is global across patterns, capped at 100,000 records and 64 MiB of
serialized JSON, and fails if the byte cap is exceeded. It requires JSON and
cannot be combined with `--limit`. No retained examples or successful completion
are published after scan failure or cancellation. With `--sample`, totals cover
that declared corpus sample rather than a corpus census.

For human output, closing stdout with a pager or `head` ends the run successfully.
JSONL and TSV output treat a closed stdout as an error: the run
reports `broken_pipe` and exits `1`.

### Filtering a framed JSONL stream

Keep every non-match record when selecting matches with `jq`. The `run`,
`definition` and `complete` records carry the catalog, corpus identity,
coverage and termination information that a downstream viewer requires.

```bash
demo=$(mktemp -d)
printf '猫。犬。\n' >"$demo/sample.txt"
ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output "$demo/sample.lingcorpus" "$demo/sample.txt"
ling --json matches --corpus "$demo/sample.lingcorpus" \
  --pattern 'noun := [upos=NOUN].' --retain 100 \
  | jq -c 'select(.type != "match" or .data.surface == "猫")' \
  >"$demo/cats.jsonl"
```

The filtered file retains the original completion counters and coverage;
`records_emitted` and match totals describe the unfiltered run, not the matches
that passed `jq`. Sequence numbers are unchanged. For this prepared run,
`complete.data.analysis_identity` identifies the analyzed artifact against which
the surviving matches' sentence ordinals resolve. Filtering does not create a
new corpus identity or a new counted run. A stream without `complete` provides
no successful completion claim, even if its retained matches look valid.

### Human KWIC rendering

Human `matches` output concatenates morphemes by default. A single ordinary
space separates a present context from the complete hit. Inside the left
context, hit, or right context a space appears only where the source has
whitespace between two tokens, or where a morpheme separator is requested. Context uses
a six-color qualitative cycle to make adjacent unseparated morphemes easier to
distinguish. Distances one through six have fixed colors; distance seven
repeats distance one, eight repeats two, and so on. On a styled terminal the
hit is bold; when styling is disabled by `--color never`, `NO_COLOR`, or
redirected `--color auto`, visible `⟦hit⟧` markers identify it without relying
on color.

Use `--context-color-depth N` to color the nearest N morphemes on each side;
distance N+1 and farther remain dim. The default is 5, and 0 disables proximity
colors without disabling the bold hit or distant-context styling. The depth
does not resample the palette, so a visible distance keeps the same color when
depth or projected context changes.

`--context-color-theme auto|dark|light` selects the dark- or light-background
variant of the six-color cycle. `auto` uses `COLORFGBG` when available and
otherwise chooses dark; set `light` explicitly on a light terminal when the
hint is absent. Truecolor is the primary path. ANSI-256 uses six fixed nearest
indices, while ANSI-16 uses six terminal roles whose actual colors depend on
the terminal theme.

```bash
# Keep all context dim.
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --context-color-depth 0 corpus/

# Color the nearest three morphemes on each side.
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --context-color-depth 3 corpus/

# Use the default depth explicitly and adapt for a light background.
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --context-color-depth 5 --context-color-theme light corpus/
```

Use `--morpheme-separator TEXT` when morpheme boundaries matter. The separator
is applied inside all three fields, including a multi-token hit, but never in
place of the spaces around the hit:

```bash
# Reproduce space-separated morphemes.
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --morpheme-separator ' ' corpus/

# Make every morpheme boundary explicit.
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --morpheme-separator '|' corpus/
```

The separator may not contain control characters, bidi controls, or Unicode
line or paragraph separators. Corpus text is escaped before terminal-width
calculation.

`--align-hits` places hit content at a fixed display column one third of the
sampled terminal width, so successive rows scan vertically; the flowing layout
is the default. Width is sampled once per run and has an effective floor of 10
columns. Both layouts stream rows without reading the full result set first.
These options affect human output only: JSONL and TSV bytes, including TSV
quote escaping, are unchanged.

### Source metadata in JSONL matches

Repeat `--source-metadata @document.KEY`, `@sentence.KEY` or
`@division(KIND).KEY` with `--json matches` to publish selected source fields.
The query/v2 run lists selectors in `published_source_metadata`. Each match
carries a `source_metadata` object with separate `document`, `sentence` and
`divisions` owners. Division entries contain `id`, `kind`, `parent_id`, authored
`source_id`, and `metadata`; a match is published once even with several
matching divisions. Fields have no implicit inheritance. Missing fields are
omitted; empty strings, zero, false and null remain values. Without the option,
both publication fields are absent.

There may be at most 256 unique selectors, with keys up to 256 UTF-8 bytes.
Selected metadata exceeding 64 KiB of serialized JSON per sentence fails the
run. This option requires JSONL output and does not alter token matching or
summary grouping.

### Dependency evidence: `--evidence`

A match's span and `--context` can omit the words on which a relation judgment
rests. In this catalog the span is the noun and particle, and `v` is bound
anywhere in the sentence:

```pattern
npv := (?<n> [upos in {NOUN, PROPN, PRON, NUM} & dep.rel in {nsubj, obj, obl}])
       (?<p> [dep.rel=case & lemma in {が, を, に, で, から, より, と, へ}])
       where (?<v> [upos=VERB]) -[nsubj|obj|obl]-> $n, $n -[case]-> $p.
```

`--evidence` adds the sentence and those words to every match. With `--tsv` it
writes `lingrep.evidence-tsv/v1` in place of `lingrep.matches-tsv/v1`: one row
per word, with the match's provenance and the whole sentence repeated on each
row.

```bash
ling matches --corpus natsume.lingcorpus --patterns class6.lx --tsv --evidence > npv-evidence.tsv
```

For `坂を一つ上った`, the `npv` match has these rows, shown from the `word`
column onwards (`cut -f9-`):

```text
1	n	true	坂	坂		NOUN	名詞	4	obj	坂を一つ上った
2	p	true	を	を		ADP	助詞	1	case	坂を一つ上った
4	v	false	上った	上る		VERB	動詞	0	root	坂を一つ上った
```

The columns are the word number, its roles, whether it lies in the span,
`text`, `lemma`, `norm`, `upos`, `xpos`, `head`, `rel`, and the sentence. The
word number is the token index plus one, the numbering that `head` uses, so `坂`
depends on word 4, `上った`, as `obj`. The words are the span, every capture,
and every word that a `where` clause binds, whatever `--context` requests; a
named enhanced edge `$e` marks its endpoints `e.head` and `e.dependent`. A value
that the corpus does not carry is an empty field, for example `norm` over
CoNLL-U, and `head` and `rel` over a morphology-only corpus.

With `--json`, each match record gains an `evidence` object with `sentence` and
`words`; an absent value is `null`. Every other field of the record is
unchanged. TSV has no run or completion record, so save the JSONL form when the
evidence must record the corpus's `analysis_identity` or prove that the run
completed. `--evidence` requires `--json` or `--tsv`.

### Comparing two patterns' occurrences

Counts cannot show which passages one pattern finds and another misses.
[`examples/compare_occurrences.py`](../examples/compare_occurrences.py) compares
two saved `--json --evidence` runs and requires only Python 3:

```bash
ling matches --corpus natsume.lingcorpus --json --evidence --limit all --pattern \
  '(?<n> [lemma=坂 & upos in {NOUN, PROPN, PRON, NUM} & dep.rel in {nsubj, obj, obl}]) (?<p> [lemma=を & dep.rel=case]) not_followed_by([dep.rel=fixed]) where (?<v> [lemma=上る & upos=VERB]) -[nsubj|obj|obl]-> $n, $n -[case]-> $p.' \
  > npv.jsonl
ling matches --corpus natsume.lingcorpus --json --evidence --limit all --pattern \
  '(?<n> [lemma=坂 & upos in {NOUN, PROPN, PRON, NUM}]) (?<p> [lemma=を]) (?<v> [lemma=上る & upos=VERB]).' \
  > adjacent.jsonl
python3 examples/compare_occurrences.py npv.jsonl adjacent.jsonl > compare.tsv
```

An occurrence is a sentence, identified by source name, document ordinal, and
document sentence ordinal, together with the word that each named role occupies.
Both patterns above declare `n`, `p`, and `v`, so the dependency match `坂を` and
the sequence match `坂を上った` are the same occurrence when their three words
coincide, although their spans differ. Roles are capture names, graph-node
names, and named edges. By default, the script compares the roles that every
record in both runs binds; pass `--roles n,p` to compare a subset. A pattern
without named roles cannot be aligned this way. Name the roles with `(?<n> …)`
as above.

`compare.tsv` has a header and one row per occurrence: `shared`, `a_only` or
`b_only`; the number of records on each side; the source name and ordinals; the
role positions; each role's word as `role=text(word,rel→head)`; and the
sentence. The same wording in two works or two sentences is two rows, and two
occurrences in one sentence are two rows. A record count above 1 means several
graph assignments share the compared roles. The totals go to stderr.

Both files may be one run of a catalog that defines both patterns; select them
with `--a-pattern NAME --b-pattern NAME`. The script exits with status 2 and
writes no rows when either file lacks its completion record, stopped at
`--limit`, came from `--sample` or `--retain`, lacks `--evidence`, or was run
over a different corpus, analysis, or `--where` scope than the other. A direct
run and a prepared run of the same files have different analysis identities, so
compare two runs of the same kind.

To compare one query across two annotations of the same sentences, such as a UD
treebank and its SUD conversion, see
[paired-treebank-comparison.md](paired-treebank-comparison.md).

### Declared result policy in published records

A catalog's metadata and its `policy … end` block declare what it publishes,
not only what it matches. Three JSONL fields on a `match` record carry the
result, and each is omitted when the catalog derives nothing for it:

| Field | Source | Shape |
|---|---|---|
| `unit` | a `transform:` list ending in `{op: join}` | string |
| `key` | `key: [pos1, $capture.span.norm, …]` | typed ordered scalar fields |
| `tags` | `tags: {name: source, …}` | `[{"name": …, "value": …}]`, by tag name |

`--trace-policy 0` writes the run's policy totals to stderr: a header with the
statement count and, when policy removed captures, the number dropped, then one
line per policy statement that the catalog declares:

```text
policy totals (2 statements, 3 captures dropped)
  resolve %heads rank: [longest, start, end, pattern]. (7)  → dropped 4, narrowed 0
  detach %trailing_copula with: @DA, relation: at_end, evidence: raw. (8)  → dropped 0, narrowed 13
```

Each line quotes the statement's text and its catalog line number. Every
declared statement is reported, including one that changed nothing, so an
inert statement is visible. The totals cover the records that the run processed,
including under `--limit`, sampling, and scope filters. Use `--trace-policy`
to request these totals; `--progress` does not print them.

Key fields keep their authored order through matching, aggregation, JSONL, TSV,
and human output. Tags keep their own canonical ordering.

```bash
ling matches --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns verb-units.lx corpus/ \
  | jq -r 'select(.type=="match") | .data | [.unit, (.key//[]|map(.value)|join("\t"))] | @tsv'
```

`surface` is always the published span's own text. A `substitute` op changes
`unit` without changing the span, so `unit` and `surface` can differ.

`surface` joins the span's token spellings with one space wherever their
character offsets show a gap. JSONL, TSV and surface count keys distinguish
`NHK news` from `NHKnews`. Several spaces or a tab between the tokens give
the same key as a single space. Use `char_span` for the original spacing.
When offsets are unavailable, match exports concatenate the unplaced tokens.
Grouping by a multi-token surface requires offsets for every internal boundary.
Single-token surfaces and explicit attribute keys require no spacing evidence.

A CoNLL-U sentence with a `# text` comment takes its offsets from that text:
each FORM is located in it, in order, with only whitespace between the forms,
so the spacing is the comment's even where `SpaceAfter=No` is missing or
wrong. A sentence has no offsets when its forms do not occur in its `# text`
that way. `ling summarize` stops at the first multi-token surface
or `--by-span` key in such a sentence with `error[missing_surface_offsets]`,
exit code 1, and writes no summary and no count table, because the key would
otherwise depend on a guess about the missing spaces. The error identifies the
file, the sentence, and the token after the unplaced boundary. To count the
corpus, correct or delete that sentence's `# text` line, since a sentence
without one takes its spacing from `SpaceAfter=No`; or declare a `key:` for
the pattern, which groups by attributes and reads no spacing. `ling matches`
reads the same sentence without an error.

A tag source is a literal (`{aspect: perfective}`), a capture-presence mapping
(`{aspect: {perf: perfective, prog: progressive}}`, where the named capture that
participated supplies the value), or a projection off one capture
(`{lemma: {$head: lemma}}`). A projection reads a single token, so the compiler
rejects one whose capture can cover more than one token or can match more than
once. `key:` accepts `$c.first.attr`, `$c.last.attr`, or `$c.span.attr` to
select which reading of a wider capture is meant. `$c.attr` requires an
exact-width-one capture. A bare `attr` projects the whole match and requires the
definition itself to have exact width one. `tags:` has no component spelling;
narrow the capture, or use a presence mapping.

`captures` has different contents in the two publication modes. By default it
carries the post-rewrite survivors: a `policy` block's `detach`, and the
`drop_edge` and `truncate_before` transforms, all rewrite the published span,
and a capture outside the rewritten span is dropped rather than clamped, so a
capture present in the raw match can be absent here. Under `--raw-matches` it
carries the raw match's captures verbatim. To obtain raw match coordinates, use
`--raw-matches`; a default run's captures are not raw coordinates. The
`complete` event's `counters.captures_dropped` counts captures removed this
way, and `counters.records_unkeyed` counts records whose declared `key:` could
not be projected. Both are omitted when zero.

JSONL match records and surviving captures carry `char_span`: half-open,
sentence-relative Unicode code-point offsets into the exact analysis text.
Analyzer paths address text after the declared normalization policy. Every
analyzer reads the source unchanged by default, so its offsets address the
source text; under `--normalization nfc` they address the NFC text. CoNLL-U
offsets address the `# text` comment, or the text reconstructed from the rows
when there is none, and are omitted when the forms do not occur in `# text`. Words sharing a multiword surface token each address its whole
surface range; their internal character boundaries are not inferred. A run
declares `char_offsets`, and `counters.offsets_omitted` counts published
records whose alignment is unavailable. Token-index `span` and the TSV columns
are unaffected. These offsets are not raw-file byte offsets; annotation export
must verify an explicit mapping to the target text view. The bounded CoNLL-U
consumer in
[`examples/soranoha_annotations.py`](../examples/soranoha_annotations.py) checks
that mapping and preserves incomplete execution as a partial or failed layer.

### Explaining one record: `--trace-policy N`

The per-statement totals above summarize a whole run. A positive
`--trace-policy N` prints those totals and then reports, for at most `N`
published records, which policy statements shaped each record or removed it:

```text
policy trace (400 records)
  …
  00002.md:19 可能である 26..29
    detach %trailing_copula with: @DA, relation: at_end, evidence: raw. (746)  → 26..27
    admit @という terminal: {exact: true, anchor: propositional_end}. (747)  → not selected
  00002.md:19 だ 27..29
    detach %trailing_copula with: @DA, relation: at_end, evidence: raw. (746)  → not selected
    admit @という terminal: {exact: true, anchor: propositional_end}. (747)  → not selected
  …
policy trace truncated at 400 records
```

Each record line is `source:sentence definition raw_start..raw_end`. Under it
is one line per statement that ran while the record was a candidate: the
statement's text, its catalog line, and its outcome: `kept`, `not selected`,
`dropped`, or the new span that a `detach` produced (together with the dropped
capture count when nonzero). A record whose lines all read `not selected`
was left unchanged because no statement selected it.

- A dropped record's lines stop at the statement that dropped it; later
  statements did not evaluate it.
- Reaching `N` is reported on the last line.
- The trace goes to stderr and is printed regardless of `--progress`. JSONL and
  TSV records are unchanged by the flag.

`--overlaps keep` skips the catalog's `resolve` statements; the block's
`exclude`, `require`, `detach` and `admit` statements still apply. `--overlaps
longest` publishes the longest of overlapping records for a catalog whose block
declares no `resolve`. `matches` and `summarize` accept both, and the trace
lists the statements that the run executed. `--overlaps` is rejected with
`--raw-matches`, and with `summarize --write-table`, whose tables hold the
catalog's own counts. See
[Overlaps chosen per run](pattern-language-reference.md#overlaps-chosen-per-run).

`--trace-policy` is rejected with `--raw-matches`, because raw publication
applies no policy. Unlike `--plan`, which validates the invocation without
scanning, `--trace-policy` scans and reports per record. Long statement text is
shortened on the trace line; compile diagnostics always quote the statement in
full.

`matches` and `summarize` accept `--trace-policy` and
report the per-statement totals. `explore` has neither, because output
written beneath the TUI would not be visible.

On `summarize` the trace covers the policy program; records with empty surfaces
or unprojectable keys can still be absent from the output.

### What `summarize` groups by

`summarize` takes its grouping value from the first of these fields present on
the record, each coarser than the one above it:

1. **The declared `key:`,** represented as an ordered typed field tuple. In
   query/v2 JSONL it is a `group` of kind `key` with a `fields` array;
   summarize-TSV/v1 renders it as a compact name-to-value JSON object in the
   fixed `group` column. Field names and source order are part of the grouping
   identity in both encodings.
2. **The rendered `unit`,** when a transform produced one.
3. **The authored lexical term,** for an unkeyed lexicon occurrence. Transforms
   are not allowed in lexicons, so their grouping order is declared key, term,
   surface.
4. **The matched surface.**

Rows are ordered by authored pattern and descending count. Equal counts use
the typed group order: surface, rendered unit, authored lexical term, then
projected key. Projected keys compare their authored field-name vector and then
their value vector, so the result does not depend on which schema the corpus
encounters first.

A record whose pattern declares a `key:` that this match cannot satisfy (an
alternation branch that skipped the capture, or a token lacking the projected
attribute) is dropped from keyed output and counted in
`counters.records_unkeyed`. It is never grouped under a partial key that could
collide with another pattern's complete key.

`--grouping-coverage` adds a `grouping_coverage` array to the JSONL `complete`
record. Each entry identifies a published pattern and reports `records_presented`,
`records_grouped` and `records_unkeyed`; presented equals grouped plus unkeyed.
Counts describe records admitted to grouping after matcher selection and catalog
publication policy and any pattern-metadata filter, before unkeyed records are
omitted or `--top` selects rows.
A missing projected attribute or capture increments unkeyed; a scalar or list
key that is available increments grouped once per record. These are record
counts, not distinct key counts or the raw `matches_found` counter.

Every eligible published pattern appears in catalog order, including patterns
with zero records. With `--by-document`, each `document` record also carries a sparse
`grouping_coverage` array: omitted patterns have zero presented records, and an
empty array means no published record reached grouping in that document.
The arrays are absent unless requested, so existing JSONL and keyed rows remain
unchanged. Coverage counters spill under the shared `--max-temp-bytes` limit;
a failed run never publishes successful completion coverage.

With a pattern-metadata filter, coverage includes only eligible published names.
Metadata category rows do not merge coverage records for their member patterns.

### Counting by a sentence span

`ling summarize --by-span bunsetsu` counts published matches by the surface of
their enclosing bunsetsu. The JSONL `run.by_span` field records the requested kind.
Two matching words in 猫が contribute two to the
猫が row. A match crossing a bunsetsu boundary or lacking an annotation of
the requested kind contributes one to `counters.records_unkeyed`. This option
overrides the pattern's grouping key. If several spans of that kind contain
the complete match, the shortest wins, with ties resolved by sentence span ID.

Span annotations come from the analyzer or CoNLL-U input and vary by
sentence. When the requested kind is absent, `--by-span` counts every match
as unkeyed. For example, Vibrato supplies no `bunsetsu` spans, and Sudachi
split mode C supplies no `sudachi.c` spans. After a full scan that admits at
least one sentence, `ling matches` and `ling summarize` report
`warning[span_kind_unavailable]` for each requested kind that never occurred.
For kinds referenced by `where span(…)`, `within_span`, or `span_dependency`,
the warning lists the definitions that reference them. Limited scans,
`ling check --schema`, the browser, and `ling explore` omit this warning.

JSONL `match` records carry `span_ids` when annotations contain the complete
published match. These zero-based IDs are local to `corpus_sentence_ordinal`.
The `whole_span_ids` object groups annotations with exactly the published
boundaries by layer. For example, `whole_span_ids.bunsetsu` gives the IDs of
coextensive bunsetsu. Empty groups are omitted. With `--evidence`, each ID
indexes the sentence annotation in `evidence.spans`.
In the web token inspector, select a word and open the Details panel to inspect
its enclosing units, their IDs, and any recorded heads, labels, and governors.

### Japanese reading predictors

The [psycholinguistics catalog](../patterns/psycholinguistics.lx) extracts
written word forms, two- and three-character fixation targets, bunsetsu and
case-marked relative-clause candidates, nominal dependent counts per verb,
and subject–object pairs with one lemma and two spellings. Summarizing
`nominal_load` by its declared key produces the count distribution, including
verbs with zero qualifying dependents. Use Japanese UD CoNLL-U or an
equivalent prepared corpus with basic dependencies. Bunsetsu analyses also
require annotated bunsetsu. Sentences without them still contribute lexical
and UD matches. The case-marked candidates provide the overt coargument,
particle and modified noun for review. The gap role is an inference.

Run the worked example from the repository root. The DuckDB step computes
the predictors that require several matches or character positions.

```bash
ling --json matches --evidence --input-format conllu \
  --patterns patterns/psycholinguistics.lx \
  lingrep/tests/fixtures/psycholinguistics.conllu > reading.jsonl
duckdb -batch -bail -cmd "SET VARIABLE matches = 'reading.jsonl'" \
  < examples/psycholinguistics.sql
```

* **Vocabulary.** Each observed two-kanji spelling is a target. 学校 has two
  distinct one-position neighbors, 学生 and 高校. The second occurrence of
  学生 increases its frequency. A separate table gives each noun spelling's
  share among occurrences with the same analyzer lemma.
* **Characters and tokens.** Each annotated bunsetsu has its own row with
  pure-script token counts, character counts, and maximal character runs.
  読む contributes one kanji and one hiragana character inside one mixed
  token. すしを has two hiragana tokens and one character run. Another table
  reports maximal runs of consecutive pure-script tokens. The example
  counts ー with katakana and counts Unicode code points in the displayed
  surface, including original spaces as run separators.
* **Syntactic load.** Earlier dependents count observed bunsetsu attachments.
  In 新聞を子どもが読む the three units have loads of 0, 0 and 2. Sentence
  ordinal and span ID keep repeated spellings separate. An absent annotation
  supplies no unit. In partially annotated data a zero measures observed links.
* **Experimental alignment.** The fixation-target table preserves sentence,
  token position, spelling, length and whether the target begins a bunsetsu.
  The clause query identifies 男性を見た女性 and 男性が見た女性 by their
  different overt arguments. Testing effects on readers requires aligning
  these observations to independent fixation or reading-time measurements.

### Sentence-level dependency complexity

Run the occurrence-measurement catalog to obtain both tree profiles, nominal
dependent counts and distances per verb, and kanji-word counts per bunsetsu.
Named numeric values and their observation statuses appear in `graph_bindings`.

```bash
ling --json matches --patterns examples/occurrence-measures.lx --input-format conllu \
  lingrep/tests/fixtures/psycholinguistics.conllu
```

For the independent DuckDB calculation, export one match covering every word of each nonempty sentence. The same DuckDB
query measures the basic UD word tree and the annotated bunsetsu tree. For a
prepared corpus, replace the input format and filename with `--corpus ARTIFACT`.

```bash
ling --json matches --pattern '^ []+ $.' --evidence --input-format conllu \
  lingrep/tests/fixtures/psycholinguistics.conllu > sentences.jsonl
duckdb -batch -bail -cmd "SET VARIABLE matches = 'sentences.jsonl'" \
  < examples/dependency-complexity.sql
```

| Column | Definition |
| --- | --- |
| `unit_count` | Number of words or annotated bunsetsu. All CoNLL-U word rows, including punctuation, contribute positions. Bunsetsu positions follow surface order. |
| `mean_distance` | Mean absolute difference in positions over non-root links. Adjacent units have distance one. A one-unit tree has no links and yields NULL. |
| `mean_depth` | Mean number of links from each unit to the root, including the root at depth zero. |
| `height` | Maximum depth in that sentence. Averaging sentence heights gives a different quantity from averaging unit depths. |
| `peak_open_links` | Largest number of links spanning any boundary between adjacent units, in either direction. A one-unit tree yields zero. |

For 新聞を子どもが読む, the word tree has mean distance 2, mean depth 1.2,
height 2 and peak 3. Immediately after 子ども, the links 新聞–読む, 子ども–読む
and 子ども–が are open. The three bunsetsu 新聞を / 子どもが / 読む give 1.5,
2/3, 1 and 2 respectively.

Each sentence has one row per unit type. A usable tree has one root and a path
from every unit to that root. Word links require basic HEAD and DEPREL.
Bunsetsu must partition the sentence, and their root span must contain an
observed root word. Missing annotations give `unavailable`; incomplete or
inconsistent trees give `invalid_tree`. Both keep the sentence in the output
with NULL measures. When comparing the two unit types, select sentences with
`ok` rows for both and report the number contributing to each mean.

### Counting per document

`--by-document` counts each group per admitted document, and `--top` ranks
within each pattern and document. It requires `--json`. Every `summary` record
carries the `document` ordinal, and before the summary rows, the run publishes
one `document` record per document with at least one admitted sentence: its
`source_name`, `sentences_admitted` and `tokens_admitted`. A document with no
match still has a `document` record, so the token count can normalize the
frequencies. A document with no admitted sentence, such as an empty one, has
no record and is not counted in `counters.documents_admitted`, which equals
the number of `document` records.
`--source-metadata @document.KEY` adds the selected fields to each `document`
record as `source_metadata.document`, as on a `match` record.

```bash
ling --json summarize --by-document --source-metadata @document.id \
  --patterns patterns/sentence-final.lx corpus.jsonl
```

Over a prepared corpus `--by-document` reads every sentence, because the
posting index has no per-document token counts.

### Counting per sentence-aligned token window

```bash
ling --json summarize --by-window 300 --corpus corpus.lingcorpus \
  --patterns patterns/sentence-final.lx
ling contracts lingrep.window-counts/v1
```

Set the window size with `--by-window N`. Within each document, divide the
zero-based token ordinal of a sentence's first word by `N` and round down. The result is the window ordinal. The entire sentence
contributes to that window, even when the sentence crosses one or more window
boundaries. Window ordinals restart at zero in each document and may have gaps.

The JSONL stream begins with the window size, corpus identities, catalog
fingerprint, and published pattern names. Each `window` record contains document
and window ordinals, sentence and token totals, and a sparse count for each
published pattern. A missing pattern count is zero. Counts apply publication
policy and include all published occurrences. Counts and ordinals are relative
to the admitted text. Check for the final `complete` record before using the
stream as a full scan.

Use `--json` and either `--corpus` or `--cache`. Window assignment uses the full
prepared document, so a window run rejects scope filters and sampling. If a sentence exceeds execution limits, increase the limit and rerun.

### Counting per section

`--by-section` counts, for every section label of a prepared corpus, each
pattern's matches and the admitted sentences with at least one match. It
requires `--json` and `--corpus` or `--cache`, and cannot be combined with
`--by-document`. The run publishes one `section` record per label with at
least one admitted sentence, in the artifact's coverage order: its `label`,
`sentences_admitted`, `tokens_admitted`, and `patterns`, which lists every
published pattern in catalog order with `matches` and `sentences_matched`. A
sentence carrying several labels is counted under each label, as
`--where '@section = X'` admits it under each. Admitted sentences with no label
are counted in a final record whose `label` is `null`. The `summary` rows are
the same as without the flag. An artifact that records no section labels is
rejected with exit code `2`.

```bash
ling --json summarize --by-section --corpus paper.lingcorpus \
  --patterns patterns/sentence-final.lx
```

`--where` applies first, so `sentences_admitted` is the section's size within
the scope. Like `--by-document`, `--by-section` reads every admitted sentence.

`--pattern-filter KEY=VALUE` selects published definitions by an exact string
metadata value for `--by-section`. For example, `--pattern-filter aspect=perfect`
keeps patterns annotated `aspect: "perfect"`. Filtering happens after catalog
publication policy; it does not change matching, policy resolution or the
admitted-sentence denominator. Matcher counters still describe the full catalog.
Missing and non-string values do not match. An unknown string metadata key is a
usage error. Everything after the first `=` is the literal value.

`--group-patterns-by KEY` aggregates the selected section rows by a string metadata
key. Both options require `--by-section` and can be combined. A grouped `section`
record has an empty `patterns` array and a `pattern_metadata` object containing
`filter`, `group_by`, `omitted_patterns`, and `groups`. Each group contains its
raw `value`, the member `patterns`, summed `matches`, and `pattern_sentences`
(the sum of each member pattern's `sentences_matched`).

Two distinct published pattern names in one sentence contribute two
pattern–sentence occurrences, including when their spans coincide;
`pattern_sentences` is not a count of distinct sentences. Rates use admitted
sentences as the denominator and may exceed 100%. Groups are ordered by
descending pattern–sentence count, then value. Missing or non-string grouping
values are omitted and included in `omitted_patterns`; no replacement category
is invented.

The ordinary keyed `summary` records remain per pattern within the selection.
The `run` record also declares `pattern_metadata` with the filter, grouping key,
and omitted-pattern count, including when the admitted scope is empty.
Definitions sharing one published `output_name` form one member: a requested
metadata key must have the same string value or absence across those definitions,
and conflicting values are rejected with the published name. Without these
options, `run` and `section` records retain their default shapes.

```bash
ling --json summarize --by-section --corpus paper.lingcorpus \
  --patterns constructions.lx --pattern-filter tense=past \
  --group-patterns-by aspect
```

The section-analysis page offers the same metadata selection and row grouping.
Distribution, rate contrast and both cluster trees use the selected category
rows, ordered by their total in the displayed sections. Category labels are not
links to an individual member pattern.

### Counting per metadata value

`--by-metadata @document.KEY` counts the same per-pattern `matches` and
`sentences_matched` under every value of one document metadata key. It
requires `--json` and `--corpus` or `--cache`, and cannot be combined with
`--by-document` or `--by-section`. The run publishes one `metadata_value`
record per value with at least one admitted sentence, in the order in which the
values first occur in the artifact's documents: its `key`, `value`,
`documents_admitted` (documents with that value and at least one admitted
sentence), `sentences_admitted`, `tokens_admitted`, and `patterns` as in a
`section` record.

`value` is the text that `--where` compares, so the string `"1"` and the
number `1` are one value. The record for a scalar value `V` counts what
`--where '@document.KEY = "V"'` admits. A document whose value is a list is
counted once under each distinct listed value, as
`--where '@document.KEY contains "V"'` admits it. Documents without the key,
or with an empty list, are counted in a final record whose `value` is `null`.
A document whose value is an object or a nested list fails the run with exit
code `2` before the scan, because no `--where` comparison selects such a value.
The `summary` rows are the same as without the flag.

```bash
ling --json summarize --by-metadata @document.author \
  --where '@document.source_edition_year >= 1950' \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx
```

`--where` applies first. Like `--by-section`, `--by-metadata` reads every
admitted sentence.

### Saving per-document counts: count tables

`summarize --write-table PATH` checks that the destination is absent and its
parent permits creating output before opening the corpus. Publication also
checks for a competing destination; on filesystems without no-replace rename,
a concurrently created empty directory can be replaced.

It scans a prepared corpus once and writes every
pattern's group counts per document, and the sentences in which each group
occurs, to a count table at `PATH`, a new directory. `ling table` then answers
a `--where` over document metadata from the table without scanning the corpus:

```bash
ling summarize --write-table soranoha.collexeme.lingtable \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx
ling --json table soranoha.collexeme.lingtable --top 100 \
  --where '@document.source_edition_year >= 1950' \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx
```

`ling table --table TABLE` is equivalent to `ling table TABLE`; use exactly one form.
The `--table` option is shared with `ling contrast`.

Before the rows, `ling table` gives each pattern's `occurrences`, the sum of
the counts of all its groups under the clause regardless of `--top`, and
`works`, the number of admitted documents with at least one of them; a clause
that admits nothing gives 0. For each pattern, it then gives each group's
`count`, equal to the `count` of `summarize --where` with the same clause, and
`works`, the number of admitted documents in which the group occurs.

Rows are ordered by count, with ties in `summarize`'s group order. With
`--top all`, or JSON output without `--top`, groups are ordered through
temporary files, which `--max-temp-bytes` limits as it does for `summarize`.
The `complete` record gives the admitted documents, sentences, and tokens. The
output contract is `lingrep.table/v1`, whose `pattern` records carry the totals.

`--select PATTERN` limits the output to one pattern's total and rows.
`--key FIELD=VALUE`, repeatable and only with `--select`, keeps the rows whose
compound key has exactly that value in that field; other fields are
unrestricted. Equality is exact, so `--key '$n.lemma=坂'` keeps `坂` and
omits `寺坂`, unlike a substring search over printed keys:

```bash
ling table class6.table --corpus natsume.lingcorpus --patterns class6.lx \
  --select npv --key '$n.lemma=坂' --key '$p.lemma=を' --top all
```

Under `--key`, a `subset` record follows the `pattern` record. It gives the
admitted occurrences of every selected row, summed before `--top`, the admitted
documents holding any of them, and the number of rows. Each row then gives its
`share` of the subset's occurrences and its rank within the subset; terminal
output prints the share as a percentage. The `pattern` record keeps the whole
pattern's total, and both totals use the same `--where` clause. A subset that
matches no row gives 0 and no rows. A FIELD that is not a field of the
pattern's key, or one whose values are lists, is rejected with exit code `2`.

`ling table` and `ling contrast` verify the table's contents before publishing
results. This integrity read scales with the table size; it does not repeat
matching or tokenization.

A table records the corpus's analysis identity, the catalog fingerprint and
the matcher revision. `ling table` rejects a table when any of the three
differs from the `--corpus` and `--patterns` given, so a table is rebuilt after
the catalog or the corpus changes. `--write-table` cannot be combined with
`--where`, `--sample`, `--top`, `--tsv`, `--by-document`, `--by-section` or
`--by-metadata`, and `ling table` accepts only `@document` conditions; both
reject the rest with exit code `2`. A table stores sentence ordinals in 32
bits, so `--write-table` fails when a group occurs in a sentence whose ordinal
is 2^32 or more.

`--entry PATTERN=KEY --by-metadata @document.NAME` counts one entry under
every value of a document metadata key, from the table:

```bash
ling --json table soranoha.collexeme.lingtable \
  --entry 'te_oku={"$verb.norm":"言う"}' --by-metadata @document.ndc \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx
```

`KEY` selects one entry of the pattern in one of two forms:

- the `group` object of the entry's JSON `row` record, such as
  `{"kind":"key","fields":[{"kind":"scalar","name":"$verb.norm","value":"言う"}]}`
  or `{"kind":"term","value":"猫"}`, with its fields in the row's order;
- the key as human rows print it: a bare value, or a JSON object of the
  key's fields, compared field by field in any order and with any JSON
  whitespace.

Human rows escape control and bidirectional characters for the terminal, and
two entries of different kinds, such as a lexicon term and a surface published
under one `output_name`, can print the same text. In those cases give the
`group` form. An unknown pattern or key, or text that selects more than one
entry, exits with code `2`.

The output is one `metadata_value` record per value. Its values, their order,
and its `documents_admitted`, `sentences_admitted` and `tokens_admitted` are
those of `summarize --by-metadata`; its `entry` holds the entry's `pattern`,
`group`, `count` and `works`. The `count` equals the `matches` that
`summarize --by-metadata` reports for a pattern that matches only that entry.
As with `--by-metadata`, a document listing several values is counted once
under each value, documents without a value are counted under `value: null`,
and `--where` restricts the documents. It requires `--json` and cannot be
combined with `--top`.

### Exporting sparse document counts

`table --by-document` exports the complete document-by-entry matrix from an
existing table, without matching or tokenizing again. It requires `--json`:

```bash
ling --json table soranoha.collexeme.lingtable --by-document \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx \
  --source-metadata @document.identifier --source-metadata @document.ndc \
  --where '@document.source_edition_year >= 1950'
```

The stream uses `lingrep.table/v1` with these records, in order:

- `run`: `by_document: true`, the corpus's `source_identity`, the table's
  checked analysis/catalog/matcher identities, the clause, and the selected
  source-metadata fields.
- `document`: each admitted table document, including works with no selected
  occurrences. `document_id` is its stable corpus ID; `document` is its
  zero-based corpus ordinal. Records include the source name, admitted
  sentence/token counts and the requested typed `@document` metadata, in the
  same shape as `summarize --by-document`.
- `entry`: the entire table dictionary, including entries with zero occurrences
  under the clause. `entry` is a table-local ID; `pattern` and `group` preserve
  the typed grouping key, including segment lists.
- `cell`: each nonzero count, identified by `document_id` and `entry`. Missing
  cells are zero. Cells follow entry order, then ascending stable document ID.
- `complete`: admitted documents/sentences/tokens, `entries` (dictionary size)
  and `rows` (number of cells).

Document records follow corpus sentence order; entry IDs retain table order
and do not change with the scope. Keep the dictionary and run identities
with the cells; resolve entry IDs through that dictionary before joining
independently written tables. An empty scope still exports the full dictionary;
its document and cell sets are empty. Only documents with at least one
admitted sentence are in the table.

`--top`, `--select`, `--key`, `--entry` and `--by-metadata` are rejected in this
mode. `--source-metadata` is repeatable and accepts only `@document.KEY`.
The export traverses existing sparse cells and bounded metadata pages; it
neither sorts rows nor creates temporary ordering files. Consumers must reject
streams without the final `complete` record or whose process exits with an
error; a truncated stream is not a complete statistical family.

### Comparing two scopes: `ling contrast`

`ling contrast` reads the same count table and ranks each pattern's entries by
how differently two `@document` scopes use them, without scanning the corpus:

```bash
ling --json contrast --table soranoha.collexeme.lingtable --top 100 \
  --where '@document.ndc contains "914"' \
  --versus '@document.ndc contains "913"' \
  --corpus soranoha.lingcorpus --patterns patterns/collexeme.lx
```

Omitting `--where` or `--versus` makes that scope every document, which
compares one scope with the whole corpus. Each row gives, per scope, the
entry's `count`, `works` (the admitted documents it occurs in) and
`per_million`, its count per million admitted tokens. Its `g2` is the two-cell
log-likelihood of UCREL's calculator: for counts `a` and `b` in scopes of `N1`
and `N2` admitted tokens, `E_i = N_i·(a+b)/(N1+N2)` and
`G² = 2 Σ O·ln(O/E)`, where a cell with `O = 0` contributes 0. `min_expected`
is the smaller `E_i`, `log2_ratio` is `log2((a/N1)/(b/N2))` with no smoothing,
and `direction` indicates the scope with the higher rate, or `equal`. G² is an
exploratory score: no p-value or significance label is given. It does not
account for clustering within documents, so compare each side's `works` as well.

Every entry that occurs in either scope is ranked, by descending `g2`, then
descending `a + b`, then `summarize`'s group order, and `--top` keeps the first
rows of that ranking. Every pattern is ranked before the first `pattern`
record, since each record carries its `entries` count, so memory and temporary
storage grow with the number of patterns. A `--top` up to 1,048,576 holds that
many rows per pattern in memory. With `--top all`, a larger `--top`, or JSON
output without `--top`, every pattern's ranking spills to temporary files at
once within `--max-temp-bytes` and retains up to 1,048,576 keys per pattern in
memory.

The two scopes may share documents, as when both match values of a list-valued
key. The `scopes` record gives the documents, sentences, and tokens that each
scope admits and that both admit, and a `relation` of `disjoint`, `overlapping`,
or `identical`. Nothing is subtracted: an overlapping comparison is computed
over both scopes as given, and each row's `shared` gives its count and documents
in the documents that both scopes admit. When a scope admits no tokens, its
`per_million` and the row's `g2`, `min_expected`, `log2_ratio`, and `direction`
are `null`.
Identical scopes give a `g2` of 0 for every entry. The output contract is
`lingrep.contrast/v1`; like `ling table`, `ling contrast` rejects a table
written for another corpus, catalog or matcher, and a clause that references
`@section` or sentence values exits with code `2`.

### Counting authored word lists

Use `lexicon target_words by [text, norm] := {"大学院", "行う"}.` when a term
may span an unknown number of morphological tokens. `summarize` groups by the
authored term. Add `key: [term, segments(text)]` to count observed
segmentations per term.

Query JSONL includes `lexical: {term, matched_by}` and list-valued key fields.
`matches --tsv` rejects lexicon catalogs because its fixed headerless row has
no place for variable-length `matched_by`; use JSON or human output.

### Sets of similar words: `ling neighbors`

`ling neighbors` reads a word-vector file and prints the values whose vectors
are closest to one word, as a `set` definition for a catalog:

```console
$ ling neighbors --vectors vectors.vec --min-similarity 0.7 重要
# 7 values near 重要 (cosine >= 0.7)
set NEIGHBORS := {重要, 大切, 大事, たいせつ, 重大, だいじ, 肝要}.
```

A pattern then tests an attribute against the set: `[unidic.orthbase in
@NEIGHBORS]`. Spell the file's values exactly as they appear in the tested
attribute.

The file is text with one `value x1 ... xn` line per value, with or without a
leading `count dimensions` line. That layout is the word2vec text format, which
fastText `.vec` and chiVe files use. `ling` reads vectors and does not compute
them.

To distribute a pinned table, copy `vectors.vec` and `manifest.json` together
into a versioned directory. Pass that directory to `--vectors`. Use the BLAKE3
of the exact `vectors.vec` bytes (for example, from `b3sum vectors.vec`).
Replace the placeholders below with the identities and recipe for your table.

```json
{
  "schema": "lingrep.word-vectors/v1",
  "blake3": "<64 lowercase hex digits>",
  "provenance": {
    "resource": "my-english-lemmas",
    "attribute": "lemma",
    "vocabulary_sources": ["<lexicon or corpus name, immutable release or digest>"],
    "selection": "<which values were included, with any frequency cutoff>",
    "encoding": "<model, exact revision and vector transformations>"
  }
}
```

The resource label follows the catalog spelling `[a-z][a-z0-9_-]*`.
Use a separate entry in `vocabulary_sources` for each supplemental source.
The CLI and server verify the table against the manifest before using it.
Choose a trusted manifest when installing a bundle. The digest identifies the
table bytes, and the publisher supplies the vocabulary and encoding history.
The attribute records how the values were spelled. An explicit test on another
attribute still compares that attribute's values to the same set of strings.
Keep a bundle immutable while it is in use and install revisions in new
directories. The `neighbors --json` run record includes the table digest and
any bundle provenance.

`--min-similarity` keeps the values at or above a cosine, `--top` keeps a
number of the most similar, and at least one is required. `--name` sets the
set's name. `--json` reports each value with its similarity (`lingrep.neighbors/v1`).
A word without a vector in the file is an error. Read the printed set before
using it: word vectors can place antonyms and frequent function words close to
many words.

A catalog can also state the similarity directly, as
`[unidic.orthbase near 重要 >= 0.7]`, and take the file at run time:

```console
$ ling summarize --vectors vectors.vec --pattern '[unidic.orthbase near 重要 >= 0.7]' corpus/
```

See [Similarity tests](pattern-language-reference.md#similarity-tests).

### Cross-catalog drift

`check --compare-with FILE` reports definitions that share a name across
catalogs but whose bodies differ. The flag is repeatable and requires
`--patterns`, because an inline `--pattern` has no catalog name to report.

```bash
ling check --patterns patterns/sentence-final.lx \
  --compare-with patterns/sentence-final.lx \
  --compare-with patterns/sf-experimental.lx
```

- **Drift is a notice by default.** `check` exits `0` even when every shared
  name diverges, unless `--fail-on-drift` is passed.
- **Name-level.** The report lists each divergent definition and which
  catalogs carry each distinct body. JSON also carries `comparison`: all
  definitions, including one-sided and unchanged definitions, with body-group
  IDs, visibility and reference-expanded, formatted declarations.
- **Reference-closed.** Bodies are compared after resolving references, so a
  definition whose own text is byte-identical across two catalogs still drifts
  when a fragment it references differs.
- **Insensitive to descriptive metadata.** Descriptive metadata and
  publication policy do not participate in body identity. `within_span` and
  `span_dependency` on an emitting pattern do, because they filter its
  matches. They compare as the `where span(...)` clause with the same
  operands, so the two spellings of one constraint do not drift.
- **Order-insensitive where order has no effect.** Attribute order and
  set-member order are normalized away. Alternation branch order is not,
  because it determines capture selection.
- **Visibility is reported, not compared.** A name declared `fragment` in one
  catalog and as an emitting pattern in another, with the same body, is not
  drift.
- **A compared catalog that fails to compile fails the run**, as a failing
  primary catalog does.

JSON output carries a `drift` array, absent when nothing drifts. Human output
appends one `drift:` line per divergent name.

For two revisions saved as files, the comparison script combines that report
with optional behavioral evidence:

```bash
python3 scripts/catalog_diff.py before.lx after.lx --corpus corpus.lingcorpus
python3 scripts/catalog_diff.py before.lx after.lx --examples --dict-path "$LINGREP_DICTIONARY_PATH"
```

The JSON report labels definitions added, removed, changed or unchanged by
reference-closed structural body and visibility; renames appear as removal and
addition. It separately flags changes to metadata on emitting definitions. Expanded
declarations are display text. Body identity follows the drift comparator's
metadata-insensitive rules, so publication policy can change results without
changing a body.

`--corpus` compares two complete scans of the same prepared artifact. `--examples`
uses the union of both catalogs' example/counterexample texts with Vibrato;
zero or malformed claims fail instead of claiming coverage. Behavioral counts
report published records exclusive to each revision. The comparison preserves
multiplicity, captures, graph bindings, lexical identity, keys, tags and character
positions.
Stream sequence and query-dependent token/context projections are excluded from
identity. Both runs' identities, counts and completion evidence accompany the
report. Incomplete or failed scans publish no report. Memory use does not grow
with the corpus. Equal output on one corpus or on the examples does not prove that
the two catalogs are equivalent or linguistically valid.

### Attributes the corpus cannot supply

A catalog may reference an attribute that the bound corpus's token schema does
not carry, such as `[unidic.cform~連用形]` over CoNLL-U or `[dep.rel=nsubj]`
over Vibrato/UniDic. Referencing an unavailable attribute is not an error: the
attribute binds as universally absent, which is per-token absence applied to
every token. Positive tests over it match nothing, `!=`/`!~`/`not in` match
nothing either, and only the true complement `!(…)` holds. A `matches` or
`summarize` run reports each such attribute as a `warning[attribute_unavailable]`
on stderr, or as a `lingrep.diagnostic/v1` warning record under `--json`,
regardless of `--progress`. The warning lists the referencing definitions and
the corpus's token schema.

Binding fails with exit code `2` for two invalid invocations: a dependency `where`
clause requiring `dep.head`/`dep.rel` that the corpus cannot supply, and an
attribute key that the schema carries under a conflicting value kind.

`check --schema SCHEMA` reports the same findings without a corpus, against a
named declared schema: `unidic-morphology`, `sudachi-morphology`, `tokenize`
(`text` and `surface`), or `conllu`.

These values identify schemas, not analyzers. A morphology layout is determined by the
dictionary that supplies the feature columns, so `unidic-morphology` is what
`--analyzer vibrato` produces over any of the packaged UniDic dictionaries. A
catalog can declare both the analyzer and the dictionary with
`analyzer: { family: "vibrato", resource: "unidic-cwj" }`.

```bash
ling check --patterns patterns/ud-dependency.lx --schema conllu
```

`check --json` also reports `capture_key_edits`, one descriptor per field of a
local key that projects `text`, `lemma`, or `norm` from a statically
single-token capture or from a `where` graph node that can bind only words. In
a key with several fields, each eligible field has its own descriptor, and
`field` is its zero-based position in the `key:` list. Each descriptor records its
definition, capture, source file, choices, and UTF-8 byte range of the attribute
in the original source; replacing that range changes only that field. Inherited
keys, span components, whole-match fields, other attributes, enhanced-graph
nodes that can be empty nodes, and edge relations have no descriptor and remain
text-only.

JSON output carries a `binding` object listing each unavailable attribute with
its referencing definition sites; human output prints one line per attribute.
Unavailable attributes never change the exit status; a rejection (a
`where`-bearing catalog against a schema without `dep.*`) is reported in the
`refusal` field and fails the check. `doctor` reports the same finding against
a real corpus; see
[Discovery, explanation, and completion](#discovery-explanation-and-completion).

### Values outside a catalog's label sets

A corpus can carry an attribute under a different annotation scheme from the
one that the catalog's literals use. A catalog that tests ClearNLP's `dobj` over
a UD parse, which writes `obj`, matches nothing, and `attribute_unavailable`
does not report it because the attribute is present. When the catalog declares
its label sets with `labels:` (see
[Label sets](pattern-language-reference.md#label-sets)), a `matches` or
`summarize` run whose corpus has values outside a declared set ends with
`warning[label_outside_declared_set]`. The warning reports the attribute, the
set, and up to eight values with token counts, and does not change the exit
status. The counts cover the sentences that the run reads, which over a prepared
corpus can be only those that the posting index selects.

## Scope conditioning with `--where`

`--where` restricts the sentences presented to matching by document and
sentence metadata, authored divisions, and the section labels active at each
sentence. One clause applies uniformly to the whole catalog; there is no
per-pattern form. It is available on `matches`, `summarize` and `coverage`.

```bash
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --where '@document.subject_area = 社会学 & (@document.year >= 2010 | @document.keywords contains 農学)' \
  corpus/
```

```ebnf
clause    = disjunct , { "&" , disjunct } ;
disjunct  = predicate | [ "not" ] , "within(" , kind , "," , clause , ")" | "(" , clause , { "|" , clause } , ")" ;
predicate = owned-key , op , value
          | key , ( "in" | "not in" ) , "{" , value , { "," , value } , "}"
          | key , ( "contains" | "not contains" ) , value
          | "@section" , ( "=" | "!=" ) , value
          | "@section" , ( "in" | "not in" ) , "{" , value , { "," , value } , "}" ;
op        = "=" | "!=" | "<" | "<=" | ">" | ">=" ;
```

Conjunction is the default and disjunction requires parentheses: `a & b | c`
is a parse error reporting the grouping to supply. Outside `not within`, negation
is per predicate, through `!=`, `not in` and `not contains`. Values are bare
tokens, with `"…"` available for a value containing a delimiter or a space.

### Section predicates

`@section` tests the labels active at a sentence:

```bash
ling matches --patterns patterns/sentence-final.lx --dict-path "$DICT" \
  --where '@section in {methods, results} & @document.year >= 2010' \
  markdown-corpus/
```

| Written | True when |
|---|---|
| `@section = methods` | `methods` is active |
| `@section != methods` | `methods` is not active |
| `@section in {a, b}` | at least one of `a`, `b` is active |
| `@section not in {a, b}` | neither is active |

Ordered comparison and `contains` are rejected: a label is an opaque string with
no order and no parts.

The `@` prefix separates labels from metadata keys. `@` cannot begin an
identifier, so `@document.section = methods` queries a metadata key named
`section`, and `"@section"` in value position is an ordinary value.

A section predicate is two-valued and never falls into the missing-field or
shape-mismatch buckets described below, which apply to metadata values. Unlike
metadata predicates, `@section = x` and `@section != x` are exact complements.

Two different labels are not mutually exclusive. A sentence's active labels
are a set, so a `.results` div opened under a `.methods` heading puts a
sentence in both `@section = methods` and `@section = results`. Counts of two
such registers add up only by inclusion–exclusion. For a partition, state it
in the clause: `@section = methods & @section != results`.

The Workbench of `ling serve` takes the same clause in its "Only where" field
and shows the `ling summarize` and `ling matches` commands for the current
search. In the other direction, `--link NAME=URL` on either command prints the
Workbench link for the catalog, the clause and `--overlaps`, without reading
the corpus:

```bash
ling matches --patterns query.lx --where '@document.genre contains 化学' \
  --link science=http://127.0.0.1:3000/
```

`NAME` is the corpus name with which the server was started. When no server is
running, `--serve` starts one over the command's own corpus, prints the same
link on the line after the listening address, and runs until interrupted:

```bash
ling matches --patterns query.lx --corpus science.lingcorpus \
  --where '@document.genre contains 化学' --serve
```

It listens on a free loopback port; `--serve=HOST:PORT` sets the address.
It also opens the link with the program set in the `BROWSER` environment
variable, or with `xdg-open` (`open` on macOS) when that is unset. When no
such program can be started, it reports that and keeps serving.
For direct text input, `--serve` supports Vibrato morphology with input
segmentation and no normalization. To use other analyzer settings, prepare
the input with `ling corpus build` and pass the artifact with `--corpus`.
`--context` and `--limit` are not carried in either direction.

`--where '@section …'` selects from sentences that have already been read.
`--md-include-sections` and `--md-exclude-sections` determine at read time which
lines become sentences. The default policy excludes `references` and ten
other labels, so `--where '@section = references'` is empty unless
`--md-exclude-sections ""` is passed. A run whose clause references such a label
reports this and identifies the flag. See [the dialect contract](markdown-corpus-dialect.md).

`--md-recover-conversion-artifacts` is also a read-time flag. It admits the
artifacts that a corpus converter emits (presentational HTML, `<si …>` notation,
and div imbalance at the document boundary), which the strict default rejects.
An unmatched bare `$$` line becomes literal text in recovery mode; following
lines remain prose or Markdown structure. Complete math blocks remain excluded. Use
it when a tree fails to build on a few documents out of many; a malformed fence
or an unterminated code block remains an error either way. The flag is part of
`reader_tag`, so a recovered read produces a different prepared artifact than a
strict one over the same tree. `lingrep-web` always reads with recovery, so one
`.md` tree can have two artifacts.

A scoped run over an artifact reads only the token blocks that contain an
admitted sentence. Other blocks are not read, hashed or decompressed, and an
excluded sentence's stored size is not cross-checked against the residency
index. A scoped run is therefore not an integrity check; use
`corpus inspect --verify` for whole-artifact verification. A sentence too large
for `--max-tokenized-bytes` does not fail a run that excludes it.

A `@section` clause over a corpus that records no sections is rejected before
processing: JSONL input, or an artifact whose section coverage is empty. An
empty label set there cannot distinguish "in no labeled section" from "this
format records none". If the corpus records sections but none with the requested
label, the run reports a warning, because one catalog can run over several corpora.

### What each operator accepts

| Operator | Rule | Shape mismatch when |
|---|---|---|
| `=`, `!=` | compares the value's text; a boolean renders `true`/`false` | the value is a list or a nested object |
| `<`, `<=`, `>`, `>=` | parses both operands as decimals, whatever type the value has | the value does not parse as a decimal (`pages: "47-56"`), or is a list or object |
| `in`, `not in` | set membership under the `=` rule | the value is a list or object |
| `contains`, `not contains` | membership in a list-valued key | the value is a scalar or object |

Ordered comparison parses rather than dispatching on type because a stored
numeric value may be a string: `year` can be `"2013"` rather than `2013`. A literal in the clause that is not a decimal (`year >= abc`) is
rejected before the run starts; a stored value that does not parse is a
per-document outcome, below.

### The four buckets

A predicate over a missing field is false in both polarities, so
`pages != "47-56"` is false on every document lacking `pages`. So is a
predicate over a value whose shape does not fit it. These cases are counted
separately, so a scoped run reports four buckets:

```json
"scope": {
  "clause": "year >= 2010",
  "basis": "corpus",
  "documents": {"admitted": 1, "excluded_by_predicate": 1,
                "excluded_missing_field": 1, "excluded_shape_mismatch": 1},
  "sentences": {"admitted": 2, "excluded_by_predicate": 1,
                "excluded_missing_field": 3, "excluded_shape_mismatch": 1}
}
```

Document and sentence counts can diverge. Under a section clause, a document
counts as admitted if its metadata did not exclude it, even if none of its
sentences is admitted. A run may therefore report one admitted document and zero
admitted sentences.

Two complementary-looking scopes do not partition the corpus: `year < 2010` and
`year >= 2010` cannot both hold, yet their admitted counts sum to less than the
whole. The shortfall is the missing-field and shape-mismatch buckets.

An unscoped run omits `scope`.

### What the denominator covers

`basis` identifies the scope of the counts. Only `basis: "corpus"` supports
use as a corpus denominator.

| Path | `coverage` | `basis` | `scope.sentences.admitted` vs `counters.sentences_admitted` |
|---|---|---|---|
| direct | `full` | `corpus` | equal |
| direct | `prefix` | `observed_prefix` | `>=`, and the excess depends on the batching limits in force |
| prepared | `full` | `corpus` | equal |
| prepared | `prefix` | `corpus` | `>=`; the universe is the whole artifact by construction |

A direct run stopped by `--limit` stops with its reader ahead of its matcher:
the reader crosses document boundaries to fill a batch while the run can stop
inside an earlier partition, so its scope counts describe neither the corpus nor
the matched prefix. A prepared run's universe is precomputed from the whole
artifact and does not shrink when the run stops early.

### Ordinals, and what a malformed artifact does

Direct input compacts ordinals: a scope-skipped sentence consumes no sentence
id, as a skipped blank line does. A prepared artifact never renumbers: an
ordinal is artifact identity, so a match identifies the same sentence it would
have identified in an unscoped run.

A prepared document carrying no stored metadata is counted as a missing
field. Stored metadata that does not parse, or that the typing rule rejects,
violates an artifact invariant: the run fails and reports the document. A value of
the wrong shape is counted, not fatal.

## Sampling a corpus with `--sample`

`--sample RATE` on `matches` and `summarize` reads a fraction of the corpus,
at proportionally lower cost. It is off by default.

```bash
# A quarter of one Soranoha author archive (see "Soranoha bulk archives"),
# at the default seed.
ling --json summarize --patterns patterns/sentence-final.lx \
  --dict-path "$DICT" --sample 0.25 \
  --input-format soranoha soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip

# The same rate, a different sample.
ling --json summarize --patterns patterns/sentence-final.lx \
  --dict-path "$DICT" --sample 0.25 --sample-seed 7 \
  --input-format soranoha soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip
```

`RATE` is a decimal greater than 0 and at most 1. `--sample 1.0` is valid and
selects everything; `--sample 0` is rejected because it would admit nothing and
report it as a corpus. Selection uses integer arithmetic and does not depend on
the platform's floating-point rounding.

The sample is a property of the corpus, not of the run. A sentence is admitted
by a hash of `--sample-seed` and its corpus sentence ordinal, so batching,
worker count, scan order and where a `--limit` stopped cannot change it. Two
runs at one seed read the same sentences, so a sampled result can be quoted and
two sampled runs can be compared.

Match and summary counts describe the sample, without extrapolation. The `run`
record declares the rate, seed and sampling unit:

```json
{"type": "run", "data": {"sample": {"rate_per_million": 250000, "seed": 0, "unit": "sentence"}}}
```

An unsampled run omits `sample`.

### Options incompatible with sampling

| Combination | Why |
|---|---|
| `--cache` | The artifact would present a fraction of the corpus as the whole, and every later run against it would be sampled. Sample a direct run, or build the artifact first and sample the query against it. |
| `--tsv` | Neither `lingrep.matches-tsv/v1` nor `lingrep.summarize-tsv/v1` has a run record, so a sampled count would be published as if it were a census. Use JSONL. |
| `--sample-blocks` without `--sample`, `--corpus`, and `--json` | Block sampling requires a declared rate, an existing artifact, and a run record. |
| `--sample-seed` without `--sample` | The seed has no effect without a rate. |

### Where the saving lands

Sentence sampling filters before analysis, so a direct run skips tokenization
of unselected sentences. On prepared artifacts it filters after decoding;
scattered selected sentences can require reading most token blocks.

Use `--json --sample-blocks --sample RATE --corpus ARTIFACT` to draw whole
physical token blocks before decompression. The run declares
`unit: "token_block"` and `token_component_blake3`, the manifest checksum that
identifies the physical block layout. A block is selected by the seed and its
first corpus sentence ordinal in the artifact directory; block sizes can vary.
Worker count, batch limits and scope filters do not move those boundaries.
Reproduction requires the same artifact layout. Scope intersects the selected
blocks; it does not renumber them.

Block sampling is cluster sampling: neighboring sentences are correlated.
Result counts are exact counts over the sample, without extrapolation to the
corpus or an independent-sentence confidence interval. Corpus inventory and
scope population still describe the whole admitted population. `--sample 1`
with `--sample-blocks` reads everything; a small sample can select no blocks
and finish with zero matched sentences. Excluded blocks are not decoded;
`ling corpus inspect --verify` still verifies the complete artifact.

## Direct roots and stdin

Formats are inferred from path extensions. A directory path without an extension
defaults to Markdown and selects its `.md` files. `--input-format plain` selects `.txt` files
instead and reads each as one plain-text document; `conllu` selects `.conllu`
and `tei` selects `.xml`. Extensions match case-insensitively. Directory
traversal is recursive and deterministically ordered. Symlink and hard-link
aliases remain distinct logical documents; targets outside the root are admitted and marked. Preview
that contract with:

```bash
ling corpus inventory --json --progress always corpus/ >sources.jsonl
```

Corpus readers and pattern catalog readers expect UTF-8. Corpus readers
remove one leading byte-order mark. Invalid UTF-8 diagnostics report the
line and byte offset of the first invalid byte. When the bytes suggest
Shift_JIS (CP932), convert the file with `iconv -f CP932 -t UTF-8` or `nkf -w`.

Source events and records use NFC for file, directory and Soranoha member
names. For directory and file inputs, canonically equivalent names contribute
the same spelling to source identity. If two names normalize to the same
spelling, the diagnostic identifies both original names.

The `source` events expose logical names, target groups, and external-target
status. The final event reports alias multiplicity and traversal high-water
values. Cycles, dangling links, invalid names, and exceeded limits fail the
run. The common traversal controls are:

- `--max-traversal-depth`
- `--max-source-entries`
- `--max-directory-entries`
- `--max-documents`
- `--max-source-index-bytes`

Omitting a root is an invocation error. Pass `-` explicitly for stdin. A
completed stdin run has a per-run stream ID and content digest but no reusable
source or analysis identity.

Long sentences normally run automatically in their own batch. Explicit limits
have different failure behavior for three cases:

- **Direct Markdown `matches`.** For direct filesystem roots resolved as
  Markdown, an admitted sentence larger than `--max-sentence-bytes` is skipped.
  The scan continues, and the command exits 1 with diagnostic code
  `oversized_sentences_skipped`. The skipped sentence retains its input ordinal,
  so later match locations contain a visible gap. JSONL and TSV may therefore
  publish later evidence but publish no `complete` record for the degraded run;
  human `--progress always` still prints processed sentence and match totals
  before the terminal diagnostic. Extensionless filesystem roots use Markdown
  inference and receive the same behavior.

- **Other direct input.** Stdin, JSONL, plaintext, and every other command over
  direct input stop at the first sentence over `--max-sentence-bytes`, exit 1
  with `sentence_too_large`, and publish no `complete` record. `summarize`
  publishes no total that omits a sentence without reporting an error.

- **Prepared input.** Both `matches` and `summarize` omit and count a sentence
  over `--max-tokenized-bytes`. The run exits 1 with
  `oversized_sentences_skipped` and no `complete` record. Stopping at the first
  oversized sentence would make an already-built artifact unqueryable at that
  limit.

If a matching batch exceeds its [output allocation budget](fsm-development.md#supported-architecture),
the executor retries smaller groups of sentences and releases each group's results
after publication. If one sentence still exceeds the budget, the run fails with
`match_batch_too_large` and publishes no `complete` record. Narrow the pattern to
produce fewer matches in that sentence. Results are not truncated to fit the budget.

With the default `--progress auto`, deferred human output, such as a
`summarize` table, can show one status line on stderr at the next count update
after one second. Both stdout and stderr must be terminals, and `TERM` must
be set to a value other than `dumb`. During streaming query execution
(`matches`, `tokenize`), and for JSON, TSV or redirected stdout, `auto` shows
no progress. Artifact preparation with `--cache` can show status before
streaming results begin.

`--progress always` shows the first available status immediately. It redraws
in place for deferred human output on capable terminals; otherwise it prints
ordinary lines, at most one intermediate update every five seconds plus the
latest final count if it has not already been printed. `never` disables
progress. Warnings, errors, creation receipts and requested `--trace-policy`
reports remain visible in every mode. A failed status erasure before
warnings or results is ignored; a failed progress write does not fail the run.
Progress does not change stdout or identity. Select JSON diagnostics with
`--json`.

Direct commands, `ling serve` and `lingrep-web` resolve their dictionary from
an explicit `--dict-path` first, then from `LINGREP_DICTIONARY_PATH`, then from
the `cwj` dictionary that `ling dictionary install` installs. If none is
present, invocation fails with exit status 2 before writing to stdout.
CLI queries over prepared corpora do not consult that environment variable.
`ling serve` and `lingrep-web` require a dictionary only when a declared
corpus is a source, which they analyze into a prepared artifact. When every
declared corpus is a prepared artifact and no dictionary resolves, they start
without one, and the Workbench sample test reports that a dictionary is
required.

### CoNLL-U and UD treebanks

A `.conllu` file is pretokenized input. `ling` uses its integer-ID word
rows directly and does not invoke the selected analyzer or require a
dictionary, so Universal Dependencies treebank files are directly queryable:

```bash
ling matches \
  --pattern 'pattern root := [dep.head=0 & dep.rel=root & upos=VERB].' \
  ja_gsd-ud-test.conllu
```

An entire treebank directory is accepted with an explicit format. Discovery is
recursive, bounded, and deterministic; it selects `.conllu` files
case-insensitively and ignores README and license sidecars:

```bash
ling matches \
  --input-format conllu \
  --pattern 'pattern root := [dep.head=0 & dep.rel=root].' \
  UD_Japanese-GSD/
```

The portable columns are `surface` (FORM as written), `text` (the folded key
of FORM that `text` literals compare with), `lemma`, `upos`, and `xpos`. The
reader does not normalize FORM, and `--normalization` does not apply to it.
Dependencies are `dep.head`, `dep.rel`, and the string-set `dep.enhanced`.
Known UD features are string-set `feat.*` attributes; all original FEATS and
MISC entries are additionally retained in `conllu.feats` and `conllu.misc`.

A dependency clause may classify a captured head by an identity-free dependent
predicate. The following patterns each emit the one-token verb once, even when
several objects satisfy the test:

```pattern
key: [$verb.lemma]
pattern pronominal_object := (?<verb> [upos=VERB])
  where $verb -[obj|obj:*]-> [upos=PRON].

key: [$verb.lemma]
pattern non_pronominal_object := (?<verb> [upos=VERB])
  where $verb -[obj|obj:*]-> _,
        not $verb -[obj|obj:*]-> [upos=PRON].
```

The dependent bracket uses the ordinary Boolean token-test language, including
named tests and sets. A negative predicate succeeds only when no compatible
dependent satisfies it and all compatible dependents supply every referenced
predicate attribute. A missing required value fails the negative constraint;
it never proves absence.

Enhanced dependencies are graph constraints rather than the word-level
`dep.enhanced` predicate. Either endpoint may introduce a named graph node;
`as $name` optionally makes the exact matched edge observable:

```pattern
key: [$predicate.node.id, $role.rel, $argument.lemma]
pattern elided_argument :=
  (?<argument> [upos in {NOUN, PROPN, PRON}])
  where (?<predicate> [node.kind=empty])
          -[enhanced nsubj|obj|obl:* as $role]-> $argument.
```

The body must still consume a surface token, and each connected enhanced
component must be anchored by an exact-width-one surface capture. A graph node
is a tagged word or decimal empty-node identity, not a token span. Each
complete assignment emits one match; a named parallel edge emits separately,
while an unnamed parallel edge is existential. Node keys can project
`node.kind`, `node.id`, `text`, `lemma`, `upos`, and `xpos`. A binding proven to
select only words by a positive basic edge or `node.kind=word` in every
predicate branch can also project scalar token attributes such as `$v.norm`.
A named edge projects `$edge.rel`. The match JSONL `graph_bindings` field preserves these
assignments in canonical node-name then edge-name order, and the browser hit
inspector displays them. Complete DEPS observation is required, so a
morphology-only prepared corpus is rejected at binding and unavailable or
incomplete CoNLL-U DEPS never produces a match.

An exact anti-edge may connect endpoints bound by the body or positive graph
constraints, or a derived basic-head endpoint described below:

```pattern
pattern private := (?<second> []) (?<private_argument> [])
  where not $second -[enhanced obj|obl:*]-> $private_argument.
```

It holds only when no matching edge connects those node identities. It creates
no graph binding and is checked after positive assignments, so constraint order
does not alter the result. `as`, endpoint binders, `_`, and predicate targets
are positive-only. `patterns/ud-enhanced-argument.lx` is the introductory
example; `patterns/ud-enhanced-propagation.lx` shows multi-edge and negative
queries.

When the endpoint is the basic HEAD of an already captured dependent, derive
it without introducing another graph binding:

```pattern
pattern missing_basic_endpoint := (?<dependent> [dep.head!=0])
  where not basic_head($dependent) -[enhanced *]-> $dependent.
```

`basic_head($dependent)` requires an exact-one-token surface capture and a
usable non-root scalar `dep.head`. It may appear on either side of a positive
or negative enhanced relation. It publishes no node identity; missing,
malformed, root, and out-of-range heads produce no endpoint. The
`patterns/ud-enhanced-consistency.lx` catalog uses this form to publish audit
candidates and representation motifs, not confirmed annotation errors.

Integer IDs are not duplicated as an attribute: matcher token index plus one
is the same identity. Multiword-token and empty-node rows do not become
matcher tokens. `corpus build` and `--cache` preserve the same bound tokens.

The reader also consumes the two UD identity comments. `# newdoc id = X`
starts a new document whose metadata carries `newdoc_id`; a file with no
`# newdoc` marker remains one document, and a bare `# newdoc` starts an
unnamed one. `# sent_id = X` becomes the sentence's own `sent_id`, a
sentence-grain metadata key: `--where '@sentence.sent_id = train-s42'` selects
exactly the cited sentence, and the run's scope accounting reports the
selection at sentence grain, as a `@section` clause does. The clause behaves
the same over a file without these comments (every sentence is a missing-field
exclusion) and over the corresponding prepared artifact. Every other comment
(`# parallel_id`, producer annotations) is ignored. Use
`--json matches --source-metadata @sentence.sent_id` to publish it with each
match.

### Soranoha bulk archives

Soranoha publishes its corpus as ZIP archives: the whole corpus, one author,
one NDC class or subclass, and the children's material. Each archive carries
one plaintext or TEI file per work and `catalog.csv` at its root. Use
`--input-format soranoha` for plaintext or `--input-format soranoha-tei` for TEI.
ZIP input requires an explicit format:

```bash
ling matches \
  --dict-path "$DICT" \
  --patterns patterns/sentence-final.lx \
  --input-format soranoha \
  --where '@document.author = "芥川 竜之介" & @document.ndc contains "913"' \
  --source-metadata @document.identifier \
  --source-metadata @document.source_content_hash \
  --source-metadata @document.release \
  soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip
```

`--where` reads the `catalog.csv` fields whether or not anything is projected; the
`--source-metadata` flags only publish those three citation fields in each
match record, and none of them is required to search or summarize the archive.

The archive is one discovered source and each work is a document; document
order is byte-sorted by member name. `catalog.csv` joins a work by `filename`,
and every column becomes document metadata under `@document.`:

| kind | columns |
|---|---|
| citation | `identifier`, `source_content_hash`, `release` |
| bibliographic | `title`, `subtitle`, `title_reading`, `author`, `orthographic_style`, `ndc` (list), `first_published`, `source_edition_title`, `source_edition_publisher`, `source_edition_year`, `url`, `doi` |
| size and locator | `body_character_count` (number), `filename` |

`body_character_count` and `source_edition_year` are numbers, so ordered
comparisons work on them. `ndc` is a list of NDC class codes: `NDC 712 914`
becomes `["712", "914"]` and `NDC K913` becomes `["K913"]`, so
`@document.ndc contains "914"` selects one class. A code keeps its leading
zeros and its `K` (children's book) prefix, so `914` and `K914` are distinct
classes, and a code repeated within one value is stored once. Every other
column is a string, `identifier` included, so its leading zeros survive. An
empty field is absent rather than an empty string: `@document.subtitle != …`
reports a missing field, not an empty one. A column outside the pinned set is
carried as string metadata. A missing pinned column, a row without
`identifier`, `filename` or `release`, an `ndc` value of another form, a
duplicate filename or identifier, and rows with conflicting release values are
rejected with the row identified.

The text is the plaintext projection: ruby readings and apparatus are absent,
Aozora Bunko line breaks and source indentation are preserved, and sentences
are framed with the same rule as `plain`, so the same member read as `plain`
admits the same sentence texts. Paragraph structure is reported unavailable.
The TEI reader produces the same principal text and sentence boundaries as the
paired Soranoha plaintext edition. It reads ruby bases and the single `lem` in
an apparatus. Within `choice`, it reads all children of the first available type
in `corr`, `reg`, `expan`, `sic`, `orig`, `abbr` priority order. Notes, running
headers and figure descriptions are excluded. Line/page/column breaks become line breaks, empty gaiji become U+FFFC,
and source indentation becomes U+3000. With the same analyzer configuration,
ordinary tokenization and match/capture offsets follow the plaintext reading.

TEI input also supplies four string-set token attributes:

| Attribute | Values |
|---|---|
| `tei.ruby` | Readings attached to the selected ruby base |
| `tei.element` | Enclosing TEI element names, such as `hi`, `head` and `p` |
| `tei.rend` | Whitespace-separated `rend` values |
| `tei.source` | Literal source references from `source` attributes |

For example, `p := [tei.ruby has ねこ].` searches readings and
`p := [tei.rend has bouten].` searches emphasis. A value applies only when its
range contains the complete analyzer surface token. Ruby readings are shared
whole-base annotations, not inferred pronunciations of individual morphemes.
Markup never forces a token boundary. Prepared artifacts preserve these attributes.
Character offsets address sentence text; source references retain their XML values.
If NFC normalization rejects an annotation boundary that splits a combining
sequence, use `--normalization none`.

`--input-format tei` reads an independent TEI P5 XML file or a directory of `.xml`
files, including names with an uppercase extension. It requires the TEI namespace
and one principal `text/body`; grouped texts and ambiguous apparatus or ruby targets
produce errors. Generic TEI separates `p`, `head`, `l`, `ab` and `item` blocks at
both boundaries. Soranoha vocabulary versions 1–3 use Soranoha's block-end policy
to preserve its plaintext projection. The first header title and author become
`title` and `author` document metadata; language identifiers and license targets
become the arrays `tei.languages` and `tei.licences`. Soranoha catalog values take
precedence. The reader does not validate a complete TEI customization or fetch
schemas, entities or linked documents. DTD declarations and foreign elements in
the selected body produce errors. RelaxNG validation can check a corpus's
structure separately; the reading policy determines which text is searched.

For plaintext, `--max-record-bytes` (default 1 MiB) bounds each physical line;
each inflated work has a separate 64 MiB memory guard. For TEI, the record limit
bounds each sentence's text and annotations. These defaults support larger
books when each record and the inflated work fit their respective limits:

```bash
ling corpus build \
  --dict-path "$DICT" \
  --input-format soranoha \
  --output soranoha.lingcorpus \
  soranoha-plaintext.zip
```

`lingrep-web` does not read the archive; declare the prepared artifact
instead. The artifact carries the same `@document.*` metadata, so queries and
`--source-metadata` behave the same over either.

### Structured JSONL sentences

A JSONL document's `sentences` array, including one inside a paragraph, accepts
strings and objects with a string `text` and metadata:

```json
{"metadata":{"video_id":"v1","speaker":"A"},"sentences":[{"text":"猫がいる。","metadata":{"cue_index":0,"start_ms":0,"speaker":"B"}},"犬もいる。"]}
```

Sentence and document metadata are independent. In this example,
`@sentence.speaker = B` selects the first sentence; `@document.speaker = A`
selects both. `@sentence.speaker = A` selects neither. Objects may put metadata
keys beside `text`; repeating a key inside `metadata` is rejected. Each metadata
record accepts at most 256 fields and 1 MiB of encoded JSON.

Authored structural metadata uses explicit whole-sentence ranges:

```json
{"sentences":["猫。","犬。"],"structure":{"observed_kinds":["part","section"],"divisions":[{"id":"p1","kind":"part","start":0,"end":2,"metadata":{"title":"Introduction"}},{"id":"s1","kind":"section","parent":"p1","start":0,"end":1,"metadata":{"role":"methods"}}]}}
```

Ranges are half-open in source-sentence coordinates. Parent IDs must reference
an earlier division in the same document whose range contains the child. Empty,
adjacent, nested and overlapping divisions retain distinct identities.
`observed_kinds` can declare an observed empty inventory. A division's kind
also declares observation for the whole document. No structure is inferred
from punctuation, labels or equal metadata. Markdown observes paragraphs,
heading sections and fenced regions; plain text and CoNLL-U paragraph structure
is unavailable. Boundaries inside a sentence are unsupported.

`within(section, role=methods & reviewer=A)` requires one containing section
to supply both fields. `within(section, role=methods) & within(section,
reviewer=A)` may use two different sections. `not within` preserves unknown
missing/ill-shaped evidence; it cannot turn unavailable structure into absence.
Unavailable kinds are rejected with an explanation. Nested `within` is unsupported.
A token pattern matches inside one sentence; only a `within=` lookaround
reads neighboring sentences. The structure budget is 256 active
divisions, 64 parent levels, 256 observed kinds per document and 16 MiB of
active division metadata; derived sentence labels have a separate 16 MiB
byte cap. Prepared storage retains metadata once per division. Sequential
metadata pages are capped at 4 MiB; random provenance/projection pages at
64 MiB of source metadata and 64 MiB of unique division metadata. An oversized
requested page is rejected rather than dropping fields or entities.

### Hugging Face datasets

Hugging Face is not a native `ling` source. Use the locked
[`huggingface_dataset.py`](../scripts/huggingface_dataset.py) adapter. It
delegates Hub formats, authentication, revisions, and streaming to the Python
`datasets` package. Its default JSONL projection selects one UTF-8 field and
emits one document per dataset row. `ling` applies its own Japanese sentence
framer to each document's `text` field. In native JSONL, `text` is document
content only when `sentences` and `paragraphs` are absent; a structured
document may still carry a metadata field named `text`.

The explicit CoNLL-U projection serves datasets with the canonical Universal
Dependencies parallel columns. It validates that schema and delegates
serialization, including comments, multiword tokens, and empty nodes, to the
UD encoder. The supplied words and dependency analysis therefore reach the
pretokenized reader without another analyzer pass:

```bash
scripts/huggingface_dataset.py \
  universal-dependencies/universal_dependencies \
  --config ja_gsd --split train --revision 2.18 \
  --output-format conllu |
  ling matches --input-format conllu --patterns patterns.lx -
```

`--text-field` and `--metadata-field` are JSONL-only and are rejected in
CoNLL-U mode. Any Hub dataset with the same canonical columns passes the same
validation.

Inspect the repository before choosing fields. `--inspect` emits JSON and reads
no dataset rows. When a repository has several configurations, the first
command lists them and the focused command adds split names and nested feature
types:

```bash
scripts/huggingface_dataset.py OWNER/DATASET --inspect
scripts/huggingface_dataset.py OWNER/DATASET --inspect --config CONFIG
```

```bash
scripts/huggingface_dataset.py hotchpotch/fineweb-2-edu-japanese \
  --config default --split train --text-field text --limit 1000 |
  ling matches --input-format jsonl --patterns patterns/sentence-final.lx -
```

Dot-separated fields select nested values. Only repeatable
`--metadata-field` selections are retained; other dataset columns are not
copied into the corpus. Piping is the one-pass path and cannot use `--cache`,
because stdin has no reopenable identity. For repeated queries, materialize a
source and then use the ordinary cache contract:

```bash
scripts/huggingface_dataset.py Helsinki-NLP/opus-100 \
  --config en-ja --split train --text-field translation.ja \
  --metadata-field translation.en --revision COMMIT \
  --output opus-en-ja.jsonl
ling summarize --cache --patterns patterns/sentence-final.lx opus-en-ja.jsonl
```

`--output` publishes either format atomically and does not replace an
existing file. Pin a commit with `--revision` when the materialized bytes must
be reproducible; an unpinned direct stream follows the repository's current
default revision. On NixOS, invoke the adapter inside `nix develop`; the shell
supplies the C++ runtime search path required by the locked Arrow/NumPy wheels.

## Choose an analyzer

Direct `matches`, `summarize`, `tokenize`, and `coverage` runs, plus
`corpus build`, accept three analyzer families. Vibrato is the default.
Direct `explore` does not expose alternate analyzer selection.

The common analysis choices are:

| Option | Values | Meaning |
| --- | --- | --- |
| `--normalization` | `nfc`, `none` | Normalize input before analysis; default `none` |
| `--analyzer` | `vibrato`, `sudachi`, `external` | Select the token producer; default `vibrato` |
| `--profile` | `morphology`, `text` | Default `morphology`. For Vibrato and Sudachi, `text` retains only the `text` and `surface` attributes. |
| `--segmentation` | `input`, `analyzer` | Preserve input sentence units or accept analyzer sentence boundaries; default `input` |

Every analyzer reads a folded view of the source (variation selectors removed,
half-width katakana widened, U+FF5E mapped to U+301C, and canonical composition
applied), and `ling` maps the resulting token boundaries back to the source.
Each token publishes the exact source text it covers as `surface` and that
surface's folded key as `text`. Vibrato and Sudachi keep a compatibility
ideograph inside a word their dictionary lists with it, such as 臭氣 with
U+FA5C; external analyzers receive the unified ideograph, which NFC also
produces.

These choices are semantic. A prepared artifact records them, and changing one
selects a different artifact rather than reinterpreting stored tokens. Prepared
queries reject analyzer and dictionary options because their tokens are already
fixed.

### Use Vibrato with UniDic

Vibrato, the default analyzer, loads a UniDic dictionary. Install the default
`cwj` once, or install another edition and pass its path to `--dict-path`:

```bash
ling dictionary install
ling tokenize corpus.txt

ling dictionary install kindai-bungo
ling tokenize \
  --dict-path ~/.local/share/lingrep/dictionaries/unidic-kindai-bungo-202512.dic \
  corpus.txt
```

Choose the UniDic edition for the text domain; see
[`DICTIONARIES.md`](../DICTIONARIES.md). `--dict-path` is invalid with the other
analyzers.

### Use native Sudachi

Sudachi requires a SudachiDict dictionary file:

```bash
ling tokenize --analyzer sudachi --sudachi-dict system_core.dic corpus.txt
```

Without further options, `ling` uses:

- split mode `c`; `--sudachi-split a` or `b` gives shorter units;
- the edition inferred from a `system_small.dic`, `system_core.dic`, or
  `system_full.dic` file name; for a renamed file, pass `--sudachi-edition`;
- the `char.def` built into `ling`; `--sudachi-resources DIRECTORY` replaces
  it. `ling` loads Sudachi without its input-text plugins, so `rewrite.def`
  and `sudachi.json` do not affect analysis; the folded view takes their
  place for half-width katakana and decomposed kana.

The split mode and edition are part of the analysis identity. Mode A also
supplies sentence spans of kinds `sudachi.b` and `sudachi.c`. Mode B supplies
`sudachi.c` spans. For example, SudachiDict core analyzes 国家公務員 in mode A
as 国家, 公務, 員. Its B spans cover 国家 and 公務員, and its C span covers
国家公務員.

The dictionary must be in the `SudachiBinaryDic` container, which SudachiDict
publishes from v20261015 on its
[GitHub releases page](https://github.com/WorksApplications/SudachiDict/releases).
Earlier releases, and the 20261015 archive on SudachiDict's CloudFront mirror,
use the earlier version-header container, which `ling` rejects with an error
identifying the container.

Inside `nix develop`, `$LINGREP_SUDACHI_DICT_SMALL`, `$LINGREP_SUDACHI_DICT_CORE`,
and `$LINGREP_SUDACHI_DICT_FULL` point to the pinned dictionaries. Outside it,
build one:

```bash
sudachi_dictionary="$(nix build .#sudachidict-core \
  --no-link --print-out-paths)/share/sudachi/system_core.dic"
```

### Inspect unexplained sentence endings

`ling matches --ending-report endings.json --patterns patterns/sentence-final.lx ROOT`
scans the full selected scope and writes exact groups of trailing surface-token
sequences, including sentences with no published match. It ignores trailing
`upos=PUNCT` tokens and uses the rightmost published match end; a match reaching
the substantive end covers the sentence. Each group records occurrence and
distinct-document counts and the first three examples with all sentence tokens,
half-open token boundaries and preceding matches. Totals count observed sentences
and documents in the selected scope. Token sequences do not preserve source spacing.

The report describes gaps in published coverage: policy-suppressed matches can
also cause them, so a gap does not prove a missing grammatical construction.
Prepared queries decode unmatched sentences too. The report fails above 4096
groups or a 32 MiB retained-data charge; narrow the scope when that happens.
It is published only after a completed scan, never replaces a file, and cannot
be combined with `--limit`, `--retain` or `--raw-matches`. On a filesystem
without no-replace rename, `ling` publishes the report through a hard link. With
neither, it creates the report exclusively and writes it in place, so an
interrupted write can leave a partial report that does not parse as JSON. If
that creation fails too, the command fails; the error reports the OS reason and
ends with "choose an output directory on a filesystem that supports one of them".

### Use an external protocol adapter

Select an external analyzer with one reusable TOML file. It applies to
`tokenize`, `matches`, `summarize`, `coverage`, `check --verify-examples`, and
`corpus build`; do not combine it with `--analyzer`, `--dict-path`, or other
analyzer-specific flags.

```toml
version = 1
kind = "spacy"
python = "/path/to/pinned/environment/bin/python"
entrypoint = "/path/to/spacy-adapter/bin/lingrep-spacy-adapter"
model = "/path/to/sealed/en_core_web_sm"
backend = "cpu"
```

With `uv` installed, prepare a CPU analyzer in a directory you choose:

```bash
ling analyzer prepare ginza --output ./analyzers/ginza

ling analyzer prepare stanza --output ./analyzers/stanza-fr \
  --language fr --package tokenize=gsd --package mwt=gsd \
  --package pos=gsd_nocharlm --package lemma=gsd_nocharlm \
  --probe-text 'Je mange du pain.'

ling matches --analyzer-config ./analyzers/ginza/analyzer.toml \
  --patterns constructions.lx transcripts/
```

Preparation downloads the release's adapter wheels, checks their SHA-256
checksums, and installs Python 3.14.9, dependencies and the selected model under
`--output`. Package versions come from the release locks, with hashes
checked during installation. Keep that directory at its original path. Deleting
download caches does not break the analyzer. Use an absolute `--analyzer-config` path when
querying from another directory.

A tokenizer-only Stanza bundle uses `--package tokenize=gsd --package mwt=gsd`
for French GSD. Add `pos` and `lemma` together to enable morphology.
Preparation reports a missing `mwt` package if the selected tokenizer loads one.
For a language without one, such as Japanese, omit it:

```bash
ling analyzer prepare stanza --output ./analyzers/stanza-ja \
  --language ja --package tokenize=gsd \
  --package pos=gsd_nocharlm --package lemma=gsd_nocharlm \
  --probe-text '猫が走る。'
```

Without `mwt`, each Stanza token is one match token.

Repeating the same preparation checks and reuses the directory. Use a new output
directory to change the model, packages or release. Upgrading `ling` alone leaves
existing bundles unchanged. Queries read the explicit configuration without
installing or updating anything; prepared-corpus queries do not require Python.

For wheels downloaded separately, pass `--wheels DIR` with the matching release's
wheels and `SHA256SUMS`. The v0.5.0 release has no adapter wheels; upgrade `ling`
to use this preparation command. The standalone `lingrep-spacy-setup` and
`lingrep-stanza-setup` wheel entry points remain available for custom model and
GPU setup, with a caller-managed Python environment.

The spaCy adapter wheel's `electra` and `bert-large` extras use the Transformers
version constraint from spacy-transformers 1.4 (`transformers<4.53.3`). The
project's `uv.lock` uses an override for `transformers==4.57.6`; that override is
not part of the wheel metadata. To apply the same override when installing a
wheel, add `--override overrides.txt` to `uv pip install`, with
`transformers==4.57.6` in `overrides.txt`.

`ling analyzer prepare ginza` installs GiNZA 5.3.1 with `ja_ginza` 5.3.0, whose
weights are inside the wheel. GiNZA's transformer models, `ja_ginza_electra`
5.3.1 and `ja_ginza_bert_large` 5.3.0 (the adapter's `electra` and `bert-large`
extras), ship without weights. For them, `lingrep-spacy-setup` downloads the
weights and tokenizer from the Hugging Face Hub once, at the commit lingrep pins
for each repository, into a temporary directory it removes afterwards, and saves
them in the model directory: about 0.4 GB for Electra and 1.3 GB for BERT large.
`model/lingrep-hub-sources.json` records the repository and commit. Setup
therefore requires network access, and `--offline` rejects these models. Analysis
reads only the saved files and disables Hugging Face Hub access, so the analyzer
runs offline, and the resource manifest digest changes when the weights change.
Bundles that earlier releases set up from these models fetch the weights from
the Hub, at whatever commit it serves, each time the analyzer starts; set them up
again into a new directory.

A GiNZA profile selection that runs GiNZA's `bunsetu_recognizer` adds the scalar
`spacy.ginza.bunsetu`: `B` on the first token of each bunsetu (文節) and `I` on
the others, the label `ginza -f conllu` prints as `BunsetuBILabel`. Write
`[spacy.ginza.bunsetu=B]` to match bunsetu-initial tokens.

The same profile supplies a sentence span of kind `bunsetsu` for each group.
GiNZA's head token carries `spacy.ginza.bunsetuphrase`. Its value supplies the
span's phrase label, such as `NP` or `VP`. When GiNZA starts a bunsetsu on a
whitespace token, the next retained word carries the boundary. With CoNLL-U
input, a complete `BunsetuBILabel` sequence in MISC supplies the
intervals, and a complete `LUWBILabel` sequence supplies spans of kind `luw`.
When the bunsetsu governors that follow from the heads and HEAD values of a
sentence form a cycle, its `bunsetsu` spans have no governors, so
`span(bunsetsu, $a -> $b)` matches nothing in that sentence.
Packaged Vibrato/UniDic dictionaries produce short-unit tokens. A corpus with
annotated long units can supply them through CoNLL-U.

Span IDs start at zero in each sentence. Each interval uses matcher token
indices and excludes its end. A span may contain spans of another kind or
overlap another annotation. A head is a token index inside the interval.
A governor is a span ID of the same kind. A missing governor means there is
no observed outgoing dependency. Prepared corpora retain these annotations
when a query requests a subset of the token attributes. Rebuild older
corpora from annotated input to query the new layer. See the
[sentence span pattern examples](pattern-language-reference.md#sentence-spans).

A whole bunsetu is a `B` token, the `I` tokens after it, and no further `I`
token. These definitions match each bunsetu, each bunsetu that ends in `が`,
and each bunsetu that starts with a verb, capturing the verb as `head`:

```pattern
fragment BUNSETU := [spacy.ginza.bunsetu=B] [spacy.ginza.bunsetu=I]* not_followed_by([spacy.ginza.bunsetu=I]).
pattern bunsetu := @BUNSETU.
pattern ga_bunsetu := [spacy.ginza.bunsetu=B] [spacy.ginza.bunsetu=I]* [text=が & spacy.ginza.bunsetu=I] not_followed_by([spacy.ginza.bunsetu=I]).
pattern verb_bunsetu := (?<head> [spacy.ginza.bunsetu=B & upos=VERB]) [spacy.ginza.bunsetu=I]* not_followed_by([spacy.ginza.bunsetu=I]).
```

In `猫が庭で寝ている。`, which GiNZA divides into `猫が`, `庭で` and `寝ている。`,
`bunsetu` matches all three, `ga_bunsetu` matches `猫が`, and `verb_bunsetu`
matches `寝ている。` with `head` on `寝`.

Setup records which pipeline components each profile selection runs: every
component up to the last one that assigns lemma, UPOS, XPOS, morphological
features or dependencies (`morphology/*`) or sentence boundaries
(`*/analyzer`). Components can read earlier components' output without
declaring it, so none before that point is left out. When the declared
assignments do not cover lemma, UPOS and XPOS, as with `en_core_web_sm`, whose
`attribute_ruler` declares none, `morphology/*` runs the whole pipeline.
GiNZA's `morphology/*` selections run its whole pipeline and
`tokenize/analyzer` runs `tok2vec` or `transformer` and `parser`. With `--segmentation analyzer`, the dependencies,
UPOS and bunsetu labels equal `ginza -f conllu`. With `--segmentation input`,
the parser runs over each input unit and still splits it where it would split
a document, so a unit holding two parser sentences has two tokens with
`dep.head` `0`, and its heads count from the unit's first token.

Setup writes a runtime receipt, `lingrep-runtime.json`, beside the model. It
records the Python interpreter, the installed package versions and a digest of
the staged adapter and protocol code. The adapter checks the running Python and
package versions against the receipt, and `ling` rejects a bundle whose adapter
or protocol code differs from the recorded digest. The model directory's
`lingrep-runtime-identity.json` holds the Python release and the package
versions, without the interpreter's build string, the interpreter path,
`thinc-apple-ops` or lingrep's own adapter packages, and is hashed with the
model. Third-party library binaries are not hashed.

The analyzer identity covers the model files, `lingrep-runtime-identity.json`,
the profile selection and the adapter's semantics revision, which lingrep
replaces only when the adapter's token output changes. It does not cover the
adapter code's bytes, its `pyproject.toml` or `uv.lock`, or the lingrep version.
A lingrep release that leaves adapter output unchanged therefore prepares the
same identity, while a different version of any other resolved package,
including a patch release of GiNZA, spaCy, SudachiPy or the model, prepares a
new one. Managed analyzer identity does not depend on the output directory, and
an x86_64 or aarch64 Linux or macOS host that installs the same Python release
and package versions prepares the same identity.

Relative paths resolve from the TOML file's directory. The model must be sealed
by setup or the repository adapter's `--prepare` command; its profile declares the
language, schema, pipeline and sentence-boundary policy. For Stanza, set
`kind = "stanza"` and point `model` at the sealed Stanza resource directory.
The adapter entrypoint remains inside its project `bin/` directory beside the
tracked source, `pyproject.toml` and `uv.lock`. Verification rejects incomplete
file closures, nested symlinks and generated Python caches in a bundle.

For the optional English CPU bundle, Nix supplies the runtime, existing adapter,
sealed model and ready-to-use file:

```bash
analyzer="$(nix build .#spacy-analyzer-en-core-web-sm \
  --no-link --print-out-paths)/share/lingrep/english.toml"

ling tokenize --analyzer-config "$analyzer" transcripts/
ling matches --analyzer-config "$analyzer" --patterns constructions.lx transcripts/
ling corpus build --analyzer-config "$analyzer" --output transcripts.corpus transcripts/
ling matches --corpus transcripts.corpus --patterns constructions.lx
```

The last command uses the prepared tokens and recorded analyzer identity without
requiring the adapter, Python or model. For direct analysis, the runtime, adapter
and model must already exist. An explicit config selects executable code;
use a file and adapter you trust. The file is read only when direct analysis is
required; prepared queries reject analysis options.

One external process owns each run's bounded analysis batches; `--workers` still
controls matching concurrency without loading one model per worker. Input errors,
invalid handshakes, adapter failures and failed shutdowns terminate the command
without a successful completion record. SIGINT and SIGTERM cancel the adapter
process group as well as the query.

The English bundle uses Explosion's MIT-licensed `en_core_web_sm` 3.8.0, pinned
by URL and SHA-256 in `flake.nix`, and does not change the Japanese defaults.
The sealed profile declares text, lemma, norm, UPOS, XPOS, morphological features
and dependencies. The adapter reads the folded view of each input, and `ling`
maps its token offsets back to the source: `surface` is the exact source text
and `text` its folded key. A word inside a multiword token, such as French
`du` analyzed as `de le`, takes the adapter's word text as `surface`. Segmentation defaults to `input`,
retaining the caller's sentence boundaries; `--segmentation analyzer` uses the
model's parser boundaries. Dependency labels use spaCy's model scheme (for
example `dobj`), not a conversion to UD labels. The model is trained for
[written web English](https://spacy.io/models/en#en_core_web_sm); integration
tests do not establish accuracy on spontaneous speech or ASR transcripts.

Custom adapters use `kind = "protocol"`, an explicit `language`, a `schema` array
of `{ key, kind }` entries, and optional command-line arguments in `arguments`.
The optional `implementation` and `resources` arrays contain file paths.
Adapters must implement the bounded
`lingrep.external-morphology/v1` protocol. The adapter receives the folded
view of each input and returns token ranges in it; when its schema has `text`,
`ling` publishes `surface` and the folded `text` as it does for spaCy, so the
schema must not declare `surface` itself. `entrypoint` is included in the
implementation manifest; the other paths declare its additional file closure.
The generic adapter supports input segmentation. Known spaCy/Stanza adapters
obtain these declarations from their sealed projects and models instead.

#### Analyzer storage and cleanup

Each `ling analyzer prepare --output DIR` directory contains its own Python
runtime, dependencies and model. Keep source corpora and prepared artifacts
outside it. Temporary model-download files are removed after preparation.
To remove the GiNZA analyzer from the example:

```bash
rm -rf ./analyzers/ginza
```

Shared downloads are stored separately in uv's cache. Show its location and
clear it with:

```bash
uv cache dir
uv cache clean
```

Cache cleanup affects uv's shared cache for all projects, but does not break
analyzers created by `ling analyzer prepare`. Prepared corpora remain queryable
after their analyzer directory is deleted.

## Reusing and preparing a corpus from a query

`matches`, `summarize`, `tokenize`, and `coverage` analyze a filesystem ROOT
directly by default. Add `--cache` to keep a reusable artifact on disk:

```bash
# Direct: reads no neighboring artifact and writes none.
ling summarize --patterns patterns/sentence-final.lx corpus.jsonl

# Cold cached: prepares the reusable artifact, then answers from it.
ling summarize --patterns patterns/sentence-final.lx --cache corpus.jsonl

# Warm cached: finds the same artifact.
ling summarize --patterns patterns/sentence-final.lx --cache corpus.jsonl
```

The cached artifact is larger than the source, but it serves repeated queries
without a preparation command or artifact path. Omit `--cache` for a one-off
run or when writing beside ROOT is undesirable.

Reuse first observes ROOT as a build would: it reads and parses the bounded input
stream and BLAKE3-hashes each logical source's raw bytes. Only after deriving that
exact source identity does it scan the immediate directory for artifact siblings
and read their bounded manifests; the sibling scan hashes neither the artifacts nor
their token components. The direct default avoids this identity-observation cost.
When the prepared artifact path is known, `--corpus` addresses it directly and
skips source observation.

The artifact is `corpus build`'s artifact: same identity, same canonical path,
same rule against overwriting. `corpus build` and `--cache` queries reuse artifacts
published by one another. Without `--output`, `ling --json corpus build ROOT`
reuses or builds that artifact and records its path in the `run` record's
`artifact_path`, so a caller observes ROOT once and passes the path to
`--corpus` on later runs.

Stdin has no reopenable logical name and rejects `--cache`. On a scoped first
run, preparation admits the complete corpus and `--where` is then applied to the
prepared read; the cached artifact never embeds the selection.

The cache search validates and reads ROOT under a build's bounds and diagnostics,
so an input that a direct query handles can still make `--cache` fail. That
failure is reported; the run does not fall back to direct execution. An
explicitly addressed `--corpus` is likewise strict.

A human-format query that prepares an artifact prints
`prepared corpus: PATH` to stderr in every `--progress` mode. Reusing an
artifact emits no creation receipt. `--progress always` reports the selected
path. The `run` record distinguishes
`prepared`, with the artifact's source and analysis identity, from `direct`.

## Export an analyzed corpus as CoNLL-U

```bash
ling tokenize --output-format conllu treebank.conllu > export.conllu
ling tokenize --output-format conllu --analyzer-config analyzer.toml source.txt > export.conllu
ling tokenize --output-format conllu --corpus corpus.lingcorpus > export.conllu
ling contracts lingrep.conllu/v1
```

Export streams admitted sentences in corpus order and defaults to all
sentences. It uses exact token surfaces for FORM. Morphology-only analyses
write `_` in HEAD and DEPREL. The export preserves pretokenized FEATS, MISC,
basic and enhanced dependencies, empty nodes, sentence IDs and document
boundaries. Multiword-token range rows and unrelated comments are omitted.
When all words have character offsets, the export reconstructs `# text` from
FORM and inserts one space wherever the offsets show a gap.

For analyzed input, `feat.*` attributes become FEATS and provider attributes
and `norm` are emitted in MISC. MISC values escape percent signs, separators
and line breaks with percent encoding. Bunsetsu and long-unit word boundaries
are emitted as `BunsetuBILabel` and `LUWBILabel` where those layers are present.
Other span layers have no CoNLL-U mapping. Analyzed sentences without an input
ID receive a run-local ID derived from their corpus sentence ordinal.

For a nonempty full export, the final sentence carries
`# lingrep_export = complete N`, where `N` counts its word and empty-node
rows. Check the row count and final blank line to detect interruption during
publication of that sentence. When an explicit limit stops the scan, the last
sentence instead carries `# lingrep_export = prefix N`. Check the process exit
status for an empty export.

## Prepared V1 corpora

```bash
ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output corpus.lingcorpus corpus/
ling corpus inspect corpus.lingcorpus
ling corpus inspect --documents --verify corpus.lingcorpus
ling --json corpus inspect --metadata corpus.lingcorpus
ling matches --corpus corpus.lingcorpus --patterns patterns/sentence-final.lx
ling summarize --corpus corpus.lingcorpus --patterns patterns/sentence-final.lx
ling tokenize --corpus corpus.lingcorpus
```

`corpus inspect --metadata` reports stored document, sentence and division
metadata separately; sentence counts do not inherit document values. Divisions
are counted once per entity, including empty extents, with observation
availability per kind. Each grain reports its unit count, exact present/missing
counts and JSON value shapes per retained key. It retains the lexicographically
first 256 keys and three sorted examples per key, with UTF-8 previews capped at
256 bytes. `fields_truncated`, `examples_complete` and each example's
`truncated` flag identify omitted content. Human and JSONL output are
supported.

Prepared artifacts are compressed. Prepared queries reject `--dict-path`.
Publication requires an absent destination and never replaces an artifact. To
rebuild at an explicit destination, verify and delete that exact artifact
directory first. For obsolete formats, see [Discovering realizations](#discovering-realizations).

`--output` is optional. Omitted, `corpus build` publishes to the canonical
content-addressed path beside ROOT,
`<root>.g<generation>.a<identity-scheme>.<analysis-identity>.<trust>.lingcorpus`,
for example `.g3.a1.….locally-executed.lingcorpus`. Artifact generation and
identity derivation are separate version axes. The required manifest field
`format_generation` identifies which artifact reader applies.

Analysis identity covers the analyzer's complete semantic configuration, not
only the dictionary, so two builds that differ in Sudachi split mode or
segmentation mode produce two distinct artifacts. NFC normalization includes its Unicode
table version in the reader and analyzer identities. NFC artifacts with a
missing or different table version must be rebuilt. The dictionary fingerprint
is recorded for inspection.

A second `corpus build` on unchanged input reuses that artifact without
rebuilding and prints the same recorded inventory the publishing run printed,
apart from the elapsed time; under `--json` the reuse carries its own fresh
`run_id`, as every artifact report does. Changed input produces a new sibling
artifact and never touches the existing one.

Reuse does not test whether the canonical path exists. `corpus build` scans the
`.lingcorpus` directories beside ROOT, reads each bounded manifest, and compares
the recorded source, reader, admission, analyzer semantics, token schema and
trust against the request. Only `g3.a1` artifacts are candidates.

Reuse also requires the candidate to open: a sibling whose token or location
component is missing or truncated is not reused. Reuse does not fully hash the
candidate; that is `corpus inspect --verify`. An unreadable sibling is reported
and skipped rather than failing the run or hiding the realizations beside it.

A directory at the canonical path whose recorded identity does not match is a hard
error reporting that path and the differing fields, not a rebuild. An explicit
`--output` is taken as spelled, is never pre-scanned, and uses the same
publication checks and filesystem fallback described for
[`check --output-dir`](#pattern-diagrams).

Long sentences are handled automatically. A sentence larger than the raw batch
budget runs on its own, with its full text and its original sentence ordinal.
Cache creation never truncates a sentence. Direct and prepared searches retain
the same sentence boundaries, including matches across internal tokenizer chunks.

Sudachi's underlying tokenizer accepts at most 49,149 bytes per call. The
adapter splits longer sentences into UTF-8 chunks and joins the tokens back
into one sentence. The adapter uses whitespace or comma-like punctuation in the
latter half of each chunk when such a boundary exists. When that latter half
contains neither kind of boundary, the adapter cuts between Unicode scalar
values. Tokenization near a chunk boundary can differ from tokenization with
unlimited context; the chunk policy is part of Sudachi's analysis identity.
Vibrato analyzes the full sentence in one call.

Input remains bounded: `--max-record-bytes` defaults to a 1 MiB buffer per
physical line or JSONL document, and `--max-tokenized-bytes` bounds resident tokens.
In a Soranoha plaintext ZIP, the record bound applies per line; inflated books
have a separate 64 MiB memory guard. TEI bounds each XML work and its projected
reading to 64 MiB, limits XML nodes and reading depth, and charges sentence
annotations against the input-record limit. Resource exhaustion still fails explicitly.
`--max-sentence-bytes` is an optional stricter admission limit. An explicit limit
that rejects a sentence aborts cache creation without publishing an artifact.

Older artifacts can record `sentence_truncation: {count, limit}`. A build rejects
reuse of any artifact with a nonzero truncation count, even under the same limit.
Queries also reject these artifacts; `corpus inspect` remains available. Remove
the artifact or use an explicit `--output` to rebuild the complete corpus.

### Discovering realizations

```bash
ling corpus variants corpus.md.g3.a1.<identity>.locally-executed.lingcorpus
ling corpus variants --json --limit all corpus/
```

`corpus variants` reports every prepared realization beside one source. When
the target is an artifact, every difference is expressed relative to that
artifact. When the target is a source, realizations are reported on their own
terms and grouped by the source content that each realization records. It never
builds or selects a realization, and it deletes only with `--prune`.

"Beside" means the target's parent directory, for every kind of target. A
realization is named with the source's own file name plus a suffix, so
`corpus/` publishes to `corpus.g3.a1.<identity>.<trust>.lingcorpus` next to
`corpus/`, never inside it. Because artifacts live beside the source root, the
second example above passes a directory. Scanning a directory that is not itself
a corpus root reports on that directory as a root, which usually finds nothing.

Each difference is a named dimension: `source`, `reader`, `admission`,
`analyzer`, `token_schema`, or `trust`. Reading one tree under two reader
policies can produce two valid artifacts without an analyzer difference.

`--limit N` caps how much detail is shown, never what is counted: the
completion record always carries the complete total. The stream contract is
`lingrep.variants/v1`.

An artifact that this build cannot open is listed as unreadable, with its size. It
is also marked prunable when it belongs to the target's source and was written
in an earlier artifact generation, manifest schema or provenance schema than
this build reads, so only a rebuild from source can replace it:

```bash
ling corpus variants corpus.md            # lists what --prune would delete
ling corpus variants --prune corpus.md    # deletes it, printing each path and size
```

`--prune` deletes nothing else: not an artifact that this build can open, whatever
its identity; not one in a newer format, which a newer `ling` reads; not a
damaged one; not another source's; and not a symbolic link or a directory
whose name is not a canonical artifact name. `lingrep-web` never deletes
artifacts.

### Disk use of prepared artifacts

```bash
ling corpus disk corpora/                       # every source in corpora/
ling corpus disk --source corpora/works.md -d unidic-cwj.dic
ling --json corpus disk --current --caches corpora/ >> disk-use.jsonl
```

`corpus disk DIR` reports prepared artifacts, count tables and cleanup
leftovers in the immediate entries of `DIR`. If `DIR` is a symbolic link,
pass its target directory. `--source SOURCE` limits the report to one source
and scans its parent directory. The `apparent_bytes` field records file lengths,
counting a file in each entry that hard-links it. Use this value to compare
entries by apparent size.

The scan runs the same structural checks as `corpus inspect`, including
component lengths, fixed headers and directory checksums. Each `--json` run
emits one `lingrep.disk/v1` stream.

A realization is *current* when it is the one that `corpus build` would reuse
for its source with the requested dictionary, analyzer, input format, and
Markdown reader options. Pass an analysis flag to classify with the requested
settings, or pass `--current` to classify with the defaults from `corpus build`.
Classification reads each source once to derive its identity. When neither
option is supplied, readable realizations are `unclassified`.

Each realization has one status:

| Status | Meaning |
|---|---|
| `current` | The realization that `corpus build` would reuse with the given inputs. |
| `superseded` | A readable realization whose name matches its manifest and that `corpus build` would skip for the requested inputs. See `differences` and, when present, `kept_by_prune`. |
| `unclassified` | A readable realization scanned without classification. |
| `older_format` | An earlier artifact format. Rebuild it from source to use it with this version. |
| `newer_format` | A later format or an unsupported manifest field. |
| `damaged` | Its manifest or a component fails the validation that `corpus inspect` runs. `kept_by_prune` is `damaged`. |
| `misnamed` | Readable, but its name records another identity or trust. `corpus build` does not reuse it. |
| `symbolic_link` | A symbolic link with a canonical artifact name. |
| `non_canonical_name` | A `.lingcorpus` directory with a name that `corpus build` does not derive, such as an explicit `--output`. |

A count table binds the analysis identity of the artifact it counts. Its
`binding` is `current` or `superseded` when that artifact in `DIR` is,
`present` when the artifact is present but unclassified or any other entry
binds it, and `absent` when no artifact in `DIR` records that identity; the
artifact may exist elsewhere. An artifact binds a table whatever its name,
including an explicit `--output` and the target of a symbolic link, and a
`newer_format` or `damaged` canonical artifact binds the identity that its name
records.
A table whose binding is `superseded` or `absent` is orphaned. Tables are
found by their `header.json`, whatever their names. A `superseded` table that
holds anything but the header and section files that its format defines is
`kept_by_prune: foreign_entries`.

A leftover is a staging directory or lock file from a build that did not
publish; an entry with such a name that is not a directory or a regular file
respectively, such as a FIFO or a symbolic link, is not reported or opened. It
is `live` while a process holds its lock, `abandoned` when its lock records this
host and is free, and `unknown` otherwise (when the lock file is missing or
records another host). The next build of the same name on the same host removes
abandoned leftovers; see the
[deployment guide](deployment-guide.md#interrupted-corpus-builds) for the
others.

A leftover can also be `interrupted_prune`: a realization or count table that
`corpus prune --execute` renamed to the hidden name `.<name>.pruning-<suffix>`
and did not finish deleting, reported with that path as `pruning` and its former
name as `published_name`. `corpus build` does not read it, `ling table` and
`ling serve --count-table` reject it, and the next `corpus prune --execute`
deletes it unless it holds anything other than the components that its manifest
or table format lists, in which case it is kept as `foreign_entries`. With
`--source`, a count table's leftover is reported when its header binds a
realization of that source.

`--caches` adds the dictionary cache, which holds the decompressed copy of
each `.dic.zst` dictionary that has been loaded, one record per copy. A copy
is named by a hash of the compressed file's metadata, so the report cannot
identify the dictionary it came from. `--analyzer-dir DIR` adds the size of a
`ling analyzer prepare` directory, and can be repeated.

### Pruning superseded artifacts

When a canonical build finds readable artifacts of the same source built with
different analysis settings, it reports each retained artifact's apparent size
on stderr when it can measure it.
Preview cleanup with `ling corpus prune --source SOURCE`, using the build's
analyzer and reader options. Review the plan before adding `--execute`.

```bash
ling corpus prune --source corpora/works.md -d unidic-cwj.dic            # lists only
ling corpus prune --source corpora/works.md -d unidic-cwj.dic --execute  # deletes
```

`corpus prune DIR` lists the entries eligible for removal. For the current
source content, it selects older-format realizations with a matching source
identity and superseded realizations that differ in analyzer or token schema.
It selects count tables only when every artifact bound to the table in the
scanned directory is eligible. This includes checking artifacts built at
explicit `--output` paths in that directory.

Other superseded and older-format realizations are kept, and listed as
`kept` with a reason:

| `kept_by_prune` | Differs in | Deleted with |
|---|---|---|
| `reader_variant` | reader or admission policy, and no analysis dimension | `--include-all-superseded` |
| `source_revision` | source content, whatever else differs | `--include-all-superseded` |
| `source_unknown` | not known: the source is missing, or an older-format manifest records another or no source identity | `--include-all-superseded` |
| `other_trust` | trust class | never |
| `identical_to_current` | nothing, yet `corpus build` would not select it | never |
| `foreign_entries` | The directory contains an unexpected entry, a non-regular component, or an entry on another filesystem, or is itself a mount point. See `message`. | never |
| `kept_table` | Removing these realizations would leave a count table kept for `foreign_entries` without a bound artifact in the scanned directory. See `message` for the table path. | never |
| `damaged` | anything; it is `damaged`, and `message` gives the failure | never |

A reader variant is typically the artifact into which another tool reads the
same source, such as `lingrep-web` with its Markdown reader policy or a `ling`
release with another admission policy. A source revision may still be served by
a process that read the source before it was edited, and no rebuild reproduces
it. An older-format manifest whose source identity differs may still analyze the
current content, because an earlier `ling` may have derived the identity
differently, so it is classified as `source_unknown`. An artifact of another
trust class is not one that `corpus build` produces, so no local rebuild
replaces it.

Without `--execute`, `corpus prune` deletes nothing and lists each path with its
apparent size and the total. With `--execute`, it deletes the same set and
reports the apparent size of each deletion. A deletion that fails ends the run:
its `removal` record carries `failure`, the completion record counts it as
`failed`, and the command exits with status 1.

`corpus prune --execute` deletes count tables before the realizations they bind,
and renames each table and realization to a hidden name before deleting it, so
an interrupted or failed run leaves either the entry under its own name or an
`interrupted_prune` leftover. Running `corpus prune --execute` again after
removing the cause of failure finishes the cleanup. If a count table bound only
to realizations selected for deletion holds anything other than its own files,
the table is kept as `foreign_entries` and those realizations are kept as
`kept_table`, so no table is left bound to nothing. The stream contract is
`lingrep.prune/v1`, with one `kept` record per retained superseded, older-format,
or damaged artifact, count table, or interrupted prune, one `removal` record per
deleted or attempted path, and a completion record.

`corpus prune` uses the defaults from `corpus build` for omitted analysis
flags. If a source has entries eligible for removal and has no current
realization, the command exits with status 2 and leaves all entries in place.
Pass `--keep ARTIFACT` to designate a readable, undamaged realization in the
scanned directory for that source. Repeat the flag to keep additional
realizations.

It never deletes the current or a kept artifact, an `unclassified`,
`newer_format`, `damaged`, `misnamed`, `symbolic_link` or `non_canonical_name` entry,
another source's artifacts when `--source` is given, an entry outside the
scanned directory, a count table that binds an artifact being kept, or a staging
leftover. Immediately before each deletion it checks again that the entry is
a directory, not a symbolic link, with the canonical name and recorded
identity the scan classified, or for a count table the bound identity, and
that a superseded realization still passes structural validation, and that
the directory holds exactly the components its manifest or table format
lists. It renames the entry to a hidden sibling before deleting any files.
After checking the renamed directory again, it removes the listed files,
then the manifest or table header, then the empty directory. It never deletes
recursively.

Before running with `--execute`, check the preview for artifacts used by
builds, queries, or `lingrep-web` instances. Stop those processes or preserve
their artifacts with `--keep`. The preview cannot detect which processes are
using an artifact.

Whether an artifact built by `ling` is also the one `lingrep-web` prepares
depends on the input format, because the name includes the reader identity. For
a JSONL or plaintext root the two tools read identically and one artifact
serves both. For a pandoc Markdown root, `lingrep-web` reads with
conversion-artifact recovery enabled and `corpus build` reads without it; that
setting is part of the reader identity, so the two tools derive different
artifact paths for the same tree and each prepares its own.

Omitting `--output` costs one extra read-and-hash pass over the source before
the build, because the destination name is derived from the source's observed
identity together with the dictionary and reader identities; a different
dictionary or section policy therefore produces a different artifact path. That
pass tokenizes nothing. `corpus build --progress always` counts the sources that
the identity pass reads as `identity pass: read N sources`, and the sentences
that the build stores as `build pass: stored N sentences`; reusing an artifact
runs no build pass.
Passing `--output` skips it. By default, the build stages and publishes the
artifact beside `ROOT`. The directory containing `ROOT` must therefore be
writable. When `ROOT` is a directory, this means its parent directory. A
read-only location is reported late, after the dictionary load and the whole
preflight read, because staging is created only once the destination is known;
pass `--output` to write elsewhere.

The canonical path derives from ROOT as spelled, not from a resolved path;
`lingrep-web` also uses the unresolved spelling. So `corpus build corpus/` from
`/data` and `corpus build /data/corpus` resolve to the same artifact path, while
passing a symlink and passing its target resolve to two different paths. A root
with no file name of its own (`.`, `..`, `/`) is resolved first.

Because the identity pass and the build pass read the source separately, a source
edited between them yields an artifact whose filename reflects the older content
while recording the newer identity. Nothing is overwritten, and that artifact is
never reused by mistake: a run that resolves to the same path again detects the
mismatched recorded identity and fails, while a run over the edited source
produces a fresh sibling.

### Reading sidecars for Markdown corpora

```bash
ling corpus render corpus.lingcorpus --source-root corpus/ \
  --md-recover-conversion-artifacts --output corpus.reading
```

`corpus render` renders every document of an artifact built from Markdown to
HTML and writes the result to a reading sidecar, a new directory at
`--output`. A reader can then show a document, and highlight an occurrence in
it, from the artifact and the sidecar alone, without the source tree.

`--source-root` specifies the tree from which the artifact was built, and the
`--md-*` options must match those used during the build. The command rejects
options whose reader identity differs from the artifact's with exit code 2. It
reads the tree once to compute its source identity, and rejects a tree whose
identity differs from the artifact's. It then reads each document again with the
Markdown reader and verifies the source identity of the bytes it renders. If
those bytes differ from the observed source, or the sentences differ from the
stored ones, the command fails and publishes nothing.

The renderer applies the dialect's own rules first: frontmatter and `<!-- -->`
comments are not rendered, `:::` regions become `<div>` elements carrying
their classes, `{.class}` spans are removed and heading classes are kept. It
renders the rest as CommonMark with tables and footnotes. Raw HTML is shown as
text, a link keeps its target only for `http:`, `https:`, `mailto:` and `#`,
and mathematics stays source text. A table ends at the first line without
`|`; if its delimiter row starts with `|`, every body row must also start
with `|`. Cells beyond the header's width are rendered. A `:::` line inside
a `$$` math block is math text and does not open or close a region.

Rebuild Markdown artifacts and their reading sidecars created before these
math and table rules to apply them. Existing artifacts retain their recorded
analysis.

Each stored sentence is mapped to the rendered characters of its source line.
Characters that render as markup, such as table pipes and footnote markers,
and ordered-list numbers, which render as list markers, have no rendered text.
The command reports how many sentences map in full and how many have no
mapped text. Exact text matches remain mapped regardless of line length.
Other alignments are attempted only when the stored and rendered text total
at most 4,096 characters. Longer lines remain readable, without highlights.

A sidecar records the artifact's analysis identity and source identity and
the renderer revision. Two renders with the same three identities write
identical bytes. `lingrep_corpus::ReadingSidecar::open` rejects a sidecar when
any of the three differs from the caller's, and reports the field that differs.
`corpus render` does not support `--json` or `--plan`.

## Tokenization and interactive workflows

```bash
ling tokenize --dict-path "$LINGREP_DICTIONARY_PATH" --json corpus/
ling explore --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sentence-final.lx corpus/
ling summarize --top all --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sentence-final.lx corpus/
```

`tokenize` publishes at most 100 sentences in human output by default. JSONL
(`--json`) and `lingrep.tokenize-tsv/v1` (`--tsv`) output are unlimited by default.
Use `--limit N` to cap any of these outputs at N whole sentences, or `--limit all`
to remove the cap. A run stopped at the cap reports `coverage: "prefix"` and
`termination: "result_limit"`; only a run that reaches EOF reports full coverage.

Sentence-final selection belongs in the catalog policy. Use
`admit all terminal: {exact: true, anchor: propositional_end}.` with a declared
anchor that excludes `[upos=PUNCT]+` to allow only trailing punctuation.
`matches` emits the selected occurrences; `summarize` counts them. The default
`patterns/sentence-final.lx` catalog instead admits matches within five tokens
of its anchor.

### Attribute coverage: `ling coverage`

```bash
ling coverage --dict-path "$LINGREP_DICTIONARY_PATH" --json corpus/
ling coverage --dict-path "$LINGREP_DICTIONARY_PATH" --json \
  --patterns patterns/sentence-final.lx corpus/
```

`coverage` counts, for each attribute in the selected analyzer's token schema,
how many tokens have it `present` (set to a value other than `*`),
`unspecified` (set to `*`, which UniDic supplies to mean "does not apply
here"; a supplied value, so neither present nor absent), and `absent`
(unset). Every attribute is always reported, so a partially populated one is
visible. Totality depends on the bound dictionary and the corpus, never on the
attribute alone.

Given a catalog, `coverage` also reports every **absence-sensitive exclusion**:
a site spelled `!=`, `!~` with a literal, or `!~` with a regex, each of which is
false for a token lacking the attribute, joined to that attribute's measured
absent count. A true complement `!(…)` is true for a missing attribute and is
never reported. Four statuses:

| status | meaning |
|---|---|
| `clean` | evaluated, and its attribute is absent on no admitted token |
| `exposed` | evaluated, and its attribute is absent somewhere |
| `inert` | never evaluated (an unreferenced fragment), so no absent token can reach it |
| `unknown` | nothing was admitted, so there is no evidence either way |

**This measures exposure, not lost matches.** `exposed` means absent tokens
exist where the predicate could be evaluated; it does not claim a match was
missed, because an absent token may never reach a position where the predicate
runs.

`--where` scopes the report exactly as it scopes `matches`. `--tsv` emits the
coverage table alone as the fixed headerless `lingrep.attributes-tsv/v1`, and
is rejected together with a catalog, because the exclusion table has different
columns. Use `--json` for both tables.

## Discovery, explanation, and completion

```bash
ling describe matches
nix run .#lingrep -- describe matches
ling contracts
ling contracts lingrep.query/v2 --json
ling doctor --dict-path "$DICT" --patterns patterns/sentence-final.lx \
  --corpus artifact.lingcorpus
ling manpages --output ./ling-manpages
```

`nix run .` is the equivalent default-app spelling. Use direct `ling`
after installation or inside `nix develop .#packaged`.

`describe [COMMAND]` reports a command's arguments together with examples and
the contracts that command exposes. The contract index does not include every
JSON Schema; selecting one contract returns its record schema together with
stream ordering, coverage, pipe, and exit semantics.

`doctor` checks only the inputs supplied to it. Without `--corpus`, `doctor`
probes the default dictionary or the dictionary specified by `--dict-path`. With
`--corpus`, `doctor` probes a dictionary only when `--dict-path` is given,
because queries over a prepared corpus read its stored tokens. A supplied
catalog is compiled and a supplied artifact is opened as separate probes, and
`ready` reports whether every selected probe passed. Its corpus check is
bounded structural admission, not payload hashing; use `corpus inspect --verify`
for full verification.

When both a catalog and a readable dictionary are checked and the catalog's
`analyzer:` declaration does not admit the dictionary, `doctor` writes
`warning[dictionary_binding_mismatch]` to stderr, or a diagnostic record under
`--json`, in the same form as `matches`. This warning does not change `ready`.
When a catalog and a corpus are both supplied and both admitted, a derived
**binding probe** follows: it binds the catalog against the corpus's stored
token schema (the same set that a `matches` run binds) and lists every
referenced attribute that the corpus cannot supply (`ok`, with evidence), while
a dependency `where` clause that the corpus cannot satisfy fails the probe with
the missing edge keys.

Human output marks a passing probe as `warning` when the catalog compiled with
warnings or the binding reports unavailable attributes, and lists at most three
attributes and three definitions for each; `--json` carries the complete
evidence. Any failed probe exits 1 after all requested results are published.

`--plan` is available on `matches`, `summarize`, `tokenize`, `coverage`,
`corpus build`, `corpus inspect`, `corpus inventory`, and
`corpus variants`; it validates bounded setup and never scans corpus records or
recursively traverses a direct root.

Generate definitions for Bash, Zsh, Fish, Elvish, or PowerShell with
`completions SHELL`. The Nix package installs definitions for Bash, Zsh, and
Fish, plus all section-1 manual pages; Elvish and PowerShell definitions are
available by explicit generation.

See [the TUI guide](tui-guide.md) and
[the package README](../lingrep/README.md) for operational examples.

## Serving several corpora from `lingrep-web`

This section covers `lingrep-web`, the browser interface, because `--corpus`
has a different meaning there: on `ling` it specifies a prepared V1 artifact to
query, and on `lingrep-web` it declares a corpus that the running server offers.
`ling serve` runs the same server and accepts the same arguments as each
`lingrep-web` command below. A `ling` built with `--no-default-features` has no
`serve` subcommand.

```bash
export LINGREP_CORPUS=/absolute/path/to/corpus.jsonl

lingrep-web \
  --dict-path "$(nix build .#vibrato-dict-cwj --no-link --print-out-paths)/share/vibrato/unidic-cwj-202512.dic" \
  --input "$LINGREP_CORPUS" \
  --corpus news=corpora/news.jsonl \
  --corpus academic=corpora/academic.md
```

`--corpus NAME=PATH` is repeatable. `PATH` is either a source corpus (a
`.jsonl` or `.md` file, or a Markdown directory, read the same way as
`--input`) or a prepared V1 artifact. A directory containing a `manifest.json`
is treated as an artifact, and everything else as a source. For a source,
`lingrep-web` prepares its own artifact beside the source file and reuses it on
the next start.

A declared artifact is read in place and nothing beside it is written. The
artifact records the reader that built it and no source is re-read, so
`lingrep-web` can serve a corpus format it cannot read itself:

```bash
# lingrep-web cannot read a BCCWJ distribution. It can serve one that was prepared.
ling corpus build --dict-path "$DICT" --input-format bccwj /data/bccwj
lingrep-web --dict-path "$DICT" \
  --corpus bccwj=/data/bccwj.g3.a1.<analysis-identity>.locally-executed.lingcorpus
```

`lingrep-web` rejects a BCCWJ root passed to it directly, because the C-XML
reader is part of `ling`. Because artifacts are read in place rather than
owned:

- No reader configuration applies to an artifact. `--input-format` with an
  artifact is an error instead of being ignored, because reader configuration is
  part of artifact identity. The flags for selecting Markdown sections do not
  apply to an artifact; they were applied when it was built.
- A declared artifact is retained if corrupt, and its corpus is marked failed.
  An artifact beside a declared source is deleted and rebuilt once per corpus
  per server process when corruption is detected during a read after a successful
  open. Existing artifacts that fail to open at startup are preserved; finish
  or quarantine an incomplete copy before restarting.
  If the rebuilt artifact is corrupt, it is deleted and the corpus fails.
  Other open errors leave the artifact on disk and mark its corpus failed.
  See [artifact recovery](deployment-guide.md#prepared-artifacts-and-how-bccwj-is-served)
  for error categories and recovery steps.

Use `--corpus NAME=PATH` instead of `--input` for an artifact: the caption is
the declared `NAME`, whereas `--input` captions a corpus by its file stem, and
an artifact's stem is the whole content-addressed name.

`NAME` is the corpus key in a shareable link. It is constrained to
`[A-Za-z0-9_-]`, at most 64 characters; it requires no URL escaping and exposes
nothing about the filesystem. It is not a caption: a corpus file may be called
`日本語コーパス.jsonl`, which is a valid filename and an invalid key. A
repeated `NAME`, a missing `=`, and a name outside that alphabet are each
rejected before the server starts. A duplicate error reports the key and a
malformed-entry error quotes the whole argument, while an over-long or
invalid-character error reports only the offending character or the
64-character limit.

`default` is reserved and may not be declared: it is the key of the `--input`
corpus, and `--corpus default=…` exits 2 with that reason. A shareable link
carrying `k=default` resolves to the `--input` corpus.

`--input` itself is optional. The server requires at least one corpus from
either flag; the first declared one is the default that the bare `/` resolves
to. Declaring only `--corpus` entries therefore starts a server on which no
corpus is keyed `default`, and a link containing `k=default` finds nothing.

`--input-format` is rejected once more than one corpus is declared, because it
is a single global override and would be applied to every path:

```text
Invalid lingrep-web configuration: --input-format applies to every declared corpus
and so cannot be used with more than one; drop it and let the format be
inferred per path
```

Format inference is per path, so a JSONL corpus and a Markdown corpus can be
declared together as long as no override is present. With exactly one corpus
the override is accepted. The Markdown section flags (`--md-include-sections`,
`--md-exclude-sections`, `--md-self-labels`) and `--max-record-bytes` are also
global and apply to every declared corpus; the section flags determine what is
read, so they are part of each corpus's prepared-artifact identity.
`--md-recover-conversion-artifacts` is accepted and has no effect, because
`lingrep-web` always reads with recovery.

Only the default corpus's build starts at startup, and the listener binds
without waiting for it to finish: the server answers requests, and the corpus
panel reports "preparing", while the first corpus is being prepared. Every
other declared corpus starts building the first time it is selected. A corpus
that a `--count-table` counts is the exception: the server opens it and reads
its document metadata before it answers any request. One
process-wide permit admits one build at a time, so N declared corpora never hold
N decoded dictionaries at once.

**Declaring more corpora adds no query concurrency.** The server admits one
in-flight scan at a time, which uses the whole Rayon pool, so scans are
serialized server-wide across corpora, not per corpus.

The server cancels a queued or running search after 30 seconds without an
open progress stream (`api/stream/<workbench>`), including searches whose
clients never open the stream.

A workbench is bound to one corpus when it is admitted and is never rebound, so
choosing a different corpus in the UI navigates to a new permalink rather than
re-pointing the open page. The selector is rendered only when more than one
corpus is declared. The browser receives each corpus's key and display name
(the file stem for the `--input` corpus, the declared `NAME` for a `--corpus`
entry) and never its path.

### Frequency view from count tables

`--count-table PATH` serves a table written by `ling summarize --write-table`
in the Frequencies view, at `frequencies` under the server's base path.
The flag is repeatable. Without it the view is absent and the workbench is
unchanged.

```bash
ling summarize --patterns patterns/collexeme.lx --corpus works.lingcorpus \
  --write-table works.collexeme.lingtable
ling serve --corpus works=works.lingcorpus --pattern-root patterns \
  --count-table works.collexeme.lingtable
```

A table binds to the declared prepared corpus whose analysis identity it
records and to the catalog under `--pattern-root` whose fingerprint it records;
neither the flag order nor the file name takes part. Startup stops with an
error identifying the table when the table cannot be read or does not verify, was
written by another matcher revision, matches no declared corpus or no catalog,
counts a document the corpus does not hold, or counts the same catalog over the
same corpus as another table:

```text
--count-table works.lingtable: matches no served corpus: it counts analysis identity ff18f3e3…
--count-table works.lingtable: matches no catalog under the pattern root: it counts catalog fingerprint c61e213c…
--count-table a.lingtable and --count-table b.lingtable both count catalog collexeme.lx over corpus "works"
```

Document metadata is indexed in temporary files shared by the corpus's table
bindings, and each binding adds files holding its documents' sentence ranges
and numbered condition values. Set `TMPDIR` to a writable, disk-backed
directory for large corpora; a memory-backed temporary directory consumes RAM
for those files. The files are removed when the server releases the corpus
index.

The view presents a query as a sentence with controls for the catalog,
pattern, document metadata conditions, sort order, and substring filter. Above
the sentence, a "Corpus:" line shows the selected table's corpus on both the
Frequencies and Compare pages. A key becomes a condition when every value is a
number (a range) or when its scalar or list values repeat (a set to keep or to
exclude). Categorical values are queried in bounded pages; the search box
searches the complete vocabulary, and selected values remain available outside
the first page. Each condition shows the number of occurrences that satisfy all
conditions as a share of those that satisfy the other conditions. The view
applies its selected sort order and substring filter separately.

For a pattern whose key has several fields, one input per field selects rows
whose field equals the typed value exactly. The figures line then gives the
subset's occurrences, documents, and rows beside the whole pattern's total, a
"Share of subset" column divides each row's count by the subset's occurrences,
and the shown command adds the equivalent `--select` and `--key` options.

The page shows the corresponding `ling table … --where …` command. Command paths
are relative to the common parent directory of the table, corpus, and catalog
files; run the copied command from that directory. Absolute server paths are
omitted. When the inputs share only the filesystem root, the command uses
`TABLE.lingtable`, `CORPUS.lingcorpus`, and `CATALOG.lx`; replace these with
your local paths. A relative path containing `..` or a path that is not UTF-8
cannot be shown. A clause with `@section` or `@sentence` is rejected with an
explanation because count tables store counts per document. The URL stores the
query state, so a copied link reopens the same view.

Selecting a row opens that entry. For each condition key, a chart shows the
entry's occurrences and documents under each value, counted under the other
conditions, which equals `ling table TABLE --entry PATTERN=GROUP
--by-metadata @document.KEY --where …` with that key's condition left out.
Categories show their 24 most frequent values, or the 12 most frequent and the
chosen ones when there are more, and documents without a value get a "no
value" bar. A range key gets one column per integer when its values are
integers and its range holds at most 160 of them, and 40 equal columns
otherwise. Each chart has a table of every shown value in full.

Below the charts are the entry's lines, 50 per page, which equal the records of
the shown `ling matches --where … | jq` command, in the same order. The server
takes the sentences to match from the table and fails the request if a
document's lines do not add up to the table's count for it.

Selecting a line's node opens the line in its document. "Inspect tokens" opens
the question page on the same corpus: it runs only the table's pattern of the
catalog, as the entry's lines do, over the line's document, and opens the token
inspection of the match in that sentence whose captures and `where`-bound words
fill the same roles, so a verb outside the span appears in the dependency panel.
Searching again with the catalog unchanged keeps that pattern and searches the
document conditions then set. If the corpus analysis or catalog differs from the
table's, the question page reports the difference and opens no example. Its
"Back to frequencies" link, shown beside the search status even when the search
fails, or the browser's Back, returns to the entry with its exact key and
conditions.

Resting the pointer on a bar for 120 ms, or focusing it, previews its value as
the key's condition: the other charts, the entry's figures, and its lines
redraw, while the sentence and the URL keep the committed conditions. A preview
draws no more rows than the committed charts and lines hold, so the page does
not move. Clicking or tapping a bar sets the condition; the arrow keys move
between bars; Escape or the back link returns to the table.

A value name that does not fit shows a shorter form that replaces whole parts
with one "…": parts end at 、, ・, or a space, a bracketed part counts as one,
and brackets stay balanced. No two values of a chart show the same form. The
full name is in the bar's accessible name, in the line under the chart's name
for the bar last pointed at, focused, or tapped, and in the chart's table.

Opening an entry requires a version 4 count table, whose surface keys preserve
word boundaries. Rebuild an earlier table with this release's
`ling summarize --write-table`; otherwise startup fails with an error
identifying it.

The same tables serve the Compare page, at `compare` under the base path.
It states two scopes, A and B, as a sentence with the Frequencies view's
condition controls, one set per scope, or as two clauses through "Edit as
clauses". It ranks one pattern's entries by G² as `ling contrast` does, 100 per
page, and shows the `ling contrast --where … --versus …` command that
reproduces the first page. Each row gives each scope's occurrences, rate per
million tokens, and documents, a bar from the center toward the scope with the
higher rate on a log2 scale shared by the shown rows, hatched for an entry
absent from one scope, and G². Below the sentence, the page lists each scope's
documents and tokens and states whether the scopes share documents, are
identical, or are disjoint. A narrow screen keeps each scope's rate and drops
its counts. The URL stores both scopes, so a copied link reopens the page.

`--metadata-labels PATH` reads a JSON file of display labels for document
metadata keys and string values, by locale. The question document, Frequencies,
and Compare display each condition using its label in the page's language; a
key without a label in that language falls back to the raw key, with spaces for
underscores in English. The flag is repeatable. Labels change no clause: the
address, the clause, and the shown `ling table` command keep the keys. A label
applies to the matching key in every served corpus.

```json
{
  "vocabulary": "lingrep.soranoha-catalog",
  "edition": "1",
  "source": "lingrep Soranoha reader: document metadata from catalog.csv",
  "licence": "MIT",
  "keys": {
    "@document.orthographic_style": { "labels": { "en": "orthography", "ja": "文字遣い" } },
    "@document.source_edition_year": { "labels": { "en": "Source book: first edition year", "ja": "底本の初版発行年" } }
  }
}
```

`vocabulary`, `edition`, `source`, and `licence` identify the labels and their
terms and must not be empty. Each entry in `keys` specifies an `@document` key
and gives non-empty labels under `en` or `ja`, the page's supported locales.
A key entry may also contain a `values` map, keyed by the exact stored string:
`"values": {"lit": {"labels": {"en": "Literature", "ja": "文学"}}}`.
A value-only entry may omit the key's `labels`. The question document shows
`lit Literature` or `lit 文学` beside the document count and searches both the
name and the code. Codes with the same name remain separate. Missing
translations use the raw value; numbers and booleans keep their original types.
Changing a label changes neither conditions nor prepared-corpus identity.
Grouping and splitting values are outside this format.

Startup stops with an error identifying the file when it cannot be read, has an
unknown or empty field, repeats a key, value, or locale, gives two keys the
same label in one locale, or labels a key that another file labels. If no corpus
with a count table contains a labeled key, the server reports the key at startup
and continues.
[`lingrep-web/labels/soranoha-catalog.json`](../lingrep-web/labels/soranoha-catalog.json)
provides labels for every Soranoha catalog column in English and Japanese. The
`lingrep` and `lingrep-web` Nix packages and the release archives install it as
`share/lingrep-web/labels/soranoha-catalog.json`. In an extracted release archive,
use `--metadata-labels ./share/lingrep-web/labels/soranoha-catalog.json` from the
archive's root directory. With Nix:

```bash
lingrep=$(nix build --no-link --print-out-paths .#lingrep)
"$lingrep/bin/ling" serve --corpus soranoha=soranoha.lingcorpus --pattern-root patterns \
  --count-table soranoha.collexeme.lingtable \
  --metadata-labels "$lingrep/share/lingrep-web/labels/soranoha-catalog.json"
```

Selecting a line of an entry opens its sentence in the document it belongs
to, in a dialog over the lines, with the matched words marked in the color of
the workbench's KWIC keyword. Escape, "Back to the lines" or the browser's
Back closes the dialog and returns focus to the line, at the previous scroll
position. The address holds the open occurrence as
`read=SENTENCE.START.END`, and `scope=paragraph` when the reader shows the
paragraph, so a copied link or a reload reopens the reader. The workbench's
reader keeps the same `read` and `scope` in its address; a link to it opens
the reader without the workbench's search scope.

Reading requests from the Frequencies view, `api/read`, run under a permit of
their own, so they do not wait for a workbench scan or export.

### Rendered documents from reading sidecars

`--reading-sidecar PATH` serves a reading sidecar written by `ling corpus
render`, so that the document reader shows a Markdown corpus's documents with
their headings, paragraphs, lists, tables and quotations, as the renderer
rendered them. The flag is repeatable.

```bash
ling corpus render works.lingcorpus --source-root works/ \
  --md-recover-conversion-artifacts --output works.reading
ling serve --corpus works=works.lingcorpus --pattern-root patterns \
  --reading-sidecar works.reading
```

A sidecar binds to a corpus declared by the path of a prepared `.lingcorpus`
artifact, `--input` or `--corpus NAME=PATH`, whose analysis identity it
records. It must also record that corpus's source identity and this release's
renderer revision. A corpus that `lingrep-web` prepares from a source tree
takes no sidecar; build it with `ling corpus build` and serve the artifact.
Startup stops with an error identifying the sidecar when the sidecar cannot be read
or does not verify, records another source identity or renderer revision,
matches no declared corpus, or renders the same corpus as another sidecar:

```text
--reading-sidecar works.reading: matches no served corpus: it renders analysis identity ff18f3e3…, and corpus "works" has analysis identity 0b9d41c7…. A corpus prepared again with another analysis needs `ling corpus render` again
--reading-sidecar a.reading and --reading-sidecar b.reading both render corpus "works"
```

Preparing a corpus again with a release whose Markdown reader or analyzer
differs gives it another analysis identity, so render its sidecar again.

Startup checks each sidecar's records digest and decodes every record, which
reads the whole sidecar once.

The reader shows the rendered document in its Document scope, which a line
of the Frequencies view and the workbench's "Open document" open, under the
same title, scope controls and document metadata as the stored-sentence
reader. It loads about 64 KiB of rendered HTML around the occurrence, with
buttons that load the text before and after. A note indicates when some or all
of the occurrence's characters are absent from the rendered page, such as table
pipes or footnote markers, and when the occurrence is not marked because its
sentence exceeds the reader's 128 KiB limit. Under a workbench search scope,
the text of excluded sentences is grey with a dashed underline. The
Paragraph scope still shows the stored sentences. A corpus without a sidecar,
including a plaintext corpus, shows its stored sentences as one run.
