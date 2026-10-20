---
title: Independent annotation guide for the Harbor linguistic benchmark
type: guide
status: draft
summary: Two linguists independently identify every target span before an adjudicator freezes benchmark gold; neither annotator sees authored targets or the other annotation.
updated: 2026-08-22
scope:
  - ling versus MeCab Harbor evaluation
  - Japanese linguistic target spans and labels
refs:
  - benchmarks/harbor/catalogs/smoke.json
---

# Independent annotation guide for the Harbor linguistic benchmark

Two linguists must annotate each evaluation case independently. The benchmark owner
then adjudicates every disagreement before freezing the evaluation catalog. A model
run must not begin from annotations that have not completed this process.

The tracked smoke catalog demonstrates the format; it is not held-out evaluation
data. Keep candidate evaluation catalogs, completed annotations, comparisons, and
adjudication notes outside the agent-visible repository until the recorded run is
frozen.

## Annotators receive no authored targets

The benchmark owner gives each annotator only the generated review packet, the blank
annotation template, and this guide. Do not give annotators the source catalog,
another annotation, an oracle solution, or prior agent traces.

Annotators may consult linguistic references and use a character-offset calculator. They
must not treat either experimental workflow's output as the authority for the gold
answer. Record a case for exclusion when its linguistic distinction cannot be applied
consistently without information absent from the task.

Use an opaque reviewer identifier rather than a personal name or email address. The
two annotation records must use different identifiers.

## Annotate complete linguistic spans

For each case, read the phenomenon definition, declared `labels` vocabulary, and
every sentence before marking any target. Emit every and only span that satisfies
the definition. Every target label must be copied exactly from that vocabulary.

Each target contains:

- `sentence_ordinal`: zero-based sentence position in the case;
- `start_char`: inclusive Unicode code-point offset within that sentence;
- `end_char`: exclusive Unicode code-point offset within that sentence;
- `surface`: the exact text selected by that character range;
- `label`: the linguistic classification required by the phenomenon.

Offsets are zero-based and count Unicode code points, not UTF-8 bytes, UTF-16 code
units, grapheme clusters, or analyzer token positions. For example, `猫` occupies
range `[0, 1)` in `猫がいる。`, while `が` occupies `[1, 2)`. Sentences are
NFC-normalized before annotation; copy the surface from the selected range and do
not normalize it again.

Use `status: "annotate"` for a usable case. An empty `targets` list is a valid
negative annotation. Use `status: "exclude"` only when the task itself is unsuitable;
leave `targets` empty and explain the reason in `notes`.

Exclude rather than guess when the supplied context does not resolve a distinction,
the requested span boundary is underspecified, or accepted inflectional variants
are unclear. Record dictionary-version segmentation and feature evidence when the
case explicitly defines gold in terms of the installed analyzer; otherwise use tool
output as evidence rather than as the authority for a higher-level linguistic
judgment.

## Produce two blind annotation records

The benchmark owner prepares a packet from a candidate catalog:

```bash
cd benchmarks/harbor
python review.py prepare /private/eval-v1.candidate.json /private/eval-v1-review
```

The command writes `packet.json`, `annotation-template.json`, and a copy of this
guide. Give separate copies of that directory to both annotators. Each annotator
replaces the template's reviewer ID, status, targets, and notes without contacting
the other annotator about case decisions.

The template binds the exact packet, source catalog, and guide by SHA-256. Do not edit
`packet.json` or the guide after annotation begins. If either changes, discard both
annotations and prepare a new packet.

## Compare annotations before adjudication

After both records are returned, the benchmark owner runs:

```bash
python review.py compare \
  /private/eval-v1-review/packet.json \
  /private/annotator-a.json \
  /private/annotator-b.json \
  > /private/eval-v1-comparison.json
```

The comparison validates schemas, hashes, case coverage, character spans, surfaces,
unique targets, and distinct reviewer IDs. Target row order is not meaningful. Each
case receives one result:

- `agree`: both annotators supplied the same canonical target set;
- `excluded_by_both`: both independently rejected the case;
- `adjudication_required`: statuses or target sets differ.

The comparison does not choose a winner. An adjudicator reviews the task definition,
both rationales, and relevant linguistic evidence for every disagreement. Remove
cases excluded by the adjudicator. For retained cases, write the adjudicated targets
into a new final catalog and preserve the comparison plus adjudication notes beside
that private catalog.

## Freeze the reviewed catalog before model trials

Validate the final catalog with `generate_dataset.py`, then record its SHA-256 digest
and this guide's digest. Do not change the task wording, sentences, labels, or targets
after the experiment begins. A correction after trials requires a new catalog
identity and a reported regrade; it must not silently replace the original gold.

The annotation gate is complete only when two distinct records cover every retained
case, all disagreements have an explicit adjudication, and the final catalog passes
the generator's span validation.
