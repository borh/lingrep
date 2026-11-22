# Comparing one query across UD and SUD

Compare the occurrences selected by two versions of a query for verbs that
govern an overt を-marked nominal. The Universal Dependencies (UD) and
Surface-Syntactic UD (SUD) patterns use different dependency paths because
the two schemes assign different heads. The comparison script requires
Python 3. [`examples/compare_paired_treebanks.py`](../examples/compare_paired_treebanks.py)

SUD_Japanese-GSD is generated from UD_Japanese-GSD by rule, so the comparison
tests the rewritten query against the conversion. It does not test either
annotation: no second annotator has judged these objects. UD_Japanese-GSD's
own `obj` label is assigned by rule to nominals marked with を
([Asahara et al. 2018](https://aclanthology.org/L18-1287.pdf), section 3.4),
and its word-level dependencies are converted from manually annotated bunsetsu
dependencies (treebank README).

## Inputs

| File | Release | SHA-256 |
|---|---|---|
| `ja_gsd-ud-test.conllu` | [UD_Japanese-GSD r2.19](https://github.com/UniversalDependencies/UD_Japanese-GSD/tree/r2.19) | `5f50ee6ed45c7ebda3787e593eaf5e9a225f25e53f6b4b32df778031862c843f` |
| `ja_gsd-ud-dev.conllu` | same | `18d266e29336ef619787a928d89d20f2e64188be70d5b37829e3aadc5bb6b841` |
| `ja_gsd-ud-train.conllu` | same | `99f67fd88257e7cfe81d81c4b8ee98aff85fc22bb525d907475a2856c8cfa9f3` |
| `ja_gsd-sud-test.conllu` | [SUD 2.19 archive](https://grew.fr/download/SUD_2.19/SUD_Japanese-GSD.tgz) | `4cda31c95bf894ec11ee7736f12bc3e128e8f0e1e64b90772bb33e7242c17be5` |
| `ja_gsd-sud-dev.conllu` | same | `072da3e37965713adb30e56aa7af07954711688a018126dc830d33792b7e5c1e` |
| `ja_gsd-sud-train.conllu` | same | `d68b2659c263c1ee586322265d362014e1e497b7a4ac4ac31ee8265163112207` |

The archive is `e79a90ca7e228f103c3ab64f94ce8fc399905757fa47180e72d05124d82c9f63`.
Its README states that the treebank "was automatically generated from the UD
treebank" r2.19. The [SUD data page](https://surfacesyntacticud.org/data/)
publishes no revision of the conversion rules for 2.19, so the archive hash is
the only pin on the SUD side. Keep both directories: the SUD files cannot be
regenerated to the same bytes from the UD files without that revision.

Within each split the two files hold the same `sent_id` values in the same
order and the same ID, FORM, LEMMA, UPOS and XPOS in every row. HEAD and DEPREL
differ, and SUD adds `ExtPos` and `Shared` to FEATS and `Idiom` and `InIdiom`
to MISC.

## The query on each side

UD attaches を to the nominal as `case` and the nominal to the verb as `obj`.
SUD 2.19 makes を the head of the nominal (`comp`) and the dependent of the
verb (`comp:obj`). Each pattern binds the nominal `n`, the particle `w` and the
verb `v`:

```bash
ling --json matches --evidence --limit all --source-metadata @sentence.sent_id --pattern \
  '(?<w> [text=を & upos=ADP]) where (?<n> [upos in {NOUN, PROPN, PRON, NUM}]) -[case]-> $w, (?<v> [upos=VERB]) -[obj]-> $n.' \
  UD_Japanese-GSD/ja_gsd-ud-test.conllu > ud.jsonl
ling --json matches --evidence --limit all --source-metadata @sentence.sent_id --pattern \
  '(?<w> [text=を & upos=ADP]) where $w -[comp]-> (?<n> [upos in {NOUN, PROPN, PRON, NUM}]), (?<v> [upos=VERB]) -[comp:obj]-> $w.' \
  SUD_Japanese-GSD/ja_gsd-sud-test.conllu > sud-direct.jsonl
```

A nominal that carries a second particle, as in 何かを, has both particles as
`case` dependents in UD. SUD nests them in word order, so the nominal is two
`comp` edges below を and a second pattern is required:

```bash
ling --json matches --evidence --limit all --source-metadata @sentence.sent_id --pattern \
  '(?<w> [text=を & upos=ADP]) where $w -[comp]-> (?<p> [upos=ADP]), $p -[comp]-> (?<n> [upos in {NOUN, PROPN, PRON, NUM}]), (?<v> [upos=VERB]) -[comp:obj]-> $w.' \
  SUD_Japanese-GSD/ja_gsd-sud-test.conllu > sud-via.jsonl
```

The SUD labels are those of release 2.19. Through 2.18 the complement of an
adposition was `comp:obj`
([SUD `comp` guideline](https://docs.surfacesyntacticud.org/guidelines/universal/deprel/comp/)),
and a relation label matches exactly, so `-[comp]->` finds no adposition
complement in an earlier release.

## Comparing

```bash
python3 examples/compare_paired_treebanks.py \
  --a-conllu UD_Japanese-GSD/ja_gsd-ud-test.conllu \
  --b-conllu SUD_Japanese-GSD/ja_gsd-sud-test.conllu \
  --a ud.jsonl --b sud-direct.jsonl --b sud-via.jsonl \
  --roles n,w,v > compare.tsv
```

For these patterns, the occurrence key consists of `sent_id` and the word ID
of each role in `--roles`. The script pools records from the `--b` runs. `compare.tsv` has one row per occurrence: `shared`,
`a_only` or `b_only`; the number of records on each side; the `sent_id`; the
role positions; each side's rows for the role words as
`role=FORM(ID,DEPREL→HEAD)`; and the sentence. A side without a record shows its
rows for the compared roles, so a one-sided row shows what the other annotation
has in those positions. Each side's record counts, `source_identity` and
`analysis_identity`, and the totals, go to stderr.

The script exits with status 2 before writing TSV rows if any of these checks
fails.

- Each sentence has a `sent_id`. IDs are unique within each file and both
  files contain the same IDs.
- For each sentence, the sequences of word IDs, FORM, and UPOS match.
- Each run is complete and unsampled and supplies match evidence and sentence
  IDs. The `--b` runs share the same corpus, analysis, and `--where` scope.
- Each run admits as many sentences as its file contains. Match records have
  the file's basename and a known `sent_id`. Where the file supplies `# text`,
  the evidence contains that sentence text.

Run each query over one file, not over a treebank directory.

## Results

`ling` 0.9.1, both SUD patterns pooled:

| Split | Sentences | を tokens | UD | SUD direct | SUD via particle | Shared | UD only | SUD only |
|---|---|---|---|---|---|---|---|---|
| test | 543 | 351 | 314 | 311 | 3 | 314 | 0 | 0 |
| dev | 507 | 331 | 293 | 289 | 4 | 293 | 0 | 0 |
| train | 7,050 | 4,658 | 4,154 | 4,063 | 69 | 4,132 | 22 | 0 |

With `--b sud-direct.jsonl` alone, test has three `a_only` rows: `test-s43`,
`test-s81` and `test-s155`. The row for `test-s81` shows that SUD has no edge
from を to 何 (one field per line, sentence omitted):

```text
a_only  1  0  test-s81  n=9 w=11 v=12
  n=何(9,obj→12) w=を(11,case→9) v=物語っ(12,acl→15)
  n=何(9,comp→10) w=を(11,comp:obj→12) v=物語っ(12,comp→13)
```

With both SUD runs, the row is `shared`. The SUD analysis column includes the
intervening か.

```text
shared  1  1  test-s81  n=9 w=11 v=12
  n=何(9,obj→12) w=を(11,case→9) v=物語っ(12,acl→15)
  n=何(9,comp→10) p=か(10,comp→11) w=を(11,comp:obj→12) v=物語っ(12,comp→13)
```

The three test rows informed the second SUD pattern. The dev and train runs
used the same patterns. On train, the UD pattern selected 22 occurrences that
neither SUD pattern selected. In 18, the path between the nominal and を contains
something other than a single `ADP`, such as a suffix tagged `PART` (凄惨さを,
`train-s1347`), the nominalizer の tagged `SCONJ` (`train-s1857`), a copula, or
two particles. In the other 4, another function word stands between を and the
verb, with を attached to that word as `comp` (`train-s682`).

The を-token column counts matches of `[text=を & upos=ADP].` on each side.
Those counts match. Both queries omit the same 37 を tokens on the test split.
Of these, 24 mark the object of a `NOUN` predicate. Four follow the last
conjunct of a coordinated object whose `obj` edge is on the first conjunct.
Six follow a host tagged `ADJ` or `VERB`. The remaining three are an object
of an `ADJ`, an `obl` nominal, and a `fixed` を. The shared count measures overlap between the selected
occurrences. An occurrence missed by both queries contributes no row.

## Controls

Comparing a run with itself returns every occurrence as shared:

```bash
python3 examples/compare_paired_treebanks.py \
  --a-conllu UD_Japanese-GSD/ja_gsd-ud-test.conllu \
  --b-conllu UD_Japanese-GSD/ja_gsd-ud-test.conllu \
  --a ud.jsonl --b ud.jsonl --roles n,w,v > /dev/null
# shared 314, a_only 0, b_only 0
```

Changing one FORM in a copy of the SUD file, here word 10 of `test-s81`, and
querying the copy stops the comparison before any row is written:

```text
compare_paired_treebanks: Word ID, FORM or UPOS sequences differ. First differing sent_id is test-s81 (1 affected).
```
