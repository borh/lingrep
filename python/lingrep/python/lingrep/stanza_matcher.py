from collections.abc import Iterator, Sequence
from typing import NamedTuple

from stanza.models.common.doc import Document, Word

from .documents import STANZA_ATTRIBUTES, adapter, schema_of, sentence
from .query import Query, as_query
from .records import RawMatch


class Capture(NamedTuple):
    name: str
    words: tuple[Word, ...]


class Match(NamedTuple):
    words: tuple[Word, ...]
    captures: tuple[Capture, ...]
    raw: RawMatch


class Matcher:
    """Compile a lingrep pattern or catalog for repeated matching on Stanza documents."""

    def __init__(self, query: str | Query, *, attributes: Sequence[str] = STANZA_ATTRIBUTES) -> None:
        self._adapter = adapter("lingrep_stanza_adapter", "lingrep-stanza-adapter")
        self._projection = self._adapter.document_projection(attributes)
        self._schema = schema_of(self._projection)
        self._query = as_query(query).compile(self._schema)

    def __call__(self, doc: Document) -> Iterator[Match]:
        text = (doc.text or "").encode("utf-8")
        for record, source in self._adapter.iter_document_sentences(doc, self._projection):
            words = source.words
            for hit in self._query.find_matches(sentence(text, self._schema, record)):
                yield Match(
                    tuple(words[hit["span"]["start"]:hit["span"]["end"]]),
                    tuple(Capture(c["name"], tuple(words[c["span"]["start"]:c["span"]["end"]]))
                          for c in hit["captures"]),
                    hit,
                )
