# ling CLI Usage Guide

`ling` is the packaged command-line executable.

## Command tree

```text
ling check
ling matches
ling summarize
ling corpus build|inspect|inventory|variants
ling tokenize
ling coverage
ling explore
ling sentence-final
ling doctor
ling describe [COMMAND]
ling completions SHELL
ling manpages --output DIRECTORY
ling contracts [CONTRACT]
```

Use `ling <command> --help` for all bounds and formats.

## Choose a workflow

| Goal | Start with | Continue with |
| --- | --- | --- |
| Validate lingrep patterns without reading a corpus | `ling check` | `ling check --help` for the language reference |
| Find individual matches | `ling matches` | [Catalogs and matching](#catalogs-and-matching) |
| Count matches or authored terms | `ling summarize` | [What `summarize` groups by](#what-summarize-groups-by) |
| Inspect analyzed tokens | `ling tokenize` | [Tokenization and interactive workflows](#tokenization-and-interactive-workflows) |
| Check attribute availability | `ling coverage` | [Attribute coverage](#attribute-coverage-ling-coverage) |
| Explore interactively | `ling explore` | [TUI guide](tui-guide.md) |
| Reuse analyzed input | `--cache` or `ling corpus build` | [Reusing and preparing a corpus](#reusing-and-preparing-a-corpus-from-a-query) |
| Select an analyzer | `--analyzer` | [Choose an analyzer](#choose-an-analyzer) |
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
ling check --patterns patterns/sfe.lx
ling check --verify-examples --dict-path "$LINGREP_DICTIONARY_PATH" --patterns patterns/sfe.lx
ling matches --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx --json --context 2 corpus/
ling summarize --dict-path "$LINGREP_DICTIONARY_PATH" \
  --pattern '[upos=VERB].' --tsv --top 50 corpus/
```

`check` compiles without analysis by default. `--verify-examples` runs
`examples` and `counterexamples` through the selected native Vibrato or Sudachi
configuration, with one sentence per claim (`--segmentation input`). The report
identifies the catalog, analyzer and resource, distinguishes zero checked claims,
and includes malformed, expected/unexpected-match and post-policy failures.
Violated claims exit 1; unavailable attributes and analysis failures cannot pass
counterexamples. Passing examples confirms consistency with authored claims,
not linguistic correctness or recall. External adapters are refused by this
verification path.

`matches` emits canonical per-match evidence. A published token carries only the
attributes the run projects: the catalog's own, plus `text`, plus those its
policy projects. The `run` record lists them in `published_attributes`.
`--context N` projects at most `N` tokens to each side and never changes the
matched span. `summarize` performs exact external aggregation before publishing
rows. Use `--limit N` to publish only the first N matches.

For exact full-scope counts with bounded examples, use
`ling --json matches --retain 100 --retain-seed 7 ...`. This scans to EOF,
then emits at most 100 published matches in corpus order with their original
sequence IDs. Completion declares the Algorithm R/SplitMix64 selection, seed,
eligible and retained counts; `records_emitted` counts retained records.
Retention is global across patterns, capped at 100,000 records and 64 MiB of
serialized JSON, and fails if the byte cap is exceeded. It requires JSON and
cannot be combined with `--limit`. No retained examples or successful completion
are published after scan failure or cancellation. With `--sample`, totals cover
that declared corpus sample rather than a corpus census.

Closing human stdout with a pager or `head` is a successful early display
termination. JSONL and TSV output treat a closed stdout as an error: the run
reports `broken_pipe` and exits `1`.

### Human KWIC rendering

Human `matches` output concatenates morphemes by default. A single ordinary
space separates a present context from the complete hit; there are no spaces
inside the left context, hit, or right context unless requested. Context uses
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
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
  --context-color-depth 0 corpus/

# Color the nearest three morphemes on each side.
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
  --context-color-depth 3 corpus/

# Use the default depth explicitly and adapt for a light background.
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
  --context-color-depth 5 --context-color-theme light corpus/
```

Use `--morpheme-separator TEXT` when morpheme boundaries matter. The separator
is applied inside all three fields, including a multi-token hit, but never in
place of the spaces around the hit:

```bash
# Reproduce space-separated morphemes.
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
  --morpheme-separator ' ' corpus/

# Make every morpheme boundary explicit.
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
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

### Declared result policy in published records

A catalog's metadata and its `policy … end` block declare what it publishes,
not only what it matches. Three JSONL fields on a `match` record carry the
result, and each is omitted when the catalog derives nothing for it:

| Field | Source | Shape |
|---|---|---|
| `unit` | a `transform:` list ending in `{op: join}` | string |
| `key` | `key: [pos1, $capture.span.norm, …]` | typed ordered scalar fields |
| `tags` | `tags: {name: source, …}` | `[{"name": …, "value": …}]`, by tag name |

Under `--progress always`, a run that applied policy and removed captures
writes one line, `policy dropped N capture(s)`, followed by one line per policy
statement the catalog declares:

```text
  resolve %heads rank: [longest, start, end, pattern]. (7)  → dropped 4, narrowed 0
  detach %trailing_copula with: @DA, relation: at_end, evidence: raw. (8)  → dropped 0, narrowed 13
```

Each line quotes the statement's text and its catalog line number. Every
declared statement is reported, including one that changed nothing, so an
inert statement is visible. A run that removed nothing writes no
`policy dropped` line.

Key fields keep their authored order through matching, aggregation, JSONL, TSV,
and human output. Tags keep their own canonical ordering.

```bash
ling matches --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns verb-units.lx corpus/ \
  | jq -r 'select(.type=="match") | .data | [.unit, (.key//[]|map(.value)|join("\t"))] | @tsv'
```

`surface` is always the published span's own text. A `substitute` op changes
`unit` without changing the span, so `unit` and `surface` can differ.

A tag source is a literal (`{aspect: perfective}`), a capture-presence mapping
(`{aspect: {perf: perfective, prog: progressive}}`, where the named capture that
participated supplies the value), or a projection off one capture
(`{lemma: {$head: lemma}}`). A projection reads a single token, so the compiler
refuses one whose capture can cover more than one token or can match more than
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
Analyzer paths address text after the declared normalization policy. CoNLL-U
uses the reconstructed surface text and omits offsets when `# text`
does not match. Words sharing a multiword surface token each address its whole
surface range; their internal character boundaries are not inferred. A run
declares `char_offsets`, and `counters.offsets_omitted` counts published
records whose alignment is unavailable. Token-index `span` and the TSV columns
are unaffected. These offsets are not raw-file byte offsets; annotation export
must verify an explicit mapping to the target text view. The bounded CoNLL-U
consumer in
[`examples/soranoha_annotations.py`](../examples/soranoha_annotations.py) checks
that mapping and preserves incomplete execution as a partial or failed layer.

### Explaining one record: `--trace-policy N`

The per-statement counters above summarize a whole run. `--trace-policy N`
reports, for at most `N` published records, which policy statements shaped
each record or removed it:

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
`dropped`, or the new span a `detach` produced, with the count of captures that
span dropped when it is not zero. A record whose lines all read `not selected`
was left unchanged because no statement selected it.

- A dropped record's lines stop at the statement that dropped it; later
  statements did not see it.
- Reaching `N` is reported on the last line.
- The trace goes to stderr and is printed regardless of `--progress`. JSONL and
  TSV records are unchanged by the flag.

`--trace-policy` is refused with `--raw-matches`, because raw publication
applies no policy. Unlike `--plan`, which validates the invocation without
scanning, `--trace-policy` scans and reports per record. Long statement text is
shortened on the trace line; compile diagnostics always quote the statement in
full.

`matches`, `summarize` and `sentence-final` all accept `--trace-policy` and
report the per-statement counters. `explore` has neither, because output
written beneath the TUI would not be visible.

On `summarize` and `sentence-final` the trace covers the policy program only.
Each command applies further filtering of its own (the sentence-final
predicate, the empty-surface skip, an unprojectable key), so a record whose
trace ends in `kept` can still be absent from the output.

### What `summarize` groups by

`summarize` takes its grouping value from the first of these the record has,
each coarser than the one above it:

1. **The declared `key:`,** represented as an ordered typed field tuple. In
   query/v2 JSONL it is a `group` of kind `key` with a `fields` array;
   summarize-TSV/v1 renders it as a compact name-to-value JSON object in the
   fixed `group` column. Field names and source order are part of the grouping
   identity in both encodings.
2. **The rendered `unit`,** when a transform produced one.
3. **The authored lexical term,** for an unkeyed lexicon occurrence. Lexicons
   refuse transforms, so their reachable order is declared key, term, surface.
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

### Counting authored word lists

Use `lexicon target_words by [text, norm] := {"大学院", "行う"}.` when a term
may span an unknown number of morphological tokens. `summarize` groups by the
authored term. Add `key: [term, segments(text)]` to count observed
segmentations per term.

Query JSONL includes `lexical: {term, matched_by}` and list-valued key fields.
`matches --tsv` refuses lexicon catalogs because its fixed headerless row has
no place for variable-length `matched_by`; use JSON or human output.

### Cross-catalog drift

`check --compare-with FILE` reports definitions that share a name across
catalogs but whose bodies differ. The flag is repeatable and requires
`--patterns`, because an inline `--pattern` has no catalog name to report.

```bash
ling check --patterns patterns/sfe.lx \
  --compare-with patterns/sfe.lx \
  --compare-with patterns/sf-experimental.lx
```

- **Drift is a notice by default.** `check` exits `0` even when every shared
  name diverges, unless `--fail-on-drift` is passed.
- **Name-level.** The report names each divergent definition and which
  catalogs carry each distinct body. JSON also carries `comparison`: all
  definitions, including one-sided and unchanged definitions, with body-group
  IDs, visibility and reference-expanded, formatted declarations.
- **Reference-closed.** Bodies are compared after resolving references, so a
  definition whose own text is byte-identical across two catalogs still drifts
  when a fragment it references differs.
- **Metadata-insensitive.** Metadata never participates in body identity.
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
addition. It separately flags emitting-definition metadata changes. Expanded
declarations are display text. Body identity follows the drift comparator's
metadata-insensitive rules, so publication policy can change results without
changing a body.

`--corpus` compares two complete scans of the same prepared artifact. `--examples`
uses the union of both catalogs' example/counterexample texts with Vibrato;
zero or malformed claims fail instead of claiming coverage. Behavioral counts
report published records exclusive to each revision, preserving multiplicity,
captures, graph bindings, lexical identity, keys, tags and character positions.
Stream sequence and query-dependent token/context projections are excluded from
identity. Both runs' identities, counts and completion evidence accompany the
report. Incomplete or failed scans publish no report. Memory use does not grow
with the corpus. Equal output on one corpus or on the examples does not prove that
the two catalogs are equivalent or linguistically valid.

### Attributes the corpus cannot supply

A catalog may reference an attribute the bound corpus's token schema does not
carry, such as `[unidic.cform~連用形]` over CoNLL-U or `[dep.rel=nsubj]` over
Vibrato/UniDic. That is not an error: the attribute binds as universally
absent, which is per-token absence applied to every token. Positive tests over
it match nothing, `!=`/`!~`/`not in` match nothing either, and only the true
complement `!(…)` holds. A `matches` or `summarize` run reports each such
attribute as a `warning[attribute_unavailable]` on stderr, regardless of
`--progress`, naming the referencing definitions and the corpus's token
schema.

Two bind-time refusals exit `2` as invalid invocations: a dependency `where`
clause whose `dep.head`/`dep.rel` the corpus cannot supply, and a key the
schema carries under a conflicting value kind.

`check --schema SCHEMA` reports the same findings without a corpus, against a
named declared schema: `unidic-morphology`, `sudachi-morphology`, `tokenize`
(surface-only), or `conllu`.

These name schemas, not analyzers. A morphology layout is determined by the
dictionary that supplies the feature columns, so `unidic-morphology` is what
`--analyzer vibrato` produces over any of the packaged UniDic dictionaries. A
catalog can declare both the analyzer and the dictionary with
`analyzer: { family: "vibrato", resource: "unidic-cwj" }`.

```bash
ling check --patterns patterns/ud-dependency.lx --schema conllu
```

JSON output carries a `binding` object listing each unavailable attribute with
its referencing definition sites; human output prints one line per attribute.
Unavailable attributes never change the exit status; a refusal (a
`where`-bearing catalog against a schema without `dep.*`) is reported as
`refusal` and fails the check. `doctor` reports the same finding against a real
corpus; see
[Discovery, explanation, and completion](#discovery-explanation-and-completion).

## Scope conditioning with `--where`

`--where` restricts the sentences presented to matching by document and
sentence metadata, authored divisions, and the section labels active at each
sentence. One clause applies uniformly to the whole catalog; there is no
per-pattern form. It is available on `matches`, `summarize` and `coverage`.

```bash
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
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
is a parse error naming the grouping to supply. Outside `not within`, negation
is per predicate, through `!=`, `not in` and `not contains`. Values are bare
tokens, with `"…"` available for a value containing a delimiter or a space.

### Section predicates

`@section` tests the labels active at a sentence:

```bash
ling matches --patterns patterns/sfe.lx --dict-path "$DICT" \
  --where '@section in {methods, results} & @document.year >= 2010' \
  markdown-corpus/
```

| Written | True when |
|---|---|
| `@section = methods` | `methods` is active |
| `@section != methods` | `methods` is not active |
| `@section in {a, b}` | at least one of `a`, `b` is active |
| `@section not in {a, b}` | neither is active |

Ordered comparison and `contains` are refused: a label is an opaque string with
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

`--where '@section …'` selects from sentences that have already been read.
`--md-include-sections` and `--md-exclude-sections` determine at read time which
lines become sentences. The default policy excludes `references` and ten
other labels, so `--where '@section = references'` is empty unless
`--md-exclude-sections ""` is passed. A run whose clause names such a label reports this
and names the flag. See [the dialect contract](markdown-corpus-dialect.md).

`--md-recover-conversion-artifacts` is also a read-time flag. It admits the
artifacts a corpus converter emits (presentational HTML, `<si …>` notation, and
div imbalance at the document boundary), which the strict default refuses. Use
it when a tree fails to build on a few documents out of many; a malformed fence
or an unterminated code block remains an error either way. The flag is part of
`reader_tag`, so a recovered read names a different prepared artifact than a
strict one over the same tree. `lingrep-web` always reads with recovery, so one
`.md` tree can have two artifacts.

A scoped run over an artifact reads only the token blocks that contain an
admitted sentence. Other blocks are not read, hashed or decompressed, and an
excluded sentence's stored size is not cross-checked against the residency
index. A scoped run is therefore not an integrity check; use
`corpus inspect --verify` for whole-artifact verification. A sentence too large
for `--max-tokenized-bytes` does not fail a run that excludes it.

A `@section` clause over a corpus that records no sections is refused before
processing: JSONL input, or an artifact whose section coverage is empty. An
empty label set there cannot distinguish "in no labelled section" from "this
format records none". A label the corpus does not happen to record only warns,
because one catalog can run over several corpora.

### What each operator accepts

| Operator | Rule | Shape mismatch when |
|---|---|---|
| `=`, `!=` | compares the value's text; a boolean renders `true`/`false` | the value is a list or a nested object |
| `<`, `<=`, `>`, `>=` | parses both operands as decimals, whatever type the value has | the value does not parse as a decimal (`pages: "47-56"`), or is a list or object |
| `in`, `not in` | set membership under the `=` rule | the value is a list or object |
| `contains`, `not contains` | membership in a list-valued key | the value is a scalar or object |

Ordered comparison parses rather than dispatching on type because a stored
numeric value may be a string: `year` can be `"2013"` rather than `2013`. A literal in the clause that is not a decimal (`year >= abc`) is
refused before the run starts; a stored value that does not parse is a
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

Document and sentence counts can diverge. Under a section clause a document is
counted admitted when its metadata did not exclude it, even if none of its
sentences survives, so a run may report one admitted document and zero
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

A `--limit`ed direct run stops with its reader ahead of its matcher: the reader
crosses document boundaries to fill a batch while the run can stop inside an
earlier partition, so its scope counts describe neither the corpus nor the
matched prefix. A prepared run's universe is precomputed from the whole
artifact and does not shrink when the run stops early.

### Ordinals, and what a malformed artifact does

Direct input compacts ordinals: a scope-skipped sentence consumes no sentence
id, as a skipped blank line does. A prepared artifact never renumbers: an
ordinal is artifact identity, so a match names the same sentence it would have
named in an unscoped run.

A prepared document carrying no stored metadata is counted as a missing
field. Stored metadata that does not parse, or that the typing rule rejects,
violates an artifact invariant: the run fails, naming the document. A value of
the wrong shape is counted, not fatal.

## Sampling a corpus with `--sample`

`--sample RATE` on `matches` and `summarize` reads a fraction of the corpus,
at proportionally lower cost. It is off by default.

```bash
# A quarter of one Soranoha author archive (see "Soranoha bulk archives"),
# at the default seed.
ling --json summarize --patterns patterns/sfe.lx \
  --dict-path "$DICT" --sample 0.25 \
  --input-format soranoha soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip

# The same rate, a different sample.
ling --json summarize --patterns patterns/sfe.lx \
  --dict-path "$DICT" --sample 0.25 --sample-seed 7 \
  --input-format soranoha soranoha-Akutagawa_Ryunosuke-000879-plaintext.zip
```

`RATE` is a decimal greater than 0 and at most 1. `--sample 1.0` is valid and
selects everything; `--sample 0` is refused because it would admit nothing and
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

### What sampling is refused with

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

File and directory roots are inferred. Directory traversal is recursive and
deterministically ordered. Symlink and hard-link aliases remain distinct
logical documents; targets outside the root are admitted and marked. Preview
that contract with:

```bash
ling corpus inventory --json --progress always corpus/ >sources.jsonl
```

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

Direct filesystem roots resolved as Markdown receive one recovery behavior in
`matches`: an admitted sentence larger than `--max-sentence-bytes` is skipped,
the scan continues, and the command exits 1 with diagnostic code
`oversized_sentences_skipped`. The skipped sentence retains its input ordinal,
so later match locations contain a visible gap. JSONL and TSV may therefore
publish later evidence but publish no `complete` record for the degraded run;
human `--progress always` still prints processed sentence and match totals
before the terminal diagnostic. Extensionless filesystem roots use Markdown
inference and receive the same behavior.

Stdin, JSONL, plaintext, and every other command over direct input stop at the
first oversized sentence with `sentence_too_large`. `summarize` publishes no
total that silently omits a sentence.

Prepared input recovers in `matches` and `summarize` alike: a sentence over
`--max-tokenized-bytes` is omitted and counted, and the run exits 1 with
`oversized_sentences_skipped` and no `complete` record. Stopping at the first
oversized sentence would make an already-built artifact unqueryable at that
limit.

Human progress defaults to `auto`. Pass `--progress always` or `never`
explicitly; it never changes stdout or identity. Select machine-readable errors
independently with `--json`.

Direct commands resolve their dictionary from an explicit `--dict-path` first,
then from `LINGREP_DICTIONARY_PATH`. If neither is present, invocation fails with
exit status 2 before publishing stdout. Prepared commands never consult that
environment variable.

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

The portable columns are `text` (FORM), `lemma`, `upos`, and `xpos`.
Dependencies are `dep.head`, `dep.rel`, and the string-set `dep.enhanced`.
Known UD features are string-set `feat.*` attributes; all original FEATS and
MISC entries are additionally retained in `conllu.feats` and `conllu.misc`.

A dependency clause may classify a captured head by an identity-free dependent
predicate. This emits the one-token verb once even when several objects satisfy
the test:

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
`node.kind`, `node.id`, `text`, `lemma`, `upos`, and `xpos`; a named edge
projects `$edge.rel`. The match JSONL `graph_bindings` field preserves these
assignments in canonical node-name then edge-name order, and the browser hit
inspector displays them. Complete DEPS observation is required, so a
morphology-only prepared corpus is refused at binding and unavailable or
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
one plaintext file per work and `catalog.csv` at its root. Declare the archive
format with `--input-format soranoha`; a `.zip` is not inferred, because a ZIP
names a container rather than a format:

```bash
ling matches \
  --dict-path "$DICT" \
  --patterns patterns/sfe.lx \
  --input-format soranoha \
  --where '@document.author = "芥川 竜之介" & @document.ndc = "NDC 913"' \
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
| bibliographic | `title`, `subtitle`, `title_reading`, `author`, `orthographic_style`, `ndc`, `first_published`, `source_edition_title`, `source_edition_publisher`, `source_edition_year`, `url`, `doi` |
| size and locator | `body_character_count` (number), `filename` |

`body_character_count` and `source_edition_year` are numbers, so ordered
comparisons work on them; every other column is a string, `identifier`
included, so its leading zeros survive. An empty field is absent rather than
an empty string: `@document.subtitle != …` reports a missing field, not an
empty one. A column outside the pinned set is carried as string metadata. A
missing pinned column, a row without `identifier`, `filename` or `release`, a
duplicate filename or identifier, and rows with conflicting release values are
refused with the row named.

The text is the plaintext projection: ruby readings and apparatus are absent,
Aozora Bunko line breaks and source indentation are preserved, and sentences
are framed with the same rule as `plain`, so the same member read as `plain`
admits the same sentence texts. Paragraph structure is reported unavailable.
A TEI archive is refused by name; use the plaintext archive.

One work is one input record, so `--max-record-bytes` (default 1 MiB) bounds a
whole work. The largest works in the full corpus exceed that default, so raise
it for a whole-corpus run:

```bash
ling corpus build \
  --dict-path "$DICT" \
  --input-format soranoha \
  --max-record-bytes 4194304 \
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
keys beside `text`; repeating a key inside `metadata` is refused. Each metadata
record accepts at most 256 fields and 1 MiB of encoded JSON.

Authored structural metadata uses explicit whole-sentence ranges:

```json
{"sentences":["猫。","犬。"],"structure":{"observed_kinds":["part","section"],"divisions":[{"id":"p1","kind":"part","start":0,"end":2,"metadata":{"title":"Introduction"}},{"id":"s1","kind":"section","parent":"p1","start":0,"end":1,"metadata":{"role":"methods"}}]}}
```

Ranges are half-open in source-sentence coordinates. Parent IDs must name an
earlier division in the same document whose range contains the child. Empty,
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
Unavailable kinds are refused with a reason. Nested `within` is unsupported.
All token patterns remain sentence-local. The structure budget is 256 active
divisions, 64 parent levels, 256 observed kinds per document and 16 MiB of
active division metadata; derived sentence labels have a separate 16 MiB
byte cap. Prepared storage retains metadata once per division. Sequential
metadata pages are capped at 4 MiB; random provenance/projection pages at
64 MiB of source metadata and 64 MiB of unique division metadata. An oversized
requested page is refused rather than dropping fields or entities.

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
  ling matches --input-format jsonl --patterns patterns/sfe.lx -
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
ling summarize --cache --patterns patterns/sfe.lx opus-en-ja.jsonl
```

`--output` publishes either format atomically and refuses to replace an
existing file. Pin a commit with `--revision` when the materialized bytes must
be reproducible; an unpinned direct stream follows the repository's current
default revision. On NixOS, invoke the adapter inside `nix develop`; the shell
supplies the C++ runtime search path required by the locked Arrow/NumPy wheels.

## Choose an analyzer

Direct `matches`, `summarize`, `tokenize`, and `coverage` runs, plus
`corpus build`, accept three analyzer families. Vibrato is the default.
Direct `explore` and `sentence-final` do not expose alternate analyzer
selection.

The common analysis choices are:

| Option | Values | Meaning |
| --- | --- | --- |
| `--normalization` | `nfc`, `none` | Normalize input before analysis; default `nfc` |
| `--analyzer` | `vibrato`, `sudachi`, `external` | Select the token producer; default `vibrato` |
| `--profile` | `morphology`, `text` | Retain full morphology or token text only; default `morphology` |
| `--segmentation` | `input`, `analyzer` | Preserve input sentence units or accept analyzer sentence boundaries; default `input` |

These choices are semantic. A prepared artifact records them, and changing one
selects a different artifact rather than reinterpreting stored tokens. Prepared
queries reject analyzer and dictionary options because their tokens are already
fixed.

### Use Vibrato with UniDic

Vibrato requires `--dict-path` or `LINGREP_DICTIONARY_PATH`:

```bash
ling tokenize \
  --analyzer vibrato \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  corpus.txt
```

Choose the UniDic edition for the text domain; see
[`DICTIONARIES.md`](../DICTIONARIES.md). `--dict-path` is invalid with the other
analyzers.

### Use native Sudachi

Sudachi requires a dictionary, its resource directory, and an explicit split
mode. Inside `nix develop`, the shell exports paths for the pinned resources:

```bash
ling tokenize \
  --analyzer sudachi \
  --sudachi-dict "$LINGREP_SUDACHI_DICT_CORE" \
  --sudachi-resources "$LINGREP_SUDACHI_RESOURCES" \
  --sudachi-split c \
  --sudachi-edition core \
  corpus.txt
```

The split mode (`a`, `b`, or `c`) changes token boundaries. Keep
`--sudachi-edition` aligned with the selected small, core, or full dictionary;
the edition is recorded as part of the analyzer identity.

Outside the development shell, build the resources explicitly:

```bash
sudachi_resources="$(nix build .#sudachi-rs \
  --no-link --print-out-paths)/share/sudachi/resources"
sudachi_dictionary="$(nix build .#sudachidict-core \
  --no-link --print-out-paths)/share/sudachi/system_core.dic"
```

### Use an external protocol adapter

An external analyzer must be an executable that implements the bounded
`lingrep.external-morphology/v1` JSONL protocol. Pass its executable separately
from each argument so no shell command string is interpreted:

```bash
ling tokenize \
  --analyzer external \
  --external-executable /absolute/path/to/adapter \
  --external-arg adapter-option \
  --profile morphology \
  --segmentation input \
  corpus.jsonl
```

The adapter handshake declares its schema, resource identity, and supported
profile and segmentation combinations. An incompatible request fails before
corpus output is published.

From a checkout inside `nix develop`, this runs the repository's pinned French
Stanza adapter and model:

```bash
stanza_python="$(uv run --project python/stanza-adapter --frozen \
  --extra cpu python -c 'import sys; print(sys.executable)')"
stanza_model="$(nix build .#stanza-model-fr-gsd-morphology \
  --no-link --print-out-paths)/share/stanza/fr-gsd-morphology"

ling tokenize \
  --analyzer external \
  --external-executable "$stanza_python" \
  --external-arg=python/stanza-adapter/bin/lingrep-stanza-adapter \
  --external-arg=--resource-root \
  --external-arg="$stanza_model" \
  --external-arg=--language \
  --external-arg=fr \
  --external-arg=--backend \
  --external-arg=cpu \
  --external-arg=--batch-size \
  --external-arg=1 \
  --profile morphology \
  --segmentation input \
  corpus.txt
```

The repository's spaCy and Stanza adapters live under `python/`.

## Reusing and preparing a corpus from a query

`matches`, `summarize`, `tokenize`, and `coverage` analyze a filesystem ROOT
directly by default. Add `--cache` to keep a reusable artifact on disk:

```bash
# Direct: reads no neighboring artifact and writes none.
ling summarize --patterns patterns/sfe.lx corpus.jsonl

# Cold cached: prepares the reusable artifact, then answers from it.
ling summarize --patterns patterns/sfe.lx --cache corpus.jsonl

# Warm cached: finds the same artifact.
ling summarize --patterns patterns/sfe.lx --cache corpus.jsonl
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
same refusal to overwrite. `corpus build` and `--cache` queries reuse artifacts
published by one another.

Stdin has no reopenable logical name and rejects `--cache`. On a scoped first
run, preparation admits the complete corpus and `--where` is then applied to the
prepared read; the cached artifact never embeds the selection.

The cache search validates and reads ROOT under a build's bounds and diagnostics,
so an input a direct query handles can still make `--cache` fail. That failure is
reported; the run does not fall back to direct execution. An explicitly
addressed `--corpus` is likewise strict.

`--progress always` reports which path answered. The `run` record distinguishes
`prepared`, with the artifact's source and analysis identity, from `direct`.

## Prepared V1 corpora

```bash
ling corpus build --dict-path "$LINGREP_DICTIONARY_PATH" \
  --output corpus.lingcorpus corpus/
ling corpus inspect corpus.lingcorpus
ling corpus inspect --documents --verify corpus.lingcorpus
ling --json corpus inspect --metadata corpus.lingcorpus
ling matches --corpus corpus.lingcorpus --patterns patterns/sfe.lx
ling summarize --corpus corpus.lingcorpus --patterns patterns/sfe.lx
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
directory first; `ling` has no cleanup or expiration command.

`--output` is optional. Omitted, `corpus build` publishes to the canonical
content-addressed path beside ROOT,
`<root>.g<generation>.a<identity-scheme>.<analysis-identity>.<trust>.lingcorpus`,
for example `.g2.a1.….locally-executed.lingcorpus`. Artifact generation and
identity derivation are separate version axes. The required manifest field
`format_generation` identifies which artifact reader applies.

Analysis identity covers the analyzer's complete semantic configuration, not
only the dictionary, so two builds that differ in Sudachi split mode or
segmentation mode name two artifacts. The dictionary fingerprint is recorded
for inspection.

A second `corpus build` on unchanged input reuses that artifact without
rebuilding and prints the same recorded inventory the publishing run printed,
apart from the elapsed time; under `--json` the reuse carries its own fresh
`run_id`, as every artifact report does. Changed input names a new sibling
artifact and never touches the existing one.

Reuse does not test whether the canonical path exists. `corpus build` scans the
`.lingcorpus` directories beside ROOT, reads each bounded manifest, and compares
the recorded source, reader, admission, analyzer semantics, token schema and
trust against the request. Only `g2.a1` artifacts are candidates.

Reuse also requires the candidate to open: a sibling whose token or location
component is missing or truncated is not reused. Reuse does not fully hash the
candidate; that is `corpus inspect --verify`. An unreadable sibling is reported
and skipped rather than failing the run or hiding the realizations beside it.

A directory at the canonical path whose recorded identity does not match is a hard
error naming that path and the differing fields, not a rebuild. An explicit
`--output` is taken as spelled, is never pre-scanned, and uses the same
no-replace commit.

`corpus build` truncates an oversized sentence rather than refusing it. A
sentence longer than `--max-sentence-bytes` is cut to fit, and the run prints
`warning[sentence_truncated]` naming every one: document, sentence ordinal,
bytes retained of bytes seen. Truncating rather than skipping keeps the
sentence present, because a prepared artifact addresses sentences by ordinal
and dropping one would renumber every later sentence.

The bound is the smaller of the configured limit and the analyzer's limit. Vibrato has no
maximum input length, so there `--max-sentence-bytes` applies alone. Sudachi
accepts at most 49,149 bytes (`u16::MAX / 4 * 3`, because its offset tables
are `u16`), which is below the 65,536-byte default. A build clamps to whichever
is smaller and reports which one applied.

The manifest records truncation as `sentence_truncation: {count, limit}`. A
later build requesting a different `--max-sentence-bytes` is refused rather
than given the truncated corpus:

```text
the artifact at … truncated 2 sentence(s) at --max-sentence-bytes 65536, and
this run specifies 262144, so it is a different corpus under the same identity;
rerun with --max-sentence-bytes 65536, or remove that artifact, or pass an
explicit --output
```

Analysis identity does not include the limit, so both builds would publish to
the same path, and publication never overwrites.

### Discovering realizations

```bash
ling corpus variants corpus.md.g2.a1.<identity>.locally-executed.lingcorpus
ling corpus variants --json --limit all corpus/
```

`corpus variants` reports every prepared realization beside one source. Given
an artifact, every difference is expressed relative to it; given a source,
realizations are reported on their own terms and grouped by the source content
each records. It only reports: it never builds, selects, or deletes a
realization.

"Beside" means the target's parent directory, for every kind of target. A
realization is named with the source's own file name plus a suffix, so
`corpus/` publishes to `corpus.g2.a1.<identity>.<trust>.lingcorpus` next to
`corpus/`, never inside it. This is why the second example above names a
directory. Scanning a directory that is not itself a corpus root reports on
that directory as a root, which usually finds nothing.

Each difference is a named dimension: `source`, `reader`, `admission`,
`analyzer`, `token_schema`, or `trust`. Reading one tree under two reader
policies can produce two valid artifacts without an analyzer difference.

`--limit N` caps how much detail is shown, never what is counted: the
completion record always carries the complete total. The stream contract is
`lingrep.variants/v1`.

Whether an artifact built by `ling` is also the one `lingrep-web` prepares
depends on the input format, because the name includes the reader identity. For
a JSONL or plaintext root the two tools read identically and one artifact
serves both. For a pandoc Markdown root, `lingrep-web` reads with
conversion-artifact recovery enabled and `corpus build` reads without it; that
setting is part of the reader identity, so the two tools name different
artifacts for the same tree and each prepares its own.

Omitting `--output` costs one extra read-and-hash pass over the source before
the build, because the destination name is derived from the source's observed
identity together with the dictionary and reader identities; a different
dictionary or section policy therefore names a different artifact. That pass
tokenizes nothing, and `corpus build` prints no progress for either pass.
Passing `--output` skips it. The default publishes and stages the artifact
beside ROOT, so the directory containing ROOT must be writable: for a directory
root that is its parent, not the root itself. A read-only location is reported
late, after the dictionary load and the whole preflight read, because staging
is created only once the destination is known; pass `--output` to write
elsewhere.

The canonical path derives from ROOT as spelled, not from a resolved path;
`lingrep-web` also uses the unresolved spelling. So `corpus build corpus/` from
`/data` and `corpus build /data/corpus` name the same artifact, while naming a
symlink and naming its target name two. A root with no file name of its own
(`.`, `..`, `/`) is resolved first.

Because the naming pass and the build pass read the source separately, a source
edited between them yields an artifact named from the older content while
recording the newer identity. Nothing is overwritten, and that artifact is
never reused by mistake: a run that names the same path again detects the
mismatched recorded identity and fails, while a run over the edited source names a
fresh sibling.

## Tokenization and interactive workflows

```bash
ling tokenize --dict-path "$LINGREP_DICTIONARY_PATH" --json corpus/
ling explore --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx corpus/
ling sentence-final --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx corpus/
```

`tokenize` defaults to a bounded human view, exposes the canonical JSONL
projection with `--json`, and offers the fixed `lingrep.tokenize-tsv/v1`
projection with `--tsv`.

### Attribute coverage: `ling coverage`

```bash
ling coverage --dict-path "$LINGREP_DICTIONARY_PATH" --json corpus/
ling coverage --dict-path "$LINGREP_DICTIONARY_PATH" --json \
  --patterns patterns/sfe.lx corpus/
```

`coverage` counts, for each attribute in the selected analyzer's token schema,
how many tokens have it **`present`** (set to a value other than `*`),
**`unspecified`** (set to `*`, which UniDic supplies to mean "does not apply
here"; a supplied value, so neither present nor absent) and **`absent`**
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
is refused together with a catalog, because the exclusion table has different
columns. Use `--json` for both tables.

## Discovery, explanation, and completion

```bash
ling describe matches
nix run .#lingrep -- describe matches
ling contracts
ling contracts lingrep.query/v2 --json
ling doctor --dict-path "$DICT" --patterns patterns/sfe.lx \
  --corpus artifact.lingcorpus
ling manpages --output ./ling-manpages
```

`nix run .` is the equivalent default-app spelling. Use direct `ling`
after installation or inside the repository development shell.

`describe [COMMAND]` reports a command's arguments together with examples and
the contracts that command exposes. The contract index does not include every
JSON Schema; selecting one contract returns its record schema together with
stream ordering, coverage, pipe, and exit semantics.

`doctor` always probes a dictionary, then independently probes an explicit
catalog and artifact. When a catalog and a corpus are both supplied and both
admitted, a derived **binding probe** follows: it binds the catalog against the
corpus's stored token schema, the same set a `matches` run binds, and names
every referenced attribute the corpus cannot supply (`ok`, with evidence),
while a dependency `where` clause the corpus cannot satisfy fails the probe
with the missing edge keys. Its corpus check is bounded structural admission,
not payload hashing; use `corpus inspect --verify` for full verification. Any
failed probe exits 1 after all requested results are published.

`--plan` is available on `matches`, `summarize`, `tokenize`, `coverage`,
`sentence-final`, `corpus build`, `corpus inspect`, `corpus inventory`, and
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
has a different meaning there: on `ling` it names a prepared V1 artifact to
query, and on `lingrep-web` it declares a corpus the running server offers.

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
`lingrep-web` prepares its own artifact, writing it beside the source file and
reusing it on the next start.

A declared artifact is read in place and nothing beside it is written. The
artifact records the reader that built it and no source is re-read, so
`lingrep-web` can serve a corpus format it cannot read itself:

```bash
# lingrep-web cannot read a BCCWJ distribution. It can serve one that was prepared.
ling corpus build --dict-path "$DICT" --input-format bccwj /data/bccwj
lingrep-web --dict-path "$DICT" \
  --corpus bccwj=/data/bccwj.g2.a1.<analysis-identity>.locally-executed.lingcorpus
```

A BCCWJ root passed directly to `lingrep-web` is refused with a message: the
C-XML reader is part of `ling`. Because artifacts are read in place rather than
owned:

- No reader configuration applies to an artifact. `--input-format` is refused
  for an artifact rather than ignored, because reader configuration is part of
  artifact identity. The Markdown section flags likewise do not apply to an
  artifact; they were applied when it was built.
- A declared artifact is never deleted. `lingrep-web` discards an artifact it
  built when that artifact is found corrupt, because the source is beside it
  and a retry rebuilds it. A declared artifact cannot be rebuilt from anything
  `lingrep-web` holds, so a corrupt one marks the corpus failed and the bytes
  stay on disk.

Prefer `--corpus NAME=PATH` over `--input` for an artifact: the caption is the
declared `NAME`, whereas `--input` captions a corpus by its file stem, and an
artifact's stem is the whole content-addressed name.

`NAME` is the key a shareable link carries, so it is constrained to
`[A-Za-z0-9_-]`, at most 64 characters; it requires no URL escaping and exposes
nothing about the filesystem. It is not a caption: a corpus file may be called
`日本語コーパス.jsonl`, which is a valid filename and an invalid key. A
repeated `NAME`, a missing `=`, and a name outside that alphabet are each
refused before the server starts. A duplicate error names the key and a
malformed-entry error quotes the whole argument, while an over-long or
invalid-character error reports only the offending character or the
64-character limit.

`default` is reserved and may not be declared: it is the key of the `--input`
corpus, and `--corpus default=…` exits 2 with that reason. A shareable link
carrying `k=default` resolves to the `--input` corpus.

`--input` itself is optional. The server requires at least one corpus from
either flag; the first declared one is the default that the bare `/` resolves
to. Declaring only `--corpus` entries therefore starts a server on which no
corpus is keyed `default`, and a link naming `k=default` finds nothing.

`--input-format` is refused once more than one corpus is declared, because it is
a single global override and would be applied to every path:

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
other declared corpus starts building the first time it is selected. One
process-wide permit admits one build at a time, so N declared corpora never hold
N decoded dictionaries at once.

**Declaring more corpora adds no query concurrency.** The server admits one
in-flight scan at a time, which uses the whole Rayon pool, so scans are
serialized server-wide across corpora, not per corpus.

A workbench is bound to one corpus when it is admitted and is never rebound, so
choosing a different corpus in the UI navigates to a new permalink rather than
re-pointing the open page. The selector is rendered only when more than one
corpus is declared. The browser receives each corpus's key and display name
(the file stem for the `--input` corpus, the declared `NAME` for a `--corpus`
entry) and never its path.
