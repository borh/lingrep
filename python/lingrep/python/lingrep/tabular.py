from collections.abc import Iterable, Iterator, Sequence
from typing import TYPE_CHECKING

from ._native import Sentence

if TYPE_CHECKING:
    from pyarrow import RecordBatch


def token_batches(sentences: Iterable[Sentence], *, attributes: Sequence[str] = ("surface", "lemma")) -> Iterator["RecordBatch"]:
    """Yield Arrow batches with sentence-relative token indices and Unicode offsets."""
    import pyarrow as pa

    if isinstance(attributes, str):
        raise TypeError("attributes must be a sequence of attribute names")
    keys = tuple(attributes)
    positions = ("sentence_index", "token_index", "char_start", "char_end")
    names = positions + keys
    if len(names) != len(set(names)):
        raise ValueError("attribute names must be unique and distinct from position columns")
    columns: list[list[object]] = [[] for _ in names]
    schema = None
    emitted = False
    for index, sentence in enumerate(sentences):
        types = [pa.uint64()] + [pa.uint32()] * 3 + [
            pa.list_(pa.string()) if sentence._is_set(k) else pa.string() for k in keys]
        current = pa.schema(zip(names, types))
        if schema is not None and current != schema:
            raise ValueError("sentence column kinds differ within the stream")
        schema = current
        spans = sentence.char_spans
        columns[0].extend([index] * len(sentence))
        columns[1].extend(range(len(sentence)))
        columns[2].extend(span["start"] if span else None for span in spans)
        columns[3].extend(span["end"] if span else None for span in spans)
        for key, column in zip(keys, columns[len(positions):]):
            column.extend(sentence.column(key))
        if len(columns[0]) >= 8192:
            yield pa.RecordBatch.from_pydict(dict(zip(names, columns)), schema=schema)
            emitted = True
            columns = [[] for _ in names]
    if schema is not None and (columns[0] or not emitted):
        yield pa.RecordBatch.from_pydict(dict(zip(names, columns)), schema=schema)
