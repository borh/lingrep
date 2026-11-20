import warnings
from os import PathLike
from pathlib import Path

from ._native import CompiledQuery, Schema


class Query:
    """An inline pattern or a catalog file."""

    def __init__(self, pattern: str) -> None:
        if not isinstance(pattern, str):
            raise TypeError("pattern must be a string; use Query.from_file for a catalog path")
        self._source: str | Path = pattern

    @classmethod
    def from_file(cls, path: str | PathLike[str]) -> "Query":
        query = cls("")
        query._source = Path(path)
        return query

    def __repr__(self) -> str:
        if isinstance(self._source, Path):
            return f"Query.from_file({str(self._source)!r})"
        return f"Query({self._source!r})"

    def compile(self, schema: Schema | None = None) -> CompiledQuery:
        """Compile for sentences of `schema`, or of `Tokenizer` when omitted."""
        source = self._source
        if isinstance(source, Path):
            compiled = CompiledQuery._compile(source.read_text(encoding="utf-8"), str(source), False, schema)
        else:
            compiled = CompiledQuery._compile(source, "inline.lx", True, schema)
        for diagnostic in compiled.diagnostics:
            warnings.warn(diagnostic, stacklevel=2)
        return compiled


def as_query(query: "str | Query") -> Query:
    return Query(query) if isinstance(query, str) else query
