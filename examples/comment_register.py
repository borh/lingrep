#!/usr/bin/env python3
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#   "tree-sitter-language-pack>=1.20,<2",
#   "markdown-it-py>=4.2,<5",
#   "mdit-py-plugins>=0.6,<1",
#   "spacy>=3.8.11,<3.8.12",
#   "spacy-transformers>=1.3,<2",
#   "en-core-web-trf",
#   "cupy-rocm-7-0==14.1.1",
#   "torch==2.13.0+rocm7.2",
# ]
#
# [tool.uv.sources]
# torch = { index = "pytorch-rocm" }
# en-core-web-trf = { url = "https://github.com/explosion/spacy-models/releases/download/en_core_web_trf-3.8.0/en_core_web_trf-3.8.0-py3-none-any.whl" }
#
# [[tool.uv.index]]
# name = "pytorch-rocm"
# url = "https://download.pytorch.org/whl/rocm7.2"
# explicit = true
# ///
"""Register profiling for code comments and prose, over lingrep patterns-selected matches.

The question this answers is not "was this written by an LLM?" but "is this
written in the register this project expects?". Provenance detection on a
5-20 word span is hopeless and, worse, unfalsifiable; register is a property
of the text, so it can be measured and argued with.

Two registers are harvested, and the second is the reason the first framing
needs restating rather than repeating. A comment is a span too short to carry
a claim on its own, so every comment finding is a claim about a distribution.
A Markdown paragraph is long enough to be read and disagreed with one at a
time, which makes a prose finding citable in a way a comment finding is not
-- and makes it correspondingly easier to overread. The output still ranks
distributions; a single paragraph is evidence about itself.

What ling contributes over a slop word list is grammatical conditioning.
A lexicon says "the LLM arm overuses `seamlessly`". A word sketch says
"in the LLM arm the ADV->VERB advmod slot is filled by `seamlessly` at N times
the human rate, while the NOUN->ADJ amod slot shifts toward `robust`" -- and
cites the comment. That is the method `patterns/sketch-grammar.lx` and
`patterns/collexeme.lx` already implement for Japanese; this is the English
CoNLL-U instance of it.

Pipeline:

    harvest   ROOT...            -> comments.jsonl  (.rs, .py, .md, .clj)
    parse     comments.jsonl     -> corpus.conllu
    profile   corpus.conllu      -> profile.json     (shells out to ling)
    compare   llm.json human.json -> keyness report
    locate    corpus.conllu + both profiles -> path:line: text

`compare` says what a corpus overuses; `locate` says where. It ranks with the
same scorer over the same family, so the rows it locates are the rows the
report already carries, and then replays the catalog to print one grep-style
line per occurrence:

    docs/design/protocol.md:142: is cited by a registry row ⟦—⟧ copy it to a

Overuse and underuse are not symmetric there and the output says so: an
overused construction has occurrences to point at, an underused one has none
by definition. `--reference-corpus` locates the underused half in the
comparison arm instead, as models rather than defects.

`harvest` needs tree-sitter and markdown-it-py, and `parse` needs spaCy; the
scoring half is standard library, so it runs anywhere `examples/gravity.py`
runs. Every dependency is imported at the point of use, so re-scoring a
corpus never pays for a parser it does not call.

RUNNING. `profile` and `compare` need nothing but a Python interpreter and,
for `profile`, `ling` on PATH. `harvest` and `parse` declare their
dependencies in the inline script metadata above, so:

    uv run --script examples/comment_register.py harvest . -o comments.jsonl
    uv run --script examples/comment_register.py parse comments.jsonl \\
        -o corpus.conllu --gpu --no-ner

The pinned wheels are the ROCm ones, matching this workspace's
`python/spacy-adapter` extra. Two notes about that, both learned the hard way:

* CuPy resolves HIP libraries by SONAME and Torch ships them unversioned, so
  a bare `uv run` fails with `libamdhip64.so.7: cannot open shared object
  file` even though Torch itself reports a working GPU. Point
  `LD_LIBRARY_PATH` at a directory of SONAME symlinks into `torch/lib`.
* For a CPU-only host, swap the `torch` source to the `cpu` index and drop
  `cupy-rocm-7-0`; `--model en_core_web_sm` is then the sensible pairing,
  since the transformer model is what makes a GPU worth having.

PARSER CHOICE. spaCy rather than stanza, measured on this workspace's Navi 31
over 64k words of harvested comments: 5,233 words/s against 750, with stanza
additionally needing MIOpen's RNN path disabled to run at all. The two emit
DIFFERENT dependency schemes -- spaCy ClearNLP, stanza UD -- and the catalog
is written on the labels they share; see its header.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import io
import json
import math
import re
import subprocess
import sys
import tempfile
import tokenize
import warnings
from collections.abc import Iterator, Sequence
from dataclasses import asdict, dataclass, replace
from functools import cache
from inspect import cleandoc
from itertools import pairwise
from pathlib import Path


# --------------------------------------------------------------------------
# Harvested records
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Comment:
    """One comment, located well enough to cite back to the source."""

    path: str
    line: int
    kind: str
    text: str


# Comment kinds are kept distinct because they are distinct registers. A Rust
# `///` item doc is written for a reader of generated documentation; a `//`
# note is written for whoever is next in the file. Pooling them would let a
# corpus that happens to be doc-heavy read as a register difference.
#
# `PROSE` is standalone Markdown -- a design document, a results write-up, a
# README -- and is the widest register of the four. It is kept separate for
# the same reason the other three are, and for one more: a paragraph carries
# enough text to make a claim that a 5-20 word comment span cannot, so a
# finding over prose is supported differently from a finding over comments
# even when the two agree.
DOC = "doc"
LINE = "line"
BLOCK = "block"
PROSE = "prose"


# --------------------------------------------------------------------------
# Scanning
# --------------------------------------------------------------------------
#
# Lexing is delegated, deliberately. Finding comments is not the work this
# repository exists for, and a hand-written scanner is the expensive kind of
# incidental code: `//` inside a string is not a comment, `"` inside a comment
# opens nothing, a raw string swallows both, and a fence indented inside a
# list item is still a fence. Every one of those mistakes fails the same
# quiet way -- nothing raises, and source ends up in a corpus of prose
# attributed to whoever the arm was labelled for.
#
# tree-sitter reads the code comments, one grammar per language, which is why
# adding a language below is a table entry and not a state machine. It also
# carries distinctions a prefix test gets wrong: rustc treats `////` as an
# ordinary comment rather than a doc comment, and the grammar says so by
# giving a fourth slash no `doc_comment` child.
#
# markdown-it-py reads the Markdown, and is CommonMark itself rather than an
# approximation of it. The hand-written block splitter it replaced tracked
# fences linearly and by column, so one fence it failed to recognise -- an
# indented one, most often -- inverted code and prose for the rest of the
# file. Over this repository's 99k harvested prose units, the count that
# parse as Rust declarations fell from 90 to 24 when the parser took over,
# and the code comments' own count fell from 3 to 0.
#
# Python is the exception, and not for want of a grammar: it ships its own
# tokenizer and its own parser, and `ast.get_docstring` resolves implicit
# concatenation, escape sequences and indentation that a syntax tree hands
# back raw.


@cache
def _grammar(name: str):
    """The tree-sitter parser for one language, built once.

    Imported here rather than at module scope so that `profile` and `compare`
    keep running on a bare interpreter; only `harvest` and `parse` need a
    dependency, and neither is the half anybody reruns to re-score a corpus.
    """
    from tree_sitter_language_pack import get_parser

    return get_parser(name)


@cache
def _markdown():
    """The CommonMark parser, with the two extensions this corpus needs.

    Tables, because a delimiter row is the only reliable way to tell one from
    prose containing a pipe, and this repository writes at length about a
    pattern language whose alternation operator is one. Front matter, because
    a results record opens with a YAML block that is metadata rather than
    argument. Both are enabled so that their contents can be *dropped*: an
    extension the parser does not know produces paragraphs, which is the one
    token this scanner keeps.
    """
    from markdown_it import MarkdownIt
    from mdit_py_plugins.front_matter import front_matter_plugin

    return (
        MarkdownIt("commonmark")
        .enable("table")
        .enable("strikethrough")
        .use(front_matter_plugin)
    )


def scan_code(source: str, path: str, grammar: str, read) -> Iterator[Comment]:
    """Yield every comment a tree-sitter grammar finds, in source order.

    `read` maps a node to `(kind, text)`, or to `None` to decline it. A node
    it claims is not descended into, so a doc comment's internal structure
    stays the grammar's business instead of arriving a second time as its own
    hit.
    """

    def visit(node) -> Iterator[Comment]:
        claimed = read(node)
        if claimed is not None:
            kind, text = claimed
            text = text.strip()
            if text:
                yield Comment(
                    path=path, line=node.start_point[0] + 1, kind=kind, text=text
                )
            return
        for child in node.children:
            yield from visit(child)

    yield from visit(_grammar(grammar).parse(source.encode("utf-8")).root_node)


def _child_text(node, kind: str) -> str | None:
    child = next((item for item in node.children if item.type == kind), None)
    return None if child is None else child.text.decode("utf-8")


GUTTER = re.compile(r"^\s*\*+ ?")


def _degutter(body: str) -> str:
    """Drop the `*` column a Rust block comment is conventionally drawn with."""
    return "\n".join(GUTTER.sub("", line) for line in body.splitlines())


def read_rust(node) -> tuple[str, str] | None:
    """Read a Rust comment node, or decline it."""
    if node.type == "line_comment":
        doc = _child_text(node, "doc_comment")
        if doc is not None:
            # Marker already removed, and correctly absent on `////`.
            return DOC, doc
        return LINE, node.text.decode("utf-8").lstrip("/")
    if node.type == "block_comment":
        doc = _child_text(node, "doc_comment")
        body = doc if doc is not None else _unwrap_block(node.text.decode("utf-8"))
        # De-guttering is confined to block comments on purpose: the same rule
        # over a `///` run would eat the markers off a bullet list.
        return (DOC if doc is not None else BLOCK), _degutter(body)
    return None


def _unwrap_block(body: str) -> str:
    # `removesuffix` and not a slice, because an unterminated block comment
    # runs to end of file and has no closer to remove.
    return body.removeprefix("/*").removesuffix("*/")


# The forms whose third element is a docstring. `defmethod`, `let` and `fn`
# are absent because they have none, so a string in that position is an
# argument and stays code.
CLOJURE_DOCUMENTED = frozenset(
    {"def", "defn", "defn-", "defmacro", "defmulti", "defprotocol", "ns"}
)


def read_clojure(node) -> tuple[str, str] | None:
    """Read a Clojure comment or docstring node, or decline it."""
    if node.type == "comment":
        return LINE, node.text.decode("utf-8").lstrip(";")
    if node.type == "str_lit" and _is_clojure_docstring(node):
        return DOC, cleandoc(node.text.decode("utf-8")[1:-1])
    return None


def _is_clojure_docstring(node) -> bool:
    """True for the string in `(defn name "..." [args] ...)` position."""
    form = node.parent
    if form is None or form.type != "list_lit":
        return False
    elements = [child for child in form.named_children if child.type != "comment"]
    if len(elements) < 3 or elements[2].id != node.id:
        return False
    head = elements[0]
    return head.type == "sym_lit" and head.text.decode("utf-8") in CLOJURE_DOCUMENTED


# --------------------------------------------------------------------------
# Python
# --------------------------------------------------------------------------


def scan_python(source: str, path: str) -> Iterator[Comment]:
    """Yield every comment and docstring in a Python source file, in order.

    The two passes are collected and sorted rather than yielded as they are
    found. `tokenize` emits every `#` comment before `ast.walk` emits the
    first docstring, and `ast.walk` is breadth-first rather than source
    order, so the raw stream interleaves nothing correctly: `merge_runs`
    downstream joins comments by adjacent line number, and the harvest is
    meant to read in source order so a `sent_id` can be found by eye.
    """
    yield from sorted(_scan_python_unordered(source, path), key=lambda c: c.line)


def _scan_python_unordered(source: str, path: str) -> Iterator[Comment]:
    try:
        readline = io.StringIO(source).readline
        for token in tokenize.generate_tokens(readline):
            if token.type != tokenize.COMMENT:
                continue
            text = token.string.lstrip("#").strip()
            yield Comment(path=path, line=token.start[0], kind=LINE, text=text)
    except (tokenize.TokenError, IndentationError, SyntaxError):
        # A file this interpreter cannot tokenize contributes nothing rather
        # than failing the harvest; `harvest` reports the count at the end.
        pass

    try:
        tree = ast.parse(source)
    except SyntaxError:
        return
    for node in ast.walk(tree):
        if not isinstance(
            node, ast.Module | ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef
        ):
            continue
        docstring = ast.get_docstring(node, clean=True)
        if not docstring:
            continue
        line = 1
        if node.body and isinstance(node.body[0], ast.Expr):
            line = node.body[0].lineno
        yield Comment(path=path, line=line, kind=DOC, text=docstring.strip())


# --------------------------------------------------------------------------
# Markdown
# --------------------------------------------------------------------------


def scan_markdown(source: str, path: str) -> Iterator[Comment]:
    """Yield every prose unit in a Markdown file, in source order.

    A unit is a paragraph -- including the paragraph inside a list item or a
    block quote, which is what a reader would call one move in the argument.
    Headings are NOT units, and that is a measurement decision rather than a
    parsing convenience: a heading is a label, it is usually verbless, and its
    density is a function of genre. A results write-up structured under a
    dozen headings would otherwise read as a corpus of sentence fragments
    beside a corpus of flowing argument, which is a fact about document layout
    and not about anybody's register.

    Everything else falls out of asking CommonMark for paragraphs and for
    nothing else. Fenced code, indented code, HTML blocks, tables, link
    reference definitions and front matter each produce some token that is
    not a paragraph, or produce no token at all, so not one of them needs a
    rule here.
    """
    for line, text in markdown_prose(source):
        yield Comment(path=path, line=line, kind=PROSE, text=text)


# The inline tokens that carry the author's own words. `code_inline` is kept
# because an identifier named in prose is exactly the concrete reference that
# referential density is meant to count. Everything else is address or
# typography: a link contributes its anchor text as an ordinary `text` child
# and its target as an attribute nobody wrote, emphasis is decoration around
# text that arrives anyway, and an image is dropped whole -- alt text
# included, because in a README it is overwhelmingly a badge.
PROSE_INLINE = frozenset({"text", "code_inline"})
LINE_BREAK = frozenset({"softbreak", "hardbreak"})


def markdown_prose(source: str) -> Iterator[tuple[int, str]]:
    """Yield `(line, text)` per paragraph, markup reduced to the prose in it."""
    for opener, token in pairwise(_markdown().parse(source)):
        if token.type != "inline" or opener.type != "paragraph_open":
            continue
        text = _inline_prose(token)
        if text:
            yield (opener.map[0] if opener.map else 0) + 1, text


def _inline_prose(token) -> str:
    parts: list[str] = []
    for child in token.children or ():
        if child.type in PROSE_INLINE:
            parts.append(child.content)
        elif child.type in LINE_BREAK:
            parts.append(" ")
    return re.sub(r"\s+", " ", "".join(parts)).strip()


# --------------------------------------------------------------------------
# Language table
# --------------------------------------------------------------------------


# Adding a language is a line here plus a reader above, and the reader is only
# needed because "which node is a comment" and "where does the marker stop"
# are per-grammar facts. `is_commented_out_code` reads this table too.
GRAMMARS = {
    ".rs": ("rust", read_rust),
    ".clj": ("clojure", read_clojure),
    ".cljc": ("clojure", read_clojure),
    ".cljs": ("clojure", read_clojure),
    ".bb": ("clojure", read_clojure),
}


def _code_scanner(grammar: str, read):
    """Bind a grammar and reader into the `(source, path)` scanner shape."""

    def scan(source: str, path: str) -> Iterator[Comment]:
        return scan_code(source, path, grammar, read)

    return scan


SCANNERS = {
    **{
        suffix: _code_scanner(grammar, read)
        for suffix, (grammar, read) in GRAMMARS.items()
    },
    ".py": scan_python,
    ".md": scan_markdown,
}


# --------------------------------------------------------------------------
# Paragraph assembly
# --------------------------------------------------------------------------


def merge_runs(comments: Iterator[Comment]) -> Iterator[Comment]:
    """Join a run of consecutive line comments into one paragraph.

    A `///` run is one paragraph of prose that happens to be typed across
    several lines, and the line breaks are a width convention, not syntax.
    Emitting each line separately hands the parser fragments -- "absent unless
    the" is not a sentence and gets no usable dependency structure -- so every
    grammatical signal downstream would be measuring the 80-column rule.

    A run breaks on a blank comment line (the paragraph separator), a change
    of kind, a gap in line numbers, or a change of file.

    The lines are rejoined with newlines and not with spaces, because what a
    run is rejoined INTO is Markdown, and `strip_markup` parses it as such. A
    fenced example inside a doc comment is three lines that only mean anything
    in that order: flattened to one line its fences become inline code spans
    and the example survives as prose, which is how `cargo test -p lingrep-corpus
    -- durability::regenerate_fixtures` entered a corpus of English sentences.

    A block comment and a Markdown prose unit arrive already whole and pass
    through. Joining those by adjacency would be actively wrong rather than
    merely redundant: the items of a tight list sit on consecutive lines and
    are separate moves, so the line-number test that reassembles a `///` run
    correctly would run a list together into one sentence-shaped heap.
    """
    run: list[Comment] = []

    def flush() -> Iterator[Comment]:
        if not run:
            return
        head = run[0]
        yield Comment(
            path=head.path,
            line=head.line,
            kind=head.kind,
            text="\n".join(item.text for item in run).strip(),
        )
        run.clear()

    for comment in comments:
        if MACHINE_DIRECTIVE.match(comment.text):
            # Dropped here rather than by `is_noise`, which only ever sees the
            # merged unit. A shebang is line 1 and the first real comment is
            # line 2, so leaving it in the run either glues a kernel directive
            # onto somebody's opening sentence or -- when the directive lands
            # first -- takes that sentence down with it.
            continue
        if comment.kind in (BLOCK, PROSE):
            yield from flush()
            yield comment
            continue
        contiguous = (
            run
            and comment.path == run[-1].path
            and comment.kind == run[-1].kind
            and comment.line == run[-1].line + 1
        )
        if not contiguous:
            yield from flush()
        if not comment.text.strip():
            # A blank comment line closes the paragraph and contributes
            # nothing to the next one.
            yield from flush()
            continue
        run.append(comment)
    yield from flush()


def strip_markup(text: str) -> str:
    """Reduce a comment's markup to the prose inside it.

    A doc comment is Markdown -- rustdoc says so outright, and a Python
    docstring is read as Markdown by every renderer that touches one -- so
    this is the Markdown scanner pointed at one comment rather than at one
    file, with the paragraphs it finds joined back into the single unit the
    caller already merged.

    Removing the code examples needs no rule of its own as a result, which is
    the whole reason to reuse the parser: a fenced or indented block is source
    rather than prose, leaving it in would let a well-exemplified corpus
    register as verbose, and a code block is simply not a paragraph.
    """
    return " ".join(prose for _, prose in markdown_prose(text))


# --------------------------------------------------------------------------
# Filtering
# --------------------------------------------------------------------------


LICENCE = re.compile(
    r"copyright|licen[sc]e|SPDX-License|Permission is hereby granted"
    r"|Licensed under|All rights reserved",
    re.IGNORECASE,
)
URL_ONLY = re.compile(r"^\s*<?(?:https?://|www\.)\S+>?\s*$")
# `tokenize` reports `#!/usr/bin/env python3` as an ordinary comment, and the
# `#` is stripped before this sees it. Encoding cookies arrive the same way.
MACHINE_DIRECTIVE = re.compile(
    r"^#?!\s*/|^-\*-\s*coding|^\s*(?:noqa|type:|pragma:|pylint:|mypy:|ruff:)"
)
RULE_ONLY = re.compile(r"^[\s\W_]*$")
# A labelled metadata field whose value is not prose: `Start Date: 2014-03-11`,
# `RFC PR: rust-lang/rfcs#1`, `User guide: docs/features/chunking.md`. Every
# rust-lang RFC opens with four of these, so admitting them would put two
# thousand copies of a form nobody composed into the reference arm.
#
# What counts as a value is where this rule earns its keep, and two classes it
# must NOT take were both measured against these corpora before the test was
# narrowed to its present shape.
#
# A colon-final clause -- `The production API is:` -- introduces a block. It
# has a finite verb and it is prose, and its rate is one of the larger
# differences between the arms (12.2% of units against 2.2%), so a rule that
# swallowed it would delete a finding rather than clean up around one.
#
# A labelled bullet whose value is a description -- `Network Isolation:
# iptables-based firewall with owner-based filtering` -- is authored content,
# and the labelled-bullet habit is itself a candidate register feature. An
# earlier length-based test dropped 23,196 units from one arm against 2,865
# from the other, which is not a filter but a thumb on the contrast.
FIELD_LINE = re.compile(r"^[A-Z][\w /()-]{0,40}:\s*(?P<value>.*)$")
PROSE_WORD = re.compile(r"^[A-Za-z]{3,}$")
TRACKED_TODO = re.compile(r"\b(?:TODO|FIXME|XXX|HACK|NOTE)\s*\([^)]+\)")
# The Rust node types that are a declaration rather than a sentence that
# happens to parse. `expression_statement` is deliberately absent, for exactly
# the reason `_is_statement` refuses `ast.Expr`: it is the shape an English
# clause arrives in. `raw/bccwj-four-cap-output.txt;` is a path and a
# semicolon, and Rust reads it as an expression statement without complaint.
RUST_DECLARATIONS = frozenset(
    {
        "let_declaration",
        "function_item",
        "use_declaration",
        "mod_item",
        "impl_item",
        "struct_item",
        "enum_item",
        "trait_item",
        "const_item",
        "static_item",
        "type_item",
        "attribute_item",
        "macro_invocation",
    }
)


def is_noise(comment: Comment) -> bool:
    """True for comments that say nothing about register.

    Deliberately conservative. Every rule here removes a class that would
    otherwise dominate a frequency count without carrying any authorial voice
    -- licence boilerplate is copied, a URL has no grammar, commented-out code
    is code. Short comments are NOT dropped: comment length is one of the
    signals, so trimming the low end would bias the measurement it feeds.
    """
    text = comment.text.strip()
    if not text:
        return True
    if RULE_ONLY.match(text):
        return True
    if MACHINE_DIRECTIVE.match(text):
        return True
    if URL_ONLY.match(text):
        return True
    if LICENCE.search(text):
        return True
    if TRACKED_TODO.search(text):
        return True
    if is_field_line(text):
        return True
    return is_commented_out_code(text, comment.path)


def is_field_line(text: str) -> bool:
    """True for a `Label: value` field whose value contains no prose at all.

    The test is on the value's composition rather than its length, for the
    reasons recorded at `FIELD_LINE`. A date, an issue reference, a path, a
    flag and a bare identifier are each a token no English sentence is made
    of; one ordinary word among them is enough to make the line a sentence
    somebody wrote, and it is kept.
    """
    match = FIELD_LINE.match(text)
    if not match:
        return False
    value = match.group("value").strip()
    if not value:
        return False
    return not any(
        PROSE_WORD.match(token.strip(",;()[]\"'")) for token in value.split()
    )


def is_commented_out_code(text: str, path: str) -> bool:
    """True when the comment body is disabled source rather than prose.

    Both languages that have a rule are parsed rather than pattern-matched.
    Rust used to be the exception, with two agreeing heuristics -- a
    keyword-initial shape and a code-shaped tail -- because the harness held
    no Rust parser, and it needed both because either alone misfires on the
    merged paragraphs this runs against: a comment opening "let the caller
    decide" is keyword-initial English, and an English sentence can end in a
    semicolon. A grammar is now to hand, and parsing finds the three
    `proptest!` blocks that rule missed while flagging nothing it kept.

    Python was already parsed, which is exact and needs no keyword list. It
    previously reused the Rust regex as a gate, which was wrong in both
    directions: the regex names Rust keywords, and its `[;{}]` tail almost
    never matches Python, so the gate collapsed to "contains `=`" and let
    `# return None` and `# import os` through as prose.

    Clojure has no rule, and the omission is measured rather than pending. A
    parse proves nothing there -- any run of words is a legal sequence of
    symbols -- so the only available signal is an opening bracket, and over
    soranoha's 359 Clojure line comments that fires zero times while taking
    five docstrings that enumerate `(a)`, `(b)`, `(c)`. A rule that costs five
    and earns none is not a conservative filter. A language gets one here when
    a corpus shows it needs one.
    """
    suffix = Path(path).suffix.lower()
    if suffix == ".py":
        try:
            # Prose fed to `ast.parse` raises SyntaxWarning for things like a
            # bare `\s`, which is noise on stderr about text that was never
            # code.
            with warnings.catch_warnings():
                warnings.simplefilter("ignore")
                tree = ast.parse(text)
        except (SyntaxError, ValueError):
            return False
        return any(_is_statement(node) for node in tree.body)
    if suffix != ".rs":
        return False
    root = _grammar("rust").parse(text.encode("utf-8")).root_node
    # tree-sitter recovers rather than raising, so "it parsed" is a property
    # of the tree and not of the call: prose reaches here as a spray of ERROR
    # nodes, which is the discriminator.
    if root.has_error:
        return False
    return any(child.type in RUST_DECLARATIONS for child in root.children)


def _is_statement(node: ast.stmt) -> bool:
    """True for a node that is code rather than a bare prose fragment.

    An expression statement is how any English clause that happens to parse
    shows up, so it never counts. A value-less annotation is the one other
    shape prose reaches: `Note: retry` parses as `AnnAssign` with no value,
    while real annotated code (`limit: int = 5`) carries one.
    """
    if isinstance(node, ast.Expr):
        return False
    return not (isinstance(node, ast.AnnAssign) and node.value is None)


# --------------------------------------------------------------------------
# harvest
# --------------------------------------------------------------------------


# Directories holding code this repository did not write. Getting this wrong
# is not a slow harvest but a wrong answer: a single unskipped `.venv`
# contributed 148,791 comments -- twenty times the first-party corpus -- all
# of them third-party, and would have been attributed to whoever the arm was
# labelled for.
SKIP_DIRECTORIES = {
    ".git",
    ".worktrees",
    ".direnv",
    ".venv",
    "venv",
    ".tox",
    ".nox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "site-packages",
    "target",
    "node_modules",
    "__pycache__",
    "third-party",
    "vendor",
    "build",
    "dist",
}


def walk_sources(roots: Sequence[Path], suffixes: Sequence[str]) -> Iterator[Path]:
    wanted = {suffix.lower() for suffix in suffixes}
    for root in roots:
        if root.is_file():
            if root.suffix.lower() in wanted:
                yield root
            continue
        for path in sorted(root.rglob("*")):
            if path.suffix.lower() not in wanted:
                continue
            if any(part in SKIP_DIRECTORIES for part in path.parts):
                continue
            if path.is_file():
                yield path


def harvest(arguments: argparse.Namespace) -> int:
    roots = [Path(root) for root in arguments.roots]
    for root in roots:
        if not root.exists():
            raise SystemExit(f"root does not exist: {root}")
    base = Path(arguments.relative_to) if arguments.relative_to else None

    kept = 0
    dropped = 0
    files = 0
    with open(arguments.output, "w", encoding="utf-8") as sink:
        for path in walk_sources(roots, list(SCANNERS)):
            scanner = SCANNERS[path.suffix.lower()]
            try:
                source = path.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            files += 1
            relative = _relative(path, base)
            for merged in merge_runs(scanner(source, relative)):
                # Markdown prose came out of the Markdown parser already
                # reduced; only the comment kinds carry markup that has not
                # been through it yet, and running a cleaned unit back
                # through CommonMark would re-read its leftover punctuation
                # as syntax.
                text = (
                    merged.text
                    if merged.kind == PROSE
                    else strip_markup(merged.text)
                )
                comment = Comment(
                    path=merged.path,
                    line=merged.line,
                    kind=merged.kind,
                    text=text,
                )
                if is_noise(comment):
                    dropped += 1
                    continue
                kept += 1
                sink.write(json.dumps(asdict(comment), ensure_ascii=False) + "\n")

    print(
        f"{files} files, {kept} comments kept, {dropped} dropped as noise",
        file=sys.stderr,
    )
    return 0


def _relative(path: Path, base: Path | None) -> str:
    if base is None:
        return str(path)
    try:
        return str(path.resolve().relative_to(base.resolve()))
    except ValueError:
        return str(path)


# --------------------------------------------------------------------------
# parse
# --------------------------------------------------------------------------


# `text` may not contain a newline, and a CoNLL-U comment may not contain a
# tab, so both are neutralised rather than escaped -- the value is a citation
# aid, and the tokens below carry the content.
def _comment_safe(text: str) -> str:
    return text.replace("\t", " ").replace("\n", " ").strip()


def _conllu_field(value: str | None) -> str:
    """CoNLL-U writes an absent value as `_`, and never as an empty column."""
    if value is None:
        return "_"
    cleaned = value.replace("\t", " ").replace("\n", " ").strip()
    return cleaned if cleaned else "_"


def write_conllu(
    sink,
    document: str | None,
    sent_id: str,
    text: str,
    rows: Sequence[tuple[int, str, str, str, str, str, int, str]],
) -> None:
    """Write one sentence, opening a document only when `document` is given.

    `# newdoc` is a BOUNDARY, not a per-sentence label: the reader splits the
    file into documents wherever one appears. Emitting it before every
    sentence therefore makes every sentence its own document -- 9,211 of them
    for 258 source files -- which leaves `newdoc_id` useless for scoping and
    `documents_admitted` reporting the sentence count. The caller passes a
    document name only when the source file changes.
    """
    if document is not None:
        sink.write(f"# newdoc id = {_comment_safe(document)}\n")
    sink.write(f"# sent_id = {_comment_safe(sent_id)}\n")
    sink.write(f"# text = {_comment_safe(text)}\n")
    for index, form, lemma, upos, xpos, feats, head, deprel in rows:
        sink.write(
            "\t".join(
                (
                    str(index),
                    _conllu_field(form),
                    _conllu_field(lemma),
                    _conllu_field(upos),
                    _conllu_field(xpos),
                    _conllu_field(feats),
                    str(head),
                    _conllu_field(deprel),
                    "_",
                    "_",
                )
            )
            + "\n"
        )
    sink.write("\n")


def parse(arguments: argparse.Namespace) -> int:
    """Parse harvested comments into CoNLL-U with citable sentence identity.

    `# newdoc id` carries the source path and `# sent_id` the path, line and
    within-comment sentence ordinal, so every finding downstream can be traced
    to the comment that produced it with `--where '@sentence.sent_id = ...'`. That is
    the whole reason for parsing to CoNLL-U rather than to a private format:
    ling answers a `sent_id` clause at sentence grain for this format.
    """
    try:
        import spacy
    except ImportError:
        raise SystemExit(
            "parse needs spaCy. Run this stage under uv:\n"
            "  uv run --with 'spacy>=3.8,<3.9' --with <model-wheel-url> "
            f"{Path(__file__).name} parse ..."
        ) from None

    if arguments.gpu:
        spacy.require_gpu(arguments.gpu_device)

    excluded = ["ner"] if arguments.no_ner else []
    nlp = spacy.load(arguments.model, exclude=excluded)

    records = []
    with open(arguments.input, encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                records.append(json.loads(line))
    if arguments.kind != "all":
        records = [record for record in records if record["kind"] == arguments.kind]

    sentences = 0
    tokens = 0
    documents = 0
    open_document: str | None = None
    with open(arguments.output, "w", encoding="utf-8") as sink:
        texts = (record["text"] for record in records)
        pairs = zip(
            nlp.pipe(texts, batch_size=arguments.batch_size), records, strict=True
        )
        for doc, record in pairs:
            document = record["path"]
            for ordinal, span in enumerate(doc.sents):
                rows = []
                # `token.head.i` is document-global; CoNLL-U HEAD is
                # sentence-local and one-based, with 0 for the root. Rebasing
                # on the sentence start is what makes the two agree.
                offset = span.start
                for token in span:
                    head = 0 if token.head.i == token.i else token.head.i - offset + 1
                    # spaCy spells the sentence root `ROOT`; CoNLL-U spells it
                    # `root`, and that is the spelling every UD-shaped query
                    # in this repository uses.
                    deprel = "root" if token.dep_ == "ROOT" else token.dep_
                    rows.append(
                        (
                            token.i - offset + 1,
                            token.text,
                            token.lemma_,
                            token.pos_,
                            token.tag_,
                            str(token.morph) or "_",
                            head,
                            deprel,
                        )
                    )
                if not rows:
                    continue
                starting = document if document != open_document else None
                if starting is not None:
                    documents += 1
                    open_document = document
                write_conllu(
                    sink,
                    document=starting,
                    sent_id=f"{record['path']}:{record['line']}#{ordinal}",
                    text=span.text,
                    rows=rows,
                )
                sentences += 1
                tokens += len(rows)

    print(
        f"{len(records)} comments -> {documents} documents, "
        f"{sentences} sentences, {tokens} tokens",
        file=sys.stderr,
    )
    return 0


# --------------------------------------------------------------------------
# mine
# --------------------------------------------------------------------------


# Each family is ONE definition whose `key:` enumerates a whole candidate
# space, rather than one definition per candidate. That is the difference
# between a catalog of six patterns and a generated catalog of tens of
# thousands: the engine does the enumeration during a single summarize pass,
# and a key tuple that never occurs simply never appears in the output.
#
# `{0,8}` bounds every gap. A dependency edge may span more, so these families
# under-count long-range edges; the bound is what keeps a two-any-token
# pattern from being quadratic in sentence length. It is a stated limit of the
# search, not of the engine.
MINING_FAMILIES: dict[str, str | None] = {
    "pos_ngram": """
key: [$a.upos, $b.upos]
pattern pos_bigram := (?<a> []) (?<b> []).

key: [$a.upos, $b.upos, $c.upos]
pattern pos_trigram := (?<a> []) (?<b> []) (?<c> []).
""",
    "opening": """
key: [$a.upos, $b.upos]
pattern opening_bigram := ^ (?<a> []) (?<b> []).

key: [$a.upos, $b.upos, $c.upos]
pattern opening_trigram := ^ (?<a> []) (?<b> []) (?<c> []).
""",
    # The two dependency-edge families are NOT here, and the omission is the
    # point. They were generated as lingrep patterns -- one definition per label, in two
    # linear orders, over a `[]{0,8}` gap -- and the counts that came back
    # were wrong in two independent ways.
    #
    # Per-start longest-candidate selection keeps ONE candidate per start,
    # including within a single label, so a head with two same-label
    # dependents in the window contributes one match rather than two.
    # Measured against the edges read straight out of columns 7 and 8: 97.3%
    # of in-window edge instances on the llm arm and 95.5% on the human arm,
    # per key ranging from 53% to 100%, never above. The arms differing is
    # what matters -- that is a confound in the contrast, not noise around
    # it, and it moved a log odds ratio by as much as 0.34, which is the size
    # of findings this report actually makes. And `{0,8}` truncates
    # independently: it admits 92.85% of basic edges, so long-range ones were
    # invisible rather than undercounted.
    #
    # One definition per (label, EXACT gap) fixes the first -- measured at
    # 100.00% recall on all 174 keys tested -- at 792 definitions and ~1.4x
    # the matcher time, and fixes the second not at all.
    #
    # So the edges are read from the CoNLL-U instead, by `edge_profile`. A
    # dependency edge is not a matching problem: it is already written in
    # columns 7 and 8. Reading it there is exact, needs no window, and does
    # both arms in about a third of a second -- faster than the matcher
    # starts. What vp is for here is everything else in this dict, the
    # anchored, quantified, regex-bearing sequence families it alone can
    # express.
    # Morphology goes through `xpos`, NOT through `feat.*`, and that is a hard
    # constraint rather than a preference. A `key:` projection over a
    # string-set attribute is refused at schema binding: a scalar key cannot
    # represent all of the values in `feat.Tense`.
    # The Penn tags `xpos` already carries are scalar and encode the same
    # distinctions this family is after -- VBG/VBN/VBD for verb form and
    # tense, NN/NNS for number.
    "morphology": """
key: [upos, xpos]
pattern morph_tag := [].

key: [$a.xpos, $b.xpos]
pattern xpos_bigram := (?<a> []) (?<b> []).
""",
}


EDGE_FAMILIES = ("dep_triple", "rel_filler")


def read_conllu_sentences(path: Path) -> Iterator[tuple[int, list[tuple]]]:
    """Yield `(document_ordinal, sentence)` from a CoNLL-U file.

    Documents are delimited by `# newdoc`, which is how ling splits the
    same file, so the ordinals here index the same documents its match
    records report. Sentences before any `# newdoc` belong to document 0.

    A row is `(id, upos, lemma, head, deprel)`. Range and empty-node IDs are
    structural rows rather than tokens and are skipped, exactly as the
    pretokenized reader skips them.
    """
    document = 0
    started = False
    sentence: list[tuple] = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line:
                if sentence:
                    yield document, sentence
                    sentence = []
                continue
            if line.startswith("#"):
                if line.startswith("# newdoc"):
                    if started:
                        document += 1
                    started = True
                continue
            columns = line.split("\t")
            if len(columns) < 8 or not columns[0].isdigit():
                continue
            sentence.append(
                (
                    int(columns[0]),
                    columns[3],
                    columns[2],
                    int(columns[6]) if columns[6].isdigit() else 0,
                    columns[7],
                )
            )
    if sentence:
        yield document, sentence


def edge_profile(
    path: Path,
) -> tuple[dict[str, dict[str, dict[int, int]]], dict[int, int]]:
    """Exact dependency-edge counts, read from columns 7 and 8.

    This is the whole of both edge families. Every basic edge is counted once,
    with no gap window and no candidate selection between them, which is what
    the lingrep patterns formulation could not do -- see the note in `MINING_FAMILIES`.

    Group strings are rendered exactly as `render_group` renders a projected
    key, `$` stripped, so a row from here is indistinguishable downstream from
    a row ling produced and `compare` needs no special case.
    """
    counts: dict[str, dict[str, dict[int, int]]] = {
        name: {} for name in EDGE_FAMILIES
    }
    tokens: dict[int, int] = {}
    for document, sentence in read_conllu_sentences(path):
        by_id = {row[0]: row for row in sentence}
        tokens[document] = tokens.get(document, 0) + len(sentence)
        for _, upos, lemma, head, relation in sentence:
            # `root` designates head 0, which is no token, so it names no
            # edge between two tokens and is not a relation this counts.
            if head == 0 or relation == "root" or head not in by_id:
                continue
            head_upos = by_id[head][1]
            for name, group in (
                (
                    "dep_triple",
                    f"h.upos={head_upos}|d.dep.rel={relation}|d.upos={upos}",
                ),
                ("rel_filler", f"d.dep.rel={relation}|d.lemma={lemma}"),
            ):
                bucket = counts[name].setdefault(group, {})
                bucket[document] = bucket.get(document, 0) + 1
    return counts, tokens


def guard_output_path(destination: Path) -> None:
    """Refuse to drop a generated catalog into a tracked source tree.

    `lingrep_core::testing::catalogs` discovers `.lx` files by walking the
    repository, tracked or not, skipping only `target/` and a few dot
    directories. A mined catalog written to the working tree would therefore
    join the pinned catalog set and turn a dozen tests red, with nothing in
    the failure naming this script. Cheaper to refuse than to explain.
    """
    resolved = destination.resolve()
    for ancestor in resolved.parents:
        if not (ancestor / ".git").exists():
            continue
        if resolved.is_relative_to(ancestor / "target"):
            return
        raise SystemExit(
            f"refusing to write {resolved}: it is inside the repository at "
            f"{ancestor}, where every `.lx` file joins the discovered "
            f"catalog set and must carry pinned counts. Write it under "
            f"{ancestor / 'target'} or outside the checkout."
        )


def mine(arguments: argparse.Namespace) -> int:
    corpora = [Path(path) for path in arguments.corpora]
    for path in corpora:
        if not path.exists():
            raise SystemExit(f"corpus does not exist: {path}")
    destination = Path(arguments.output)
    guard_output_path(destination)

    families = arguments.family or sorted(MINING_FAMILIES)
    unknown = sorted(set(families) - set(MINING_FAMILIES))
    if unknown:
        raise SystemExit(f"unknown families: {', '.join(unknown)}")

    sections = [
        "# GENERATED by examples/comment_register.py -- do not edit or track.",
        "#",
        "# A candidate space for register mining. Each definition's `key:`",
        "# enumerates a family of patterns, so one summarize pass counts every",
        "# candidate in the family at once. Rank them with `compare`, then",
        "# promote whatever survives into a hand-written catalog with",
        "# `compare --emit-pattern`.",
        "#",
        "# The dependency-edge families are NOT here: they are read from the",
        "# CoNLL-U columns by `profile --edges`, because the lingrep patterns formulation",
        "# could not count them exactly. See `MINING_FAMILIES` for the",
        "# measurements that settled it.",
    ]
    for family in families:
        sections.append(f"\n# --- family: {family} ---")
        sections.append(MINING_FAMILIES[family].strip())
    # The denominator every rate in `compare` is taken against.
    sections.append("\nkey: [lemma, upos]\npattern token := [].")

    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text("\n".join(sections) + "\n", encoding="utf-8")
    print(
        f"wrote {destination} with {len(families)} sequence famil"
        f"{'y' if len(families) == 1 else 'ies'}. The dependency-edge families "
        "are not matched: pass `profile --edges CONLLU` to add them.",
        file=sys.stderr,
    )
    return 0


# --------------------------------------------------------------------------
# profile
# --------------------------------------------------------------------------


SCHEMA = "lingrep.query/v2"
DENOMINATOR = "token"


def read_summary_stream(source) -> tuple[dict[str, dict[str, int]], dict[str, int]]:
    """Consume a `ling --json summarize` stream into per-pattern counts.

    Validated as strictly as `examples/gravity.py` validates its own input.
    The `complete` event is not decoration: a stream that stopped early still
    looks like a well-formed prefix, and silently profiling a truncated corpus
    would understate every rate in it by an unknown factor.
    """
    patterns: dict[str, dict[str, int]] = {}
    counters: dict[str, int] = {}
    saw_run = False
    complete = False

    for number, line in enumerate(source, 1):
        if not line.strip():
            continue
        if complete:
            raise ValueError("event follows the complete event")
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"line {number}: invalid JSON: {error.msg}") from error
        if not isinstance(event, dict) or event.get("schema") != SCHEMA:
            raise ValueError(f"line {number}: expected {SCHEMA}")
        kind = event.get("type")
        data = event.get("data")

        if not saw_run:
            if kind != "run" or not isinstance(data, dict):
                raise ValueError("stream must begin with a ling run event")
            if data.get("mode") != "summarize":
                raise ValueError("profile accepts only ling summarize output")
            saw_run = True
            continue
        if kind == "run":
            raise ValueError("stream has more than one run event")
        if kind == "summary":
            if not isinstance(data, dict):
                raise ValueError(f"line {number}: summary is missing data")
            name = data.get("pattern")
            count = data.get("count")
            if not isinstance(name, str) or not isinstance(count, int):
                raise ValueError(f"line {number}: summary has no pattern and count")
            patterns.setdefault(name, {})[render_group(data.get("group"))] = count
        elif kind == "complete":
            if not isinstance(data, dict):
                raise ValueError("complete event is missing data")
            if data.get("coverage") != "full" or data.get("termination") != "eof":
                raise ValueError("profile requires full coverage terminating at eof")
            found = data.get("counters")
            if isinstance(found, dict):
                counters = {
                    key: value
                    for key, value in found.items()
                    if isinstance(value, int)
                }
            complete = True
        elif kind != "definition":
            raise ValueError(f"line {number}: unexpected event type `{kind}`")

    if not complete:
        raise ValueError("profile requires the final ling complete event")
    return patterns, counters


def render_group(group: object) -> str:
    """Render a summary group as one stable, comparable string.

    A projected key is an ordered list of typed fields; flattening it to
    `name=value` pairs keeps authored field order, which is what makes the
    same construction comparable across two independently profiled corpora.
    """
    if not isinstance(group, dict):
        raise ValueError("summary group must be an object")
    kind = group.get("kind")
    if kind in {"surface", "unit", "term"}:
        value = group.get("value")
        if not isinstance(value, str):
            raise ValueError(f"{kind} group must carry a string value")
        return value
    if kind != "key":
        raise ValueError(f"unsupported summary group kind `{kind}`")
    fields = group.get("fields")
    if not isinstance(fields, list):
        raise ValueError("key group must carry a field list")
    rendered = []
    for field in fields:
        if not isinstance(field, dict):
            raise ValueError("key field must be an object")
        name = str(field.get("name", "")).lstrip("$")
        if field.get("kind") == "list":
            values = field.get("values")
            if not isinstance(values, list):
                raise ValueError("list field must carry values")
            rendered.append(f"{name}=" + "+".join(str(item) for item in values))
        else:
            rendered.append(f"{name}={field.get('value')}")
    return "|".join(rendered)


def match_group(data: dict) -> str:
    """The group string `summarize` would have used for this match record.

    `summarize` groups a keyed definition by its projected key and an unkeyed
    one by surface. A match record carries `key` in exactly the field shape a
    summary key group carries, so rendering it through `render_group` is what
    keeps the two paths comparable rather than merely similar -- the counts
    this module derives from `matches` have to equal the ones it derives from
    `summarize`, or the dispersion column would describe a different
    population than the rate column beside it.
    """
    key = data.get("key")
    if isinstance(key, list) and key:
        return render_group({"kind": "key", "fields": key})
    surface = data.get("surface")
    if not isinstance(surface, str):
        raise ValueError("an unkeyed match must carry a surface to group by")
    return surface


def read_match_stream(
    source,
    allowed: dict[str, dict[str, int]],
) -> tuple[dict[str, dict[str, dict[int, int]]], dict[int, int], int]:
    """Consume a `ling --json matches` stream into per-document counts.

    Held to `read_summary_stream`'s standard, and for the same reason: a
    truncated stream is a well-formed prefix, and here it would understate
    dispersion as well as frequency -- a construction whose later documents
    were never read looks more concentrated than it is, which is precisely
    the judgement this data exists to support.

    `allowed` is the profile `summarize` already published, and it is the
    authority on which groups exist. This reader adds a distribution to those
    counts; it does not recompute them. That matters because the two modes do
    not agree on their own: `summarize` DROPS a record whose declared key
    cannot be satisfied, while a match record for the same span simply
    carries no key, and nothing in the stream says whether a definition
    declared a key at all -- `metadata` carries descriptive keys only. Rather
    than infer it, anything `summarize` did not publish is skipped and
    counted.

    Returns the per-(pattern, group, document) counts, the per-document token
    totals, and the number of records skipped as unpublished.
    """
    counts: dict[str, dict[str, dict[int, int]]] = {}
    document_tokens: dict[int, int] = {}
    skipped = 0
    saw_run = False
    complete = False

    for number, line in enumerate(source, 1):
        if not line.strip():
            continue
        if complete:
            raise ValueError("event follows the complete event")
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise ValueError(f"line {number}: invalid JSON: {error.msg}") from error
        if not isinstance(event, dict) or event.get("schema") != SCHEMA:
            raise ValueError(f"line {number}: expected {SCHEMA}")
        kind = event.get("type")
        data = event.get("data")

        if not saw_run:
            if kind != "run" or not isinstance(data, dict):
                raise ValueError("stream must begin with a ling run event")
            if data.get("mode") != "matches":
                raise ValueError("dispersion accepts only ling matches output")
            saw_run = True
            continue
        if kind == "run":
            raise ValueError("stream has more than one run event")
        if kind == "match":
            if not isinstance(data, dict):
                raise ValueError(f"line {number}: match is missing data")
            name = data.get("pattern")
            document = data.get("document_ordinal")
            if not isinstance(name, str) or not isinstance(document, int):
                raise ValueError(
                    f"line {number}: match has no pattern and document ordinal"
                )
            group = match_group(data)
            if group not in allowed.get(name, {}):
                skipped += 1
                continue
            bucket = counts.setdefault(name, {}).setdefault(group, {})
            bucket[document] = bucket.get(document, 0) + 1
            if name == DENOMINATOR:
                document_tokens[document] = document_tokens.get(document, 0) + 1
        elif kind == "complete":
            if not isinstance(data, dict):
                raise ValueError("complete event is missing data")
            if data.get("coverage") != "full" or data.get("termination") != "eof":
                raise ValueError("dispersion requires full coverage terminating at eof")
            complete = True
        elif kind != "definition":
            raise ValueError(f"line {number}: unexpected event type `{kind}`")

    if not complete:
        raise ValueError("dispersion requires the final ling complete event")
    if not document_tokens:
        raise ValueError(
            f"the catalog must declare a `{DENOMINATOR}` pattern for dispersion; "
            "per-document token totals are the expected proportions Gries' DP "
            "is measured against and there is no other source for them"
        )
    return counts, document_tokens, skipped


def is_prepared_artifact(path: Path) -> bool:
    """Whether `path` is a prepared V8 artifact rather than a source corpus.

    This is `lingrep_corpus::is_prepared_artifact`'s rule -- a directory carrying a
    `manifest.json` -- restated because the CLI takes the two through
    different flags and gives no way to ask.
    """
    return path.is_dir() and (path / "manifest.json").is_file()


def corpus_label(corpus: Path) -> str:
    """A short arm name for a source path or a prepared artifact.

    An artifact is named `<source>.g6.a2.<64 hex>.<trust>.lingcorpus`, so its
    stem carries the whole identity and reads as noise at the head of a
    report column. Cutting at the first generation component recovers the
    source name the artifact was built from.
    """
    name = corpus.name
    for suffix in (".lingcorpus",):
        name = name.removesuffix(suffix)
    name = re.split(r"\.g\d+\.a\d+\.", name)[0]
    return Path(name).stem or name


def run_lingrep(command: Sequence[str], consume):
    """Run ling and hand `consume` its stdout as a line iterator.

    Streamed rather than captured: the dispersion path reads one record per
    match, which is a gigabyte of JSON on the mined catalog, and
    `capture_output` would hold all of it before the first line was parsed.
    stderr goes to a temporary file so a chatty run cannot deadlock against a
    full pipe buffer while this end is still draining stdout.
    """
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as diagnostics:
        process = subprocess.Popen(
            list(command),
            stdout=subprocess.PIPE,
            stderr=diagnostics,
            text=True,
            encoding="utf-8",
        )
        # A stream error and a failed run look the same from here -- a run
        # that exited 2 wrote no complete event either -- so the exit status
        # is established before any parse failure is allowed to surface.
        # Reporting "requires the final complete event" for a catalog that did
        # not compile would name the wrong problem.
        failure: Exception | None = None
        result = None
        try:
            result = consume(process.stdout)
        except (ValueError, json.JSONDecodeError) as error:
            failure = error
        finally:
            if process.stdout is not None:
                process.stdout.read()
                process.stdout.close()
            status = process.wait()
        diagnostics.seek(0)
        message = diagnostics.read()
    if status != 0:
        sys.stderr.write(message)
        raise SystemExit(f"ling exited {status}")
    if message.strip():
        sys.stderr.write(message)
    if failure is not None:
        raise failure
    return result


def profile(arguments: argparse.Namespace) -> int:
    # `matches` is the only mode that attributes a record to a document, and
    # dispersion and the holdout split both need that attribution. It costs
    # about 8x summarize on the mined catalog, which is why it is opt-in
    # rather than the default: the mining sweep runs without it and the
    # confirmatory pass over a hand-written catalog runs with it.
    dispersion = getattr(arguments, "dispersion", False)
    command = [
        arguments.lingrep,
        "--json",
        "summarize",
        "--top",
        "all",
        "--patterns",
        arguments.patterns,
    ]

    # Mining reuses one corpus under many catalogs. Addressing a prepared
    # artifact directly avoids observing the source again on every iteration;
    # `--cache` must read, parse and hash the source to prove its exact identity
    # before the bounded, manifest-only sibling scan. `--cache` is still the
    # right call for a source path: it reuses a compatible artifact or creates
    # one that a later run can address directly.
    corpus = Path(arguments.corpus)
    if is_prepared_artifact(corpus):
        command.extend(["--corpus", str(corpus)])
    else:
        command.extend(["--cache", str(corpus)])
    if arguments.where:
        command.extend(["--where", arguments.where])

    patterns, counters = run_lingrep(command, read_summary_stream)

    per_document: dict[str, dict[str, dict[int, int]]] = {}
    document_tokens: dict[int, int] = {}
    if dispersion:
        # A second pass, deliberately. `summarize` stays the authority on
        # what a count is, so turning dispersion on cannot move a single
        # number in the report it annotates; this pass only says which
        # documents those counts came from.
        matches = command.copy()
        matches[2:5] = ["matches", "--limit", "all"]
        per_document, document_tokens, skipped = run_lingrep(
            matches, lambda stream: read_match_stream(stream, patterns)
        )
        if skipped:
            print(
                f"dispersion: {skipped} match record(s) carried no published "
                "group and were not attributed to a document",
                file=sys.stderr,
            )

    if DENOMINATOR not in patterns:
        raise SystemExit(
            f"the catalog must declare a `{DENOMINATOR}` pattern to supply the "
            "token denominator; every rate is per token and there is no other "
            "way to count the tokens ling actually admitted"
        )
    tokens = sum(patterns[DENOMINATOR].values())

    if arguments.edges:
        edges, edge_tokens = edge_profile(Path(arguments.edges))
        for name, groups in edges.items():
            if name in patterns:
                raise SystemExit(
                    f"the catalog already publishes `{name}`; the edge families "
                    "are read from the CoNLL-U and must not also be matched"
                )
            patterns[name] = {
                group: sum(rows.values()) for group, rows in groups.items()
            }
            if dispersion:
                per_document[name] = groups
        # The edge reader and ling split documents on the same `# newdoc`
        # boundaries, so disagreement here means they are not reading the same
        # file -- worth saying rather than silently dispersing against the
        # wrong denominator.
        if dispersion and edge_tokens != document_tokens:
            observed = sum(edge_tokens.values())
            print(
                f"warning: --edges saw {len(edge_tokens)} document(s) and "
                f"{observed} tokens; ling saw {len(document_tokens)} and "
                f"{tokens}. Dispersion for the edge families may be measured "
                "against a different partition than the rest.",
                file=sys.stderr,
            )

    document = {
        "corpus": arguments.corpus,
        "patterns_file": arguments.patterns,
        "label": arguments.label or corpus_label(corpus),
        "tokens": tokens,
        "sentences": counters.get("sentences_admitted", 0),
        "counters": counters,
        "profile": patterns,
    }
    if dispersion:
        document["documents"] = per_document
        document["document_tokens"] = document_tokens
    with open(arguments.output, "w", encoding="utf-8") as sink:
        json.dump(document, sink, ensure_ascii=False, indent=2, sort_keys=True)
    print(
        f"{document['label']}: {tokens} tokens, "
        f"{document['sentences']} sentences, {len(patterns)} patterns",
        file=sys.stderr,
    )
    return 0


# --------------------------------------------------------------------------
# compare
# --------------------------------------------------------------------------


@dataclass(frozen=True)
class Keyness:
    """One construction or collexeme contrasted across the two corpora."""

    item: str
    focus: int
    reference: int
    focus_rate: float
    reference_rate: float
    g2: float
    p_value: float
    log_odds: float
    # The ranking statistic and its interval. `effect` is what the report
    # sorts on; `log_odds` remains the point estimate the reader quotes.
    low: float = 0.0
    high: float = 0.0
    effect: float = 0.0
    # Filled by `compare` once every test in the run is known, because a
    # false-discovery rate is a property of the whole family and cannot be
    # computed one item at a time.
    q_value: float = 1.0
    # Present only when the profiles carry per-document counts.
    dispersion: float | None = None
    documents: int | None = None
    # Effect measured on the fold that did not nominate the item.
    holdout: float | None = None


def log_likelihood(focus: int, reference: int, focus_n: int, reference_n: int) -> float:
    """Dunning's G-squared for a 2x2 contingency of counts against totals.

    Reported alongside an effect size and never alone: at corpus sizes like
    these almost everything clears significance, so G-squared ranks by
    confidence that a difference is real, not by how much it matters.
    """
    total = focus_n + reference_n
    if total == 0 or (focus + reference) == 0:
        return 0.0
    expected_focus = focus_n * (focus + reference) / total
    expected_reference = reference_n * (focus + reference) / total
    value = 0.0
    if focus > 0 and expected_focus > 0:
        value += focus * math.log(focus / expected_focus)
    if reference > 0 and expected_reference > 0:
        value += reference * math.log(reference / expected_reference)
    return 2.0 * value


def chi_square_p(statistic: float) -> float:
    """Upper-tail probability of chi-square with one degree of freedom.

    Exactly `erfc(sqrt(x/2))` for 1 df, so this needs no SciPy -- which keeps
    the scoring half of the pipeline standard-library only.
    """
    if statistic <= 0:
        return 1.0
    return math.erfc(math.sqrt(statistic / 2.0))


def log_odds_ratio(
    focus: int, reference: int, focus_n: int, reference_n: int
) -> float:
    """Haldane-Anscombe corrected log odds ratio.

    The +0.5 keeps a zero cell finite, which matters here because the most
    interesting collexemes are often the ones absent from one arm entirely.
    """
    a = focus + 0.5
    b = max(focus_n - focus, 0) + 0.5
    c = reference + 0.5
    d = max(reference_n - reference, 0) + 0.5
    return math.log((a / b) / (c / d))


# The 95% two-sided normal quantile. A constant rather than a parameter
# because nothing here varies the confidence level, and the report's prose
# names 95% in three places.
Z_95 = 1.96


def log_odds_interval(
    focus: int, reference: int, focus_n: int, reference_n: int
) -> tuple[float, float]:
    """Wald confidence interval for the Haldane-Anscombe log odds ratio.

    The standard error of a log odds ratio is the root of the summed
    reciprocal cell counts, on the same +0.5-corrected cells the point
    estimate uses, so a zero cell stays finite here too.
    """
    a = focus + 0.5
    b = max(focus_n - focus, 0) + 0.5
    c = reference + 0.5
    d = max(reference_n - reference, 0) + 0.5
    centre = math.log((a / b) / (c / d))
    error = math.sqrt(1.0 / a + 1.0 / b + 1.0 / c + 1.0 / d)
    return centre - Z_95 * error, centre + Z_95 * error


def conservative_effect(low: float, high: float) -> float:
    """The end of the interval nearest zero, or zero if it spans zero.

    This is the ranking statistic, and it is the answer to the small-count
    inflation that ranking by the point estimate alone would introduce: an
    item seen once in one arm and never in the other has an enormous log odds
    ratio and an interval so wide it reaches zero, so it sorts where it
    belongs instead of at the top. Reading it as "the effect is at least this
    large, in this direction" is exactly right.
    """
    if low > 0.0:
        return low
    if high < 0.0:
        return high
    return 0.0


def benjamini_hochberg(p_values: Sequence[float]) -> list[float]:
    """Benjamini-Hochberg adjusted q-values, in the input order.

    Replaces the Bonferroni threshold this module used to apply. Bonferroni
    controls the probability of even one false positive, which is the wrong
    error to spend power on when the output is a ranked shortlist for a human
    to read: over ~18,000 tests it discards findings that are almost
    certainly real. BH controls the expected PROPORTION of false positives
    among those reported, which is what a shortlist's reader actually needs
    to know.

    The running minimum walking down from the largest p-value is what keeps
    the adjusted values monotone in the raw ones.
    """
    total = len(p_values)
    if total == 0:
        return []
    order = sorted(range(total), key=lambda index: p_values[index])
    adjusted = [1.0] * total
    running = 1.0
    for rank in range(total - 1, -1, -1):
        index = order[rank]
        scaled = p_values[index] * total / (rank + 1)
        running = min(running, scaled)
        adjusted[index] = min(1.0, running)
    return adjusted


def gries_dp(by_document: dict[int, int], document_tokens: dict[int, int]) -> float:
    """Gries' deviation of proportions over the documents of one corpus.

    0 means the item is spread exactly in proportion to document size; values
    approaching 1 mean it is concentrated in a few documents. The measure is
    here because frequency alone cannot tell those apart, and for register
    work the difference is the whole question: a construction used once each
    by two hundred files is a property of how the corpus writes, while the
    same count from three files is a property of three files.

    Sizes come from the denominator pattern rather than from a document
    length the profile does not otherwise carry, so the expected proportions
    are the same tokens every rate in this module is taken over.
    """
    total_tokens = sum(document_tokens.values())
    total_count = sum(by_document.values())
    if total_tokens <= 0 or total_count <= 0:
        return 0.0
    deviation = 0.0
    for document, size in document_tokens.items():
        expected = size / total_tokens
        observed = by_document.get(document, 0) / total_count
        deviation += abs(observed - expected)
    return 0.5 * deviation


def fold_of(document: int) -> int:
    """Which of two holdout folds a document belongs to.

    Documents, not sentences: sentences inside one file share an author, a
    subject and a house style, so splitting on them would leak the very
    signal the holdout exists to keep honest.

    Keyed on a digest of the ordinal rather than `ordinal % folds` so that the
    assignment does not align with any ordering the corpus was built in --
    harvest walks the tree in directory order, which would otherwise put whole
    subtrees in one fold.
    """
    digest = hashlib.blake2b(str(document).encode("utf-8"), digest_size=8).digest()
    return int.from_bytes(digest, "big") % 2


def score_items(
    focus_counts: dict[str, int],
    reference_counts: dict[str, int],
    focus_n: int,
    reference_n: int,
    minimum: int,
    focus_documents: dict[str, dict[int, int]] | None = None,
    reference_documents: dict[str, dict[int, int]] | None = None,
    focus_tokens: dict[int, int] | None = None,
    reference_tokens: dict[int, int] | None = None,
) -> list[Keyness]:
    # The holdout arm totals are fixed for the whole call, so they are
    # computed once rather than per item.
    holdout_totals: tuple[int, int] | None = None
    if focus_tokens and reference_tokens:
        holdout_totals = (
            sum(size for doc, size in focus_tokens.items() if fold_of(doc) == 1),
            sum(size for doc, size in reference_tokens.items() if fold_of(doc) == 1),
        )

    scored = []
    for item in set(focus_counts) | set(reference_counts):
        focus = focus_counts.get(item, 0)
        reference = reference_counts.get(item, 0)
        if focus + reference < minimum:
            continue
        g2 = log_likelihood(focus, reference, focus_n, reference_n)
        low, high = log_odds_interval(focus, reference, focus_n, reference_n)

        dispersion = None
        documents = None
        focus_by_document = (focus_documents or {}).get(item)
        if focus_by_document is not None and focus_tokens:
            dispersion = gries_dp(focus_by_document, focus_tokens)
            documents = len(focus_by_document)

        holdout = None
        if holdout_totals is not None:
            reference_by_document = (reference_documents or {}).get(item, {})
            held_focus = sum(
                count
                for doc, count in (focus_by_document or {}).items()
                if fold_of(doc) == 1
            )
            held_reference = sum(
                count
                for doc, count in reference_by_document.items()
                if fold_of(doc) == 1
            )
            focus_held_n, reference_held_n = holdout_totals
            if focus_held_n and reference_held_n:
                holdout = log_odds_ratio(
                    held_focus, held_reference, focus_held_n, reference_held_n
                )

        scored.append(
            Keyness(
                item=item,
                focus=focus,
                reference=reference,
                focus_rate=focus / focus_n * 10_000 if focus_n else 0.0,
                reference_rate=reference / reference_n * 10_000 if reference_n else 0.0,
                g2=g2,
                p_value=chi_square_p(g2),
                log_odds=log_odds_ratio(focus, reference, focus_n, reference_n),
                low=low,
                high=high,
                effect=conservative_effect(low, high),
                dispersion=dispersion,
                documents=documents,
                holdout=holdout,
            )
        )
    return scored


def marginalize(counts: dict[str, int]) -> dict[str, dict[str, int]]:
    """Split a joint key profile into one profile per key field.

    The joint `(adjective, noun)` sketch is the word profile, but as a
    REGISTER signal it is confounded with subject matter: two codebases about
    different things differ on `binary/data` and `compile/time` for reasons
    that have nothing to do with how they are written. Marginalizing onto the
    adjective alone holds topic much closer to constant, because the modifier
    inventory is shared vocabulary where the head-noun inventory is domain
    vocabulary. Both are reported; they answer different questions.
    """
    fields: dict[str, dict[str, int]] = {}
    for item, count in counts.items():
        parts = [part for part in item.split("|") if "=" in part]
        if len(parts) < 2:
            continue
        for part in parts:
            name, _, value = part.partition("=")
            bucket = fields.setdefault(name, {})
            bucket[value] = bucket.get(value, 0) + count
    return fields


def _document_tokens(document: dict) -> dict[int, int]:
    """Per-document token totals from a profile, keyed back to integers.

    JSON object keys are strings, so the ordinals a profile round-trips come
    back as text; `fold_of` and the dispersion expectations both key on the
    integer.
    """
    found = document.get("document_tokens")
    if not isinstance(found, dict):
        return {}
    return {int(key): value for key, value in found.items() if isinstance(value, int)}


def _normalize_documents(found: object) -> dict[str, dict[str, dict[int, int]]] | None:
    """Re-key a profile's per-document table from JSON strings to ordinals.

    Done once at load rather than at each use, because both consumers --
    dispersion's expected proportions and `fold_of` -- key on the integer,
    and a string ordinal would silently land every document in one fold.
    """
    if not isinstance(found, dict):
        return None
    table: dict[str, dict[str, dict[int, int]]] = {}
    for pattern, groups in found.items():
        if not isinstance(groups, dict):
            continue
        rows: dict[str, dict[int, int]] = {}
        for group, by_document in groups.items():
            if not isinstance(by_document, dict):
                continue
            rows[group] = {
                int(key): value
                for key, value in by_document.items()
                if isinstance(value, int)
            }
        table[pattern] = rows
    return table


def _pattern_documents(
    documents: dict[str, dict[str, dict[int, int]]] | None,
) -> dict[str, dict[int, int]] | None:
    """Collapse a per-group document table to one row per pattern.

    `constructions` scores whole patterns, whose count is the sum over their
    groups; this is that same sum taken per document so the dispersion beside
    it describes the same quantity.
    """
    if not documents:
        return None
    collapsed: dict[str, dict[int, int]] = {}
    for pattern, groups in documents.items():
        bucket: dict[int, int] = {}
        for by_document in groups.values():
            for key, count in by_document.items():
                bucket[key] = bucket.get(key, 0) + count
        collapsed[pattern] = bucket
    return collapsed


def load_profile(path: str) -> dict:
    with open(path, encoding="utf-8") as handle:
        document = json.load(handle)
    for required in ("profile", "tokens", "label"):
        if required not in document:
            raise SystemExit(f"{path} is not a profile: missing `{required}`")
    return document


@dataclass(frozen=True)
class Scored:
    """Everything `compare` ranks, so `locate` can rank it identically.

    Factored out because the Benjamini-Hochberg correction is a property of
    the whole family of tests: recomputing it over a different family would
    give `locate` different q-values from the report the reader is holding,
    for the same data. One scorer, one family, one set of numbers.
    """

    focus: dict
    reference: dict
    constructions: list[Keyness]
    collexemes: dict[str, list[Keyness]]
    tests: int
    dispersed: bool


def score_profiles(arguments: argparse.Namespace) -> Scored:
    focus = load_profile(arguments.focus)
    reference = load_profile(arguments.reference)
    focus_n = focus["tokens"]
    reference_n = reference["tokens"]
    if not focus_n or not reference_n:
        raise SystemExit("both profiles must carry a non-zero token total")

    focus_totals = {
        name: sum(groups.values())
        for name, groups in focus["profile"].items()
        if name != DENOMINATOR
    }
    reference_totals = {
        name: sum(groups.values())
        for name, groups in reference["profile"].items()
        if name != DENOMINATOR
    }
    # Per-document counts are present only when both profiles were built with
    # `profile --dispersion`. One arm alone cannot answer either question --
    # dispersion is reported for the focus arm but the holdout needs both --
    # so the pair is taken together or not at all.
    focus_documents = _normalize_documents(focus.get("documents"))
    reference_documents = _normalize_documents(reference.get("documents"))
    focus_tokens = _document_tokens(focus)
    reference_tokens = _document_tokens(reference)
    dispersed = bool(
        focus_documents and reference_documents and focus_tokens and reference_tokens
    )
    if not dispersed:
        focus_documents = reference_documents = None
        focus_tokens = reference_tokens = None

    def scored_for(
        focus_counts: dict[str, int],
        reference_counts: dict[str, int],
        pattern: str | None,
    ) -> list[Keyness]:
        per_document = (focus_documents or {}).get(pattern) if pattern else None
        reference_per_document = (
            (reference_documents or {}).get(pattern) if pattern else None
        )
        return score_items(
            focus_counts,
            reference_counts,
            focus_n,
            reference_n,
            arguments.min_count,
            focus_documents=per_document,
            reference_documents=reference_per_document,
            focus_tokens=focus_tokens,
            reference_tokens=reference_tokens,
        )

    # Construction totals sum over a pattern's groups, so their per-document
    # counts have to be summed the same way before dispersion means anything.
    constructions = score_items(
        focus_totals,
        reference_totals,
        focus_n,
        reference_n,
        arguments.min_count,
        focus_documents=_pattern_documents(focus_documents),
        reference_documents=_pattern_documents(reference_documents),
        focus_tokens=focus_tokens,
        reference_tokens=reference_tokens,
    )

    collexemes: dict[str, list[Keyness]] = {}
    for name in sorted(set(focus["profile"]) | set(reference["profile"])):
        if name == DENOMINATOR and not arguments.include_baseline:
            continue
        focus_groups = focus["profile"].get(name, {})
        reference_groups = reference["profile"].get(name, {})
        scored = scored_for(focus_groups, reference_groups, name)
        if scored:
            collexemes[name] = scored
        if arguments.no_marginals:
            continue
        focus_fields = marginalize(focus_groups)
        reference_fields = marginalize(reference_groups)
        for field in sorted(set(focus_fields) | set(reference_fields)):
            # A marginal sums several joint keys, so it has no per-document
            # table of its own; it is scored without dispersion rather than
            # with a wrong one.
            marginal = score_items(
                focus_fields.get(field, {}),
                reference_fields.get(field, {}),
                focus_n,
                reference_n,
                arguments.min_count,
            )
            if marginal:
                collexemes[f"{name} [{field}]"] = marginal

    # One Benjamini-Hochberg family over every test actually performed,
    # constructions and collexemes together. Splitting them would understate
    # the correction, since the same data produced both.
    ordered_names = list(collexemes)
    everything = constructions + [
        item for name in ordered_names for item in collexemes[name]
    ]
    q_values = iter(benjamini_hochberg([entry.p_value for entry in everything]))
    tests = len(everything)
    constructions = [replace(e, q_value=next(q_values)) for e in constructions]
    collexemes = {
        name: [replace(e, q_value=next(q_values)) for e in collexemes[name]]
        for name in ordered_names
    }

    return Scored(
        focus=focus,
        reference=reference,
        constructions=constructions,
        collexemes=collexemes,
        tests=tests,
        dispersed=dispersed,
    )


def compare(arguments: argparse.Namespace) -> int:
    scored = score_profiles(arguments)
    focus = scored.focus
    reference = scored.reference
    constructions = scored.constructions
    collexemes = scored.collexemes
    tests = scored.tests
    dispersed = scored.dispersed

    if arguments.emit_pattern:
        report = render_pattern(
            collexemes,
            threshold=arguments.alpha,
            top=arguments.top,
            tests=tests,
            minimum_documents=arguments.min_documents,
        )
    else:
        report = render_report(
            focus,
            reference,
            constructions,
            collexemes,
            threshold=arguments.alpha,
            tests=tests,
            top=arguments.top,
            json_output=arguments.json,
            dispersed=dispersed,
        )
    if arguments.output:
        Path(arguments.output).write_text(report, encoding="utf-8")
    else:
        sys.stdout.write(report)
    return 0


def render_pattern(
    collexemes: dict[str, list[Keyness]],
    threshold: float,
    top: int,
    tests: int,
    minimum_documents: int,
) -> str:
    """Emit surviving mined findings as promotable lingrep patterns definitions."""
    # A finding carried by one or two files is a fact about those files. It is
    # not rare for it to look like a register finding: over this corpus 1,219
    # of the 5,626 significant rows carrying dispersion sat in fewer than five
    # documents, and the median significant row had DP 0.88. Document support
    # is the criterion rather than a DP ceiling because it needs no explaining
    # and DP already implies it -- an item in one document has DP near 1 by
    # construction.
    measurable = any(
        item.documents is not None
        for items in collexemes.values()
        for item in items
    )
    if minimum_documents <= 0:
        support = "# No document-support floor is applied."
    elif not measurable:
        support = (
            f"# The --min-documents {minimum_documents} floor could NOT be"
            " applied: these\n# profiles carry no per-document counts. Rebuild"
            " both with `profile\n# --dispersion` to withhold findings carried"
            " by a handful of files."
        )
    else:
        support = (
            f"# Findings supported by fewer than {minimum_documents} documents"
            " are withheld:\n# concentrated in a few files is not a property of"
            " the corpus."
        )
    lines = [
        "# Mined register candidates, ranked by effect size.",
        "#",
        f"# {tests:,} candidates tested; only those whose Benjamini-Hochberg",
        f"# q-value clears {threshold:g} appear, ranked by the end of the 95%",
        "# interval nearest zero. Rates are per 10,000 tokens of the focus",
        "# corpus against the reference corpus.",
        "#",
        support,
        "#",
        "# These are OBSERVATIONS, not conclusions. A construction can differ",
        "# because the two corpora are written differently or because they are",
        "# about different things, and this file cannot tell you which.",
        "#",
        "# A promoted SEQUENCE pattern reproduces its mined count. Measured",
        "# over this corpus, all 90 of them agreed exactly -- including the",
        "# seven whose mined count was zero, which matched nothing here too.",
        "# That is not luck: the family already constrains every position its",
        "# key projects, so promotion rewrites the same query without the",
        "# captures and containment filtering removes the same overlaps.",
        "#",
        "# A promoted EDGE pattern does NOT, and the gap is not marginal. Its",
        "# family is counted from the CoNLL-U columns, exactly and with no",
        "# window, while the pattern below is matched over a bounded gap under",
        "# per-start selection. Over this corpus the matched pair recovered a",
        "# median of 98% of the exact count and as little as 40%. Each one says",
        "# so above itself; treat the rate in its comment as the measurement",
        "# and the pattern as an instrument for reading examples.",
        "",
    ]
    emitted = 0
    unrenderable = 0
    unsupported = 0
    for name in sorted(collexemes):
        # Marginals are reported for reading, not for promotion: a single
        # field of a key tuple is not a pattern.
        if name.endswith("]"):
            continue
        survivors = []
        for item in collexemes[name]:
            if item.q_value >= threshold:
                continue
            # `documents` is None when the profiles carry no per-document
            # counts, and then the floor cannot be applied at all. Passing the
            # item through is the honest reading -- withholding everything
            # would silently empty the file over a missing measurement rather
            # than a failed one -- and the header says which happened.
            if item.documents is not None and item.documents < minimum_documents:
                unsupported += 1
                continue
            survivors.append(item)
        ranked = sorted(survivors, key=lambda item: -abs(item.effect))[:top]
        rendered = []
        for item in ranked:
            text = emit_pattern(name, item)
            if text is None:
                unrenderable += 1
            else:
                rendered.append(text)
        if not rendered:
            continue
        lines.append(f"# --- from {name} ---")
        lines.extend(f"\n{text}" for text in rendered)
        lines.append("")
        emitted += len(rendered)
    if not emitted:
        lines.append("# Nothing cleared the threshold.")
    if unsupported:
        # Withheld findings are stated rather than dropped quietly: the count
        # is usually large, and a reader who does not know it has been applied
        # would read this file as everything that cleared significance.
        lines.append(
            f"\n# {unsupported} finding(s) cleared the threshold but were "
            f"supported by fewer than {minimum_documents} documents and are "
            f"absent from this file. Lower --min-documents to include them."
        )
    if unrenderable:
        # Say so rather than letting the file read as complete coverage. A
        # finding is unrenderable when its key names no known template, or
        # when a value cannot be written as an lingrep patterns literal at all -- an empty
        # lemma, or one outside the BMP that the four-digit escape cannot
        # address.
        lines.append(
            f"\n# {unrenderable} finding(s) cleared the threshold but could not "
            f"be rendered as lingrep patterns and are absent from this file."
        )
    return "\n".join(lines) + "\n"


def _row(entry: Keyness, threshold: float) -> str:
    mark = "*" if entry.q_value < threshold else " "
    dispersion = "-" if entry.dispersion is None else f"{entry.dispersion:.2f}"
    documents = "-" if entry.documents is None else str(entry.documents)
    holdout = "-" if entry.holdout is None else f"{entry.holdout:+.2f}"
    return (
        f"| {entry.item} | {entry.focus} | {entry.reference} | "
        f"{entry.focus_rate:.2f} | {entry.reference_rate:.2f} | "
        f"{entry.log_odds:+.2f} | {entry.low:+.2f}..{entry.high:+.2f} | "
        f"{entry.effect:+.2f} | {entry.q_value:.1e}{mark} | "
        f"{dispersion} | {documents} | {holdout} |"
    )


def parse_key(item: str) -> dict[str, str]:
    """Recover the `name -> value` mapping `render_group` flattened."""
    fields = {}
    for part in item.split("|"):
        name, separator, value = part.partition("=")
        if separator:
            fields[name] = value
    return fields


def emit_pattern(name: str, entry: Keyness) -> str | None:
    """Render one mined finding as a concrete lingrep patterns definition.

    This is the point of mining: a keyness row is an observation, and a
    definition is something a catalog can carry, a test can pin, and a
    reviewer can argue with. The templates mirror `MINING_FAMILIES` exactly,
    so a promoted pattern counts what the mined family counted -- with one
    deliberate exception noted below.
    """
    fields = parse_key(entry.item)
    comment = (
        f"# mined: {entry.focus_rate:.2f} vs {entry.reference_rate:.2f} per 10k, "
        f"log-odds {entry.log_odds:+.2f}, G2 {entry.g2:.1f}"
    )

    # An edge finding is counted from the CoNLL-U columns, where an edge has
    # no direction on the page, so promotion emits BOTH linear orders. They
    # cannot be one definition with an alternation: a capture inside an
    # alternation branch binds only when that branch is taken, which is
    # `WhereExclusiveCaptures`. Neither will reproduce the mined count -- the
    # gap truncates and per-start selection deflates -- so the pair is an
    # instrument for looking at examples, not for recounting.
    edge_caveat = (
        "\n# NOTE: matched, so this undercounts the exact edge count above:\n"
        "# the gap bounds the span and per-start selection keeps one candidate."
    )

    if {"h.upos", "d.dep.rel", "d.upos"} <= fields.keys():
        head, relation, dependent = (
            fields["h.upos"],
            fields["d.dep.rel"],
            fields["d.upos"],
        )
        identifier = _identifier(name, entry.item)
        return "\n\n".join(
            f"{comment}{edge_caveat}\npattern {identifier}_{suffix} :=\n  {order}\n"
            f"  where $h -[{relation}]-> $d."
            for suffix, order in (
                ("hf", f"(?<h> [upos={head}]) []{{0,8}} (?<d> [upos={dependent}])"),
                ("df", f"(?<d> [upos={dependent}]) []{{0,8}} (?<h> [upos={head}])"),
            )
        )

    if {"d.dep.rel", "d.lemma"} <= fields.keys():
        relation = fields["d.dep.rel"]
        lemma = _pattern_literal(fields["d.lemma"])
        if lemma is None:
            return None
        identifier = _identifier(name, entry.item)
        return "\n\n".join(
            f"{comment}{edge_caveat}\npattern {identifier}_{suffix} :=\n  {order}\n"
            f"  where $h -[{relation}]-> $d."
            for suffix, order in (
                ("hf", f"(?<h> []) []{{0,8}} (?<d> [lemma={lemma}])"),
                ("df", f"(?<d> [lemma={lemma}]) []{{0,8}} (?<h> [])"),
            )
        )

    if {"upos", "xpos"} <= fields.keys():
        part_of_speech = _pattern_literal(fields["upos"])
        tag = _pattern_literal(fields["xpos"])
        if part_of_speech is None or tag is None:
            return None
        return (
            f"{comment}\npattern {_identifier(name, entry.item)} := "
            f"[upos={part_of_speech} & xpos={tag}]."
        )

    tags = [fields[slot] for slot in ("a.xpos", "b.xpos", "c.xpos") if slot in fields]
    if tags:
        # Penn tags include `,`, `.`, `-LRB-` and `` `` ``, none of which
        # survive as a bare literal, so every one goes through the escape.
        escaped = [_pattern_literal(tag) for tag in tags]
        if any(tag is None for tag in escaped):
            return None
        body = " ".join(f"[xpos={tag}]" for tag in escaped)
        anchor = "^ " if name.startswith("opening") else ""
        return f"{comment}\npattern {_identifier(name, entry.item)} := {anchor}{body}."

    sequence = [fields[slot] for slot in ("a.upos", "b.upos", "c.upos") if slot in fields]
    if sequence:
        # The mined family captured each position only so the key could name
        # it. A promoted sequence needs no captures, so they are dropped --
        # the matched span is identical either way.
        anchor = "^ " if name.startswith("opening") else ""
        body = " ".join(f"[upos={value}]" for value in sequence)
        return f"{comment}\npattern {_identifier(name, entry.item)} := {anchor}{body}."

    return None


def _identifier(name: str, item: str) -> str:
    """A legal, unique lingrep patterns definition name for one mined finding.

    The family name is part of it, not decoration: the head-first and
    dep-first variants of one edge produce the SAME key tuple, so a name
    derived from the key alone collides and the promoted catalog fails to
    compile with a duplicate definition.

    Sanitizing is lossy on top of that -- the lemmas `.` and `,` both reduce
    to nothing -- so a digest of the untouched key is appended. Without it,
    two findings differing only in punctuation collide, which is a confusing
    way to learn that the readable half of the name is not the identifying
    half.
    """
    source = f"{name}\0{item}"
    digest = hashlib.blake2b(source.encode("utf-8"), digest_size=3).hexdigest()
    parts = re.sub(r"[^0-9A-Za-z]+", "_", f"{name}_{item}".replace(".", ""))
    cleaned = parts.strip("_").lower()
    return f"mined_{cleaned}_{digest}" if cleaned else f"mined_{digest}"


def _pattern_literal(value: str) -> str | None:
    r"""Escape a corpus value for use as an unquoted lingrep patterns literal.

    lingrep patterns has no quoted-value form, and an unquoted literal accepts neither a
    bare `.`, `(` nor `)`, while `&`, `|` and whitespace terminate it. Escape
    decoding runs after tokenization, so `\uXXXX` reaches every one of those
    contexts -- which is the same mechanism `patterns/double-wo.lx` uses for
    U+3000. Corpus lemmas are arbitrary text, so anything not plainly
    alphanumeric goes through the escape rather than being trusted.

    Above the BMP the four-digit escape cannot address the character, but the
    literal itself is UTF-8 and needs no escape: nothing that terminates an
    unquoted literal -- `&`, `|`, `.`, `(`, `)`, whitespace -- lives outside
    the BMP, so an astral character is written raw. `[text=U+1F600]` compiles.

    Returns None for a value that cannot be written at all, which an empty
    lemma is: `[lemma=]` is a syntax error, not an empty-string test.
    """
    if not value:
        return None
    out = []
    for character in value:
        if character.isalnum() or character == "_" or ord(character) > 0xFFFF:
            out.append(character)
        else:
            out.append(f"\\u{ord(character):04x}")
    return "".join(out)


def _unkeyed(document: dict) -> int:
    counters = document.get("counters")
    value = counters.get("records_unkeyed") if isinstance(counters, dict) else None
    return value if isinstance(value, int) else 0


HEADER = (
    "| item | focus n | ref n | focus /10k | ref /10k | log-odds | 95% CI | "
    "effect | q | DP | docs | holdout |\n"
    "|---|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|---:|"
)


def render_report(
    focus: dict,
    reference: dict,
    constructions: list[Keyness],
    collexemes: dict[str, list[Keyness]],
    threshold: float,
    tests: int,
    top: int,
    json_output: bool,
    dispersed: bool = False,
) -> str:
    if json_output:
        return (
            json.dumps(
                {
                    "focus": focus["label"],
                    "reference": reference["label"],
                    "focus_tokens": focus["tokens"],
                    "reference_tokens": reference["tokens"],
                    "tests": tests,
                    "fdr_alpha": threshold,
                    "dispersion_available": dispersed,
                    "constructions": [asdict(item) for item in constructions],
                    "collexemes": {
                        name: [asdict(item) for item in items]
                        for name, items in collexemes.items()
                    },
                },
                indent=2,
                sort_keys=True,
            )
            + "\n"
        )

    lines = [
        f"# Register contrast: {focus['label']} vs {reference['label']}",
        "",
        f"Focus corpus `{focus['label']}`: {focus['tokens']:,} tokens, "
        f"{focus.get('sentences', 0):,} sentences.",
        f"Reference corpus `{reference['label']}`: {reference['tokens']:,} tokens, "
        f"{reference.get('sentences', 0):,} sentences.",
        "",
        "Rates are per 10,000 tokens. A positive log-odds means the item is "
        "overrepresented in the focus corpus.",
        "",
        "Rows are ranked by `effect`, the end of the 95% interval nearest "
        "zero -- read it as \"at least this large, in this direction\", and "
        "zero as \"the interval spans zero\". Ranking on the point estimate "
        "would put every one-versus-zero item on top; ranking on G-squared, "
        "as this report used to, ranks confidence that a difference exists "
        "rather than how large it is.",
        "",
        f"`q` is the Benjamini-Hochberg false-discovery rate over all {tests:,} "
        f"tests and `*` marks q < {threshold:g}. BH replaced Bonferroni here "
        "because the output is a shortlist for a reader, so the useful "
        "guarantee is the expected proportion of false positives among the "
        "rows shown, not the probability of even one across the whole family.",
        "",
        (
            "`DP` is Gries' deviation of proportions over the focus corpus's "
            f"{len(_document_tokens(focus)):,} documents: 0 is spread exactly "
            "in proportion to document length, and values toward 1 are "
            "concentrated in few documents. `docs` is how many contain the "
            "item at all. A large effect at high DP is a fact about a few "
            "files, not about the corpus. `holdout` is the log-odds recomputed "
            "on a disjoint half of the documents in each arm, split by a "
            "digest of the document ordinal; an effect that shrinks toward "
            "zero there was carried by the other half."
            if dispersed
            else "`DP`, `docs` and `holdout` are empty because these profiles "
            "carry no per-document counts. Rebuild both with `profile "
            "--dispersion` to populate them."
        ),
        "",
        # A record whose declared key cannot be satisfied is dropped from
        # keyed output rather than published as a partial tuple, so it is
        # absent from every count above. Stating the number keeps that from
        # reading as coverage it is not.
        f"Records dropped as unkeyable: {_unkeyed(focus)} in "
        f"{focus['label']}, {_unkeyed(reference)} in {reference['label']}.",
        "",
        "## Constructions",
        "",
        HEADER,
    ]
    ranked = sorted(constructions, key=lambda item: -abs(item.effect))
    lines.extend(_row(entry, threshold) for entry in ranked)

    lines.extend(["", "## Distinctive collexemes", ""])
    for name in sorted(collexemes):
        items = [item for item in collexemes[name] if item.q_value < threshold]
        if not items:
            continue
        # Split by direction before taking the top N. Ranking the whole set
        # and slicing would put items into a table whose heading contradicts
        # their sign whenever one direction has fewer than `top` survivors.
        # Direction comes from the point estimate and magnitude from the
        # conservative bound, so an item whose interval spans zero sorts last
        # in its own direction rather than switching sides.
        over = sorted(
            (item for item in items if item.log_odds > 0),
            key=lambda item: -item.effect,
        )[:top]
        under = sorted(
            (item for item in items if item.log_odds < 0),
            key=lambda item: item.effect,
        )[:top]
        if not over and not under:
            continue
        lines.append(f"### {name}")
        for heading, group in (
            (focus["label"], over),
            (reference["label"], under),
        ):
            lines.extend(["", f"Overrepresented in {heading}:", ""])
            if not group:
                lines.append("_none clearing the threshold._")
                continue
            lines.append(HEADER)
            lines.extend(_row(entry, threshold) for entry in group)
        lines.append("")
    return "\n".join(lines) + "\n"


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


# --------------------------------------------------------------------------
# locate
# --------------------------------------------------------------------------
#
# `compare` answers "what does this corpus overuse?". `locate` answers "where",
# which is the question a writer can act on. It ranks with exactly the same
# scorer -- same family, same Benjamini-Hochberg correction, same numbers as
# the report already read -- and then replays the catalog over the corpus to
# print one `path:line: text` per occurrence, grep-style, so an editor can jump
# to it.
#
# Overuse and underuse are NOT symmetric here, and the output says so rather
# than papering over it. An overused construction has occurrences to point at.
# An underused one has none by definition: there is no line number for a thing
# somebody did not write. What can be shown instead is where the REFERENCE
# corpus writes it, as a model rather than a defect, which is what
# `--reference-corpus` does.


SENT_ID = re.compile(r"^#\s*sent_id\s*=\s*(?P<value>.*)$")


def read_conllu_locations(path: Path) -> list[str]:
    """The `# sent_id` of each sentence, in the order ling admits them.

    `parse` writes `path:line#ordinal`, so this list is the whole provenance
    chain: a match record's `corpus_sentence_ordinal` indexes it and the entry
    names the file and the line the comment came from.

    A sentence is counted on the blank line that closes it and only when it
    carried token rows, which is the rule the pretokenized reader applies too.
    That is what keeps the two orderings aligned -- and `locate` checks the
    length against the run's own `sentences_admitted` rather than trusting it,
    because a drift of one would not fail, it would cite the wrong line.
    """
    located: list[str] = []
    pending: str | None = None
    rows = 0
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.rstrip("\n")
            if not line:
                if rows:
                    located.append(pending or "")
                pending = None
                rows = 0
                continue
            if line.startswith("#"):
                found = SENT_ID.match(line)
                if found:
                    pending = found.group("value").strip()
                continue
            if line.split("\t", 1)[0].isdigit():
                rows += 1
    if rows:
        located.append(pending or "")
    return located


def split_sent_id(value: str) -> tuple[str, int]:
    """`path:line#ordinal` back into a file and a line number."""
    head = value.rsplit("#", 1)[0]
    path, _, line = head.rpartition(":")
    if path and line.isdigit():
        return path, int(line)
    return head or "?", 0


@dataclass(frozen=True)
class Target:
    """One ranked item and the test that decides whether a match record is it.

    Three shapes, because `compare` ranks three. A construction is a pattern
    name. A collexeme is a pattern name plus the rendered key group. A
    marginal is a pattern name plus one key field and its value -- and that
    last is the one worth locating for prose work, because "the adjective slot
    is over-filled by `robust`" points at words a writer can change.
    """

    label: str
    entry: Keyness
    pattern: str
    level: str = "construction"
    group: str | None = None
    field: str | None = None
    value: str | None = None

    def selects(self, data: dict, group: str | None) -> bool:
        """`group` is `match_group(data)`, computed once per record.

        Both keyed shapes go through that one rendering rather than reading
        the record's `key` list directly, because it is the same function
        `profile` grouped by and `marginalize` split -- so a target either
        agrees with the profile it was ranked from or the mismatch is a bug in
        one shared place instead of a silent miss in two parallel ones. The
        `$` on a capture projection, for one, is stripped there and nowhere
        else.
        """
        if data.get("pattern") != self.pattern:
            return False
        if self.group is not None:
            return group == self.group
        if self.field is not None:
            return group is not None and parse_key(group).get(self.field) == self.value
        return True


MARGINAL_NAME = re.compile(r"^(?P<pattern>.+?) \[(?P<field>[^\]]+)\]$")


def targets_from(scored: Scored, arguments: argparse.Namespace) -> tuple[list, list]:
    """Rank every item, then split it into what can be located and what cannot.

    `token` is the denominator and never a finding. `dep_triple` and
    `rel_filler` are the edge families, which exist to supply a denominator
    for dispersion rather than to be read as constructions.
    """
    excluded = {DENOMINATOR, *EDGE_FAMILIES}
    candidates: list[Target] = []

    for entry in scored.constructions:
        if entry.item in excluded:
            continue
        candidates.append(
            Target(
                label=entry.item,
                entry=entry,
                pattern=entry.item,
                level="construction",
            )
        )

    for name, entries in scored.collexemes.items():
        marginal = MARGINAL_NAME.match(name)
        pattern = marginal.group("pattern") if marginal else name
        if pattern in excluded:
            continue
        for entry in entries:
            if marginal:
                field = marginal.group("field")
                candidates.append(
                    Target(
                        label=f"{pattern} \u00b7 {field}={entry.item}",
                        entry=entry,
                        pattern=pattern,
                        level="collexeme",
                        field=field,
                        value=entry.item,
                    )
                )
            else:
                candidates.append(
                    Target(
                        label=f"{pattern} \u00b7 {entry.item}",
                        entry=entry,
                        pattern=pattern,
                        level="collexeme",
                        group=entry.item,
                    )
                )

    surviving = [
        target
        for target in candidates
        if target.entry.q_value <= arguments.alpha
        and target.entry.effect != 0.0
        and (arguments.level == "all" or target.level == arguments.level)
    ]
    if arguments.min_documents and scored.dispersed:
        surviving = [
            target
            for target in surviving
            if target.entry.documents is None
            or target.entry.documents >= arguments.min_documents
        ]
    surviving.sort(key=lambda target: -abs(target.entry.effect))
    over = [t for t in surviving if t.entry.effect > 0][: arguments.top]
    under = [t for t in surviving if t.entry.effect < 0][: arguments.top]
    return over, under


def render_match_text(data: dict) -> str:
    """The match and its context on one line, with the span bracketed."""

    def words(items) -> list[str]:
        return [
            str(item.get("attributes", {}).get("text", ""))
            for item in (items or [])
            if isinstance(item, dict)
        ]

    left = " ".join(word for word in words(data.get("left_context")) if word)
    span = " ".join(word for word in words(data.get("tokens")) if word)
    right = " ".join(word for word in words(data.get("right_context")) if word)
    return " ".join(part for part in (left, f"⟦{span}⟧", right) if part)


def collect_locations(
    corpus: Path,
    targets: Sequence[Target],
    arguments: argparse.Namespace,
) -> dict[str, list[tuple[str, int, str]]]:
    """Replay the catalog over one corpus and keep the hits the targets select."""
    if not targets:
        return {}
    located = read_conllu_locations(corpus)
    found: dict[str, list[tuple[str, int, str]]] = {t.label: [] for t in targets}
    wanted = {t.pattern for t in targets}

    def consume(stream) -> None:
        sentences = None
        for line in stream:
            if not line.strip():
                continue
            event = json.loads(line)
            if event.get("schema") != SCHEMA:
                raise ValueError(f"expected {SCHEMA}")
            if event.get("type") == "complete":
                sentences = (event.get("data") or {}).get("counters", {}).get(
                    "sentences_admitted"
                )
                continue
            if event.get("type") != "match":
                continue
            data = event.get("data") or {}
            if data.get("pattern") not in wanted:
                continue
            ordinal = data.get("corpus_sentence_ordinal")
            if not isinstance(ordinal, int) or not 0 <= ordinal < len(located):
                continue
            # An unkeyed record has no group, which is not an error here: a
            # construction-level target never asks for one.
            try:
                group = match_group(data)
            except ValueError:
                group = None
            path, line_number = split_sent_id(located[ordinal])
            text = None
            for target in targets:
                if len(found[target.label]) >= arguments.limit:
                    continue
                if target.selects(data, group):
                    if text is None:
                        text = render_match_text(data)
                    found[target.label].append((path, line_number, text))
        # Checked after the stream rather than before it, because the count
        # lives in the closing event. An index one short of the corpus would
        # cite every line after the gap to the wrong file, silently.
        #
        # This is only a corpus total because the run below is `--limit all`.
        # A stopped run reports the prefix its reader observed instead -- the
        # same counter, a different quantity -- so a partial limit here would
        # make every run look misaligned.
        if sentences is not None and sentences != len(located):
            raise SystemExit(
                f"{corpus}: ling admitted {sentences} sentences but the "
                f"file indexes {len(located)}; locations would be misattributed"
            )

    command = [
        arguments.lingrep,
        "matches",
        "--patterns",
        arguments.patterns,
        "--json",
        "--limit",
        "all",
        "--context",
        str(arguments.context),
        str(corpus),
    ]
    run_lingrep(command, consume)
    return found


def render_locations(
    heading: str,
    note: str,
    targets: Sequence[Target],
    found: dict[str, list[tuple[str, int, str]]],
    by_file: bool,
) -> list[str]:
    if not targets:
        return []
    lines = [f"## {heading}", "", note, ""]
    if by_file:
        rows = [
            (path, number, target.label, text)
            for target in targets
            for path, number, text in found.get(target.label, ())
        ]
        rows.sort(key=lambda row: (row[0], row[1], row[2]))
        for path, number, label, text in rows:
            lines.append(f"{path}:{number}: [{label}] {text}")
        lines.append("")
        return lines

    for target in targets:
        entry = target.entry
        hits = sorted(found.get(target.label, ()))
        lines.append(f"### {target.label}")
        dispersion = (
            f", DP {entry.dispersion:.2f} over {entry.documents} documents"
            if entry.dispersion is not None
            else ""
        )
        lines.append(
            f"{entry.focus_rate:.2f} against {entry.reference_rate:.2f} per 10k, "
            f"effect {entry.effect:+.2f}, q {entry.q_value:.1e}{dispersion}"
        )
        lines.append("")
        if not hits:
            lines.append("_no occurrence in this corpus_")
        for path, number, text in hits:
            lines.append(f"{path}:{number}: {text}")
        lines.append("")
    return lines


def locate(arguments: argparse.Namespace) -> int:
    """Print `path:line: text` for the occurrences behind a keyness report."""
    scored = score_profiles(arguments)
    over, under = targets_from(scored, arguments)

    report = [
        (
            f"# Located register findings: {scored.focus['label']} "
            f"against {scored.reference['label']}"
        ),
        "",
        (
            f"{scored.tests} tests scored, q <= {arguments.alpha}, "
            f"{arguments.level} level, at most {arguments.limit} occurrences "
            f"shown per item."
        ),
        "",
    ]
    if arguments.level != "construction":
        report += [
            (
                "**Collexemes are confounded with subject matter.** A "
                "construction is a shape and survives a change of topic; a "
                "collexeme is a word, and two corpora about different things "
                "differ on their own vocabulary for reasons that are nothing "
                "to do with register. Expect domain nouns near the top, and "
                "read the marginals -- which hold the head noun constant -- "
                "before the joint keys."
            ),
            "",
        ]

    corpus = Path(arguments.corpus)
    report += render_locations(
        "Overused — places to look",
        "Each line is one occurrence of something this corpus does more than "
        "the reference does. The bracketed span is the match.",
        over,
        collect_locations(corpus, over, arguments),
        arguments.by_file,
    )

    if not under:
        pass
    elif arguments.reference_corpus:
        report += render_locations(
            "Underused — models from the reference corpus",
            "There is no line number for a thing nobody wrote, so these are "
            "occurrences in the REFERENCE corpus: what the comparison arm does "
            "that this one does not.",
            under,
            collect_locations(
                Path(arguments.reference_corpus), under, arguments
            ),
            arguments.by_file,
        )
    else:
        report += ["## Underused — not locatable here", ""]
        report += [
            (
                "An underused construction has no occurrences to point at. "
                "Pass `--reference-corpus` to see where the comparison arm "
                "writes them."
            ),
            "",
        ]
        for target in under:
            entry = target.entry
            report.append(
                f"- `{target.label}`: {entry.focus_rate:.2f} against "
                f"{entry.reference_rate:.2f} per 10k, effect "
                f"{entry.effect:+.2f}, q {entry.q_value:.1e}"
            )
        report.append("")

    text = "\n".join(report) + "\n"
    if arguments.output:
        Path(arguments.output).write_text(text, encoding="utf-8")
    else:
        sys.stdout.write(text)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="comment_register",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    subcommands = parser.add_subparsers(dest="command", required=True)

    harvester = subcommands.add_parser(
        "harvest", help="extract comments and docstrings from source trees"
    )
    harvester.add_argument(
        "roots",
        nargs="+",
        help="files or directories to scan; .rs and .py contribute comments "
        "and docstrings, .md contributes standalone prose",
    )
    harvester.add_argument("-o", "--output", required=True, help="JSONL destination")
    harvester.add_argument(
        "--relative-to",
        help="report paths relative to this directory, so `sent_id` values "
        "stay stable across checkouts",
    )
    harvester.set_defaults(handler=harvest)

    parse_command = subcommands.add_parser(
        "parse", help="parse harvested comments into CoNLL-U"
    )
    parse_command.add_argument("input", help="JSONL produced by `harvest`")
    parse_command.add_argument("-o", "--output", required=True, help="CoNLL-U path")
    parse_command.add_argument(
        "--model",
        default="en_core_web_trf",
        help="spaCy model; the transformer model is the accurate GPU path and "
        "en_core_web_sm the quick CPU one",
    )
    parse_command.add_argument(
        "--gpu", action="store_true", help="require a GPU rather than running on CPU"
    )
    parse_command.add_argument("--gpu-device", type=int, default=0)
    parse_command.add_argument("--batch-size", type=int, default=64)
    parse_command.add_argument(
        "--kind",
        default="all",
        choices=("all", DOC, LINE, BLOCK, PROSE),
        help="restrict to one comment kind; doc comments, inline notes and "
        "standalone Markdown prose are different registers and pooling them "
        "can manufacture a difference",
    )
    parse_command.add_argument(
        "--no-ner",
        action="store_true",
        help="skip the entity recognizer, which this catalog never reads",
    )
    parse_command.set_defaults(handler=parse)

    miner = subcommands.add_parser(
        "mine", help="generate a candidate catalog for register mining"
    )
    # Optional since the edge families left: the remaining families are fixed
    # templates over portable attributes and read nothing from a corpus. Kept
    # accepted, rather than removed, so an existing invocation still runs.
    miner.add_argument(
        "corpora", nargs="*", help="ignored; the remaining families read no corpus"
    )
    miner.add_argument("-o", "--output", required=True, help="generated .lx path")
    miner.add_argument(
        "--family",
        action="append",
        choices=sorted(MINING_FAMILIES),
        help="restrict to one candidate family; repeatable",
    )
    miner.set_defaults(handler=mine)

    profiler = subcommands.add_parser(
        "profile", help="count catalog matches over a parsed corpus"
    )
    profiler.add_argument("corpus", help="CoNLL-U file or directory")
    profiler.add_argument(
        "--patterns", default="patterns/comment-register.lx", help="lingrep patterns catalog"
    )
    profiler.add_argument("-o", "--output", required=True, help="profile JSON path")
    profiler.add_argument("--label", help="name for this arm in reports")
    profiler.add_argument("--where", help="ling scope clause")
    profiler.add_argument(
        "--edges",
        metavar="CONLLU",
        help="add exact dependency-edge families read from this CoNLL-U file's "
        "columns 7 and 8, rather than matched. Needed because the lingrep patterns "
        "formulation of those families undercounts non-uniformly",
    )
    profiler.add_argument(
        "--dispersion",
        action="store_true",
        help="record per-document counts, so `compare` can report Gries' DP "
        "and a held-out half. Reads `matches` instead of `summarize`, which "
        "is roughly 8x slower on a large mined catalog: use it for the "
        "confirmatory pass, not the mining sweep",
    )
    profiler.add_argument(
        "--ling", default="ling", help="ling binary to run"
    )
    profiler.set_defaults(handler=profile)

    comparison = subcommands.add_parser(
        "compare", help="keyness of one profile against another"
    )
    comparison.add_argument("focus", help="profile whose overuse is being asked about")
    comparison.add_argument("reference", help="profile it is measured against")
    comparison.add_argument("-o", "--output", help="write here instead of stdout")
    comparison.add_argument("--json", action="store_true", help="emit JSON")
    comparison.add_argument(
        "--top", type=int, default=15, help="collexemes shown per direction"
    )
    comparison.add_argument(
        "--min-count",
        type=int,
        default=5,
        help="pooled count below which an item is not tested at all",
    )
    comparison.add_argument(
        "--alpha",
        type=float,
        default=0.05,
        help="Benjamini-Hochberg false-discovery rate: the expected share of "
        "reported rows that are false positives (default 0.05)",
    )
    comparison.add_argument(
        "--min-documents",
        type=int,
        default=5,
        help="documents a finding must occur in before it is promoted by "
        "--emit-pattern (default 5). Needs profiles built with `profile "
        "--dispersion`; without them nothing is withheld",
    )
    comparison.add_argument(
        "--emit-pattern",
        action="store_true",
        help="render surviving findings as lingrep patterns definitions ready to promote "
        "into a hand-written catalog",
    )
    comparison.add_argument(
        "--no-marginals",
        action="store_true",
        help="skip the per-field marginals of each joint word sketch; the "
        "marginals are what separate register from subject matter",
    )
    comparison.add_argument(
        "--include-baseline",
        action="store_true",
        help="also rank the plain unigram profile, the bag-of-words control "
        "that shows what grammatical conditioning adds over a word list",
    )
    comparison.set_defaults(handler=compare)

    locator = subcommands.add_parser(
        "locate",
        help="grep-style occurrences behind a keyness report: path:line: text",
    )
    locator.add_argument("corpus", help="CoNLL-U the focus profile was built from")
    locator.add_argument("focus", help="profile whose overuse is being asked about")
    locator.add_argument("reference", help="profile it is measured against")
    locator.add_argument(
        "--patterns", required=True, help="lingrep patterns catalog, the same one `profile` used"
    )
    locator.add_argument("-o", "--output", help="write here instead of stdout")
    locator.add_argument(
        "--reference-corpus",
        help="CoNLL-U behind the reference profile. Underused constructions "
        "have no occurrences here to point at, so with this they are located "
        "in the comparison arm instead, as models rather than defects",
    )
    locator.add_argument(
        "--level",
        choices=("construction", "collexeme", "all"),
        default="construction",
        help="what to locate. `construction` is the default because a "
        "construction is a grammatical shape and survives a change of "
        "subject, where a collexeme is a word and mostly will not; scoring "
        "always covers everything, so the q-values match `compare` either way",
    )
    locator.add_argument(
        "--top", type=int, default=10, help="items located per direction"
    )
    locator.add_argument(
        "--limit", type=int, default=20, help="occurrences shown per item"
    )
    locator.add_argument(
        "--context",
        type=int,
        default=6,
        help="tokens of context each side of the match (default 6)",
    )
    locator.add_argument(
        "--by-file",
        action="store_true",
        help="sort by path and line rather than grouping by item, so the "
        "output reads as one pass through the files",
    )
    locator.add_argument(
        "--ling", default="ling", help="ling binary to run"
    )
    # Shared with `compare` so the two rank identically. A different threshold
    # here would locate rows the report the reader is holding does not carry.
    locator.add_argument("--min-count", type=int, default=5, help=argparse.SUPPRESS)
    locator.add_argument("--alpha", type=float, default=0.05, help=argparse.SUPPRESS)
    locator.add_argument("--min-documents", type=int, default=5, help=argparse.SUPPRESS)
    locator.add_argument("--no-marginals", action="store_true", help=argparse.SUPPRESS)
    locator.add_argument(
        "--include-baseline", action="store_true", help=argparse.SUPPRESS
    )
    locator.set_defaults(handler=locate)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    return int(arguments.handler(arguments))


if __name__ == "__main__":
    sys.exit(main())
