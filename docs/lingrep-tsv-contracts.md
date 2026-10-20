# `ling` TSV contracts

All TSV projections are UTF-8, headerless, and terminated by LF. Fields use
RFC-4180-style quoting when they contain TAB, LF, CR, or `"`; embedded quotes
are doubled. Rows never contain terminal styling. If stdout is closed, `ling` exits with a
nonzero status and writes no diagnostic; the rows read are not complete
evidence.

<!-- BEGIN GENERATED TSV CONTRACTS -->
| Contract | Command | Columns in order | Ordering | Coverage |
|---|---|---|---|---|
| `lingrep.attributes-tsv/v1` | `coverage` | `attribute`, `present`, `unspecified`, `absent`, `admitted_tokens` | TokenAttributeIndex::ALL order. | Every attribute, always. Exclusions require --json. |
| `lingrep.matches-tsv/v1` | `matches` | `sequence`, `pattern`, `source_name`, `document_ordinal`, `document_sentence_ordinal`, `corpus_sentence_ordinal`, `match_start`, `match_end`, `left_surface`, `surface`, `right_surface` | Canonical match order. | Complete evidence or the requested first-N prefix. |
| `lingrep.sentence-final-tsv/v1` | `sentence-final` | `pattern`, `surface`, `count` | Pattern then UTF-8 surface order. | Exact aggregate after successful execution. |
| `lingrep.sources-tsv/v1` | `corpus inventory` | `ordinal`, `logical_name`, `target_group_ordinal`, `external_target`, `target_multiplicity` | Stable source ordinal order. | Live-discovered or recorded inventory as selected by the command. |
| `lingrep.summarize-tsv/v1` | `summarize` | `pattern`, `rank`, `group`, `count` | Authored definition; count descending; group kind surface, rendered unit, lexical term, projected key; keyed field-name vector in authored order; keyed value vector. | Exact aggregate, optionally top-N per pattern. |
| `lingrep.tokenize-tsv/v1` | `tokenize` | `corpus_sentence_ordinal`, `source_name`, `document_ordinal`, `document_sentence_ordinal`, `token_index`, `orth`, `lemma`, `pos`, `cform` | Canonical corpus sentence and token order. | Complete token evidence unless the command fails. |
<!-- END GENERATED TSV CONTRACTS -->

For a projected key, the TSV `group` field is a compact JSON object whose
properties retain authored projection order. `lingrep.query/v2` JSONL
represents the same typed value as a `fields` array.

TSV has no completion row, so a consumer must reject the output of a run that
exits nonzero, even if rows were read.
