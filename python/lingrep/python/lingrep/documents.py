from collections.abc import Iterable, Iterator, Sequence
from importlib import import_module
from typing import TYPE_CHECKING, Any

from ._native import Schema, Sentence

if TYPE_CHECKING:
    from spacy.tokens import Doc
    from stanza import Document

STANZA_ATTRIBUTES = ("text", "lemma", "upos", "xpos")
SPACY_ATTRIBUTES = ("text", "lemma", "norm", "upos", "xpos")


def adapter(module: str, distribution: str) -> Any:
    try:
        return import_module(module)
    except ModuleNotFoundError as error:
        if error.name != module:
            raise
        raise ModuleNotFoundError(
            f"install the {distribution} wheel from the lingrep release that matches this package",
            name=error.name) from error


def schema_of(projection: Sequence[Any]) -> Schema:
    return Schema._from_fields([(a.key, a.kind == "string_set") for a in projection])


def sentence(text: bytes, schema: Schema, record: dict[str, Any]) -> Sentence:
    """Build a sentence from one adapter record, whose ranges are UTF-8 offsets into `text`."""
    start, end = record["source_range"]
    ranges = [(a - start, b - start) for a, b in record["surface_ranges"]]
    alignment = record["alignment"]
    words = ([(a, b) for a, b in alignment["word_ranges"]] if alignment["kind"] == "expanded"
             else [(i, i + 1) for i in range(len(ranges))])
    return Sentence._from_projection(text[start:end].decode("utf-8"), schema,
                                     record["values"], ranges, words, record["word_count"])


def _sentences(text: str, projection: Sequence[Any],
               records: Iterable[tuple[dict[str, Any], Any]]) -> Iterator[Sentence]:
    schema, source = schema_of(projection), text.encode("utf-8")
    for record, _ in records:
        yield sentence(source, schema, record)


def from_spacy(doc: "Doc", *, attributes: Sequence[str] = SPACY_ATTRIBUTES) -> Iterator[Sentence]:
    """Project an existing spaCy or GiNZA Doc using its annotated sentence boundaries."""
    spacy = adapter("lingrep_spacy_adapter", "lingrep-spacy-adapter")
    projection = spacy.document_projection(attributes)
    return _sentences(doc.text, projection, spacy.iter_document_sentences(doc, projection))


def from_stanza(doc: "Document", *, attributes: Sequence[str] = STANZA_ATTRIBUTES) -> Iterator[Sentence]:
    """Project Stanza words, preserving the enclosing surface range of each multiword token."""
    stanza = adapter("lingrep_stanza_adapter", "lingrep-stanza-adapter")
    projection = stanza.document_projection(attributes)
    return _sentences(doc.text or "", projection, stanza.iter_document_sentences(doc, projection))
