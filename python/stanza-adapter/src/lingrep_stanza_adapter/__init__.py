"""Stanza morphology adapter for lingrep."""

from .adapter import (
    ProjectionAttribute,
    analyze_text,
    document_projection,
    iter_analyzed_sentences,
    iter_document_sentences,
)
from .bundle import load_profile, prepare_model_bundle

__all__ = [
    "ProjectionAttribute",
    "analyze_text",
    "document_projection",
    "iter_analyzed_sentences",
    "iter_document_sentences",
    "load_profile",
    "prepare_model_bundle",
]
