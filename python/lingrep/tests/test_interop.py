import importlib.util
import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from lingrep import Query, Schema, Sentence, Tokenizer, from_spacy, from_stanza, token_batches


def available(*packages):
    return all(importlib.util.find_spec(p) is not None for p in packages)


class InteropTest(unittest.TestCase):
    @unittest.skipUnless(available("polars"), "requires Polars")
    def test_lazy_expressions_preserve_native_results_and_row_alignment(self):
        import polars as pl
        from lingrep.polars_expr import matches, tokenize

        tok = Tokenizer(workers=2)
        with TemporaryDirectory() as directory:
            path = Path(directory) / "cats.lx"
            path.write_text(
                'interpret $cat as animal {species: "猫"}\n'
                'classify by animal\n'
                'noun := (?<cat> [lemma=猫]).\n'
                'lexicon cats by [text, norm] := {"猫"}.', encoding="utf-8",
            )
            source = Query.from_file(path)
            query = source.compile()
            from_source = matches(pl.col("text"), source, tokenizer=tok)
        texts = [None, "🍣 猫 と 猫", "", "犬", None]
        plan = pl.LazyFrame({"id": range(len(texts)), "text": texts}).with_columns(
            tokens=tokenize(pl.col("text"), tokenizer=tok),
            from_source=from_source,
            hits=matches(pl.col("text"), query, tokenizer=tok),
        )
        self.assertEqual(plan.collect_schema()["tokens"], pl.List(pl.String))
        self.assertEqual(plan.head(0).collect().schema, plan.collect_schema())
        rows = plan.collect(engine="streaming").to_dicts()
        self.assertEqual(len(rows), len(texts))
        for i, row in enumerate(rows):
            self.assertEqual(row["id"], i)
            self.assertEqual(row["from_source"], row["hits"])
            if texts[i] is None:
                self.assertIsNone(row["tokens"])
                self.assertIsNone(row["hits"])
                continue
            self.assertEqual(row["tokens"], tok.tokenize(texts[i]))
            expected = query.find_matches(tok.analyze(texts[i]))
            for hit in row["hits"]:
                for key in ("graph_bindings", "interpretations"):
                    hit[key] = json.loads(hit.pop(key + "_json"))
            self.assertEqual(row["hits"], json.loads(json.dumps(expected)))

    @unittest.skipUnless(available("polars"), "requires Polars")
    def test_lazy_token_attributes_preserve_source_offsets_and_requested_order(self):
        import polars as pl
        from lingrep.polars_expr import tokenize

        tok = Tokenizer()
        keys = ["lemma", "char_end", "surface", "upos", "char_start"]
        texts = [None, "", "🍣 猫 が眠った。"]
        tokens = tokenize(pl.col("text"), tokenizer=tok, attributes=keys)
        plan = pl.LazyFrame({"text": texts}).select(tokens.alias("tokens"))
        fields = {k: pl.UInt32 if k.startswith("char_") else pl.String for k in keys}
        self.assertEqual(plan.collect_schema()["tokens"], pl.List(pl.Struct(fields)))
        sentence = tok.analyze(texts[2])
        expected = [
            {"lemma": lemma, "char_end": end, "surface": surface, "upos": upos, "char_start": start}
            for (start, end), lemma, surface, upos in zip(
                [(s["start"], s["end"]) for s in sentence.char_spans], sentence.column("lemma"),
                sentence.column("surface"), sentence.column("upos"), strict=True)
        ]
        with self.assertRaisesRegex(ValueError, "no attribute bogus"):
            tokenize(pl.col("text"), tokenizer=tok, attributes=["bogus"])
        self.assertEqual(plan.collect()["tokens"].to_list(), [None, [], expected])

    def test_vibrato_rejects_a_query_compiled_for_another_sentence_schema(self):
        sentence = Sentence._from_projection("猫", Schema._from_fields([("text", False)]), ["猫"],
                                             [(0, 3)], [(0, 1)], 1)
        query = Query("[text=猫].").compile(sentence.schema)
        self.assertEqual([hit["span"] for hit in query.find_matches(sentence)], [{"start": 0, "end": 1}])
        tok = Tokenizer()
        with self.assertRaisesRegex(ValueError, "different token schema"):
            list(tok.match_pipe(["猫"], query))
        with self.assertRaisesRegex(ValueError, "different token schema"):
            query.find_matches(tok.analyze("猫"))

    @unittest.skipUnless(available("pyarrow", "polars"), "requires PyArrow and Polars")
    def test_polars_preserves_token_and_sentence_coordinates(self):
        import polars as pl
        sentences = Tokenizer().analyze_pipe(["", "猫 が眠る。"])
        [batch] = token_batches(sentences)
        frame = pl.from_arrow(batch)
        self.assertEqual(frame["surface"].to_list(), ["猫", "が", "眠る", "。"])
        self.assertEqual(frame["char_start"].to_list(), [0, 2, 3, 5])
        self.assertEqual(frame.schema["char_start"], pl.UInt32)
        self.assertEqual(frame["sentence_index"].to_list(), [1] * 4)

    @unittest.skipUnless(available("spacy", "lingrep_spacy_adapter"), "requires spaCy adapter")
    def test_spacy_document_keeps_sentence_local_dependency_heads(self):
        from spacy.tokens import Doc
        from spacy.vocab import Vocab

        doc = Doc(Vocab(), words=["猫", "眠る", "犬", "走る"], spaces=[False]*4,
                  heads=[1, 1, 3, 3], deps=["nsubj", "ROOT", "nsubj", "ROOT"])
        sentences = list(from_spacy(doc, attributes=["text", "dep.head", "dep.rel"]))
        self.assertEqual([s.text for s in sentences], ["猫眠る", "犬走る"])
        query = Query("s := (?<n> []) where (?<v> []) -[nsubj]-> $n.").compile(sentences[0].schema)
        for sentence in sentences:
            self.assertEqual(sentence.column("dep.head"), ["2", "0"])
            self.assertTrue(query.find_matches(sentence))

    @unittest.skipUnless(available("spacy", "lingrep_spacy_adapter"), "requires spaCy adapter")
    def test_spacy_graph_bindings_use_the_published_record_fields(self):
        from spacy.tokens import Doc
        from spacy.vocab import Vocab

        doc = Doc(Vocab(), words=["猫", "眠る"], spaces=[False]*2, heads=[1, 1], deps=["nsubj", "ROOT"])
        [sentence] = from_spacy(doc, attributes=["text", "dep.head", "dep.rel"])
        query = Query("s := (?<n> []) where (?<v> []) -[nsubj]-> $n "
                      "measure count($n -[*]-> []) as $k.").compile(sentence.schema)
        [hit] = query.find_matches(sentence)
        measure, node = hit["graph_bindings"]
        self.assertEqual(measure, {"binding_kind": "measure", "name": "k", "value": 0.0, "status": "ok"})
        self.assertEqual((node["binding_kind"], node["name"], node["node"]),
                         ("node", "v", {"node_kind": "word", "token_index": 1}))
        self.assertIn({"name": "text", "value": "眠る"}, node["attributes"])

    @unittest.skipUnless(available("spacy", "lingrep_spacy_adapter"), "requires spaCy adapter")
    def test_spacy_matcher_aligns_matches_and_captures_to_original_documents(self):
        from spacy.tokens import Doc
        from spacy import blank
        from lingrep.spacy_matcher import Matcher

        vocab = blank("xx").vocab
        with TemporaryDirectory() as directory:
            path = Path(directory) / "cats.lx"
            path.write_text("pair := (?<cat> [text=猫]) [text=犬].", encoding="utf-8")
            matcher = Matcher(Query.from_file(path), attributes=["text"])

        words = ["🍣", "。", "  ", "猫", "\n", "犬"]
        for boundaries in ([True, False, True, False, False, False], None):
            doc = Doc(vocab, words=words, spaces=[False]*6, sent_starts=boundaries)
            [hit] = matcher(doc)
            self.assertIs(hit.span.doc, doc)
            self.assertEqual((hit.span.start, hit.span.end, hit.span.label_), (3, 6, "pair"))
            [capture] = hit.captures
            self.assertEqual((capture.start, capture.end, capture.label_), (3, 4, "cat"))
            self.assertEqual(hit.raw["captures"][0]["hygienic_path"], "pair/cat")
        self.assertEqual(list(matcher(Doc(vocab))), [])

    @unittest.skipUnless(available("spacy", "lingrep_spacy_adapter"), "requires spaCy adapter")
    def test_spacy_empty_capture_stays_at_the_end_of_its_sentence(self):
        from spacy import blank
        from spacy.tokens import Doc
        from lingrep.spacy_matcher import Matcher

        doc = Doc(blank("xx").vocab, words=["猫", "  ", "犬"], spaces=[False] * 3,
                  sent_starts=[True, False, True])
        hits = list(Matcher("p := [] (?<opt> [text=z]?).")(doc))
        self.assertEqual([(h.captures[0].start, h.captures[0].end) for h in hits],
                         [(2, 2), (3, 3)])

    @unittest.skipUnless(available("stanza", "lingrep_stanza_adapter"), "requires Stanza adapter")
    def test_stanza_multiword_token_shares_source_coordinates(self):
        from stanza.models.common.doc import Document
        doc = Document([[{"id": (1, 2), "text": "du", "start_char": 0, "end_char": 2},
                         {"id": 1, "text": "de", "upos": "ADP"},
                         {"id": 2, "text": "le", "upos": "DET", "feats": "Number=Sing"}]],
                       text="du")
        from lingrep.stanza_matcher import Matcher

        matcher = Matcher("article := (?<det> [upos=DET]).")
        [match] = matcher(doc)
        self.assertEqual(match.words, (doc.sentences[0].words[1],))
        self.assertIs(match.words[0].parent, doc.sentences[0].tokens[0])
        [capture] = match.captures
        self.assertEqual((capture.name, capture.words), ("det", match.words))
        [s] = from_stanza(doc, attributes=["text", "upos", "feat.Number"])
        self.assertEqual(s.column("text"), ["de", "le"])
        self.assertEqual(s.column("feat.Number")[1], ["Sing"])
        self.assertEqual(s.column("surface"), ["du", "du"])
        self.assertEqual(s.char_spans, [{"start": 0, "end": 2}] * 2)
        [hit] = Query("[upos=DET].").compile(s.schema).find_matches(s)
        self.assertEqual((hit["span"], hit["char_span"]), ({"start": 1, "end": 2}, {"start": 0, "end": 2}))

    @unittest.skipUnless(available("stanza", "lingrep_stanza_adapter"), "requires Stanza adapter")
    def test_stanza_dependency_relations_bind_where_clauses(self):
        from stanza.models.common.doc import Document
        from lingrep.stanza_matcher import Matcher

        doc = Document([[{"id": 1, "text": "cats", "upos": "NOUN", "head": 2, "deprel": "nsubj",
                          "start_char": 0, "end_char": 4},
                         {"id": 2, "text": "sleep", "upos": "VERB", "head": 0, "deprel": "root",
                          "start_char": 5, "end_char": 10}]], text="cats sleep")
        attributes = ["text", "upos", "dep.head", "dep.rel"]
        [sentence] = from_stanza(doc, attributes=attributes)
        self.assertEqual((sentence.column("dep.head"), sentence.column("dep.rel")),
                         (["2", "0"], ["nsubj", "root"]))
        matcher = Matcher("s := (?<n> [upos=NOUN]) where (?<v> []) -[nsubj]-> $n.", attributes=attributes)
        [match] = matcher(doc)
        self.assertEqual([word.text for word in match.words], ["cats"])
        unparsed = Document([[{"id": 1, "text": "cats", "start_char": 0, "end_char": 4}]], text="cats")
        with self.assertRaisesRegex(ValueError, "no dependency annotations"):
            list(matcher(unparsed))

    @unittest.skipUnless(available("stanza", "lingrep_stanza_adapter"), "requires Stanza adapter")
    def test_stanza_reports_unsupported_documents_and_attributes(self):
        from stanza.models.common.doc import Document

        pretokenized = Document([[{"id": 1, "text": "le", "upos": "DET"}]])
        with self.assertRaisesRegex(ValueError, "source text"):
            list(from_stanza(pretokenized))
        doc = Document([[{"id": 1, "text": "le", "upos": "DET", "start_char": 0, "end_char": 2}]],
                       text="le")
        with self.assertRaisesRegex(ValueError, "unavailable key"):
            list(from_stanza(doc, attributes=["text", "norm", "upos"]))
        from lingrep.stanza_matcher import Matcher
        with self.assertRaisesRegex(ValueError, "unavailable key"):
            Matcher("[upos=DET].", attributes=["text", "upos", "dep.enhanced"])

    @unittest.skipUnless(available("spacy", "lingrep_spacy_adapter"), "requires spaCy adapter")
    def test_spacy_matcher_rejects_unsupported_attributes_when_constructed(self):
        from lingrep.spacy_matcher import Matcher
        with self.assertRaisesRegex(ValueError, "unsupported spaCy projection key"):
            Matcher("[text=猫].", attributes=["text", "surface"])

    @unittest.skipIf(available("lingrep_stanza_adapter"), "requires the Stanza adapter to be absent")
    def test_missing_adapter_names_the_distribution_to_install(self):
        with self.assertRaisesRegex(ModuleNotFoundError, "lingrep-stanza-adapter wheel"):
            list(from_stanza(object()))
