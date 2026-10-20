//! Tokenizer for the `--where` clause language.
//!
//! Every token carries its byte range so a diagnostic can point at the text
//! the author wrote rather than at the clause as a whole.
//!
//! Byte offsets are resolved with [`str::get`] rather than by indexing: the
//! workspace denies `clippy::string_slice`, and a clause is routinely
//! non-ASCII — `subject_area = 社会学` is the motivating example, not an edge
//! case.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Identifier(String),
    Value(String),
    /// `@section`. Structural facts live in their own namespace so a metadata
    /// key spelled `section` — which comes from a corpus the researcher does
    /// not control and cannot rename — stays reachable.
    Section,
    Within,
    NotWithin,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
    NotIn,
    Contains,
    NotContains,
    And,
    Or,
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    Comma,
}

#[derive(Clone, Debug)]
pub(crate) struct Lexed {
    pub(crate) token: Token,
    pub(crate) span: Range<usize>,
}

/// A bare value or identifier ends at whitespace or at one of these.
const DELIMITERS: &[char] = &['&', '|', '(', ')', '{', '}', ',', '=', '!', '<', '>', '"'];

type LexError = (String, Range<usize>);

pub(crate) fn lex(source: &str) -> Result<Vec<Lexed>, LexError> {
    let mut lexed = Vec::new();
    let mut cursor = 0usize;
    while let Some(rest) = source.get(cursor..).filter(|rest| !rest.is_empty()) {
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first.is_whitespace() {
            cursor += first.len_utf8();
            continue;
        }
        let start = cursor;
        let (token, width) = match first {
            '&' => (Token::And, 1),
            '|' => (Token::Or, 1),
            '(' => (Token::OpenParen, 1),
            ')' => (Token::CloseParen, 1),
            '{' => (Token::OpenBrace, 1),
            '}' => (Token::CloseBrace, 1),
            ',' => (Token::Comma, 1),
            '=' => (Token::Eq, 1),
            '!' if rest.starts_with("!=") => (Token::Ne, 2),
            '!' => {
                return Err(("`!` is only available as `!=`".to_owned(), start..start + 1));
            }
            '<' if rest.starts_with("<=") => (Token::Le, 2),
            '<' => (Token::Lt, 1),
            '>' if rest.starts_with(">=") => (Token::Ge, 2),
            '>' => (Token::Gt, 1),
            '"' => {
                let (value, width) = quoted(rest, start)?;
                lexed.push(Lexed {
                    token: Token::Value(value),
                    span: start..start + width,
                });
                cursor += width;
                continue;
            }
            _ => {
                let width = rest
                    .find(|character: char| {
                        character.is_whitespace() || DELIMITERS.contains(&character)
                    })
                    .unwrap_or(rest.len());
                let word = rest.get(..width).unwrap_or(rest);
                match word {
                    "within" => (Token::Within, width),
                    "in" => (Token::In, width),
                    "contains" => (Token::Contains, width),
                    "not" => {
                        let (token, consumed) = not_form(rest, width, start)?;
                        lexed.push(Lexed {
                            token,
                            span: start..start + consumed,
                        });
                        cursor += consumed;
                        continue;
                    }
                    // `@` is deliberately not a delimiter: a bare word already
                    // runs to the next delimiter or whitespace, so `@section`
                    // arrives here whole, and making it one would split a
                    // value like `a@b`.
                    _ if word.starts_with('@') => {
                        if word.starts_with("@document.") || word.starts_with("@sentence.") {
                            (Token::Identifier(word.to_owned()), width)
                        } else if word == "@section" {
                            (Token::Section, width)
                        } else {
                            return Err((
                                format!(
                                    "unknown scope selector `{word}`; use @document.KEY, @sentence.KEY, @section, or within(KIND, ...)"
                                ),
                                start..start + width,
                            ));
                        }
                    }
                    _ => (Token::Identifier(word.to_owned()), width),
                }
            }
        };
        lexed.push(Lexed {
            token,
            span: start..start + width,
        });
        cursor += width;
    }
    Ok(lexed)
}

/// `not` exists only as `not in` and `not contains`.
///
/// Refusing a bare `not` here rather than in the parser is what lets the
/// message name the construct: by the time the parser sees an identifier it no
/// longer knows the author wrote a negation.
fn not_form(rest: &str, width: usize, start: usize) -> Result<(Token, usize), LexError> {
    let tail = rest.get(width..).unwrap_or_default().trim_start();
    let gap = rest.len().saturating_sub(width).saturating_sub(tail.len());
    if tail.strip_prefix("within").is_some_and(ends_word) {
        return Ok((Token::NotWithin, width + gap + 6));
    }
    if tail.strip_prefix("in").is_some_and(ends_word) {
        return Ok((Token::NotIn, width + gap + 2));
    }
    if tail.strip_prefix("contains").is_some_and(ends_word) {
        return Ok((Token::NotContains, width + gap + 8));
    }
    Err((
        "use `not within(...)`, `not in` or `not contains`; general clause-level `not` is unsupported"
            .to_owned(),
        start..start + width,
    ))
}

fn ends_word(remainder: &str) -> bool {
    remainder
        .chars()
        .next()
        .is_none_or(|character| character.is_whitespace() || DELIMITERS.contains(&character))
}

fn quoted(rest: &str, start: usize) -> Result<(String, usize), LexError> {
    let mut value = String::new();
    let mut width = 1usize;
    for character in rest.get(1..).unwrap_or_default().chars() {
        width += character.len_utf8();
        if character == '"' {
            return Ok((value, width));
        }
        value.push(character);
    }
    Err(("unterminated quoted value".to_owned(), start..start + width))
}
