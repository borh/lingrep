import importlib.util
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("mecab_reference", ROOT / "mecab_reference.py")
MECAB = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
SPEC.loader.exec_module(MECAB)

# Default `mecab` output under UniDic-CWJ for 猫と猫が眠っている。, 承認された。 and
# OpenAIが発表した。; the unknown word OpenAI carries six features, and が quotes one.
CANNED = (
    "猫\t名詞,普通名詞,一般,*,*,*,ネコ,猫,猫,ネコ,猫,ネコ,和,*,*,*,*,*,*,体,ネコ,ネコ,ネコ,ネコ,1,C4,*,7918141678166528,28806\n"
    "と\t助詞,格助詞,*,*,*,*,ト,と,と,ト,と,ト,和,*,*,*,*,*,*,格助,ト,ト,ト,ト,*,\"動詞%F2@0,名詞%F1\",*,7099014038299136,25826\n"
    "猫\t名詞,普通名詞,一般,*,*,*,ネコ,猫,猫,ネコ,猫,ネコ,和,*,*,*,*,*,*,体,ネコ,ネコ,ネコ,ネコ,1,C4,*,7918141678166528,28806\n"
    "が\t助詞,格助詞,*,*,*,*,ガ,が,が,ガ,が,ガ,和,*,*,*,*,*,*,格助,ガ,ガ,ガ,ガ,*,\"動詞%F2@0,名詞%F1\",*,2168520431510016,7889\n"
    "眠っ\t動詞,一般,*,*,五段-ラ行,連用形-促音便,ネムル,眠る,眠っ,ネムッ,眠る,ネムル,和,*,*,*,*,*,*,用,ネムッ,ネムル,ネムッ,ネムル,0,C2,*,8166617094758017,29710\n"
    "て\t助詞,接続助詞,*,*,*,*,テ,て,て,テ,て,テ,和,*,*,*,*,*,*,接助,テ,テ,テ,テ,*,\"動詞%F1,形容詞%F2@-1\",*,6837321680953856,24874\n"
    "いる\t動詞,非自立可能,*,*,上一段-ア行,終止形-一般,イル,居る,いる,イル,いる,イル,和,*,*,*,*,*,*,用,イル,イル,イル,イル,0,C4,M4@1,710568013079169,2585\n"
    "。\t補助記号,句点,*,*,*,*,*,。,。,*,。,*,記号,*,*,*,*,*,*,補助,*,*,*,*,*,*,*,6880571302400,25\n"
    "EOS\n"
    "承認\t名詞,普通名詞,サ変可能,*,*,*,ショウニン,承認,承認,ショーニン,承認,ショーニン,漢,*,*,*,*,*,*,体,ショウニン,ショウニン,ショウニン,ショウニン,0,C2,*,4832646203458048,17581\n"
    "さ\t動詞,非自立可能,*,*,サ行変格,未然形-サ,スル,為る,さ,サ,する,スル,和,*,*,*,*,*,*,用,サ,スル,サ,スル,0,C5,*,5370298291593857,19537\n"
    "れ\t助動詞,*,*,*,助動詞-レル,連用形-一般,レル,れる,れ,レ,れる,レル,和,*,*,*,*,*,*,助動,レ,レル,レ,レル,*,\"動詞%F2@0\",*,10800155602813505,39291\n"
    "た\t助動詞,*,*,*,助動詞-タ,終止形-一般,タ,た,た,タ,た,タ,和,*,*,*,*,*,*,助動,タ,タ,タ,タ,*,\"動詞%F2@1,形容詞%F4@-2\",*,5948916285711019,21642\n"
    "。\t補助記号,句点,*,*,*,*,*,。,。,*,。,*,記号,*,*,*,*,*,*,補助,*,*,*,*,*,*,*,6880571302400,25\n"
    "EOS\n"
    "OpenAI\t名詞,固有名詞,人名,一般,*,*\n"
    "が\t助詞,格助詞,*,*,*,*,ガ,が,が,ガ,が,ガ,和,*,*,*,*,*,*,格助,ガ,ガ,ガ,ガ,*,\"動詞%F2@0,名詞%F1\",*,2168520431510016,7889\n"
    "発表\t名詞,普通名詞,サ変可能,*,*,*,ハッピョウ,発表,発表,ハッピョー,発表,ハッピョー,漢,*,*,*,*,*,*,体,ハッピョウ,ハッピョウ,ハッピョウ,ハッピョウ,0,C2,*,8216933895905792,29893\n"
    "し\t動詞,非自立可能,*,*,サ行変格,連用形-一般,スル,為る,し,シ,する,スル,和,*,*,*,*,*,*,用,シ,スル,シ,スル,0,C5,*,5370298291593857,19537\n"
    "た\t助動詞,*,*,*,助動詞-タ,終止形-一般,タ,た,た,タ,た,タ,和,*,*,*,*,*,*,助動,タ,タ,タ,タ,*,\"動詞%F2@1,形容詞%F4@-2\",*,5948916285711019,21642\n"
    "。\t補助記号,句点,*,*,*,*,*,。,。,*,。,*,記号,*,*,*,*,*,*,補助,*,*,*,*,*,*,*,6880571302400,25\n"
    "EOS\n"
)


def canned_mecab(monkeypatch):
    calls = []

    def run(command, **kwargs):
        calls.append((command, kwargs["input"]))
        return subprocess.CompletedProcess(command, 0, stdout=CANNED, stderr="")

    monkeypatch.setattr(MECAB.subprocess, "run", run)
    return calls


def test_offsets_follow_repeated_surfaces_and_predicates_absorb_sahen_nouns(monkeypatch):
    calls = canned_mecab(monkeypatch)
    first, second, third = MECAB.analyze(["猫と猫が眠っている。", "承認された。", "OpenAIが発表した。"])

    assert calls[0][1] == "猫と猫が眠っている。\n承認された。\nOpenAIが発表した。\n"
    assert calls[0][0] == ["mecab"]
    assert first[1].pos3 == "" and first[1].lemma == "と"
    assert third[0] == {
        "surface": "OpenAI",
        "pos1": "名詞",
        "pos2": "固有名詞",
        "pos3": "人名",
        "pos4": "一般",
        "ctype": "",
        "cform": "",
        "lemma": "",
        "start": 0,
        "end": 6,
    }
    assert (third[2].surface, third[2].start, third[2].end) == ("発表", 7, 9)
    assert [(token.surface, token.start, token.end) for token in first[:4]] == [
        ("猫", 0, 1),
        ("と", 1, 2),
        ("猫", 2, 3),
        ("が", 3, 4),
    ]
    assert first[6].lemma == "居る" and first[6].cform == "終止形-一般"
    assert MECAB.sentence_final(first, 6) and not MECAB.sentence_final(first, 5)
    assert MECAB.predicate_start(second, 1) == 0
    assert MECAB.auxiliary_run(second, 2) == 4
    assert second[2].ctype == "助動詞-レル"


def test_main_writes_sorted_evidence_rows_with_character_spans(monkeypatch, tmp_path):
    canned_mecab(monkeypatch)
    documents = tmp_path / "input.jsonl"
    documents.write_text(
        json.dumps(
            {
                "eval_document_id": "doc-001",
                "sentences": ["猫と猫が眠っている。", "承認された。", "OpenAIが発表した。"],
            }
        )
        + "\n"
    )
    output = tmp_path / "evidence.jsonl"

    def select(tokens, document):
        for index, token in enumerate(tokens):
            if token.pos1 == "動詞" and index + 1 < len(tokens) and tokens[index + 1].ctype == "助動詞-レル":
                yield MECAB.predicate_start(tokens, index), MECAB.auxiliary_run(tokens, index + 1), "passive"
            if token.pos2 == "普通名詞" and index + 1 < len(tokens) and tokens[index + 1].surface == "が":
                yield index, index + 1, "subject"

    monkeypatch.setattr(MECAB.sys, "argv", ["run-analysis", str(documents), str(output)])
    MECAB.main(select)

    assert [json.loads(line) for line in output.read_text().splitlines()] == [
        {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": "doc-001",
            "sentence_ordinal": 0,
            "start_char": 2,
            "end_char": 3,
            "label": "subject",
        },
        {
            "schema": "lingrep-linguistic-eval.evidence/v1",
            "eval_document_id": "doc-001",
            "sentence_ordinal": 1,
            "start_char": 0,
            "end_char": 5,
            "label": "passive",
        },
    ]
