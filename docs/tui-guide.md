# Explore matches in the terminal

`ling explore` provides an interactive view of pattern frequencies,
surfaces, contexts, source definitions, and one-off sample matches. It uses the
same matcher and bounded corpus path as the batch commands; direct roots use
Vibrato analysis.

## Start the explorer

Choose exactly one catalog source and one corpus source. A reopenable filesystem
root is required unless `--corpus` names a prepared artifact. Unlike the batch
commands, `explore` rejects stdin because Surface and Context rescan the input.

```bash
ling dictionary install
ling explore --patterns patterns/sfe.lx corpus.txt
```

Use `--pattern` for one inline definition:

```bash
ling explore --pattern '[upos=VERB].' corpus.txt
```

Prepared artifacts already contain analyzed tokens and do not take a
dictionary:

```bash
ling explore \
  --patterns patterns/sfe.lx \
  --corpus corpus.lingcorpus
```

Direct `explore` sessions support the Vibrato analyzer only. A prepared artifact
may contain tokens produced by another analyzer. Use `matches`, `summarize`,
`tokenize`, `coverage`, or `corpus build` to select Sudachi or an external
analyzer; see [Choose an analyzer](cli-usage-guide.md#choose-an-analyzer).

## Move between views

The normal viewing mode has five tabs:

| Tab | Contents |
| --- | --- |
| Frequencies | Match count for each emitting definition |
| Surface | Surface-form distribution for the selected definition |
| Context | Key word in context (KWIC) rows for the selected definition |
| Parsed | The current lingrep patterns source text |
| Playground | A pattern and sample-text form for one-off matching |

Outside the Playground, `Tab` and `Shift+Tab` move forward and backward through
the tabs. The status line at the bottom shows the current mode, active work,
selected pattern, and available keys.

### Select a pattern

Select a pattern in the Frequencies tab:

| Key | Action |
| --- | --- |
| `j` / `k` | Select the next or previous pattern |
| `Enter` | Open the selected pattern in Surface |
| `s` | Cycle the frequency sort mode |
| `Up` / `Down` | Scroll the visible rows |
| `PageUp` / `PageDown` | Scroll by 20 rows |

The selection remains active when changing tabs. Return to Frequencies with
`Tab` or `Shift+Tab`; there is no separate back key.

### Inspect surfaces and contexts

Surface and Context start their queries when their tab becomes active and a
pattern is selected. Use `Up`, `Down`, `PageUp`, and `PageDown` to scroll.

Press `d` to toggle the detail panel. In Context, the selected KWIC row follows
the scroll position. Pressing `Enter` outside Frequencies does not open the
panel; `d` is the panel control.

### Read the current source

Parsed displays the current lingrep patterns source under “Pattern Definition.” It is a
source view, not an abstract syntax tree. Press `e` to edit that source.

## Edit and rerun the catalog

Press `e` from a normal tab to enter the pattern editor. The editor is a
single-line text field: printable characters append to the source and `Backspace` removes
the final character. It does not provide cursor movement, multiline editing,
syntax highlighting, or command history.

| Key | Action |
| --- | --- |
| `Enter` or `Alt+Enter` | Compile the draft and rerun matching |
| `Esc` or `Tab` | Leave editing without running the draft |
| `Ctrl+C` | Cancel active pattern matching |
| `Ctrl+Q` | Exit the explorer |

Enter a complete lingrep patterns document in the editor. For example:

```pattern
fragment verb := [upos=VERB].
pattern final_verb := @verb $.
```

When `--patterns FILE` is used, the explorer also watches that file and reloads
it after an external edit.

## Test a pattern in the Playground

The Playground matches a complete lingrep patterns document against one sample string
without scanning the corpus again.

1. Use `Tab` or `Shift+Tab` to switch focus between Pattern and Sample.
2. Type into the focused field; `Backspace` removes the final character.
3. Press `Enter` to tokenize the sample and display published matches.
4. Press `Esc` to return to Frequencies.

For example, enter `pattern verb := [upos=VERB].` as the pattern and
`猫が走る。` as the sample.

Playground samples run only in direct mode. A session opened with `--corpus`
can browse the prepared tokens, but running a Playground sample reports that
the artifact does not retain a tokenizer.

## Cancel work, dismiss errors, or exit

| Mode | Key | Action |
| --- | --- | --- |
| Normal viewing | `Ctrl+C` | Cancel the active pattern, surface, or context task |
| Normal viewing | `q` | Exit |
| Editing | `Ctrl+Q` | Exit |
| Playground | `Ctrl+Q` | Exit |
| Error dialog | `Enter` or `Esc` | Dismiss the error and return to Frequencies |
| Error dialog | `Ctrl+Q` | Exit |

The mouse wheel scrolls the current view. Mouse clicks do not select tabs or
rows.

## Troubleshoot startup and display problems

If the dictionary cannot be loaded, verify it independently before reopening
the TUI:

```bash
ling doctor --dict-path "$LINGREP_DICTIONARY_PATH" \
  --patterns patterns/sfe.lx
```

If a full corpus is slow to reopen during repeated exploration, prepare it once:

```bash
ling corpus build \
  --output corpus.lingcorpus \
  --dict-path "$LINGREP_DICTIONARY_PATH" \
  corpus.txt
ling explore --patterns patterns/sfe.lx --corpus corpus.lingcorpus
```

If the screen is garbled, use a Unicode-capable terminal with a valid `TERM`
setting. The TUI requires an interactive terminal; use `matches` or `summarize`
when output must be redirected.

For input formats, caching, and corpus preparation, see the
[CLI usage guide](cli-usage-guide.md). For lingrep patterns syntax, run
`ling check --help`.
