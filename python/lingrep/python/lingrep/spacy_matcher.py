from collections.abc import Iterator, Sequence
from typing import NamedTuple

from spacy.tokens import Doc, Span

from .documents import SPACY_ATTRIBUTES, adapter, schema_of, sentence
from .query import Query, as_query
from .records import RawMatch
from .records import Span as TokenRange


class Match(NamedTuple):
    span: Span
    captures: tuple[Span, ...]
    raw: RawMatch


def _span(doc: Doc, indices: list[int], words: TokenRange, label: str) -> Span:
    """The Doc span from the first matched word through the last, or an empty span at its position."""
    start, end = words["start"], words["end"]
    if start < len(indices):
        first = indices[start]
    elif indices and doc.has_annotation("SENT_START"):
        first = doc[indices[-1]].sent.end
    else:
        first = len(doc)
    return Span(doc, first, indices[end - 1] + 1 if end > start else first, label=label)


class Matcher:
    """Compile a lingrep pattern or catalog for repeated matching on spaCy documents."""

    def __init__(self, query: str | Query, *, attributes: Sequence[str] = SPACY_ATTRIBUTES) -> None:
        self._adapter = adapter("lingrep_spacy_adapter", "lingrep-spacy-adapter")
        self._projection = self._adapter.document_projection(attributes)
        self._schema = schema_of(self._projection)
        self._query = as_query(query).compile(self._schema)

    def __call__(self, doc: Doc) -> Iterator[Match]:
        text = doc.text.encode("utf-8")
        for record, indices in self._adapter.iter_document_sentences(doc, self._projection):
            for hit in self._query.find_matches(sentence(text, self._schema, record)):
                yield Match(
                    _span(doc, indices, hit["span"], hit["pattern"]),
                    tuple(_span(doc, indices, c["span"], c["name"]) for c in hit["captures"]),
                    hit,
                )
