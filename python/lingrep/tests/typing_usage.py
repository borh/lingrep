from typing_extensions import assert_type
from lingrep import CompiledQuery, Ling, Query, Schema, Sentence, Token, Tokenizer
from lingrep.records import RawMatch


def use_bindings(tok: Tokenizer, sentence: Sentence) -> None:
    assert_type(tok.tokenize("猫"), list[str])
    assert_type(tok.analyze("猫", attributes=["lemma"])[0], Token)
    assert_type(sentence[0].lemma, str | None)
    assert_type(tok.schema, Schema)
    query = Query("[upos=NOUN].")
    assert_type(query.compile(sentence.schema), CompiledQuery)
    assert_type(sentence.column("lemma"), list[str | None])
    assert_type(next(tok.match_pipe(["猫"], query)), list[RawMatch])
    for match in query.compile().find_matches(sentence):
        assert_type(match["captures"][0]["name"], str)
        assert_type(match["span"]["start"], int)
    with Ling().search(query, text="猫") as results:
        for hit in results:
            assert_type(hit["surface"], str)
