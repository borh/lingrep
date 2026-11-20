"""Generic spaCy morphology adapter for lingrep."""

from .adapter import (
    ProjectionAttribute,
    analyze_document,
    document_projection,
    iter_document_sentences,
    morph_attribute_key,
)

__all__ = [
    "ProjectionAttribute",
    "analyze_document",
    "document_projection",
    "iter_document_sentences",
    "morph_attribute_key",
]
