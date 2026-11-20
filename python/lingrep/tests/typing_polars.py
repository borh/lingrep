from typing_extensions import assert_type
import polars as pl
from lingrep import Query, Tokenizer
from lingrep.polars_expr import matches, tokenize


def expressions(text: pl.Expr, tokenizer: Tokenizer, query: Query) -> None:
    assert_type(tokenize(text, tokenizer=tokenizer), pl.Expr)
    assert_type(tokenize(text, tokenizer=tokenizer, attributes=["lemma", "char_start"]), pl.Expr)
    assert_type(matches(text, query, tokenizer=tokenizer), pl.Expr)
    assert_type(matches(text, query.compile(), tokenizer=tokenizer), pl.Expr)
