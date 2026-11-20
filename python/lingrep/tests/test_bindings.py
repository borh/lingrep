import json
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from lingrep import Ling, Query, Tokenizer


def span(start, end):
    return {"start": start, "end": end}


class BindingsTest(unittest.TestCase):
    def test_composed_letter_and_remaining_mark_keep_their_source_spans(self):
        sentence = Tokenizer().analyze("U\u0304\u0304")
        self.assertEqual(sentence.column("surface"), ["U\u0304", "\u0304"])
        self.assertEqual(sentence.char_spans, [span(0, 2), span(2, 3)])

    def test_surfaces_arrive_in_input_order(self):
        tok = Tokenizer(workers=3)
        texts = ["猫が眠る。", "", "犬。"] * 100
        self.assertEqual(list(tok.pipe(iter(texts))), [tok.tokenize(s) for s in texts])
        self.assertEqual(tok.tokenize("猫が眠る。"), ["猫", "が", "眠る", "。"])
        self.assertEqual(tok.tokenize(""), [])

    def test_sentence_tokens_read_the_attributes_the_sentence_carries(self):
        tok = Tokenizer()
        sentence = tok.analyze("🍣 猫 が眠る。")
        cat = sentence[1]
        self.assertEqual((cat.surface, cat.upos, cat["unidic.cform"], cat.index), ("猫", "NOUN", "*", 1))
        self.assertEqual(cat.char_span, span(2, 3))
        self.assertEqual([t.surface for t in sentence.__iter__()], sentence.column("surface"))
        self.assertEqual(sentence[-1].surface, "。")
        self.assertEqual(sentence.schema, tok.schema)
        self.assertEqual(sentence.attributes, tok.schema.attributes)
        with self.assertRaisesRegex(KeyError, "no attribute lemma"):
            tok.analyze("", attributes=[]).column("lemma")
        with self.assertRaises(IndexError):
            sentence[len(sentence)]
        with self.assertRaises(AttributeError):
            cat.bogus
        narrow = tok.analyze("猫が", attributes=["lemma"])
        self.assertEqual(narrow.attributes, ["lemma"])
        self.assertEqual([(t.lemma, t.surface) for t in narrow], [("猫", "猫"), ("が", "が")])
        with self.assertRaisesRegex(KeyError, "no attribute upos"):
            narrow[0]["upos"]
        with self.assertRaises(ValueError):
            tok.analyze("猫", attributes=["bogus"])

    def test_objects_describe_themselves(self):
        tok = Tokenizer()
        sentence = tok.analyze("猫が")
        self.assertEqual(repr(sentence), 'Sentence("猫が", 2 tokens)')
        self.assertEqual(repr(sentence[0]), 'Token("猫")')
        self.assertEqual(repr(Query("[upos=NOUN].").compile()), "CompiledQuery(reads upos)")
        self.assertEqual(repr(Query("[upos=NOUN].")), "Query('[upos=NOUN].')")
        self.assertTrue(repr(tok.schema).startswith("Schema(text, "))
        self.assertIn(tok.dictionary_path.name, repr(tok))

    def test_search(self):
        with Ling().search("[upos=NOUN].", text="猫が眠る。") as run:
            self.assertEqual([hit["surface"] for hit in run], ["猫"])
            self.assertIsNotNone(run.completion)

    def test_complete_match_records_and_source_metadata(self):
        root = Path(__file__).resolve().parents[3]
        path = root / "lingrep/tests/fixtures/psycholinguistics.conllu"
        pattern = "[upos=VERB]."
        selector = "@sentence.sent_id"
        reference = subprocess.check_output([
            "ling", "--json", "matches", "--pattern", pattern,
            "--source-metadata", selector, path,
        ])
        records = [json.loads(line) for line in reference.splitlines()]
        expected = [row["data"] for row in records if row["type"] == "match"]
        self.assertTrue(expected)
        for selectors in ([selector], selector):
            with Ling().search(Query(pattern), path=path, source_metadata=selectors) as run:
                self.assertEqual(list(run), expected)
                self.assertEqual(run.completion["coverage"], "full")

    def test_failed_search(self):
        with self.assertRaisesRegex(subprocess.CalledProcessError, r"Syntax: missing `\]` \(inline.lx:1:"):
            with Ling().search("[", text="猫") as run:
                list(run)

    def test_search_collects_the_warnings_ling_reports(self):
        with self.assertWarnsRegex(UserWarning, "attribute_unavailable"):
            with Ling().search("[dep.rel=nsubj].", text="猫") as run:
                self.assertEqual(list(run), [])
        self.assertEqual([d["code"] for d in run.diagnostics], ["attribute_unavailable"])

    def test_invalid_arguments_are_rejected_before_iteration(self):
        tok = Tokenizer()
        for call in (lambda: tok.pipe("猫"), lambda: tok.analyze_pipe("猫"),
                     lambda: tok.match_pipe("猫", "[upos=NOUN]."),
                     lambda: tok.analyze_pipe(["猫"], attributes="lemma")):
            with self.assertRaises(TypeError):
                call()
        with self.assertRaises(TypeError):
            Query(Path("catalog.lx"))

    def test_vibrato_handles_a_long_sentence_within_the_record_limit(self):
        text = "猫" * 327679
        self.assertEqual(Tokenizer().tokenize(text), list(text))

    def test_a_sentence_over_the_record_limit_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "1048576-byte limit"):
            Tokenizer().tokenize("猫" * 349526)

    def test_two_compiled_queries_share_source_aligned_analysis(self):
        sentence = Tokenizer().analyze("🍣 猫 が眠る。")
        noun = Query("subject := (?<who> [surface=猫]) [surface=が].").compile()
        verb = Query("[lemma=眠る].").compile()
        [hit] = noun.find_matches(sentence)
        self.assertEqual(hit, {
            "pattern": "subject", "span": span(1, 3), "char_span": span(2, 5),
            "captures": [{"name": "who", "hygienic_path": "subject/who", "span": span(1, 2),
                          "char_span": span(2, 3)}],
            "graph_bindings": [], "interpretations": [], "lexical": None,
        })
        who = hit["captures"][0]["char_span"]
        self.assertEqual(sentence.text[who["start"]:who["end"]], "猫")
        slept = verb.find_matches(sentence)[0]["char_span"]
        self.assertEqual(sentence.text[slept["start"]:slept["end"]], "眠る")
        self.assertEqual(noun.find_matches(sentence), [hit])

    def test_compiled_file_and_batched_sentences_outlive_their_sources(self):
        with TemporaryDirectory() as directory:
            path = Path(directory) / "animals.lx"
            path.write_text('lexicon animals by [text, norm] := {"猫"}.', encoding="utf-8")
            query = Query.from_file(path).compile()
        sentences = list(Tokenizer(workers=2).analyze_pipe(iter(["犬", "", "猫"])))
        self.assertEqual([s.text for s in sentences], ["犬", "", "猫"])
        self.assertEqual([query.find_matches(s) for s in sentences], [
            [], [], [{"pattern": "animals", "span": span(0, 1), "char_span": span(0, 1),
                      "captures": [], "interpretations": [], "graph_bindings": [],
                      "lexical": {"term": "猫", "matched_by": ["text", "norm"]}}],
        ])

    def test_compiler_reports_syntax_and_unavailable_attributes(self):
        with self.assertRaisesRegex(ValueError, "inline.lx"):
            Query("[").compile()
        with self.assertWarnsRegex(UserWarning, "attribute_unavailable.*dep.rel") as warned:
            Query("[dep.rel=nsubj].").compile()
        self.assertEqual(warned.filename, __file__)

    def test_a_query_matches_any_sentence_that_carries_the_attributes_it_reads(self):
        tok = Tokenizer()
        text = "猫が眠る。"
        nouns = Query("[upos=NOUN].").compile()
        self.assertEqual(nouns.attributes, ["upos"])
        expected = nouns.find_matches(tok.analyze(text))
        self.assertEqual([hit["span"] for hit in expected], [span(0, 1)])
        for attributes in (["upos"], ["lemma", "upos"], ["upos", "unidic.cform"]):
            self.assertEqual(nouns.find_matches(tok.analyze(text, attributes=attributes)), expected)
        with self.assertRaisesRegex(ValueError, "lacks attributes the query reads: upos"):
            nouns.find_matches(tok.analyze(text, attributes=["lemma"]))

    def test_columns_and_batch_search(self):
        tok = Tokenizer(workers=2)
        texts = ["🍣 猫 が眠る。", "", "猫。"] * 100
        query = Query("noun := (?<cat> [lemma=猫]).")
        compiled = query.compile()
        retained = tok.analyze(texts[0])
        self.assertEqual(retained.column("surface")[1], "猫")
        self.assertEqual(retained.char_spans[1], span(2, 3))
        expected = [compiled.find_matches(s) for s in tok.analyze_pipe(texts)]
        self.assertEqual(list(tok.match_pipe(iter(texts), query)), expected)
        self.assertEqual(list(tok.match_pipe(texts, compiled)), expected)
        self.assertEqual(list(tok.match_pipe(texts, "noun := (?<cat> [lemma=猫]).")), expected)
        self.assertEqual(len(tok.dictionary_digest), 64)
        self.assertTrue(tok.dictionary_path.is_file())
