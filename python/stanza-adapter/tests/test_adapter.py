from __future__ import annotations

import json
import types
import unittest
from pathlib import Path

from lingrep_stanza_adapter.adapter import (
    ProjectionAttribute,
    analyze_text,
    iter_analyzed_sentences,
)


def word(
    text: str,
    *,
    lemma: str | None = None,
    upos: str | None = "NOUN",
    xpos: str | None = "NOUN",
    feats: str | None = None,
) -> object:
    return types.SimpleNamespace(
        text=text,
        lemma=lemma if lemma is not None else text,
        upos=upos,
        xpos=xpos,
        feats=feats,
    )


def token(text: str, start: int, end: int, words: list[object] | None = None) -> object:
    return types.SimpleNamespace(
        text=text,
        start_char=start,
        end_char=end,
        words=words if words is not None else [word(text)],
    )


def document(text: str, sentences: list[list[object]]) -> object:
    return types.SimpleNamespace(
        text=text,
        sentences=[types.SimpleNamespace(tokens=tokens) for tokens in sentences],
    )


def projection() -> list[ProjectionAttribute]:
    return [
        ProjectionAttribute("text", "scalar"),
        ProjectionAttribute("lemma", "scalar"),
        ProjectionAttribute("upos", "scalar"),
        ProjectionAttribute("xpos", "scalar"),
        ProjectionAttribute("feat.Definite", "string_set"),
        ProjectionAttribute("feat.Gender", "string_set"),
        ProjectionAttribute("feat.Number", "string_set"),
    ]


def mwt_document() -> object:
    value = json.loads(
        (Path(__file__).parent / "fixtures" / "mwt.json").read_text(encoding="utf-8")
    )
    sentences = []
    for sentence in value["sentences"]:
        tokens = []
        for source in sentence["tokens"]:
            words = [word(**item) for item in source["words"]]
            tokens.append(
                token(source["text"], source["start_char"], source["end_char"], words)
            )
        sentences.append(tokens)
    return document(value["text"], sentences)


class AdapterMappingTests(unittest.TestCase):
    def test_identity_tokens_preserve_astral_utf8_source_ranges(self) -> None:
        text = "🐈 chat"
        doc = document(text, [[token("🐈", 0, 1), token("chat", 2, 6)]])
        result = list(
            iter_analyzed_sentences(
                doc,
                text,
                [ProjectionAttribute("text", "scalar")],
                "morphology",
                "input",
            )
        )
        self.assertEqual(result[0]["source_range"], [0, 9])
        self.assertEqual(result[0]["surface_ranges"], [[0, 4], [5, 9]])
        self.assertEqual(result[0]["alignment"], {"kind": "identity"})

    def test_mwt_preserves_one_surface_range_and_two_ordered_words(self) -> None:
        doc = mwt_document()
        result = list(
            iter_analyzed_sentences(doc, doc.text, projection(), "morphology", "input")
        )[0]
        self.assertEqual(result["surface_ranges"], [[0, 2], [3, 7]])
        self.assertEqual(
            result["alignment"],
            {"kind": "expanded", "word_ranges": [[0, 2], [2, 3]]},
        )
        self.assertEqual(result["word_count"], 3)
        self.assertEqual(result["values"][0:4], ["de", "de", "ADP", "ADP"])
        self.assertEqual(result["values"][7:11], ["le", "le", "DET", "DET"])

    def test_tokenize_profile_keeps_one_match_word_per_surface_token(self) -> None:
        doc = mwt_document()
        result = list(
            iter_analyzed_sentences(
                doc,
                doc.text,
                [ProjectionAttribute("text", "scalar")],
                "tokenize",
                "input",
            )
        )[0]
        self.assertEqual(result["values"], ["du", "pain"])
        self.assertEqual(result["word_count"], 2)
        self.assertEqual(result["alignment"], {"kind": "identity"})

    def test_mwt_preserves_source_gaps_without_fabricating_word_ranges(self) -> None:
        doc = mwt_document()
        result = list(
            iter_analyzed_sentences(doc, doc.text, projection(), "morphology", "input")
        )[0]
        self.assertEqual(doc.text[result["surface_ranges"][0][1] : 3], " ")
        self.assertNotIn("word_source_ranges", result)
        self.assertEqual(result["source_range"], [0, 7])

    def test_features_are_canonical_sorted_unique_sets(self) -> None:
        doc = document(
            "chats",
            [[token("chats", 0, 5, [word("chats", feats="Number=Sing,Plur,Sing")])]],
        )
        result = list(
            iter_analyzed_sentences(doc, doc.text, projection(), "morphology", "input")
        )[0]
        self.assertEqual(result["values"][6], ["Plur", "Sing"])

    def test_input_mode_rejects_cr_or_lf_before_pipeline_call(self) -> None:
        calls: list[str] = []

        def pipeline(text: str) -> object:
            calls.append(text)
            return document(text, [])

        for text in ["alpha\nbeta", "alpha\rbeta", "alpha\r\nbeta"]:
            with self.assertRaisesRegex(ValueError, "CR or LF"):
                analyze_text(
                    pipeline,
                    text,
                    [ProjectionAttribute("text", "scalar")],
                    "morphology",
                    "input",
                )
        self.assertEqual(calls, [])

    def test_analyzer_mode_preserves_returned_sentence_order_and_ranges(self) -> None:
        text = "Un. Deux."
        doc = document(
            text,
            [
                [token("Un", 0, 2), token(".", 2, 3)],
                [token("Deux", 4, 8), token(".", 8, 9)],
            ],
        )
        results = list(
            iter_analyzed_sentences(
                doc,
                text,
                [ProjectionAttribute("text", "scalar")],
                "morphology",
                "analyzer",
            )
        )
        self.assertEqual([item["source_range"] for item in results], [[0, 3], [4, 9]])
        self.assertEqual(
            [item["values"] for item in results], [["Un", "."], ["Deux", "."]]
        )

    def test_invalid_offsets_and_unknown_features_fail_terminally(self) -> None:
        bad_offset = document("chat", [[token("chat", 1, 5)]])
        with self.assertRaisesRegex(ValueError, "range"):
            list(
                iter_analyzed_sentences(
                    bad_offset,
                    bad_offset.text,
                    [ProjectionAttribute("text", "scalar")],
                    "morphology",
                    "input",
                )
            )
        with self.assertRaisesRegex(ValueError, "projection"):
            list(
                iter_analyzed_sentences(
                    mwt_document(),
                    "du pain",
                    [
                        ProjectionAttribute("text", "scalar"),
                        ProjectionAttribute("feat.Unknown", "string_set"),
                    ],
                    "morphology",
                    "input",
                )
            )


if __name__ == "__main__":
    unittest.main()
