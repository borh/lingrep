"""Result types for native sentence matches and published corpus searches."""
from typing import Any, TypedDict

Cell = str | list[str] | None


class Span(TypedDict):
    """A half-open range: token indices for `span`, code points for `char_span`."""
    start: int
    end: int


class Capture(TypedDict):
    name: str
    hygienic_path: str
    span: Span
    char_span: Span | None


class LexicalMatch(TypedDict):
    term: str
    matched_by: list[str]


class RawMatch(TypedDict):
    pattern: str
    span: Span
    char_span: Span | None
    captures: list[Capture]
    interpretations: list[dict[str, Any]]
    graph_bindings: list[dict[str, Any]]
    lexical: LexicalMatch | None


class _OptionalHitFields(TypedDict, total=False):
    char_span: Span
    source_metadata: dict[str, Any]


class PublishedMatch(_OptionalHitFields):
    pattern: str
    surface: str
    span: Span
    captures: list[dict[str, Any]]
    tokens: list[dict[str, Any]]
    corpus_sentence_ordinal: int
