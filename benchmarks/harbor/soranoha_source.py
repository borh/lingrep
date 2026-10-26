#!/usr/bin/env python3
"""Derive case documents from a Soranoha plaintext archive and check that a
catalog's stored documents still derive from the archive it names."""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import sys
import unicodedata
import zipfile
from pathlib import Path

SOURCE_KIND = "soranoha"
METADATA_COLUMNS = (
    "identifier",
    "title",
    "author",
    "ndc",
    "orthographic_style",
    "first_published",
    "release",
)
SENTENCE_DELIMITERS = frozenset("!?。！？")
CLOSING_QUOTATIONS = frozenset(")）」』】］〕〉》]")


def split_sentence_ja(line: str) -> list[str]:
    """lingrep-core `utils::split_sentence_ja`: a delimiter ends a sentence and
    takes one following closing quotation with it."""
    sentences = []
    start = 0
    end = 0
    while end < len(line):
        if line[end] in SENTENCE_DELIMITERS:
            if end + 1 < len(line) and line[end + 1] in CLOSING_QUOTATIONS:
                end += 1
            sentences.append(line[start : end + 1])
            start = end + 1
        end += 1
    if start < len(line):
        sentences.append(line[start:])
    return sentences


def frame(text: str) -> list[str]:
    """Sentences as `ling --input-format soranoha` admits them: NFC, one physical
    line at a time, blank segments dropped."""
    normalized = unicodedata.normalize("NFC", text)
    return [
        segment
        for line in normalized.splitlines()
        for segment in split_sentence_ja(line)
        if segment.strip()
    ]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def catalog_rows(archive: zipfile.ZipFile) -> dict[str, dict[str, str]]:
    rows = csv.DictReader(io.StringIO(archive.read("catalog.csv").decode("utf-8-sig")))
    return {row["identifier"]: row for row in rows}


def work_document(archive: zipfile.ZipFile, row: dict[str, str], work: dict) -> dict:
    sentences = frame(archive.read(row["filename"]).decode("utf-8"))
    first = work["first_sentence"]
    last = first + work["sentence_count"]
    if last > len(sentences):
        raise ValueError(
            f"work {row['identifier']} has {len(sentences)} sentences; "
            f"[{first}, {last}) is out of range"
        )
    metadata = {column: row[column] for column in METADATA_COLUMNS if row.get(column)}
    return {
        "eval_document_id": row["identifier"],
        "metadata": metadata,
        "sentences": sentences[first:last],
    }


def extract(archive_path: Path, works: list[dict]) -> list[dict]:
    with zipfile.ZipFile(archive_path) as archive:
        rows = catalog_rows(archive)
        documents = []
        for work in works:
            row = rows.get(work["identifier"])
            if row is None:
                raise ValueError(f"work {work['identifier']} is not in catalog.csv")
            documents.append(work_document(archive, row, work))
    return documents


def source_block(archive_path: Path, url: str, works: list[dict]) -> dict:
    with zipfile.ZipFile(archive_path) as archive:
        releases = {row["release"] for row in catalog_rows(archive).values()}
    if len(releases) != 1:
        raise ValueError(f"archive declares {len(releases)} releases")
    return {
        "kind": SOURCE_KIND,
        "url": url,
        "sha256": sha256_file(archive_path),
        "release": releases.pop(),
        "works": works,
    }


def verify_catalog(catalog_path: Path, archive_path: Path) -> list[str]:
    """Ids of the cases whose stored documents were re-derived from `archive_path`;
    raises on the first case that differs."""
    digest = sha256_file(archive_path)
    checked = []
    for case in json.loads(catalog_path.read_text()):
        source = case.get("source")
        if source is None or source["sha256"] != digest:
            continue
        derived = extract(archive_path, source["works"])
        if derived != case["documents"]:
            raise ValueError(f"documents of {case['id']} do not derive from {archive_path}")
        checked.append(case["id"])
    return checked


def parse_work(value: str) -> dict:
    identifier, first, count = value.split(":")
    return {
        "identifier": identifier,
        "first_sentence": int(first),
        "sentence_count": int(count),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    extracting = commands.add_parser("extract")
    extracting.add_argument("archive", type=Path)
    extracting.add_argument("--url", required=True)
    extracting.add_argument(
        "--work", action="append", required=True, metavar="IDENTIFIER:FIRST:COUNT"
    )
    verifying = commands.add_parser("verify")
    verifying.add_argument("catalog", type=Path)
    verifying.add_argument("archive", type=Path)
    args = parser.parse_args()
    if args.command == "extract":
        works = [parse_work(value) for value in args.work]
        json.dump(
            {
                "source": source_block(args.archive, args.url, works),
                "documents": extract(args.archive, works),
            },
            sys.stdout,
            ensure_ascii=False,
            indent=2,
        )
        print()
    else:
        for case_id in verify_catalog(args.catalog, args.archive):
            print(f"derived: {case_id}")


if __name__ == "__main__":
    main()
