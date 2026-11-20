from collections.abc import Iterable, Iterator, Sequence
from os import PathLike
from pathlib import Path

from ._native import CompiledQuery, Schema, Sentence, Token
from ._native import Engine as _Engine
from .documents import from_spacy, from_stanza
from .ling import Ling, SearchError, SearchResults
from .query import Query, as_query
from .tabular import token_batches
from .records import RawMatch

__all__ = ["CompiledQuery", "Ling", "Query", "Schema", "SearchError", "SearchResults", "Sentence",
           "Token", "Tokenizer", "from_spacy", "from_stanza", "token_batches"]


class Tokenizer:
    """Analyze independent Japanese sentences using one mapped UniDic dictionary."""

    def __init__(self, dict_path: str | PathLike[str] | None = None, *, workers: int = 1) -> None:
        self._engine = _Engine(dict_path, workers)

    def __repr__(self) -> str:
        return f"Tokenizer({str(self.dictionary_path)!r})"

    @property
    def dictionary_path(self) -> Path:
        return Path(self._engine.dictionary_path)

    @property
    def dictionary_digest(self) -> str:
        return self._engine.dictionary_digest

    @property
    def analysis_identity(self) -> str:
        return self._engine.analysis_identity

    @property
    def schema(self) -> Schema:
        return self._engine.schema

    def tokenize(self, text: str) -> list[str]:
        return next(self.pipe([text]))

    def pipe(self, sentences: Iterable[str]) -> Iterator[list[str]]:
        """Yield one list of surfaces per sentence in input order, including empty lists."""
        _reject_single_string(sentences)
        return (tokens for batch in _batches(sentences) for tokens in self._engine.surfaces(batch))

    def analyze(self, text: str, *, attributes: Sequence[str] | None = None) -> Sentence:
        """Keep one sentence's analysis; `attributes` narrows it from every schema attribute."""
        return next(self.analyze_pipe([text], attributes=attributes))

    def analyze_pipe(self, sentences: Iterable[str], *,
                     attributes: Sequence[str] | None = None) -> Iterator[Sentence]:
        _reject_single_string(sentences)
        keys = None if attributes is None else attribute_names(attributes)
        return (sentence for batch in _batches(sentences)
                for sentence in self._engine.sentences(batch, keys))

    def match_pipe(self, sentences: Iterable[str],
                   query: str | Query | CompiledQuery) -> Iterator[list[RawMatch]]:
        """Yield one list of raw matches per sentence, analyzing only the attributes the query reads."""
        _reject_single_string(sentences)
        compiled = query if isinstance(query, CompiledQuery) else as_query(query).compile()
        return (hits for batch in _batches(sentences) for hits in compiled._batch(self._engine, batch))


def attribute_names(attributes: Sequence[str]) -> tuple[str, ...]:
    if isinstance(attributes, str):
        raise TypeError("attributes must be a sequence of attribute names")
    keys = tuple(attributes)
    if len(set(keys)) != len(keys):
        raise ValueError("attributes must be unique")
    return keys


def _reject_single_string(sentences: Iterable[str]) -> None:
    if isinstance(sentences, (str, bytes)):
        raise TypeError("expected an iterable of sentences, not one string")


def _batches(sentences: Iterable[str]) -> Iterator[list[str]]:
    batch: list[str] = []
    size = 0
    for sentence in sentences:
        if not isinstance(sentence, str):
            raise TypeError("expected a sentence string")
        # Four bytes per code point bounds UTF-8 storage without encoding twice.
        weight = 4 * len(sentence)
        if batch and size + weight > 1024 * 1024:
            yield batch
            batch = []
            size = 0
        batch.append(sentence)
        size += weight
        if len(batch) == 256:
            yield batch
            batch = []
            size = 0
    if batch:
        yield batch
