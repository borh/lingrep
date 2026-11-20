import json
import subprocess
import warnings
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from os import PathLike
from pathlib import Path
from tempfile import TemporaryFile
from typing import IO, Any

from .query import Query, as_query
from .records import PublishedMatch


def _diagnostics(stderr: str) -> tuple[list[dict[str, Any]], list[str]]:
    """Split `ling --json` stderr into diagnostic records and one summary line for each."""
    records, summaries = [], []
    for line in stderr.splitlines():
        try:
            data = json.loads(line)["data"]
            summary = f"{data['code']}: {data['message']}"
        except (ValueError, KeyError, TypeError):
            summaries.append(line)
            continue
        where = data.get("location") or {}
        if where.get("source_name"):
            summary += f" ({where['source_name']}:{where.get('line')}:{where.get('column')})"
        records.append(data)
        summaries.append(summary)
    return records, summaries


class SearchError(subprocess.CalledProcessError):
    """`ling` exited with a failure; `stderr` holds what it reported."""

    def __str__(self) -> str:
        return "\n".join([super().__str__(), *_diagnostics(self.stderr)[1]])


def _source_args(query: str | Query) -> list[str]:
    source = as_query(query)._source
    return ["--patterns", str(source)] if isinstance(source, Path) else ["--pattern", source]


class Ling:
    """Run queries through the `ling` executable, which applies catalog publication policy."""

    def __init__(self, executable: str | PathLike[str] = "ling") -> None:
        self.executable = executable

    def __repr__(self) -> str:
        return f"Ling({str(self.executable)!r})"

    def check(self, query: str | Query) -> "subprocess.CompletedProcess[bytes]":
        return subprocess.run([self.executable, "check", *_source_args(query)], check=True)

    @contextmanager
    def search(self, query: str | Query, *, text: str | None = None,
               path: str | PathLike[str] | None = None,
               corpus: str | PathLike[str] | None = None, source_metadata: Sequence[str] = (),
               analyzer_config: str | PathLike[str] | None = None) -> Iterator["SearchResults"]:
        """Stream published match mappings. Inspect completion after exhausting the iterator."""
        if sum(x is not None for x in (text, path, corpus)) != 1:
            raise ValueError("supply exactly one of text, path or corpus")
        args: list[str | PathLike[str]] = [self.executable, "--json", "matches", *_source_args(query)]
        if isinstance(source_metadata, str):
            source_metadata = (source_metadata,)
        for selector in source_metadata:
            args.extend(["--source-metadata", selector])
        if analyzer_config is not None:
            args.extend(["--analyzer-config", analyzer_config])
        with TemporaryFile() as stdin, TemporaryFile() as stderr:
            if text is not None:
                stdin.write(text.encode("utf-8"))
                stdin.seek(0)
                args.extend(["--input-format", "plain", "-"])
            elif corpus is not None:
                args.extend(["--corpus", corpus])
            elif path is not None:
                args.extend(["--", path])
            with subprocess.Popen(args, stdin=stdin, stdout=subprocess.PIPE, stderr=stderr) as proc:
                try:
                    yield SearchResults(proc, stderr)
                finally:
                    if proc.poll() is None:
                        proc.kill()


class SearchResults:
    """Iterate once for matches; `completion` and `diagnostics` are set when the stream ends."""

    def __init__(self, proc: "subprocess.Popen[bytes]", stderr: IO[bytes]) -> None:
        self.run: dict[str, Any] | None = None
        self.definitions: list[dict[str, Any]] = []
        self.completion: dict[str, Any] | None = None
        self.diagnostics: list[dict[str, Any]] = []
        self._matches = self._read(proc, stderr)

    def __iter__(self) -> Iterator[PublishedMatch]:
        return self._matches

    def _read(self, proc: "subprocess.Popen[bytes]", stderr: IO[bytes]) -> Iterator[PublishedMatch]:
        completion = None
        assert proc.stdout is not None
        for line in proc.stdout:
            record = json.loads(line)
            if record["schema"] != "lingrep.query/v2":
                raise ValueError("unsupported ling query schema")
            kind, data = record["type"], record["data"]
            if completion is not None:
                raise ValueError("unexpected record after completion")
            if kind == "run" and self.run is None:
                self.run = data
            elif self.run is None:
                raise ValueError("missing ling run header")
            elif kind == "definition":
                self.definitions.append(data)
            elif kind == "match":
                yield data
            elif kind == "complete":
                completion = data
        status = proc.wait()
        stderr.seek(0)
        reported = stderr.read().decode("utf-8", "replace")
        if status:
            raise SearchError(status, proc.args, stderr=reported)
        self.diagnostics, summaries = _diagnostics(reported)
        for summary in summaries:
            warnings.warn(summary)
        if completion is None:
            raise ValueError("incomplete ling query stream")
        self.completion = completion
