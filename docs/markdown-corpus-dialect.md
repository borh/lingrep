# The Markdown corpus dialect

`--input-format markdown` reads a specific dialect, not general Markdown. This
document is the contract. `lingrep-core` structured-corpus tests and `ling`
direct/prepared protocol tests enforce it against repository fixtures.

## Flags

`ling` (every input-taking subcommand) and `lingrep-web` accept the same
reader flags:

| Flag | Meaning |
|---|---|
| `--input-format markdown` | Read the input as this dialect. Without it, the format is inferred from the path; directories and extensionless files default to `markdown`. |
| `--md-include-sections`, `--md-exclude-sections` | Select lines by section label; see [Selecting sections](#selecting-sections). |
| `--md-self-labels` | Declare heading classes that label only their own heading; see [Self-labels](#self-labels). |
| `--md-recover-conversion-artifacts` | Accept a fixed set of converter artifacts; see [Conversion-artifact recovery](#conversion-artifact-recovery). Off by default in `ling`. `lingrep-web` always recovers, and the flag has no effect there. |

These flags decide what is read. They change which lines become sentences at
all and are part of the reader configuration (`MarkdownReaderConfig::reader_tag`),
so two runs that differ in any of them read two different corpora and produce
two different prepared artifacts. Query-time selection is separate and is
spelled `--where '@section …'`; see
[the CLI usage guide](cli-usage-guide.md#scope-conditioning-with---where).

The default exclusion list removes `references` and ten other labels before a
line becomes a sentence, so `--where '@section = references'` over a corpus
read with the defaults is always empty. A run whose `--where` clause names
such a label prints a warning that names the flag that admits it.

## Contract

- **One sentence per line.** The input is pre-split by the corpus author. The
  reader never splits or joins across lines. Unsegmented text is read with the
  plain-text reader, not with this dialect.
- **A blank line separates paragraphs.** The structured stream assigns stable
  paragraph identity; a paragraph may span heading or div boundaries because
  blank lines, not presentation structure, define it.
- **A leading `---` block is YAML frontmatter.** It is parsed as typed document
  metadata. A `---` anywhere else is a thematic break.
- **`:::+ {.classes}` opens a region; a bare `:::+` closes it.** This follows
  pandoc's rule that a fence without attributes is always a closing fence. A
  colon fence that is neither is an error, not an open region.
- **`#{1,6} text {.classes}` labels the section it introduces.** Headings are
  structure and are never emitted as sentences.
- **`{.class}` spans are structure; every other `{...}` is content.** LaTeX such
  as `{T \to \infty}` is preserved.
- **`<!-- ... -->` is skipped. Every other raw HTML form is an error**:
  elements, declarations, processing instructions and CDATA alike. Autolinks
  (`<https://…>`) are prose.
- **Fenced code is skipped.**
- **Unbalanced structure is an error**, at the fence and at end of input.

## Conversion-artifact recovery

`--md-recover-conversion-artifacts` accepts a fixed set of artifacts that
Markdown converters emit and that the strict read refuses:

- presentational HTML and `<si ...>` linguistic notation, which the strict
  read refuses as raw HTML;
- div imbalance at the physical document boundary: an unclosed div at end of
  input, and a closing fence that matches no open div.

A malformed colon fence, an unterminated fenced code block, and unbalanced
structure anywhere other than the document boundary remain errors under both
readings, because each of them would absorb the lines that follow it.

Recovery is part of `reader_tag`, so a recovered read and a strict read of one
tree are two different corpora with two different prepared artifacts.
`lingrep-web` always reads with recovery and `ling` defaults to the strict
read, so the same `.md` tree prepared by each has two artifacts.

## Section labels

Labels are opaque strings; the vocabulary is open. Two carriers contribute,
tracked independently:

| carrier | opens | closes |
|---|---|---|
| `:::` div | a fence with a class attribute | a bare fence |
| heading | `#{n} … {.class}` | the next heading at level ≤ n |

The active labels of a line are every open div's labels plus the labels of the
nearest labeled ancestor heading, counting only section labels, never
[self-labels](#self-labels).

Heading resolution skips classless headings: `### (1) 課題` under
`## 方法 {.methods}` leaves `methods` active. Labels of outer labeled headings
are not accumulated: a `## 引用文献 {.references}` under `# 5. 結語 {.conclusion}`
makes `references`, not `conclusion`, active. Heading level is not used to
interpret labels.

### Self-labels

A heading class is one of two things:

- a section label, which names the section the heading introduces and applies
  to every line until the next labeled heading (`{.references}` on an H2);
- a self-label, which describes the heading's own text and applies to no other
  line (`{.title}` on an H1).

A heading is never emitted as a sentence, so a self-label labels no emitted
line. The reader removes declared self-labels from a heading's classes before
the heading opens a section.

    --md-self-labels LIST   default: title, subtitle

The list replaces the default rather than adding to it, so a corpus that uses
`title` as a section label can declare that. An empty list is valid and
declares that every heading class introduces a section. (Contrast
`--md-include-sections ""`, which is an error.)

A self-label applies only to headings. `::: {.title}` still labels its content,
and that content is selected or excluded by `title` like any other div label
(`a_div_class_is_never_treated_as_a_self_label`). A heading that carries both a
self-label and a section label keeps the section label: `{.title .methods}`
labels its section `methods`
(`a_heading_mixing_a_self_label_and_a_section_label_keeps_the_section_label`).

Self-labels cannot be inferred from structure: `results` and `title` are both
carried mostly by headings, at every heading level, so the distinction is
declared.

## Selecting sections

    --md-include-sections LIST   default: every label
    --md-exclude-sections LIST   default: see below

A line is emitted when any active label is included and none is excluded.
Exclusion takes precedence.

An empty list means something different on each side:

- `--md-exclude-sections ""` disables exclusion. Use it to read unfiltered
  text.
- `--md-include-sections ""` is an error reported before the run starts. An
  empty include list would admit no line and read an empty corpus while
  exiting successfully, which is easy to reach from an unset variable in
  `--md-include-sections "$SECTIONS"`. Omit the flag to admit every label. A
  list that names no label after trimming (`"  ,  "`) is the same error.
  `SectionPolicy::new` accepts `Some(empty)`, so a library caller that
  constructs it directly reads an empty corpus.

A non-empty include list drops unlabeled lines: a line with no active label
matches no named label.

Labels named in a `--where '@section …'` clause that the input never carried
are reported at the end of the run. Labels named on `--md-include-sections` or
`--md-exclude-sections` are not checked against the input.

The default exclusion is bibliographic matter and figure/table apparatus:
`references, caption, figure, table, notes, author, author_bio, keywords,
submission_date, acknowledgments, toc`. Pass `--md-exclude-sections ""` for
unfiltered text.

Passing the flag replaces this list rather than adding to it, so
`--md-exclude-sections foo` excludes `foo` and admits `references`. List the
defaults explicitly to extend them.

### Selecting a self-label is a reported no-op

A self-label never labels an emitted line, so naming one on
`--md-include-sections` or `--md-exclude-sections` matches nothing. The run
prints a warning that names the labels and points to `--md-self-labels`.
