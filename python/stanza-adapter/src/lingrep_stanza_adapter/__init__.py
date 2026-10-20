"""Stanza morphology adapter for lingrep."""

from .adapter import ProjectionAttribute, analyze_text, iter_analyzed_sentences
from .bundle import load_profile, prepare_model_bundle

__all__ = [
    "ProjectionAttribute",
    "analyze_text",
    "iter_analyzed_sentences",
    "load_profile",
    "prepare_model_bundle",
]
