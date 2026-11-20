import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from wo_predicate_compare import read_conllu


class ComparisonTests(unittest.TestCase):
    def test_ordinals_skip_blank_sentences_and_include_the_implicit_document(self):
        word = "1\t本\t本\tNOUN\t_\t_\t0\troot\t_\t"
        blank = "1\t \t \tPUNCT\t_\t_\t0\troot\t_\t_"
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "input.conllu"
            path.write_text(
                f"{word}_\n\n# text =  \n{blank}\n\n{word}BunsetuBILabel=B\n\n"
                f"# newdoc id = two\n{word}BunsetuBILabel=I",
                encoding="utf-8",
            )
            self.assertEqual(
                [(s.doc, s.ordinal, s.has_bunsetsu_boundaries) for s in read_conllu(path)],
                [(0, 0, False), (0, 1, True), (1, 0, False)],
            )
