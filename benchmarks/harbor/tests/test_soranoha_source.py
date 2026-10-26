import csv
import importlib.util
import io
import json
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("soranoha_source", ROOT / "soranoha_source.py")
SORANOHA = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(SORANOHA)

HEADER = (
    "identifier,title,subtitle,title_reading,author,orthographic_style,ndc,first_published,"
    "body_character_count,source_edition_title,source_edition_publisher,source_edition_year,"
    "filename,source_content_hash,url,release,doi"
)
RELEASE = "c" * 64


def write_archive(path: Path, works: dict[str, str]) -> Path:
    buffer = io.StringIO()
    writer = csv.writer(buffer, quoting=csv.QUOTE_ALL, lineterminator="\r\n")
    writer.writerow(HEADER.split(","))
    for index, (identifier, text) in enumerate(works.items()):
        writer.writerow(
            [
                identifier,
                f"題{index}",
                "",
                "",
                "夏目 漱石",
                "新字新仮名",
                "NDC 913" if index == 0 else "",
                "1906年",
                str(len(text)),
                "",
                "",
                "",
                f"work-{identifier}.txt",
                "h" * 64,
                f"https://soranoha.org/works/{identifier}/",
                RELEASE,
                "",
            ]
        )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("catalog.csv", "﻿" + buffer.getvalue())
        for identifier, text in works.items():
            archive.writestr(f"work-{identifier}.txt", text)
    return path


# Lines of 夢十夜 (000799_000148) and the sentences `ling tokenize --input-format
# soranoha` emitted for them on 2026-10-24 (lingrep 61b7c9c5).
LING_FRAMING = [
    ("　こんな夢を見た。", ["　こんな夢を見た。"]),
    (
        "　自分は黙って首肯いた。女は静かな調子を一段張り上げて、",
        ["　自分は黙って首肯いた。", "女は静かな調子を一段張り上げて、"],
    ),
    ("「もう死にます」と云った。", ["「もう死にます」と云った。"]),
    ("猫がいる。犬もいる！『本当？』", ["猫がいる。", "犬もいる！", "『本当？』"]),
    ("", []),
    ("　", []),
]


@pytest.mark.parametrize(("line", "expected"), LING_FRAMING)
def test_frame_matches_the_lingrep_soranoha_reader(line, expected):
    assert SORANOHA.frame(line) == expected


def test_frame_normalizes_to_nfc_before_splitting():
    assert SORANOHA.frame("が。") == ["が。"]


def test_extract_selects_sentence_ranges_with_catalog_metadata(tmp_path):
    archive = write_archive(
        tmp_path / "a.zip",
        {"000001_000148": "一。二。\n三！", "000002_000148": "四。"},
    )
    documents = SORANOHA.extract(
        archive,
        [
            {"identifier": "000001_000148", "first_sentence": 1, "sentence_count": 2},
            {"identifier": "000002_000148", "first_sentence": 0, "sentence_count": 1},
        ],
    )
    assert documents == [
        {
            "eval_document_id": "000001_000148",
            "metadata": {
                "identifier": "000001_000148",
                "title": "題0",
                "author": "夏目 漱石",
                "ndc": "NDC 913",
                "orthographic_style": "新字新仮名",
                "first_published": "1906年",
                "release": RELEASE,
            },
            "sentences": ["二。", "三！"],
        },
        {
            "eval_document_id": "000002_000148",
            "metadata": {
                "identifier": "000002_000148",
                "title": "題1",
                "author": "夏目 漱石",
                "orthographic_style": "新字新仮名",
                "first_published": "1906年",
                "release": RELEASE,
            },
            "sentences": ["四。"],
        },
    ]
    with pytest.raises(ValueError, match="out of range"):
        SORANOHA.extract(
            archive, [{"identifier": "000002_000148", "first_sentence": 0, "sentence_count": 2}]
        )
    with pytest.raises(ValueError, match="not in catalog.csv"):
        SORANOHA.extract(
            archive, [{"identifier": "missing", "first_sentence": 0, "sentence_count": 1}]
        )


def test_source_block_pins_the_archive_and_verify_rederives_catalog_documents(tmp_path):
    archive = write_archive(tmp_path / "a.zip", {"000001_000148": "一。二。"})
    works = [{"identifier": "000001_000148", "first_sentence": 0, "sentence_count": 2}]
    source = SORANOHA.source_block(archive, "https://example.invalid/a.zip", works)
    assert source == {
        "kind": "soranoha",
        "url": "https://example.invalid/a.zip",
        "sha256": SORANOHA.sha256_file(archive),
        "release": RELEASE,
        "works": works,
    }
    case = {
        "id": "pinned",
        "source": source,
        "documents": SORANOHA.extract(archive, works),
    }
    other = {"id": "hand-written", "documents": []}
    catalog = tmp_path / "catalog.json"
    catalog.write_text(json.dumps([case, other], ensure_ascii=False))
    assert SORANOHA.verify_catalog(catalog, archive) == ["pinned"]

    case["documents"][0]["sentences"][0] = "壱。"
    catalog.write_text(json.dumps([case, other], ensure_ascii=False))
    with pytest.raises(ValueError, match="do not derive"):
        SORANOHA.verify_catalog(catalog, archive)
