from collections.abc import Callable, Iterable, Iterator, Sequence
import json

import polars as pl

from . import CompiledQuery, Query, Tokenizer, attribute_names
from .query import as_query

_span = pl.Struct({"start": pl.UInt32, "end": pl.UInt32})
_capture = pl.Struct({"name": pl.String, "hygienic_path": pl.String,
                      "span": _span, "char_span": _span})
_hits = pl.List(pl.Struct({
    "pattern": pl.String, "span": _span, "char_span": _span,
    "captures": pl.List(_capture),
    "lexical": pl.Struct({"term": pl.String, "matched_by": pl.List(pl.String)}),
    "interpretations_json": pl.String, "graph_bindings_json": pl.String,
}))


def _map_sentences(text: pl.Expr, dtype: pl.DataType,
                   process: Callable[[Iterable[str]], Iterator[object]]) -> pl.Expr:
    def batch(series: pl.Series) -> pl.Series:
        if series.dtype not in (pl.String, pl.Null):
            raise TypeError(f"expected a string column, got {series.dtype}")
        rows = series.to_list()
        results = process(row for row in rows if row is not None)
        return pl.Series([None if row is None else next(results) for row in rows], dtype=dtype)

    return text.map_batches(batch, return_dtype=dtype, is_elementwise=True)


def tokenize(text: pl.Expr, *, tokenizer: Tokenizer,
             attributes: Sequence[str] | None = None) -> pl.Expr:
    """Return surfaces, or structs with the requested fields and sentence-relative offsets."""
    if attributes is None:
        return _map_sentences(text, pl.List(pl.String), tokenizer.pipe)
    keys = attribute_names(attributes)
    if not keys:
        raise ValueError("attributes must be nonempty")
    offsets = {"char_start": "start", "char_end": "end"}
    morphology = [k for k in keys if k not in offsets]
    unknown = sorted(set(morphology) - set(tokenizer.schema.attributes))
    if unknown:
        raise ValueError(f"tokenizer schema has no attribute {', '.join(unknown)}")
    dtype = pl.List(pl.Struct({k: pl.UInt32 if k in offsets else pl.String for k in keys}))

    def records(sentences: Iterable[str]) -> Iterator[object]:
        for sentence in tokenizer.analyze_pipe(sentences, attributes=morphology):
            columns: dict[str, Sequence[object]] = {k: sentence.column(k) for k in morphology}
            spans = sentence.char_spans
            for key, bound in offsets.items():
                if key in keys:
                    columns[key] = [None if span is None else span[bound] for span in spans]  # type: ignore[literal-required]
            yield [dict(zip(keys, row, strict=True))
                   for row in zip(*(columns[k] for k in keys), strict=True)]

    return _map_sentences(text, dtype, records)


def matches(text: pl.Expr, query: str | Query | CompiledQuery, *, tokenizer: Tokenizer) -> pl.Expr:
    """Return sentence-local matches for a query compiled against the tokenizer's schema."""
    compiled = query if isinstance(query, CompiledQuery) else as_query(query).compile(tokenizer.schema)

    def search(sentences: Iterable[str]) -> Iterator[object]:
        for hits in tokenizer.match_pipe(sentences, compiled):
            yield [{
                "pattern": hit["pattern"], "span": hit["span"], "char_span": hit["char_span"],
                "captures": hit["captures"], "lexical": hit["lexical"],
                "interpretations_json": json.dumps(hit["interpretations"], ensure_ascii=False),
                "graph_bindings_json": json.dumps(hit["graph_bindings"], ensure_ascii=False),
            } for hit in hits]

    return _map_sentences(text, _hits, search)
